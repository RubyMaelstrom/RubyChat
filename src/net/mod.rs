//! The network thread (DESIGN.md §5–6). One mio reactor runs every
//! connection—resolution, connection racing, TLS, registration, keepalive,
//! reconnection and flood control—so the UI thread never touches a socket.
//! The UI sends `NetCommand`s and receives `NetEvent`s.

mod flood;
mod framing;
mod link;
mod reactor;
mod tls;

use std::io;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use mio::{Poll, Waker};

use crate::irc::Message;

pub use link::{ConnectConfig, LinkStatus, redact};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NetworkId(pub u64);

#[derive(Debug)]
pub enum NetCommand {
    /// Connects, or reconnects with a new configuration.
    Connect(NetworkId, ConnectConfig),
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
