//! One network's connection: name resolution, Happy Eyeballs connection
//! racing (RFC 8305), TLS, registration (Modern IRC "Connection
//! Registration" with IRCv3 capability negotiation), keepalive, flood
//! control and reconnection with backoff (DESIGN.md §6.1–6.7).

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use mio::net::TcpStream;
use mio::{Interest, Token};
use rustls::ClientConnection;
use rustls::pki_types::ServerName;

use crate::irc::{Fallback, Message, decode_line, serialize};

use super::flood::Flood;
use super::framing::LineReader;
use super::reactor::Ctx;
use super::{NetEvent, NetworkId, tls};

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
/// Capabilities requested when offered; each arrives with the milestone
/// that handles it (DESIGN.md §6.4).
const WANTED_CAPS: &[&str] = &[
    "message-tags",
    "server-time",
    "multi-prefix",
    "userhost-in-names",
];

#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub fallback: Fallback,
    /// The first reconnection delay; it doubles up to five minutes.
    pub reconnect_delay: Duration,
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
            fallback: Fallback::default(),
            reconnect_delay: Duration::from_secs(2),
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
}

impl Close {
    fn retry(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry: true,
        }
    }

    fn fatal(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry: false,
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
        let tls = if self.config.tls {
            let connection = match ServerName::try_from(self.config.host.clone()) {
                Ok(name) => match ctx.tls {
                    Some(config) => {
                        ClientConnection::new(config.clone(), name).map_err(|e| e.to_string())
                    }
                    None => Err(String::from("TLS is unavailable")),
                },
                Err(_) => Err(format!(
                    "{} is not a valid TLS server name",
                    self.config.host
                )),
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
            interest: Interest::READABLE | Interest::WRITABLE,
            reader: LineReader::default(),
            outgoing: Vec::new(),
            queue: VecDeque::new(),
            flood: Flood::default(),
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
                                    closed = Some(tls_close(&error));
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
        if let State::Open(open) = &mut self.state {
            let line = serialize(&[], None, "QUIT", &[quit.unwrap_or("RubyChat")])
                .unwrap_or_else(|_| String::from("QUIT"));
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

    /// Closes whatever sockets the link holds; it becomes idle.
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
            }
            State::Idle | State::Resolving | State::Waiting { .. } => {}
        }
    }

    /// Closes for good as the network thread stops.
    pub fn shutdown(&mut self, ctx: &mut Ctx<'_>, quit: &str) {
        if matches!(self.state, State::Open(_)) {
            self.disconnect(ctx, Some(quit));
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
    ping: Option<(u64, Instant)>,
    ping_counter: u64,
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
            ping: None,
            ping_counter: 0,
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
                self.registered = true;
                if let Some(nick) = message.param(0) {
                    self.nick = nick.to_string();
                }
                vec![Action::Registered(self.nick.clone())]
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
        match subcommand.as_str() {
            "LS" => {
                self.offered
                    .extend(names().map(|cap| cap_name(cap).to_string()));
                // All but the last line of a 302 reply carry "*" before the list.
                if message.params().len() >= 4 && message.param(2) == Some("*") {
                    return Vec::new();
                }
                let offered = std::mem::take(&mut self.offered);
                self.request(offered.iter().map(String::as_str))
            }
            "NEW" => {
                let offered: Vec<String> = names().map(|cap| cap_name(cap).to_string()).collect();
                self.request(offered.iter().map(String::as_str))
            }
            "ACK" => {
                for cap in names() {
                    match cap.strip_prefix('-') {
                        Some(name) => self.enabled.retain(|enabled| enabled != name),
                        None => self.enabled.push(cap.to_string()),
                    }
                }
                self.end()
            }
            "NAK" => self.end(),
            "DEL" => {
                for cap in names() {
                    self.enabled.retain(|enabled| enabled != cap_name(cap));
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Requests the wanted capabilities among `offered`, or ends
    /// negotiation when there are none.
    fn request<'a>(&mut self, offered: impl Iterator<Item = &'a str>) -> Vec<Action> {
        let offered: Vec<&str> = offered.collect();
        let wanted: Vec<&str> = WANTED_CAPS
            .iter()
            .copied()
            .filter(|cap| offered.contains(cap) && !self.enabled.iter().any(|e| e == cap))
            .collect();
        if wanted.is_empty() {
            return self.end();
        }
        vec![Action::Send(format!("CAP REQ :{}", wanted.join(" ")))]
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

fn tls_close(error: &rustls::Error) -> Close {
    match error {
        rustls::Error::InvalidCertificate(reason) => Close::fatal(format!(
            "The server's certificate was rejected ({reason:?})"
        )),
        error => Close::retry(format!("TLS failed: {error}")),
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
        assert_eq!(
            handle(
                &mut s,
                ":srv CAP * ACK :message-tags server-time multi-prefix"
            ),
            [Action::Send(String::from("CAP END"))]
        );
        assert_eq!(s.enabled, ["message-tags", "server-time", "multi-prefix"]);
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
        assert!(handle(&mut late, ":srv CAP ruby ACK :server-time").is_empty());
        handle(&mut late, ":srv CAP ruby DEL :server-time");
        assert!(late.enabled.is_empty());
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
