//! One network's connection: name resolution, Happy Eyeballs connection
//! racing (RFC 8305), TLS, registration (Modern IRC "Connection
//! Registration" with IRCv3 capability negotiation), keepalive, flood
//! control and reconnection with backoff (DESIGN.md §6.1–6.7).

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mio::net::TcpStream;
use mio::{Interest, Token};
use rustls::ClientConnection;
use rustls::pki_types::ServerName;

use crate::config::known_hosts::Fingerprint;
use crate::config::networks::CertificatePolicy;
use crate::irc::{Fallback, Message, decode_line, serialize};

use super::flood::Flood;
use super::framing::LineReader;
use super::reactor::Ctx;
use super::sasl::{Exchange, Step};
use super::{CertificateProblem, NetEvent, NetworkId, tls, x509};

pub use super::sasl::Mechanism;

/// How long Happy Eyeballs lets one attempt run before starting the next
/// (RFC 8305 §8, "Connection Attempt Delay").
const ATTEMPT_DELAY: Duration = Duration::from_millis(250);
/// A connection that hasn't completed by now has failed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Silence after which RubyChat checks the connection with PING.
const IDLE_PING: Duration = Duration::from_secs(90);
/// How long that PING may go unanswered before the connection is dead.
const PING_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(300);
/// Capabilities requested when offered (DESIGN.md §6.4). `sts` is never
/// requested, as its specification forbids.
const WANTED_CAPS: &[&str] = &[
    "message-tags",
    "server-time",
    "multi-prefix",
    "userhost-in-names",
    "account-notify",
    "account-tag",
    "away-notify",
    "batch",
    "cap-notify",
    "chghost",
    "echo-message",
    "extended-join",
    "invite-notify",
    "labeled-response",
    "setname",
    "standard-replies",
    "draft/chathistory",
    "draft/event-playback",
    "draft/read-marker",
];

/// How a session logs in beyond the server password (DESIGN.md §6.5).
#[derive(Clone, Default, PartialEq, Eq)]
pub enum Login {
    #[default]
    None,
    Sasl {
        mechanism: Mechanism,
        account: String,
        password: String,
    },
    /// `IDENTIFY` to NickServ once registered.
    NickServ { account: String, password: String },
}

impl std::fmt::Debug for Login {
    /// Passwords stay out of logs and panics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Login::None => write!(f, "None"),
            Login::Sasl {
                mechanism, account, ..
            } => write!(f, "Sasl({mechanism:?}, {account:?}, ********)"),
            Login::NickServ { account, .. } => write!(f, "NickServ({account:?}, ********)"),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConnectConfig {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub nick: String,
    pub alt_nicks: Vec<String>,
    pub username: String,
    pub realname: String,
    /// A server or bouncer password, sent with PASS.
    pub password: Option<String>,
    pub login: Login,
    /// Disconnect rather than continue unidentified when SASL fails.
    pub require_login: bool,
    pub fallback: Fallback,
    /// Flood control's penalty per line and the window it may run ahead.
    pub flood: (Duration, Duration),
    /// Reconnect after a lost connection.
    pub reconnect: bool,
    /// The first reconnection delay; it doubles up to five minutes.
    pub reconnect_delay: Duration,
    /// QUIT's message; RubyChat's name when empty.
    pub quit_message: String,
    pub certificate: CertificatePolicy,
    /// Certificates accepted for this server whatever their issuer: its
    /// pin, and one trusted once.
    pub pins: Vec<Fingerprint>,
    /// A PEM file with a client certificate chain and its key.
    pub client_certificate: Option<std::path::PathBuf>,
}

impl std::fmt::Debug for ConnectConfig {
    /// The server password stays out of logs and panics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("nick", &self.nick)
            .field("password", &self.password.as_ref().map(|_| "********"))
            .field("login", &self.login)
            .field("certificate", &self.certificate)
            .finish_non_exhaustive()
    }
}

impl ConnectConfig {
    pub fn new(host: impl Into<String>, port: u16, tls: bool, nick: impl Into<String>) -> Self {
        let nick = nick.into();
        Self {
            host: host.into(),
            port,
            tls,
            username: nick.clone(),
            realname: String::from("RubyChat"),
            nick,
            alt_nicks: Vec::new(),
            password: None,
            login: Login::None,
            require_login: false,
            fallback: Fallback::default(),
            flood: (Duration::from_secs(2), Duration::from_secs(10)),
            reconnect: true,
            reconnect_delay: Duration::from_secs(2),
            quit_message: String::new(),
            certificate: CertificatePolicy::Verify,
            pins: Vec::new(),
            client_certificate: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    Resolving,
    Connecting,
    /// Connected (and encrypted, with TLS); registering.
    Registering,
    Registered {
        nick: String,
    },
    /// Disconnected; reconnecting after `retry_in`, or not at all.
    Disconnected {
        reason: String,
        retry_in: Option<Duration>,
    },
    /// Stopped because retrying cannot help: a rejected certificate, a ban.
    Failed {
        reason: String,
    },
}

/// Why a connection ended, and whether trying again could help.
struct Close {
    reason: String,
    retry: bool,
    /// The certificate that wasn't trusted, for the prompt.
    certificate: Option<Box<CertificateProblem>>,
}

impl Close {
    fn retry(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry: true,
            certificate: None,
        }
    }

    fn fatal(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry: false,
            certificate: None,
        }
    }
}

enum State {
    Idle,
    Resolving,
    Connecting(Connecting),
    Open(Box<Open>),
    Waiting { until: Instant },
}

struct Connecting {
    pending: VecDeque<SocketAddr>,
    attempts: Vec<Attempt>,
    next_attempt: Instant,
    deadline: Instant,
    last_error: Option<String>,
}

struct Attempt {
    token: Token,
    stream: TcpStream,
    addr: SocketAddr,
}

struct Open {
    token: Token,
    stream: TcpStream,
    tls: Option<Box<ClientConnection>>,
    /// The TLS connection's verifier, which keeps a refused certificate.
    verifier: Option<Arc<tls::Verifier>>,
    interest: Interest,
    reader: LineReader,
    /// Plaintext connections' unsent bytes; TLS buffers its own.
    outgoing: Vec<u8>,
    /// Lines waiting for flood control.
    queue: VecDeque<String>,
    flood: Flood,
    session: Session,
    last_received: Instant,
    handshake_reported: bool,
    /// The reason the server gave with ERROR before closing.
    error: Option<String>,
}

impl Open {
    fn write(&mut self, line: &str) {
        match &mut self.tls {
            // Before the handshake completes rustls holds plaintext until it
            // can send it, so registration can be written at once.
            Some(tls) => {
                let mut writer = tls.writer();
                let _ = writer.write_all(line.as_bytes());
                let _ = writer.write_all(b"\r\n");
            }
            None => {
                self.outgoing.extend_from_slice(line.as_bytes());
                self.outgoing.extend_from_slice(b"\r\n");
            }
        }
    }
}

pub(super) struct Link {
    id: NetworkId,
    config: ConnectConfig,
    state: State,
    /// Distinguishes this connection's lookups from abandoned ones.
    generation: u64,
    /// Connections that failed in a row, for backoff.
    failures: u32,
}

impl Link {
    pub fn new(id: NetworkId, config: ConnectConfig) -> Self {
        Self {
            id,
            config,
            state: State::Idle,
            generation: 0,
            failures: 0,
        }
    }

    pub fn set_config(&mut self, config: ConnectConfig) {
        self.config = config;
    }

    fn status(&self, ctx: &mut Ctx<'_>, status: LinkStatus) {
        (ctx.sink)(NetEvent::Status(self.id, status));
    }

    fn notice(&self, ctx: &mut Ctx<'_>, text: String) {
        (ctx.sink)(NetEvent::Notice(self.id, text));
    }

    /// Starts connecting, abandoning any connection in progress.
    pub fn connect(&mut self, ctx: &mut Ctx<'_>) {
        self.teardown(ctx);
        self.generation += 1;
        self.state = State::Resolving;
        self.status(ctx, LinkStatus::Resolving);
        self.notice(ctx, format!("Looking up {}…", self.config.host));
        ctx.resolver.lookup(
            self.id,
            self.generation,
            self.config.host.clone(),
            self.config.port,
        );
    }

    pub fn resolved(
        &mut self,
        ctx: &mut Ctx<'_>,
        generation: u64,
        result: Result<Vec<SocketAddr>, String>,
    ) {
        if generation != self.generation || !matches!(self.state, State::Resolving) {
            return;
        }
        match result {
            Ok(addrs) if !addrs.is_empty() => {
                self.state = State::Connecting(Connecting {
                    pending: interleave(addrs).into(),
                    attempts: Vec::new(),
                    next_attempt: ctx.now,
                    deadline: ctx.now + CONNECT_TIMEOUT,
                    last_error: None,
                });
                self.status(ctx, LinkStatus::Connecting);
                self.start_attempt(ctx);
            }
            Ok(_) => {
                let reason = format!("{} has no addresses", self.config.host);
                self.retry_later(ctx, reason);
            }
            Err(error) => {
                let reason = format!("Could not look up {}: {error}", self.config.host);
                self.retry_later(ctx, reason);
            }
        }
    }

    /// Starts the next address's attempt; with none left and none running,
    /// the connection has failed.
    fn start_attempt(&mut self, ctx: &mut Ctx<'_>) {
        let State::Connecting(connecting) = &mut self.state else {
            return;
        };
        while let Some(addr) = connecting.pending.pop_front() {
            match TcpStream::connect(addr) {
                Ok(mut stream) => {
                    let token = ctx.tokens.alloc(self.id);
                    if let Err(error) = ctx.registry.register(
                        &mut stream,
                        token,
                        Interest::READABLE | Interest::WRITABLE,
                    ) {
                        ctx.tokens.free(token);
                        connecting.last_error = Some(format!("{addr}: {error}"));
                        continue;
                    }
                    connecting.attempts.push(Attempt {
                        token,
                        stream,
                        addr,
                    });
                    connecting.next_attempt = ctx.now + ATTEMPT_DELAY;
                    (ctx.sink)(NetEvent::Notice(self.id, format!("Connecting to {addr}…")));
                    return;
                }
                Err(error) => connecting.last_error = Some(format!("{addr}: {error}")),
            }
        }
        if connecting.attempts.is_empty() {
            let reason = connecting
                .last_error
                .take()
                .unwrap_or_else(|| String::from("no address answered"));
            self.retry_later(ctx, format!("Could not connect: {reason}"));
        }
    }

    /// Readiness on one of this link's sockets.
    pub fn ready(&mut self, ctx: &mut Ctx<'_>, token: Token, readable: bool) {
        let attempt = match &self.state {
            State::Connecting(connecting) => {
                connecting.attempts.iter().position(|a| a.token == token)
            }
            _ => None,
        };
        if let Some(index) = attempt {
            self.attempt_ready(ctx, index);
            return;
        }
        if matches!(&self.state, State::Open(open) if open.token == token) {
            let result = if readable { self.read(ctx) } else { Ok(()) };
            if let Err(close) = result.and_then(|()| self.flush(ctx)) {
                self.lost(ctx, close);
            }
        }
    }

    /// A connection attempt became writable: connected, or failed.
    fn attempt_ready(&mut self, ctx: &mut Ctx<'_>, index: usize) {
        let State::Connecting(connecting) = &mut self.state else {
            return;
        };
        let attempt = &mut connecting.attempts[index];
        let outcome = match attempt.stream.take_error() {
            Ok(Some(error)) | Err(error) => Err(error.to_string()),
            Ok(None) => match attempt.stream.peer_addr() {
                Ok(_) => Ok(()),
                // Not finished: the readiness was spurious.
                Err(error) if error.kind() == io::ErrorKind::NotConnected => return,
                Err(error) => Err(error.to_string()),
            },
        };
        let mut attempt = connecting.attempts.swap_remove(index);
        match outcome {
            Ok(()) => {
                for mut other in connecting.attempts.drain(..) {
                    let _ = ctx.registry.deregister(&mut other.stream);
                    ctx.tokens.free(other.token);
                }
                self.open(ctx, attempt);
            }
            Err(error) => {
                let _ = ctx.registry.deregister(&mut attempt.stream);
                ctx.tokens.free(attempt.token);
                connecting.last_error = Some(format!("{}: {error}", attempt.addr));
                if connecting.attempts.is_empty() {
                    self.start_attempt(ctx);
                }
            }
        }
    }

    fn open(&mut self, ctx: &mut Ctx<'_>, attempt: Attempt) {
        let Attempt {
            token,
            mut stream,
            addr,
        } = attempt;
        let _ = stream.set_nodelay(true);
        let mut verifier = None;
        let tls = if self.config.tls {
            let config = &self.config;
            let connection = match (ServerName::try_from(config.host.clone()), ctx.tls) {
                (Ok(name), Some(shared)) => tls::client_config(
                    shared,
                    config.certificate,
                    &config.pins,
                    config.client_certificate.as_deref(),
                )
                .and_then(|(client, check)| {
                    verifier = Some(check);
                    ClientConnection::new(client, name).map_err(|e| e.to_string())
                }),
                (Ok(_), None) => Err(String::from("TLS is unavailable")),
                (Err(_), _) => Err(format!("{} is not a valid TLS server name", config.host)),
            };
            match connection {
                Ok(connection) => Some(Box::new(connection)),
                Err(reason) => {
                    let _ = ctx.registry.deregister(&mut stream);
                    ctx.tokens.free(token);
                    self.state = State::Idle;
                    self.stopped(ctx, reason);
                    return;
                }
            }
        } else {
            None
        };
        self.notice(
            ctx,
            if tls.is_some() {
                format!("Connected to {addr}; negotiating TLS…")
            } else {
                format!("Connected to {addr}, unencrypted")
            },
        );
        let session = Session::new(&self.config);
        let registration = session.registration(&self.config);
        self.state = State::Open(Box::new(Open {
            token,
            stream,
            tls,
            verifier,
            interest: Interest::READABLE | Interest::WRITABLE,
            reader: LineReader::default(),
            outgoing: Vec::new(),
            queue: VecDeque::new(),
            flood: Flood::new(self.config.flood.0, self.config.flood.1),
            session,
            last_received: ctx.now,
            handshake_reported: false,
            error: None,
        }));
        self.status(ctx, LinkStatus::Registering);
        // Registration goes out at once, without waiting for a banner.
        for line in registration {
            self.send_line(ctx, line, false);
        }
        if let Err(close) = self.flush(ctx) {
            self.lost(ctx, close);
        }
    }

    /// A line the user asked to send.
    pub fn send(&mut self, ctx: &mut Ctx<'_>, line: String) {
        if !matches!(self.state, State::Open(_)) {
            self.notice(ctx, String::from("Not connected; nothing was sent."));
            return;
        }
        self.send_line(ctx, line, false);
        if let Err(close) = self.flush(ctx) {
            self.lost(ctx, close);
        }
    }

    /// Queues `line` behind flood control, or with `urgent` sends it at once:
    /// PONG and QUIT never wait. Neither does registration, a short fixed
    /// exchange that servers expect at once.
    fn send_line(&mut self, ctx: &mut Ctx<'_>, line: String, urgent: bool) {
        let State::Open(open) = &mut self.state else {
            return;
        };
        if urgent || !open.session.registered {
            open.write(&line);
            (ctx.sink)(NetEvent::Sent(self.id, redact(&line)));
        } else {
            open.queue.push_back(line);
            self.drain_queue(ctx);
        }
    }

    fn drain_queue(&mut self, ctx: &mut Ctx<'_>) {
        let State::Open(open) = &mut self.state else {
            return;
        };
        while open.flood.ready_at(ctx.now) <= ctx.now {
            let Some(line) = open.queue.pop_front() else {
                break;
            };
            open.flood.charge(ctx.now);
            open.write(&line);
            (ctx.sink)(NetEvent::Sent(self.id, redact(&line)));
        }
    }

    /// Writes what the socket takes now and watches for writability while
    /// anything remains.
    fn flush(&mut self, ctx: &mut Ctx<'_>) -> Result<(), Close> {
        let State::Open(open) = &mut self.state else {
            return Ok(());
        };
        let wants_write = match &mut open.tls {
            Some(tls) => {
                while tls.wants_write() {
                    match tls.write_tls(&mut open.stream) {
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => return Err(Close::retry(error.to_string())),
                    }
                }
                tls.wants_write()
            }
            None => {
                while !open.outgoing.is_empty() {
                    match open.stream.write(&open.outgoing) {
                        Ok(0) => return Err(Close::retry("the connection closed while sending")),
                        Ok(written) => {
                            open.outgoing.drain(..written);
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => return Err(Close::retry(error.to_string())),
                    }
                }
                !open.outgoing.is_empty()
            }
        };
        let interest = if wants_write {
            Interest::READABLE | Interest::WRITABLE
        } else {
            Interest::READABLE
        };
        if interest != open.interest {
            ctx.registry
                .reregister(&mut open.stream, open.token, interest)
                .map_err(|e| Close::retry(e.to_string()))?;
            open.interest = interest;
        }
        Ok(())
    }

    fn read(&mut self, ctx: &mut Ctx<'_>) -> Result<(), Close> {
        let mut lines: Vec<Vec<u8>> = Vec::new();
        let mut closed: Option<Close> = None;
        let mut handshake: Option<String> = None;
        let dropped;
        {
            let State::Open(open) = &mut self.state else {
                return Ok(());
            };
            let mut buffer = [0u8; 16 * 1024];
            loop {
                let received = match &mut open.tls {
                    None => match open.stream.read(&mut buffer) {
                        Ok(0) => None,
                        Ok(count) => {
                            open.reader.push(&buffer[..count]);
                            Some(count)
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            closed = Some(Close::retry(error.to_string()));
                            break;
                        }
                    },
                    Some(tls) => match tls.read_tls(&mut open.stream) {
                        Ok(0) => None,
                        Ok(count) => {
                            let state = match tls.process_new_packets() {
                                Ok(state) => state,
                                Err(error) => {
                                    let refusal = open
                                        .verifier
                                        .as_ref()
                                        .and_then(|verifier| verifier.refusal());
                                    closed = Some(tls_close(&error, refusal, &self.config));
                                    break;
                                }
                            };
                            let mut available = state.plaintext_bytes_to_read();
                            while available > 0 {
                                match tls.reader().read(&mut buffer) {
                                    Ok(n) if n > 0 => {
                                        open.reader.push(&buffer[..n]);
                                        available = available.saturating_sub(n);
                                    }
                                    _ => break,
                                }
                            }
                            if state.peer_has_closed() {
                                None
                            } else {
                                Some(count)
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            closed = Some(Close::retry(error.to_string()));
                            break;
                        }
                    },
                };
                match received {
                    Some(_) => open.last_received = ctx.now,
                    None => {
                        let reason = open
                            .error
                            .take()
                            .unwrap_or_else(|| String::from("The server closed the connection"));
                        closed = Some(Close {
                            retry: !banned(&reason),
                            reason,
                            certificate: None,
                        });
                        break;
                    }
                }
            }
            if let Some(tls) = &open.tls
                && !open.handshake_reported
                && !tls.is_handshaking()
            {
                open.handshake_reported = true;
                handshake = Some(tls::describe(tls));
            }
            open.reader.lines(|line| lines.push(line.to_vec()));
            dropped = open.reader.take_dropped();
        }
        if let Some(handshake) = handshake {
            self.notice(ctx, format!("Encrypted: {handshake}"));
        }
        if dropped > 0 {
            self.notice(
                ctx,
                format!("Dropped {dropped} line(s) longer than IRC allows"),
            );
        }
        for line in lines {
            self.handle_line(ctx, &line);
        }
        closed.map_or(Ok(()), Err)
    }

    fn handle_line(&mut self, ctx: &mut Ctx<'_>, bytes: &[u8]) {
        let text = decode_line(bytes, self.config.fallback);
        let Ok(message) = Message::parse(&text) else {
            return;
        };
        let actions = {
            let State::Open(open) = &mut self.state else {
                return;
            };
            if message.command().eq_ignore_ascii_case("ERROR") {
                open.error = message.last_param().map(str::to_owned);
            }
            open.session.handle(&message, ctx.now)
        };
        (ctx.sink)(NetEvent::Received(self.id, message));
        for action in actions {
            match action {
                Action::Send(line) => self.send_line(ctx, line, false),
                Action::Urgent(line) => self.send_line(ctx, line, true),
                Action::Registered(nick) => {
                    self.failures = 0;
                    self.status(ctx, LinkStatus::Registered { nick });
                }
                Action::Lag(lag) => (ctx.sink)(NetEvent::Lag(self.id, lag)),
                Action::Notice(text) => self.notice(ctx, text),
                Action::Fail(reason) => {
                    let _ = self.flush(ctx);
                    self.lost(ctx, Close::fatal(reason));
                    return;
                }
                Action::Caps(caps) => (ctx.sink)(NetEvent::Caps(self.id, caps)),
                Action::StsUpgrade(port) => {
                    // "It MUST close the insecure connection and reconnect
                    // securely on the stated port", with verification: the
                    // UI reconnects with settings for that port (its pins).
                    self.config.tls = true;
                    self.config.port = port;
                    if self.config.certificate == CertificatePolicy::None {
                        self.config.certificate = CertificatePolicy::Verify;
                    }
                    self.notice(
                        ctx,
                        format!(
                            "The server requires TLS (STS): reconnecting securely to port {port}"
                        ),
                    );
                    self.teardown(ctx);
                    self.generation += 1;
                    self.status(ctx, LinkStatus::Connecting);
                    self.sts(ctx, None, true);
                    return;
                }
                Action::StsDuration(duration) => self.sts(ctx, Some(duration), false),
            }
        }
    }

    /// When this link next needs `on_timer`.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        match &self.state {
            State::Waiting { until } => Some(*until),
            State::Connecting(connecting) if connecting.pending.is_empty() => {
                Some(connecting.deadline)
            }
            State::Connecting(connecting) => Some(connecting.next_attempt.min(connecting.deadline)),
            State::Open(open) => {
                let keepalive = match open.session.ping {
                    Some((_, sent)) => sent + PING_TIMEOUT,
                    None => open.last_received + IDLE_PING,
                };
                Some(if open.queue.is_empty() {
                    keepalive
                } else {
                    keepalive.min(open.flood.ready_at(now))
                })
            }
            State::Idle | State::Resolving => None,
        }
    }

    pub fn on_timer(&mut self, ctx: &mut Ctx<'_>) {
        let now = ctx.now;
        match &mut self.state {
            State::Waiting { until } if *until <= now => self.connect(ctx),
            State::Connecting(connecting) if now >= connecting.deadline => {
                let reason = connecting
                    .last_error
                    .take()
                    .unwrap_or_else(|| String::from("no answer"));
                self.retry_later(ctx, format!("Connecting timed out ({reason})"));
            }
            State::Connecting(connecting)
                if now >= connecting.next_attempt && !connecting.pending.is_empty() =>
            {
                self.start_attempt(ctx)
            }
            State::Open(open) => {
                let silent = now.saturating_duration_since(open.last_received);
                match open.session.ping {
                    Some((_, sent)) if now >= sent + PING_TIMEOUT && open.last_received < sent => {
                        let close =
                            Close::retry(format!("No reply for {} seconds", silent.as_secs()));
                        self.lost(ctx, close);
                        return;
                    }
                    None if silent >= IDLE_PING => {
                        open.session.ping_counter += 1;
                        let counter = open.session.ping_counter;
                        open.session.ping = Some((counter, now));
                        self.send_line(ctx, format!("PING :rubychat-{counter}"), true);
                    }
                    _ => {}
                }
                self.drain_queue(ctx);
                if let Err(close) = self.flush(ctx) {
                    self.lost(ctx, close);
                }
            }
            _ => {}
        }
    }

    /// The user's disconnect: QUIT, then close without reconnecting.
    pub fn disconnect(&mut self, ctx: &mut Ctx<'_>, quit: Option<&str>) {
        let fallback = if self.config.quit_message.is_empty() {
            "RubyChat"
        } else {
            &self.config.quit_message
        };
        let quit = quit
            .filter(|quit| !quit.is_empty())
            .unwrap_or(fallback)
            .to_string();
        if let State::Open(open) = &mut self.state {
            let line =
                serialize(&[], None, "QUIT", &[&quit]).unwrap_or_else(|_| String::from("QUIT"));
            open.write(&line);
            if let Some(tls) = &mut open.tls {
                tls.send_close_notify();
            }
            (ctx.sink)(NetEvent::Sent(self.id, line));
            let _ = self.flush(ctx);
        }
        self.teardown(ctx);
        self.generation += 1;
        self.status(
            ctx,
            LinkStatus::Disconnected {
                reason: String::from("Disconnected"),
                retry_in: None,
            },
        );
    }

    fn lost(&mut self, ctx: &mut Ctx<'_>, close: Close) {
        self.teardown(ctx);
        if let Some(problem) = close.certificate {
            (ctx.sink)(NetEvent::Certificate(self.id, problem));
        }
        if close.retry {
            self.retry_later(ctx, close.reason);
        } else {
            self.stopped(ctx, close.reason);
        }
    }

    fn stopped(&mut self, ctx: &mut Ctx<'_>, reason: String) {
        self.notice(ctx, format!("{reason}. Not reconnecting."));
        self.status(ctx, LinkStatus::Failed { reason });
    }

    fn retry_later(&mut self, ctx: &mut Ctx<'_>, reason: String) {
        self.teardown(ctx);
        if !self.config.reconnect {
            self.notice(ctx, format!("{reason}."));
            self.status(
                ctx,
                LinkStatus::Disconnected {
                    reason,
                    retry_in: None,
                },
            );
            return;
        }
        self.failures += 1;
        let seed = self.id.0 ^ u64::from(self.failures) ^ clock_seed();
        let delay = backoff(self.config.reconnect_delay, self.failures, seed);
        self.state = State::Waiting {
            until: ctx.now + delay,
        };
        self.notice(
            ctx,
            format!("{reason}. Reconnecting in {}.", seconds(delay)),
        );
        self.status(
            ctx,
            LinkStatus::Disconnected {
                reason,
                retry_in: Some(delay),
            },
        );
    }

    /// Tells the UI about an STS policy for this server.
    fn sts(&self, ctx: &mut Ctx<'_>, duration: Option<u64>, upgraded: bool) {
        (ctx.sink)(NetEvent::Sts(
            self.id,
            super::Sts {
                host: self.config.host.clone(),
                port: self.config.port,
                duration,
                upgraded,
            },
        ));
    }

    /// Closes whatever sockets the link holds; it becomes idle. A secure
    /// connection's STS policy is rescheduled from now, as the
    /// specification asks when connections close.
    fn teardown(&mut self, ctx: &mut Ctx<'_>) {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Connecting(mut connecting) => {
                for mut attempt in connecting.attempts.drain(..) {
                    let _ = ctx.registry.deregister(&mut attempt.stream);
                    ctx.tokens.free(attempt.token);
                }
            }
            State::Open(mut open) => {
                let _ = ctx.registry.deregister(&mut open.stream);
                ctx.tokens.free(open.token);
                if let Some(duration) = open.session.sts_duration {
                    self.sts(ctx, Some(duration), false);
                }
            }
            State::Idle | State::Resolving | State::Waiting { .. } => {}
        }
    }

    /// Closes for good as the network thread stops, with the network's quit
    /// message if it has one.
    pub fn shutdown(&mut self, ctx: &mut Ctx<'_>) {
        if matches!(self.state, State::Open(_)) {
            self.disconnect(ctx, None);
        } else {
            self.teardown(ctx);
        }
    }
}

/// What the session asks the link to do after a message.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Send behind flood control.
    Send(String),
    /// Send at once.
    Urgent(String),
    Registered(String),
    Lag(Duration),
    /// For the server buffer.
    Notice(String),
    /// Close without reconnecting, for this reason.
    Fail(String),
    /// The capabilities enabled now.
    Caps(Vec<String>),
    /// An insecure connection's STS upgrade policy: reconnect with TLS on
    /// this port.
    StsUpgrade(u16),
    /// A secure connection's STS persistence policy, in seconds.
    StsDuration(u64),
}

/// SASL's progress in a session.
enum Sasl {
    /// Not configured, or over.
    Off,
    /// Configured; waiting for the capability.
    Ready,
    Running(Exchange),
    /// Abandoned with `AUTHENTICATE *`, for this reason; 906 ends it.
    Aborting(String),
}

/// Registration and the protocol work that must not wait for the UI.
struct Session {
    nick: String,
    candidates: Vec<String>,
    tried: usize,
    registered: bool,
    /// Capabilities offered so far by a multi-line `CAP LS`.
    offered: Vec<String>,
    enabled: Vec<String>,
    login: Login,
    require_login: bool,
    sasl: Sasl,
    /// The `sasl` capability's mechanisms from `CAP LS`: `None` while it
    /// isn't offered, an empty list when offered without one.
    sasl_offer: Option<Vec<String>>,
    logged_in: bool,
    ping: Option<(u64, Instant)>,
    ping_counter: u64,
    /// The connection is TLS, so an STS persistence policy counts.
    secure: bool,
    /// The STS persistence policy advertised on this secure connection, in
    /// seconds, for rescheduling its expiry when the connection closes.
    sts_duration: Option<u64>,
}

impl Session {
    fn new(config: &ConnectConfig) -> Self {
        let mut candidates = vec![config.nick.clone()];
        candidates.extend(config.alt_nicks.iter().cloned());
        candidates.push(format!("{}_", config.nick));
        candidates.push(format!("{}__", config.nick));
        Self {
            nick: config.nick.clone(),
            candidates,
            tried: 0,
            registered: false,
            offered: Vec::new(),
            enabled: Vec::new(),
            login: config.login.clone(),
            require_login: config.require_login,
            sasl: if matches!(config.login, Login::Sasl { .. }) {
                Sasl::Ready
            } else {
                Sasl::Off
            },
            sasl_offer: None,
            logged_in: false,
            ping: None,
            ping_counter: 0,
            secure: config.tls,
            sts_duration: None,
        }
    }

    /// Modern IRC's registration order: `CAP LS 302`, `PASS`, then `NICK`
    /// and `USER`; negotiation follows the server's replies.
    fn registration(&self, config: &ConnectConfig) -> Vec<String> {
        let mut lines = vec![String::from("CAP LS 302")];
        if let Some(password) = &config.password
            && let Ok(line) = serialize(&[], None, "PASS", &[password])
        {
            lines.push(line);
        }
        lines.push(
            serialize(&[], None, "NICK", &[&self.nick])
                .unwrap_or_else(|_| String::from("NICK RubyChat")),
        );
        let username = if config.username.is_empty() || config.username.contains(' ') {
            "rubychat"
        } else {
            &config.username
        };
        lines.push(
            serialize(&[], None, "USER", &[username, "0", "*", &config.realname])
                .unwrap_or_else(|_| String::from("USER rubychat 0 * :RubyChat")),
        );
        lines
    }

    fn handle(&mut self, message: &Message, now: Instant) -> Vec<Action> {
        let command = message.command();
        if command.eq_ignore_ascii_case("PING") {
            let token = message.param(0).unwrap_or("");
            return serialize(&[], None, "PONG", &[token])
                .map(Action::Urgent)
                .into_iter()
                .collect();
        }
        if command.eq_ignore_ascii_case("PONG") {
            if let Some((counter, sent)) = self.ping
                && message.last_param() == Some(&format!("rubychat-{counter}"))
            {
                self.ping = None;
                return vec![Action::Lag(now.saturating_duration_since(sent))];
            }
            return Vec::new();
        }
        if command.eq_ignore_ascii_case("CAP") {
            return self.cap(message);
        }
        if command.eq_ignore_ascii_case("AUTHENTICATE") {
            let Sasl::Running(exchange) = &mut self.sasl else {
                return Vec::new();
            };
            return match exchange.receive(message.param(0).unwrap_or("+")) {
                Step::Wait => Vec::new(),
                Step::Send(lines) => lines.into_iter().map(Action::Send).collect(),
                // The server answers the abort with 906.
                Step::Abort(reason) => {
                    self.sasl = Sasl::Aborting(reason);
                    vec![Action::Send(String::from("AUTHENTICATE *"))]
                }
            };
        }
        if command.eq_ignore_ascii_case("NICK") {
            if self.registered
                && message
                    .nick()
                    .is_some_and(|nick| nick.eq_ignore_ascii_case(&self.nick))
                && let Some(nick) = message.param(0)
            {
                self.nick = nick.to_string();
            }
            return Vec::new();
        }
        match command {
            "001" => {
                // A required login that never happened: no SASL, or a
                // server that skipped it (IRCv3 SASL 3.2).
                if self.require_login && matches!(self.login, Login::Sasl { .. }) && !self.logged_in
                {
                    return vec![Action::Fail(String::from(
                        "The server registered RubyChat without logging in",
                    ))];
                }
                self.registered = true;
                if let Some(nick) = message.param(0) {
                    self.nick = nick.to_string();
                }
                let mut actions = vec![Action::Registered(self.nick.clone())];
                if let Login::NickServ { account, password } = &self.login {
                    let identify = if account.is_empty() {
                        format!("IDENTIFY {password}")
                    } else {
                        format!("IDENTIFY {account} {password}")
                    };
                    if let Ok(line) = serialize(&[], None, "PRIVMSG", &["NickServ", &identify]) {
                        actions.push(Action::Notice(String::from("Identifying to NickServ…")));
                        actions.push(Action::Send(line));
                    }
                }
                actions
            }
            // RPL_SASLSUCCESS (IRCv3 SASL 3.1).
            "903" if matches!(self.sasl, Sasl::Running(_)) => {
                self.sasl = Sasl::Off;
                self.logged_in = true;
                self.end()
            }
            // ERR_NICKLOCKED, ERR_SASLFAIL, ERR_SASLTOOLONG, ERR_SASLABORTED.
            "902" | "904" | "905" | "906"
                if matches!(self.sasl, Sasl::Running(_) | Sasl::Aborting(_)) =>
            {
                let reason = match std::mem::replace(&mut self.sasl, Sasl::Off) {
                    Sasl::Aborting(reason) => reason,
                    _ => message
                        .last_param()
                        .unwrap_or("the server refused")
                        .to_string(),
                };
                self.sasl_failed(reason)
            }
            // ERR_SASLALREADY.
            "907" if matches!(self.sasl, Sasl::Running(_)) => {
                self.sasl = Sasl::Off;
                self.logged_in = true;
                self.end()
            }
            // ERR_ERRONEUSNICKNAME, ERR_NICKNAMEINUSE, ERR_NICKCOLLISION and
            // ERR_UNAVAILRESOURCE before registration: try the next nick.
            "432" | "433" | "436" | "437" if !self.registered => {
                self.tried += 1;
                self.nick = self
                    .candidates
                    .get(self.tried)
                    .cloned()
                    .unwrap_or_else(|| format!("{}{}", self.candidates[0], self.tried));
                serialize(&[], None, "NICK", &[&self.nick])
                    .map(Action::Send)
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// IRCv3 capability negotiation (`CAP LS 302`, `ACK`, `NAK`, `NEW`,
    /// `DEL`).
    fn cap(&mut self, message: &Message) -> Vec<Action> {
        let subcommand = message.param(1).unwrap_or("").to_ascii_uppercase();
        let caps = message.last_param().unwrap_or("");
        let names = || caps.split(' ').filter(|cap| !cap.is_empty());
        // IRCv3 STS: an insecure connection upgrades at once; a secure one
        // keeps the persistence policy.
        if matches!(subcommand.as_str(), "LS" | "NEW")
            && let Some(policy) = names().find_map(|cap| cap.strip_prefix("sts="))
        {
            let (port, duration) = sts_policy(policy);
            match (self.secure, port, duration) {
                (false, Some(port), _) => return vec![Action::StsUpgrade(port)],
                (true, _, Some(duration)) => {
                    self.sts_duration = Some(duration);
                    let mut actions = vec![Action::StsDuration(duration)];
                    actions.extend(self.cap_after_sts(message, &subcommand));
                    return actions;
                }
                _ => {}
            }
        }
        self.cap_after_sts(message, &subcommand)
    }

    fn cap_after_sts(&mut self, message: &Message, subcommand: &str) -> Vec<Action> {
        let caps = message.last_param().unwrap_or("");
        let names = || caps.split(' ').filter(|cap| !cap.is_empty());
        // `sasl=PLAIN,EXTERNAL` lists the mechanisms (IRCv3 SASL 3.2).
        if matches!(subcommand, "LS" | "NEW") {
            for cap in names().filter(|cap| cap_name(cap) == "sasl") {
                let mechanisms = cap.split_once('=').map(|(_, list)| {
                    list.split(',')
                        .filter(|m| !m.is_empty())
                        .map(str::to_string)
                        .collect()
                });
                self.sasl_offer = Some(mechanisms.unwrap_or_default());
            }
        }
        match subcommand {
            "LS" => {
                self.offered
                    .extend(names().map(|cap| cap_name(cap).to_string()));
                // All but the last line of a 302 reply carry "*" before the list.
                if message.params().len() >= 4 && message.param(2) == Some("*") {
                    return Vec::new();
                }
                let offered = std::mem::take(&mut self.offered);
                self.request(offered.iter().map(String::as_str), true)
            }
            "NEW" => {
                let offered: Vec<String> = names().map(|cap| cap_name(cap).to_string()).collect();
                self.request(offered.iter().map(String::as_str), false)
            }
            "ACK" => {
                for cap in names() {
                    match cap.strip_prefix('-') {
                        Some(name) => self.enabled.retain(|enabled| enabled != name),
                        None => self.enabled.push(cap.to_string()),
                    }
                }
                let mut actions = vec![Action::Caps(self.enabled.clone())];
                if names().any(|cap| cap == "sasl") && matches!(self.sasl, Sasl::Ready) {
                    actions.extend(self.start_sasl());
                } else {
                    actions.extend(self.end());
                }
                actions
            }
            "NAK" if names().any(|cap| cap == "sasl") && matches!(self.sasl, Sasl::Ready) => {
                self.sasl_failed(String::from("the server refused the sasl capability"))
            }
            "NAK" => self.end(),
            "DEL" => {
                for cap in names() {
                    self.enabled.retain(|enabled| enabled != cap_name(cap));
                }
                vec![Action::Caps(self.enabled.clone())]
            }
            _ => Vec::new(),
        }
    }

    /// Requests the wanted capabilities among `offered`, `sasl` among them
    /// when it offers the configured mechanism, or ends negotiation when
    /// there are none. `complete`: the last line of the first `CAP LS`,
    /// when a missing `sasl` is final.
    fn request<'a>(
        &mut self,
        offered: impl Iterator<Item = &'a str>,
        complete: bool,
    ) -> Vec<Action> {
        let offered: Vec<&str> = offered.collect();
        let mut wanted: Vec<&str> = WANTED_CAPS
            .iter()
            .copied()
            .filter(|cap| offered.contains(cap) && !self.enabled.iter().any(|e| e == cap))
            .collect();
        let mut actions = Vec::new();
        if let (Sasl::Ready, Login::Sasl { mechanism, .. }) = (&self.sasl, &self.login) {
            let name = mechanism.name();
            match &self.sasl_offer {
                Some(list)
                    if list.is_empty() || list.iter().any(|m| m.eq_ignore_ascii_case(name)) =>
                {
                    wanted.push("sasl")
                }
                Some(list) => {
                    let reason =
                        format!("the server offers SASL {} but not {name}", list.join(", "));
                    actions = self.sasl_failed(reason);
                }
                None if complete => {
                    actions = self.sasl_failed(String::from("the server doesn't offer SASL"))
                }
                None => {}
            }
            if actions
                .iter()
                .any(|action| matches!(action, Action::Fail(_)))
            {
                return actions;
            }
        }
        // One CAP END at most, and none while requesting more.
        actions.retain(|action| !matches!(action, Action::Send(line) if line == "CAP END"));
        if wanted.is_empty() {
            actions.extend(self.end());
        } else {
            actions.push(Action::Send(format!("CAP REQ :{}", wanted.join(" "))));
        }
        actions
    }

    fn start_sasl(&mut self) -> Vec<Action> {
        let Login::Sasl {
            mechanism,
            account,
            password,
        } = &self.login
        else {
            return self.end();
        };
        match Exchange::new(*mechanism, account, password) {
            Ok(exchange) => {
                let start = exchange.start();
                self.sasl = Sasl::Running(exchange);
                vec![
                    Action::Notice(format!("Logging in with SASL {}…", mechanism.name())),
                    Action::Send(start),
                ]
            }
            Err(reason) => self.sasl_failed(reason),
        }
    }

    /// SASL didn't work: disconnect when the login is required, else go on
    /// unidentified.
    fn sasl_failed(&mut self, reason: String) -> Vec<Action> {
        self.sasl = Sasl::Off;
        if self.require_login {
            return vec![Action::Fail(format!("Login failed: {reason}"))];
        }
        let mut actions = vec![Action::Notice(format!(
            "Login failed: {reason}. Continuing without logging in"
        ))];
        actions.extend(self.end());
        actions
    }

    /// `CAP END` resumes registration; once registered there is nothing to end.
    fn end(&self) -> Vec<Action> {
        if self.registered {
            Vec::new()
        } else {
            vec![Action::Send(String::from("CAP END"))]
        }
    }
}

/// An STS policy's port and duration (IRCv3 STS, "Details"); unknown keys
/// and malformed values are ignored.
fn sts_policy(value: &str) -> (Option<u16>, Option<u64>) {
    let (mut port, mut duration) = (None, None);
    for token in value.split(',') {
        match token.split_once('=') {
            Some(("port", value)) => port = value.parse().ok(),
            Some(("duration", value)) => duration = value.parse().ok(),
            _ => {}
        }
    }
    (port, duration)
}

/// A capability's name without its `=value` (IRCv3 CAP LS 302 values).
fn cap_name(cap: &str) -> &str {
    cap.split_once('=').map_or(cap, |(name, _)| name)
}

/// Whether the server's reason for closing means retrying cannot help.
fn banned(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    ["k-lined", "g-lined", "z-lined", "d-lined", "banned"]
        .iter()
        .any(|word| reason.contains(word))
}

/// Why TLS failed. A refused certificate stops reconnection until someone
/// decides to trust it.
fn tls_close(
    error: &rustls::Error,
    refusal: Option<tls::Refusal>,
    config: &ConnectConfig,
) -> Close {
    match (error, refusal) {
        (_, Some(refusal)) => {
            let reason = if refusal.changed {
                format!("The server's certificate changed: {}", refusal.reason)
            } else {
                format!("The server's certificate isn't trusted: {}", refusal.reason)
            };
            Close {
                reason,
                retry: false,
                certificate: Some(Box::new(CertificateProblem {
                    host: config.host.clone(),
                    port: config.port,
                    fingerprint: refusal.fingerprint,
                    reason: refusal.reason,
                    changed: refusal.changed,
                    facts: x509::parse(&refusal.der),
                })),
            }
        }
        (rustls::Error::InvalidCertificate(reason), None) => Close::fatal(format!(
            "The server's certificate was rejected ({reason:?})"
        )),
        (error, None) => Close::retry(format!("TLS failed: {error}")),
    }
}

/// Lines worth keeping out of view: passwords and authentication.
pub fn redact(line: &str) -> String {
    let upper = line
        .get(..line.len().min(24))
        .unwrap_or("")
        .to_ascii_uppercase();
    if upper.starts_with("PASS ") {
        return String::from("PASS ********");
    }
    if upper.starts_with("AUTHENTICATE ")
        && !matches!(
            line[13..].trim(),
            "+" | "*" | "PLAIN" | "EXTERNAL" | "SCRAM-SHA-256"
        )
    {
        return String::from("AUTHENTICATE ********");
    }
    if upper.starts_with("PRIVMSG NICKSERV ") && line.to_ascii_uppercase().contains("IDENTIFY") {
        return String::from("PRIVMSG NickServ :IDENTIFY ********");
    }
    line.to_string()
}

/// Alternates address families, starting with the system's first choice
/// (RFC 8305 §4).
fn interleave(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let Some(first) = addrs.first() else {
        return addrs;
    };
    let first_is_v6 = first.is_ipv6();
    let count = addrs.len();
    let (mut preferred, mut other): (VecDeque<SocketAddr>, VecDeque<SocketAddr>) = addrs
        .into_iter()
        .partition(|addr| addr.is_ipv6() == first_is_v6);
    let mut out = Vec::with_capacity(count);
    loop {
        match (preferred.pop_front(), other.pop_front()) {
            (None, None) => break,
            (a, b) => out.extend(a.into_iter().chain(b)),
        }
    }
    out
}

/// Exponential backoff with ±20% jitter, so many clients don't reconnect in
/// step.
fn backoff(base: Duration, failures: u32, seed: u64) -> Duration {
    let exponent = failures.saturating_sub(1).min(16);
    let delay = base
        .saturating_mul(1u32 << exponent)
        .min(MAX_RECONNECT_DELAY);
    let mut x = seed | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    let jitter = (x % 4001) as f64 / 10_000.0 - 0.2;
    delay.mul_f64(1.0 + jitter)
}

fn clock_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as u64)
}

fn seconds(delay: Duration) -> String {
    let seconds = delay.as_secs_f32();
    if seconds < 10.0 {
        format!("{seconds:.1} s")
    } else {
        format!("{} s", seconds.round())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(&ConnectConfig::new("irc.example.net", 6697, true, "ruby"))
    }

    fn handle(session: &mut Session, line: &str) -> Vec<Action> {
        session.handle(&Message::parse(line).unwrap(), Instant::now())
    }

    fn login_session(login: Login, require: bool) -> Session {
        let mut config = ConnectConfig::new("irc.example.net", 6697, true, "ruby");
        config.login = login;
        config.require_login = require;
        Session::new(&config)
    }

    fn sent(actions: &[Action]) -> Vec<&str> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::Send(line) | Action::Urgent(line) => Some(line.as_str()),
                _ => None,
            })
            .collect()
    }

    fn plain() -> Login {
        Login::Sasl {
            mechanism: Mechanism::Plain,
            account: String::from("jilles"),
            password: String::from("sesame"),
        }
    }

    #[test]
    fn sasl_plain_logs_in_before_cap_end() {
        // IRCv3 SASL 3.1's example, with CAP 302 mechanism lists (3.2).
        let mut s = login_session(plain(), true);
        assert_eq!(
            sent(&handle(
                &mut s,
                ":srv CAP * LS :multi-prefix sasl=PLAIN,EXTERNAL"
            )),
            ["CAP REQ :multi-prefix sasl"]
        );
        assert_eq!(
            sent(&handle(&mut s, ":srv CAP ruby ACK :multi-prefix sasl")),
            ["AUTHENTICATE PLAIN"]
        );
        assert_eq!(
            sent(&handle(&mut s, "AUTHENTICATE +")),
            ["AUTHENTICATE amlsbGVzAGppbGxlcwBzZXNhbWU="]
        );
        assert!(
            handle(
                &mut s,
                ":srv 900 ruby ruby!r@h jilles :You are now logged in as jilles"
            )
            .is_empty()
        );
        assert_eq!(
            sent(&handle(
                &mut s,
                ":srv 903 ruby :SASL authentication successful"
            )),
            ["CAP END"]
        );
        assert_eq!(
            handle(&mut s, ":srv 001 ruby :Welcome"),
            [Action::Registered(String::from("ruby"))]
        );
    }

    #[test]
    fn required_logins_fail_closed() {
        let failed = |actions: Vec<Action>| {
            actions
                .iter()
                .any(|action| matches!(action, Action::Fail(_)))
        };
        // No sasl capability at all.
        let mut s = login_session(plain(), true);
        assert!(failed(handle(&mut s, ":srv CAP * LS :multi-prefix")));
        // Not the configured mechanism.
        let mut s = login_session(plain(), true);
        assert!(failed(handle(&mut s, ":srv CAP * LS :sasl=EXTERNAL")));
        // Wrong password.
        let mut s = login_session(plain(), true);
        handle(&mut s, ":srv CAP * LS :sasl");
        handle(&mut s, ":srv CAP ruby ACK :sasl");
        handle(&mut s, "AUTHENTICATE +");
        assert!(failed(handle(
            &mut s,
            ":srv 904 ruby :SASL authentication failed"
        )));
        // A server without CAP that registers straight away.
        let mut s = login_session(plain(), true);
        assert!(failed(handle(&mut s, ":srv 001 ruby :Welcome")));
        // Not required: carry on unidentified.
        let mut s = login_session(plain(), false);
        handle(&mut s, ":srv CAP * LS :sasl");
        handle(&mut s, ":srv CAP ruby ACK :sasl");
        handle(&mut s, "AUTHENTICATE +");
        let actions = handle(&mut s, ":srv 904 ruby :SASL authentication failed");
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, Action::Fail(_)))
        );
        assert_eq!(sent(&actions), ["CAP END"]);
        let mut s = login_session(plain(), false);
        let actions = handle(&mut s, ":srv CAP * LS :multi-prefix");
        assert_eq!(
            sent(&actions),
            ["CAP REQ :multi-prefix"],
            "one request, no early CAP END"
        );
    }

    #[test]
    fn scram_aborts_on_a_forged_server_and_external_sends_nothing() {
        let scram = Login::Sasl {
            mechanism: Mechanism::ScramSha256,
            account: String::from("user"),
            password: String::from("pencil"),
        };
        let mut s = login_session(scram, false);
        handle(&mut s, ":srv CAP * LS :sasl=SCRAM-SHA-256");
        assert_eq!(
            sent(&handle(&mut s, ":srv CAP ruby ACK :sasl")),
            ["AUTHENTICATE SCRAM-SHA-256"]
        );
        let first = handle(&mut s, "AUTHENTICATE +");
        assert!(sent(&first)[0].starts_with("AUTHENTICATE "));
        // A server-first message whose nonce isn't ours: abandon.
        let forged = super::super::sasl::base64_encode(b"r=forged,s=AAAA,i=4096");
        assert_eq!(
            sent(&handle(&mut s, &format!("AUTHENTICATE {forged}"))),
            ["AUTHENTICATE *"]
        );
        let ended = handle(&mut s, ":srv 906 ruby :SASL authentication aborted");
        assert!(
            ended
                .iter()
                .any(|a| matches!(a, Action::Notice(n) if n.contains("nonce"))),
            "{ended:?}"
        );
        let external = Login::Sasl {
            mechanism: Mechanism::External,
            account: String::new(),
            password: String::new(),
        };
        let mut s = login_session(external, true);
        handle(&mut s, ":srv CAP * LS :sasl");
        assert_eq!(
            sent(&handle(&mut s, ":srv CAP ruby ACK :sasl")),
            ["AUTHENTICATE EXTERNAL"]
        );
        assert_eq!(sent(&handle(&mut s, "AUTHENTICATE +")), ["AUTHENTICATE +"]);
    }

    #[test]
    fn nickserv_identifies_once_registered() {
        let login = Login::NickServ {
            account: String::from("ruby"),
            password: String::from("hunter2"),
        };
        let mut s = login_session(login, false);
        let actions = handle(&mut s, ":srv 001 ruby :Welcome");
        assert_eq!(sent(&actions), ["PRIVMSG NickServ :IDENTIFY ruby hunter2"]);
        assert_eq!(
            redact(sent(&actions)[0]),
            "PRIVMSG NickServ :IDENTIFY ********"
        );
        assert!(
            !format!(
                "{:?}",
                ConnectConfig {
                    login: Login::NickServ {
                        account: String::new(),
                        password: String::from("hunter2")
                    },
                    ..ConnectConfig::new("h", 1, true, "n")
                }
            )
            .contains("hunter2")
        );
    }

    #[test]
    fn registration_follows_the_recommended_order() {
        let mut config = ConnectConfig::new("irc.example.net", 6697, true, "ruby");
        config.password = Some(String::from("hunter two"));
        config.realname = String::from("Ruby M");
        assert_eq!(
            Session::new(&config).registration(&config),
            [
                "CAP LS 302",
                "PASS :hunter two",
                "NICK ruby",
                "USER ruby 0 * :Ruby M"
            ]
        );
    }

    #[test]
    fn multiline_cap_ls_requests_wanted_caps_then_ends() {
        let mut s = session();
        assert!(handle(&mut s, ":srv CAP * LS * :multi-prefix server-time").is_empty());
        assert_eq!(
            handle(&mut s, ":srv CAP * LS :sasl=PLAIN,EXTERNAL message-tags"),
            [Action::Send(String::from(
                "CAP REQ :message-tags server-time multi-prefix"
            ))]
        );
        let enabled = vec![
            String::from("message-tags"),
            String::from("server-time"),
            String::from("multi-prefix"),
        ];
        assert_eq!(
            handle(
                &mut s,
                ":srv CAP * ACK :message-tags server-time multi-prefix"
            ),
            [
                Action::Caps(enabled.clone()),
                Action::Send(String::from("CAP END"))
            ]
        );
        assert_eq!(s.enabled, enabled);
        // Nothing wanted: end at once.
        let mut other = session();
        assert_eq!(
            handle(&mut other, ":srv CAP * LS :draft/example"),
            [Action::Send(String::from("CAP END"))]
        );
        assert_eq!(
            handle(&mut other, ":srv CAP * NAK :x"),
            [Action::Send(String::from("CAP END"))]
        );
        // After registration, NEW requests and DEL removes, with no END.
        let mut late = session();
        handle(&mut late, ":srv 001 ruby :Welcome");
        assert_eq!(
            handle(&mut late, ":srv CAP ruby NEW :server-time=x"),
            [Action::Send(String::from("CAP REQ :server-time"))]
        );
        assert_eq!(
            handle(&mut late, ":srv CAP ruby ACK :server-time"),
            [Action::Caps(vec![String::from("server-time")])]
        );
        assert_eq!(
            handle(&mut late, ":srv CAP ruby DEL :server-time"),
            [Action::Caps(Vec::new())]
        );
        assert!(late.enabled.is_empty());
    }

    #[test]
    fn sts_upgrades_insecure_connections_and_keeps_secure_policies() {
        // Insecure: the upgrade policy wins, whatever else is offered.
        let mut insecure =
            Session::new(&ConnectConfig::new("irc.example.net", 6667, false, "ruby"));
        assert_eq!(
            handle(
                &mut insecure,
                ":srv CAP * LS :multi-prefix sts=port=6697,duration=300"
            ),
            [Action::StsUpgrade(6697)]
        );
        // A persistence policy alone on an insecure connection is ignored.
        let mut ignored = Session::new(&ConnectConfig::new("irc.example.net", 6667, false, "ruby"));
        assert_eq!(
            handle(&mut ignored, ":srv CAP * LS :sts=duration=300 multi-prefix"),
            [Action::Send(String::from("CAP REQ :multi-prefix"))]
        );
        // Secure: the duration is kept and negotiation goes on; unknown
        // keys are skipped, and `sts` is never requested.
        let mut secure = session();
        assert_eq!(
            handle(
                &mut secure,
                ":srv CAP * LS :sts=unknown,duration=2592000,port=6697 multi-prefix"
            ),
            [
                Action::StsDuration(2_592_000),
                Action::Send(String::from("CAP REQ :multi-prefix"))
            ]
        );
        assert_eq!(secure.sts_duration, Some(2_592_000));
        assert_eq!(sts_policy("port=x,duration=0"), (None, Some(0)));
    }

    #[test]
    fn taken_nicks_fall_back_and_ping_is_answered() {
        let mut s = session();
        assert_eq!(
            handle(&mut s, ":srv 433 * ruby :Nickname is already in use"),
            [Action::Send(String::from("NICK ruby_"))]
        );
        assert_eq!(
            handle(&mut s, ":srv 433 * ruby_ :in use"),
            [Action::Send(String::from("NICK ruby__"))]
        );
        assert_eq!(
            handle(&mut s, ":srv 433 * ruby__ :in use"),
            [Action::Send(String::from("NICK ruby3"))]
        );
        assert_eq!(
            handle(&mut s, ":srv 001 ruby3 :Welcome"),
            [Action::Registered(String::from("ruby3"))]
        );
        assert!(
            handle(&mut s, ":srv 433 ruby3 x :in use").is_empty(),
            "after registration the UI handles it"
        );
        assert_eq!(
            handle(&mut s, "PING :irc.example.net"),
            [Action::Urgent(String::from("PONG irc.example.net"))]
        );
        handle(&mut s, ":ruby3!u@h NICK :ruby");
        assert_eq!(s.nick, "ruby");
    }

    #[test]
    fn helpers_interleave_back_off_and_redact() {
        let v4 = |n: u8| SocketAddr::from(([10, 0, 0, n], 6697));
        let v6 = |n: u16| SocketAddr::from(([0x2001, 0xdb8, 0, 0, 0, 0, 0, n], 6697));
        assert_eq!(
            interleave(vec![v6(1), v6(2), v6(3), v4(1)]),
            [v6(1), v4(1), v6(2), v6(3)]
        );
        let base = Duration::from_secs(2);
        for failures in 1..12 {
            let delay = backoff(base, failures, failures as u64 * 7919).as_secs_f64();
            let nominal = (2.0 * 2f64.powi(failures as i32 - 1)).min(300.0);
            assert!(
                delay >= nominal * 0.8 - 1e-9 && delay <= nominal * 1.2 + 1e-9,
                "{failures}: {delay}"
            );
        }
        assert_eq!(redact("PASS :secret"), "PASS ********");
        assert_eq!(redact("AUTHENTICATE PLAIN"), "AUTHENTICATE PLAIN");
        assert_eq!(redact("AUTHENTICATE cnVieQ=="), "AUTHENTICATE ********");
        assert_eq!(
            redact("PRIVMSG NickServ :IDENTIFY pw"),
            "PRIVMSG NickServ :IDENTIFY ********"
        );
        assert_eq!(redact("PRIVMSG #rust :hi"), "PRIVMSG #rust :hi");
        assert!(banned("Closing Link: K-Lined (spam)") && !banned("Ping timeout"));
    }
}
