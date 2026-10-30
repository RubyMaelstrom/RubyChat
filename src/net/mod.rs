//! The network thread (DESIGN.md §5–6). One mio reactor runs every
//! connection—resolution, connection racing, TLS, registration, keepalive,
//! reconnection and flood control—so the UI thread never touches a socket.
//! The UI sends `NetCommand`s and receives `NetEvent`s.

mod flood;
mod framing;
mod link;
mod reactor;
mod sasl;
mod tls;
pub mod x509;

use std::io;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use mio::{Poll, Waker};

use crate::irc::Message;

pub use link::{ConnectConfig, LinkStatus, Login, Mechanism, redact};
pub use tls::fingerprint;

use crate::config::known_hosts::Fingerprint;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NetworkId(pub u64);

#[derive(Debug)]
pub enum NetCommand {
    /// Connects, or reconnects with a new configuration.
    Connect(NetworkId, ConnectConfig),
    /// A configuration for the next attempt, without reconnecting now: the
    /// next of a network's servers.
    Update(NetworkId, ConnectConfig),
    /// A line to send, without its line ending.
    Send(NetworkId, String),
    /// QUIT with this message, then stay disconnected.
    Disconnect(NetworkId, Option<String>),
    /// Disconnects and forgets the network.
    Close(NetworkId),
    /// QUIT everywhere and stop the thread.
    Shutdown,
}

#[derive(Debug)]
pub enum NetEvent {
    Status(NetworkId, LinkStatus),
    Received(NetworkId, Message),
    /// A line written to the server, with passwords redacted.
    Sent(NetworkId, String),
    /// Connection progress and problems, for the server buffer.
    Notice(NetworkId, String),
    /// The round trip of a keepalive PING.
    Lag(NetworkId, Duration),
    /// A server's certificate wasn't trusted; the connection stopped.
    Certificate(NetworkId, Box<CertificateProblem>),
}

/// A certificate the network's policy refused, for the trust prompt
/// (DESIGN.md §6.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateProblem {
    pub host: String,
    pub port: u16,
    pub fingerprint: Fingerprint,
    /// Why, as "it has expired".
    pub reason: String,
    /// The server's pinned certificate changed.
    pub changed: bool,
    /// What the certificate says about itself, if it could be read.
    pub facts: Option<x509::Certificate>,
}

pub struct NetHandle {
    commands: mpsc::Sender<NetCommand>,
    waker: Arc<Waker>,
    thread: Option<JoinHandle<()>>,
}

impl NetHandle {
    /// Starts the network thread. `sink` receives every event on that
    /// thread: it should hand them to the UI and wake it.
    pub fn spawn(sink: impl FnMut(NetEvent) + Send + 'static) -> io::Result<Self> {
        let poll = Poll::new()?;
        let waker = Arc::new(Waker::new(poll.registry(), reactor::WAKER)?);
        let (commands, receiver) = mpsc::channel();
        let thread_waker = waker.clone();
        let thread = std::thread::Builder::new()
            .name(String::from("rubychat-net"))
            .spawn(move || reactor::run(poll, thread_waker, receiver, Box::new(sink)))?;
        Ok(Self {
            commands,
            waker,
            thread: Some(thread),
        })
    }

    pub fn send(&self, command: NetCommand) {
        if self.commands.send(command).is_ok() {
            let _ = self.waker.wake();
        }
    }
}

impl Drop for NetHandle {
    /// Says goodbye to every server and waits for the thread to finish.
    fn drop(&mut self) {
        self.send(NetCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Instant;

    /// One side of a scripted IRC server.
    struct Server {
        reader: BufReader<TcpStream>,
        writer: TcpStream,
    }

    impl Server {
        fn accept(listener: &TcpListener) -> Self {
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::yield_now();
                    }
                    Err(error) => panic!("no connection: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            Self {
                reader: BufReader::new(stream.try_clone().unwrap()),
                writer: stream,
            }
        }

        fn line(&mut self) -> String {
            let mut line = String::new();
            self.reader
                .read_line(&mut line)
                .expect("the client sends a line");
            assert!(line.ends_with("\r\n"), "{line:?} ends with CR LF");
            line.trim_end_matches(['\r', '\n']).to_string()
        }

        fn expect(&mut self, wanted: &str) {
            assert_eq!(self.line(), wanted);
        }

        fn send(&mut self, line: &str) {
            self.writer
                .write_all(format!("{line}\r\n").as_bytes())
                .unwrap();
        }
    }

    #[test]
    fn registers_negotiates_answers_ping_reconnects_and_quits() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let net = NetHandle::spawn(move |event| {
            let _ = tx.send(event);
        })
        .unwrap();
        let mut config = ConnectConfig::new("127.0.0.1", port, false, "tester");
        config.reconnect_delay = Duration::from_millis(50);
        net.send(NetCommand::Connect(NetworkId(7), config));

        let mut server = Server::accept(&listener);
        server.expect("CAP LS 302");
        server.expect("NICK tester");
        server.expect("USER tester 0 * RubyChat");
        server.send(":srv CAP * LS * :multi-prefix server-time");
        server.send(":srv CAP * LS :message-tags sasl=PLAIN");
        server.expect("CAP REQ :message-tags server-time multi-prefix");
        server.send(":srv CAP * ACK :message-tags server-time multi-prefix");
        server.expect("CAP END");
        server.send(":srv 433 * tester :Nickname is already in use");
        server.expect("NICK tester_");
        server.send(":srv 001 tester_ :Welcome to the test network");
        server.send("PING :abc123");
        server.expect("PONG abc123");
        drop(server);

        // The connection drops; it comes back on its own.
        let mut again = Server::accept(&listener);
        again.expect("CAP LS 302");
        net.send(NetCommand::Disconnect(
            NetworkId(7),
            Some(String::from("bye now")),
        ));
        while again.line() != "QUIT :bye now" {}
        drop(net);

        let events: Vec<NetEvent> = rx.try_iter().collect();
        let statuses: Vec<&LinkStatus> = events
            .iter()
            .filter_map(|event| match event {
                NetEvent::Status(_, status) => Some(status),
                _ => None,
            })
            .collect();
        assert!(statuses.contains(&&LinkStatus::Registered {
            nick: String::from("tester_")
        }));
        assert!(statuses.iter().any(|s| matches!(
            s,
            LinkStatus::Disconnected {
                retry_in: Some(_),
                ..
            }
        )));
        assert!(matches!(
            statuses.last(),
            Some(LinkStatus::Disconnected { retry_in: None, .. })
        ));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, NetEvent::Received(_, m) if m.command() == "001"))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, NetEvent::Sent(_, line) if line == "PONG abc123"))
        );
    }

    /// Statuses the network thread reported, after it stopped.
    fn statuses(events: &mpsc::Receiver<NetEvent>) -> Vec<LinkStatus> {
        events
            .try_iter()
            .filter_map(|event| match event {
                NetEvent::Status(_, status) => Some(status),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn sasl_logs_in_and_a_required_login_that_fails_stops() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let net = NetHandle::spawn(move |event| {
            let _ = tx.send(event);
        })
        .unwrap();
        let mut config = ConnectConfig::new("127.0.0.1", port, false, "jilles");
        config.login = link::Login::Sasl {
            mechanism: link::Mechanism::Plain,
            account: String::from("jilles"),
            password: String::from("sesame"),
        };
        config.require_login = true;
        net.send(NetCommand::Connect(NetworkId(1), config.clone()));
        let mut server = Server::accept(&listener);
        server.expect("CAP LS 302");
        server.expect("NICK jilles");
        server.expect("USER jilles 0 * RubyChat");
        server.send(":srv CAP * LS :sasl=PLAIN,EXTERNAL");
        server.expect("CAP REQ :sasl");
        server.send(":srv CAP jilles ACK :sasl");
        server.expect("AUTHENTICATE PLAIN");
        server.send("AUTHENTICATE +");
        server.expect("AUTHENTICATE amlsbGVzAGppbGxlcwBzZXNhbWU=");
        server.send(":srv 900 jilles jilles!j@h jilles :You are now logged in as jilles");
        server.send(":srv 903 jilles :SASL authentication successful");
        server.expect("CAP END");
        server.send(":srv 001 jilles :Welcome");
        // A required login that fails closes without reconnecting.
        net.send(NetCommand::Connect(
            NetworkId(2),
            ConnectConfig {
                reconnect_delay: Duration::from_millis(20),
                ..config
            },
        ));
        let mut refused = Server::accept(&listener);
        refused.expect("CAP LS 302");
        refused.expect("NICK jilles");
        refused.expect("USER jilles 0 * RubyChat");
        refused.send(":srv CAP * LS :sasl");
        refused.expect("CAP REQ :sasl");
        refused.send(":srv CAP jilles ACK :sasl");
        refused.expect("AUTHENTICATE PLAIN");
        refused.send("AUTHENTICATE +");
        refused.line();
        refused.send(":srv 904 jilles :SASL authentication failed");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut buffer = String::new();
        // The client hangs up: the read ends.
        while Instant::now() < deadline
            && refused.reader.read_line(&mut buffer).is_ok_and(|n| n > 0)
        {
            buffer.clear();
        }
        drop(net);
        let statuses = statuses(&rx);
        assert!(statuses.contains(&LinkStatus::Registered {
            nick: String::from("jilles")
        }));
        assert!(
            statuses.iter().any(|status| matches!(status, LinkStatus::Failed { reason } if reason.contains("Login failed"))),
            "{statuses:?}"
        );
    }

    /// A client certificate verifier that accepts any certificate and keeps
    /// it, to see what the client presented.
    #[derive(Debug)]
    struct AnyClient {
        seen: std::sync::Mutex<Option<Vec<u8>>>,
        provider: Arc<rustls::crypto::CryptoProvider>,
    }

    impl rustls::server::danger::ClientCertVerifier for AnyClient {
        fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
            &[]
        }

        fn verify_client_cert(
            &self,
            end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
            *self.seen.lock().unwrap() = Some(end_entity.to_vec());
            Ok(rustls::server::danger::ClientCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &rustls::pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.provider.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &rustls::pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.provider.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.provider
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    /// A TLS server on `listener` presenting `name`'s test certificate; it
    /// reports the first line it reads, `None` when the handshake fails.
    fn tls_server(
        listener: TcpListener,
        name: &'static str,
        client: Option<Arc<AnyClient>>,
    ) -> std::thread::JoinHandle<Option<String>> {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let read = |file: &str| {
            std::fs::read(format!(
                "{}/src/net/testdata/{file}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap()
        };
        let chain: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(&read(&format!("{name}.pem")))
                .collect::<Result<_, _>>()
                .unwrap();
        let key = PrivateKeyDer::from_pem_slice(&read(&format!("{name}.key"))).unwrap();
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let builder = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap();
        let builder = match client {
            Some(verifier) => builder.with_client_cert_verifier(verifier),
            None => builder.with_no_client_auth(),
        };
        let config = Arc::new(builder.with_single_cert(chain, key).unwrap());
        std::thread::spawn(move || {
            let stream = Server::accept(&listener).writer;
            let connection = rustls::ServerConnection::new(config).unwrap();
            let mut tls = BufReader::new(rustls::StreamOwned::new(connection, stream));
            let mut line = String::new();
            tls.read_line(&mut line)
                .ok()
                .filter(|&n| n > 0)
                .map(|_| line.trim_end().to_string())
        })
    }

    fn certificate(name: &str) -> Vec<u8> {
        use rustls::pki_types::pem::PemObject;
        let pem = std::fs::read(format!(
            "{}/src/net/testdata/{name}.pem",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        rustls::pki_types::CertificateDer::from_pem_slice(&pem)
            .unwrap()
            .to_vec()
    }

    /// Connects with `config` to a TLS server presenting `name`'s
    /// certificate; returns the server's first line and the events.
    fn handshake(
        config: impl FnOnce(&mut ConnectConfig),
        name: &'static str,
        client: Option<Arc<AnyClient>>,
    ) -> (Option<String>, Vec<NetEvent>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tls_server(listener, name, client);
        let (tx, rx) = mpsc::channel();
        let net = NetHandle::spawn(move |event| {
            let _ = tx.send(event);
        })
        .unwrap();
        let mut connect = ConnectConfig::new("127.0.0.1", port, true, "ruby");
        config(&mut connect);
        net.send(NetCommand::Connect(NetworkId(1), connect));
        let line = server.join().unwrap();
        drop(net);
        (line, rx.try_iter().collect())
    }

    fn problem(events: &[NetEvent]) -> Option<&CertificateProblem> {
        events.iter().find_map(|event| match event {
            NetEvent::Certificate(_, problem) => Some(&**problem),
            _ => None,
        })
    }

    #[test]
    fn untrusted_certificates_stop_with_what_the_prompt_needs() {
        let (line, events) = handshake(|_| {}, "server", None);
        assert_eq!(line, None, "the client hung up");
        let problem = problem(&events).expect("a certificate problem");
        assert_eq!(problem.fingerprint, fingerprint(&certificate("server")));
        assert!(
            !problem.changed && problem.reason.contains("authority"),
            "{problem:?}"
        );
        assert_eq!(
            problem.facts.as_ref().unwrap().subject.display(),
            "localhost (RubyChat test server)"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, NetEvent::Status(_, LinkStatus::Failed { .. })))
        );
    }

    #[test]
    fn pinned_certificates_pass_and_changed_ones_fail_closed() {
        let pin = fingerprint(&certificate("server"));
        let (line, events) = handshake(|config| config.pins = vec![pin], "server", None);
        assert_eq!(line.as_deref(), Some("CAP LS 302"), "trusted by its pin");
        assert!(problem(&events).is_none());
        let other = fingerprint(&certificate("other"));
        let (line, events) = handshake(
            |config| {
                config.certificate = crate::config::networks::CertificatePolicy::Pinned;
                config.pins = vec![other];
            },
            "server",
            None,
        );
        assert_eq!(line, None);
        assert!(
            problem(&events).is_some_and(|problem| problem.changed),
            "the pin no longer matches"
        );
        let (line, _) = handshake(
            |config| config.certificate = crate::config::networks::CertificatePolicy::None,
            "other",
            None,
        );
        assert_eq!(
            line.as_deref(),
            Some("CAP LS 302"),
            "no verification, on request"
        );
    }

    #[test]
    fn client_certificates_are_presented() {
        let client = Arc::new(AnyClient {
            seen: std::sync::Mutex::new(None),
            provider: Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        });
        let pin = fingerprint(&certificate("server"));
        let path = std::path::PathBuf::from(format!(
            "{}/src/net/testdata/client.pem",
            env!("CARGO_MANIFEST_DIR")
        ));
        let (line, _) = handshake(
            |config| {
                config.pins = vec![pin];
                config.client_certificate = Some(path);
            },
            "server",
            Some(client.clone()),
        );
        assert_eq!(line.as_deref(), Some("CAP LS 302"));
        let seen = client
            .seen
            .lock()
            .unwrap()
            .clone()
            .expect("a client certificate");
        assert_eq!(seen, certificate("client"));
    }

    /// `cargo test --lib connects_to_libera -- --ignored --nocapture` checks
    /// TLS and registration against Libera.Chat, then quits.
    #[test]
    #[ignore = "connects to irc.libera.chat over the internet"]
    fn connects_to_libera_over_tls() {
        let (tx, rx) = mpsc::channel();
        let net = NetHandle::spawn(move |event| {
            let _ = tx.send(event);
        })
        .unwrap();
        let nick = format!("rubychat{}", std::process::id() % 100_000);
        net.send(NetCommand::Connect(
            NetworkId(1),
            ConnectConfig::new("irc.libera.chat", 6697, true, nick),
        ));
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut registered = false;
        let mut motd_end = false;
        while Instant::now() < deadline && !motd_end {
            let Ok(event) = rx.recv_timeout(Duration::from_secs(1)) else {
                continue;
            };
            match &event {
                NetEvent::Notice(_, text) => println!("* {text}"),
                NetEvent::Status(_, status) => {
                    println!("= {status:?}");
                    registered |= matches!(status, LinkStatus::Registered { .. });
                }
                NetEvent::Received(_, message) => {
                    println!("< {message}");
                    motd_end |= matches!(message.command(), "376" | "422");
                }
                NetEvent::Sent(_, line) => println!("> {line}"),
                NetEvent::Lag(_, lag) => println!("lag {lag:?}"),
                NetEvent::Certificate(_, problem) => println!("! {problem:?}"),
            }
        }
        net.send(NetCommand::Disconnect(
            NetworkId(1),
            Some(String::from("RubyChat test")),
        ));
        drop(net);
        assert!(registered && motd_end, "registered and read the MOTD");
    }
}
