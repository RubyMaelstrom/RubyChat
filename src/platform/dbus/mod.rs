//! RubyChat's own D-Bus client (DESIGN.md §12.2), for the tray, its menu
//! and notifications: session bus addresses (`address`), the type system
//! and wire format (`wire`), messages (`message`), and here the connection.
//!
//! Connecting authenticates with SASL EXTERNAL, as the specification's
//! "Authentication Protocol" describes (the bus learns the user from the
//! socket's credentials), then says Hello for a unique name. Those steps
//! block, with time limits, on the desktop thread; afterwards the socket is
//! nonblocking and the desktop thread's poll drives it. No file
//! descriptors are passed, so none are negotiated.

pub mod address;
pub mod message;
pub mod wire;

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub use message::{Kind, Message};
pub use wire::Value;

use address::Address;

/// The longest message read or written. The specification allows 128 MiB;
/// nothing RubyChat exchanges comes near this.
pub const MAX_MESSAGE: usize = 4 << 20;
/// Bytes waiting for the bus to read them, at most: past this the bus
/// isn't reading, and the connection is given up.
const MAX_OUTPUT: usize = 8 << 20;
/// How long each step of connecting may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest line the authentication exchange accepts.
const MAX_AUTH_LINE: usize = 1024;

pub struct Connection {
    stream: mio::net::UnixStream,
    unique_name: String,
    serial: u32,
    input: Vec<u8>,
    output: Vec<u8>,
    /// How much of `output` the socket took.
    written: usize,
    /// Messages that arrived with Hello's reply, not yet handed out.
    early: Vec<Message>,
}

impl Connection {
    /// Connects to the session bus. It blocks for at most a few seconds:
    /// call it from the desktop thread.
    pub fn session() -> Result<Self, String> {
        let addresses = address::session();
        if addresses.is_empty() {
            return Err(String::from("no session bus address"));
        }
        let mut failure = String::new();
        for address in addresses {
            match connect(&address).and_then(Self::open) {
                Ok(connection) => return Ok(connection),
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }

    /// Authenticates on a connected socket and says Hello, then makes the
    /// socket nonblocking.
    pub fn open(mut stream: UnixStream) -> Result<Self, String> {
        let failed = |error: io::Error| format!("D-Bus: {error}");
        stream
            .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .map_err(failed)?;
        stream
            .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
            .map_err(failed)?;
        let mut input = authenticate(&mut stream)?;
        let hello = Message::bus_call("Hello", Vec::new()).encode(1)?;
        stream.write_all(&hello).map_err(failed)?;
        let mut early = Vec::new();
        let unique_name = loop {
            let message = read_blocking(&mut stream, &mut input)?;
            if message.reply_serial == Some(1) {
                match (message.kind, message.arg_str(0)) {
                    (Kind::Return, Some(name)) => break name.to_string(),
                    _ => return Err(String::from("D-Bus: the bus refused Hello")),
                }
            }
            early.push(message);
        };
        stream.set_nonblocking(true).map_err(failed)?;
        Ok(Self {
            stream: mio::net::UnixStream::from_std(stream),
            unique_name,
            serial: 1,
            input,
            output: Vec::new(),
            written: 0,
            early,
        })
    }

    /// The name the bus gave this connection, such as `:1.42`.
    pub fn unique_name(&self) -> &str {
        &self.unique_name
    }

    /// The socket, for a poll to watch.
    pub fn source(&mut self) -> &mut mio::net::UnixStream {
        &mut self.stream
    }

    /// Queues `message`, writing what the socket takes now; its serial.
    pub fn send(&mut self, mut message: Message) -> Result<u32, String> {
        self.serial = self.serial.checked_add(1).unwrap_or(1);
        message.serial = self.serial;
        let bytes = message.encode(self.serial)?;
        if bytes.len() > MAX_MESSAGE {
            return Err(String::from("D-Bus: message too long"));
        }
        if self.output.len() - self.written + bytes.len() > MAX_OUTPUT {
            return Err(String::from("D-Bus: the bus isn't reading"));
        }
        self.output.extend_from_slice(&bytes);
        self.flush()?;
        Ok(self.serial)
    }

    /// Whether bytes wait for the socket to become writable.
    pub fn wants_write(&self) -> bool {
        self.written < self.output.len()
    }

    /// Writes what the socket takes without waiting.
    pub fn flush(&mut self) -> Result<(), String> {
        while self.wants_write() {
            match self.stream.write(&self.output[self.written..]) {
                Ok(0) => return Err(String::from("D-Bus: the bus closed the connection")),
                Ok(count) => self.written += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("D-Bus: {error}")),
            }
        }
        if !self.wants_write() {
            self.output.clear();
            self.written = 0;
        } else if self.written > MAX_OUTPUT / 2 {
            self.output.drain(..self.written);
            self.written = 0;
        }
        Ok(())
    }

    /// The messages complete so far, reading what the socket has without
    /// waiting. An error means the connection is over.
    pub fn receive(&mut self) -> Result<Vec<Message>, String> {
        let mut messages = std::mem::take(&mut self.early);
        let mut chunk = [0u8; 16 * 1024];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    // What did arrive still counts; the next call reports
                    // the end.
                    split(&mut self.input, &mut messages)?;
                    if messages.is_empty() {
                        return Err(String::from("D-Bus: the bus closed the connection"));
                    }
                    return Ok(messages);
                }
                Ok(count) => {
                    self.input.extend_from_slice(&chunk[..count]);
                    split(&mut self.input, &mut messages)?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("D-Bus: {error}")),
            }
        }
        Ok(messages)
    }
}

/// Moves the whole messages at the front of `input` to `messages`.
fn split(input: &mut Vec<u8>, messages: &mut Vec<Message>) -> Result<(), String> {
    let mut start = 0;
    while let Some(length) =
        Message::length(&input[start..], MAX_MESSAGE).map_err(|error| format!("D-Bus: {error}"))?
    {
        if input.len() - start < length {
            break;
        }
        let message = Message::decode(&input[start..start + length])
            .map_err(|error| format!("D-Bus: malformed message: {error}"))?;
        messages.push(message);
        start += length;
    }
    input.drain(..start);
    Ok(())
}

/// The next message, waiting for it, while the socket still blocks.
fn read_blocking(stream: &mut UnixStream, input: &mut Vec<u8>) -> Result<Message, String> {
    let mut messages = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        // Hello's reply comes first; anything after it waits in `input`.
        if let Some(length) =
            Message::length(input, MAX_MESSAGE).map_err(|error| format!("D-Bus: {error}"))?
            && input.len() >= length
        {
            let mut whole: Vec<u8> = input.drain(..length).collect();
            split(&mut whole, &mut messages)?;
            return messages
                .pop()
                .ok_or_else(|| String::from("D-Bus: lost a message"));
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(String::from("D-Bus: the bus closed the connection")),
            Ok(count) => input.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("D-Bus: {error}")),
        }
    }
}

fn connect(address: &Address) -> Result<UnixStream, String> {
    let result = match address {
        Address::Path(path) => UnixStream::connect(path),
        #[cfg(any(target_os = "linux", target_os = "android"))]
        Address::Abstract(name) => {
            use std::os::linux::net::SocketAddrExt;
            std::os::unix::net::SocketAddr::from_abstract_name(name)
                .and_then(|address| UnixStream::connect_addr(&address))
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        Address::Abstract(_) => Err(io::Error::from(io::ErrorKind::Unsupported)),
    };
    result.map_err(|error| format!("D-Bus: connecting to {address:?}: {error}"))
}

/// The client's side of "Authentication Protocol": the nul byte, `AUTH
/// EXTERNAL` naming this user by numeric ID, then `BEGIN` once the bus says
/// `OK`. A bus that asks for the (empty) response with `DATA` gets it.
/// Returns any bytes read past the last line.
fn authenticate(stream: &mut UnixStream) -> Result<Vec<u8>, String> {
    let failed = |error: io::Error| format!("D-Bus: authenticating: {error}");
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let identity: String = uid
        .to_string()
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    stream
        .write_all(format!("\0AUTH EXTERNAL {identity}\r\n").as_bytes())
        .map_err(failed)?;
    let mut buffered = Vec::new();
    let mut line = read_line(stream, &mut buffered)?;
    if line == "DATA" || line.starts_with("DATA ") {
        stream.write_all(b"DATA\r\n").map_err(failed)?;
        line = read_line(stream, &mut buffered)?;
    }
    if !line.starts_with("OK ") {
        return Err(format!("D-Bus: the bus refused authentication: {line}"));
    }
    stream.write_all(b"BEGIN\r\n").map_err(failed)?;
    Ok(buffered)
}

fn read_line(stream: &mut UnixStream, buffered: &mut Vec<u8>) -> Result<String, String> {
    loop {
        if let Some(end) = buffered.windows(2).position(|pair| pair == b"\r\n") {
            let line: Vec<u8> = buffered.drain(..end + 2).take(end).collect();
            return String::from_utf8(line)
                .ok()
                .filter(|line| line.is_ascii())
                .ok_or_else(|| String::from("D-Bus: authentication line isn't ASCII"));
        }
        if buffered.len() > MAX_AUTH_LINE {
            return Err(String::from("D-Bus: authentication line too long"));
        }
        let mut chunk = [0u8; 256];
        match stream.read(&mut chunk) {
            Ok(0) => return Err(String::from("D-Bus: the bus closed the connection")),
            Ok(count) => buffered.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("D-Bus: authenticating: {error}")),
        }
    }
}

/// A scripted message bus for tests: the server's side of a socket pair.
#[cfg(test)]
pub mod testing {
    use super::*;

    pub struct FakeBus {
        pub stream: UnixStream,
        input: Vec<u8>,
        serial: u32,
    }

    impl FakeBus {
        /// A connected client and the bus serving it, past Hello.
        pub fn pair() -> (Connection, FakeBus) {
            let (client, server) = UnixStream::pair().unwrap();
            let bus = std::thread::spawn(move || {
                let mut bus = FakeBus {
                    stream: server,
                    input: Vec::new(),
                    serial: 0,
                };
                bus.stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                bus.handshake();
                bus
            });
            let connection = Connection::open(client).unwrap();
            (connection, bus.join().unwrap())
        }

        fn handshake(&mut self) {
            let mut line = Vec::new();
            while !line.ends_with(b"\r\n") {
                let mut byte = [0u8];
                self.stream.read_exact(&mut byte).unwrap();
                line.push(byte[0]);
            }
            let text = String::from_utf8(line).unwrap();
            assert!(text.starts_with("\0AUTH EXTERNAL "), "{text:?}");
            self.stream.write_all(b"OK 0123456789abcdef\r\n").unwrap();
            let mut begin = [0u8; 7];
            self.stream.read_exact(&mut begin).unwrap();
            assert_eq!(&begin, b"BEGIN\r\n");
            let hello = self.read();
            assert_eq!(hello.member.as_deref(), Some("Hello"));
            self.write(Message::reply(&hello, vec![Value::str(":1.42")]));
        }

        /// The next message the client sent.
        pub fn read(&mut self) -> Message {
            loop {
                if let Some(length) = Message::length(&self.input, MAX_MESSAGE).unwrap()
                    && self.input.len() >= length
                {
                    let message = Message::decode(&self.input[..length]).unwrap();
                    self.input.drain(..length);
                    return message;
                }
                let mut chunk = [0u8; 4096];
                let count = self.stream.read(&mut chunk).expect("a message in time");
                assert!(count > 0, "the client hung up");
                self.input.extend_from_slice(&chunk[..count]);
            }
        }

        /// Sends `message` as the bus would deliver it.
        pub fn write(&mut self, mut message: Message) {
            self.serial += 1;
            message
                .sender
                .get_or_insert_with(|| String::from(message::BUS_NAME));
            self.stream
                .write_all(&message.encode(self.serial).unwrap())
                .unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeBus;
    use super::*;

    fn receive_one(connection: &mut Connection) -> Message {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let mut messages = connection.receive().unwrap();
            if !messages.is_empty() {
                assert_eq!(messages.len(), 1);
                return messages.remove(0);
            }
            assert!(std::time::Instant::now() < deadline, "no message in time");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn connects_calls_and_hears_back() {
        let (mut connection, mut bus) = FakeBus::pair();
        assert_eq!(connection.unique_name(), ":1.42");
        let serial = connection
            .send(Message::bus_call(
                "GetNameOwner",
                vec![Value::str("org.freedesktop.Notifications")],
            ))
            .unwrap();
        let call = bus.read();
        assert_eq!(
            (call.serial, call.member.as_deref(), call.arg_str(0)),
            (
                serial,
                Some("GetNameOwner"),
                Some("org.freedesktop.Notifications")
            )
        );
        bus.write(Message::reply(&call, vec![Value::str(":1.10")]));
        let reply = receive_one(&mut connection);
        assert_eq!(
            (reply.reply_serial, reply.arg_str(0)),
            (Some(serial), Some(":1.10"))
        );
    }

    #[test]
    fn a_closed_or_garbled_bus_ends_the_connection() {
        let (mut connection, bus) = FakeBus::pair();
        drop(bus);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match connection.receive() {
                Err(_) => break,
                Ok(messages) => assert!(messages.is_empty()),
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let (mut connection, mut bus) = FakeBus::pair();
        bus.stream
            .write_all(b"garbage that isn't a message")
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if connection.receive().is_err() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_refused_login_is_reported() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let bus = std::thread::spawn(move || {
            let mut line = [0u8; 64];
            let _ = server.read(&mut line).unwrap();
            server.write_all(b"REJECTED EXTERNAL\r\n").unwrap();
        });
        assert!(Connection::open(client).is_err());
        bus.join().unwrap();
    }

    /// The real session bus: connects and asks it for the notification
    /// server's owner (`cargo test --lib session_bus -- --ignored`).
    #[test]
    #[ignore]
    fn session_bus() {
        let mut connection = Connection::session().unwrap();
        eprintln!("unique name {}", connection.unique_name());
        let serial = connection
            .send(Message::bus_call(
                "NameHasOwner",
                vec![Value::str("org.freedesktop.Notifications")],
            ))
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            for message in connection.receive().unwrap() {
                if message.reply_serial == Some(serial) {
                    eprintln!("notification server present: {:?}", message.body);
                    return;
                }
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
