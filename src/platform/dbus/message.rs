//! D-Bus messages (D-Bus Specification, "Message Protocol"): a header of
//! fixed signature `yyyyuua(yv)` padded to 8 bytes, then the body.

use super::wire::{Reader, Value, Writer, split_signature, valid_path};

/// The message bus's name, path and interface.
pub const BUS_NAME: &str = "org.freedesktop.DBus";
pub const BUS_PATH: &str = "/org/freedesktop/DBus";

/// "This message does not expect method return replies or error replies."
pub const NO_REPLY_EXPECTED: u8 = 0x1;

/// The longest name ("Valid Names").
const MAX_NAME: usize = 255;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Call,
    Return,
    Error,
    Signal,
    /// A type this version doesn't know, which must be ignored.
    Unknown(u8),
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Kind::Call => 1,
            Kind::Return => 2,
            Kind::Error => 3,
            Kind::Signal => 4,
            Kind::Unknown(code) => code,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub kind: Kind,
    pub flags: u8,
    /// Set when the message is sent, or read.
    pub serial: u32,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub reply_serial: Option<u32>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    pub body: Vec<Value>,
}

impl Message {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            flags: 0,
            serial: 0,
            path: None,
            interface: None,
            member: None,
            error_name: None,
            reply_serial: None,
            destination: None,
            sender: None,
            body: Vec::new(),
        }
    }

    pub fn call(
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        body: Vec<Value>,
    ) -> Self {
        Self {
            destination: Some(destination.to_string()),
            path: Some(path.to_string()),
            interface: Some(interface.to_string()),
            member: Some(member.to_string()),
            body,
            ..Self::new(Kind::Call)
        }
    }

    /// A call to the message bus itself.
    pub fn bus_call(member: &str, body: Vec<Value>) -> Self {
        Self::call(BUS_NAME, BUS_PATH, BUS_NAME, member, body)
    }

    pub fn signal(path: &str, interface: &str, member: &str, body: Vec<Value>) -> Self {
        Self {
            path: Some(path.to_string()),
            interface: Some(interface.to_string()),
            member: Some(member.to_string()),
            body,
            ..Self::new(Kind::Signal)
        }
    }

    /// The method return answering `call`.
    pub fn reply(call: &Message, body: Vec<Value>) -> Self {
        Self {
            reply_serial: Some(call.serial),
            destination: call.sender.clone(),
            body,
            ..Self::new(Kind::Return)
        }
    }

    /// The error answering `call`, with a human-readable message.
    pub fn error(call: &Message, name: &str, text: &str) -> Self {
        Self {
            error_name: Some(name.to_string()),
            reply_serial: Some(call.serial),
            destination: call.sender.clone(),
            body: vec![Value::str(text)],
            ..Self::new(Kind::Error)
        }
    }

    /// Whether the sender of this call wants an answer.
    pub fn wants_reply(&self) -> bool {
        self.kind == Kind::Call && self.flags & NO_REPLY_EXPECTED == 0
    }

    pub fn arg_str(&self, index: usize) -> Option<&str> {
        self.body.get(index).and_then(Value::as_str)
    }

    /// The message in the wire format, with serial `serial`.
    pub fn encode(&self, serial: u32) -> Result<Vec<u8>, String> {
        let mut body = Writer::default();
        let mut signature = String::new();
        for value in &self.body {
            signature.push_str(&value.signature());
            body.value(value)?;
        }
        let mut fields = Vec::new();
        let mut field = |code: u8, value: Value| {
            fields.push(Value::Struct(vec![
                Value::Byte(code),
                Value::variant(value),
            ]));
        };
        if let Some(path) = &self.path {
            field(1, Value::Path(path.clone()));
        }
        if let Some(interface) = &self.interface {
            field(2, Value::str(interface.as_str()));
        }
        if let Some(member) = &self.member {
            field(3, Value::str(member.as_str()));
        }
        if let Some(name) = &self.error_name {
            field(4, Value::str(name.as_str()));
        }
        if let Some(serial) = self.reply_serial {
            field(5, Value::U32(serial));
        }
        if let Some(destination) = &self.destination {
            field(6, Value::str(destination.as_str()));
        }
        if let Some(sender) = &self.sender {
            field(7, Value::str(sender.as_str()));
        }
        if !signature.is_empty() {
            field(8, Value::Signature(signature));
        }
        let mut header = Writer::default();
        header
            .bytes
            .extend_from_slice(&[b'l', self.kind.code(), self.flags, 1]);
        header.u32(body.bytes.len() as u32);
        header.u32(serial);
        header.value(&Value::Array(String::from("(yv)"), fields))?;
        while !header.bytes.len().is_multiple_of(8) {
            header.bytes.push(0);
        }
        header.bytes.extend_from_slice(&body.bytes);
        Ok(header.bytes)
    }

    /// The whole length of the message that `prefix` starts, once its fixed
    /// part has arrived; an error if it would exceed `limit`.
    pub fn length(prefix: &[u8], limit: usize) -> Result<Option<usize>, String> {
        if prefix.len() < 16 {
            return Ok(None);
        }
        let big_endian = byte_order(prefix[0])?;
        let word = |at: usize| {
            let bytes: [u8; 4] = prefix[at..at + 4].try_into().expect("4 bytes");
            if big_endian {
                u32::from_be_bytes(bytes)
            } else {
                u32::from_le_bytes(bytes)
            }
        };
        let (body, fields) = (word(4) as usize, word(12) as usize);
        let header = (16usize).saturating_add(fields).next_multiple_of(8);
        let total = header.saturating_add(body);
        if total > limit {
            return Err(format!("a message of {total} bytes"));
        }
        Ok(Some(total))
    }

    /// Reads one whole message.
    pub fn decode(data: &[u8]) -> Result<Message, String> {
        if data.len() < 16 {
            return Err(String::from("truncated message"));
        }
        let big_endian = byte_order(data[0])?;
        if data[3] != 1 {
            return Err(format!("protocol version {}", data[3]));
        }
        let mut reader = Reader::new(data, big_endian);
        reader.at = 4;
        let body_length = reader.u32()? as usize;
        let serial = reader.u32()?;
        if serial == 0 {
            return Err(String::from("serial 0"));
        }
        let kind = match data[1] {
            0 => return Err(String::from("message type 0")),
            1 => Kind::Call,
            2 => Kind::Return,
            3 => Kind::Error,
            4 => Kind::Signal,
            code => Kind::Unknown(code),
        };
        let mut message = Message {
            flags: data[2],
            serial,
            ..Message::new(kind)
        };
        let mut signature = String::new();
        let fields = reader.value("a(yv)")?;
        for field in fields.as_array().unwrap_or_default() {
            let Value::Struct(parts) = field else {
                continue;
            };
            let (Some(Value::Byte(code)), Some(Value::Variant(value))) =
                (parts.first(), parts.get(1))
            else {
                continue;
            };
            let text = || value.as_str().map(str::to_string);
            let wrong = || format!("header field {code} of the wrong type");
            match (code, value.as_ref()) {
                (0, _) => return Err(String::from("header field 0")),
                (1, Value::Path(_)) => message.path = text(),
                (2, Value::Str(name)) if valid_interface(name) => message.interface = text(),
                (3, Value::Str(name)) if valid_member(name) => message.member = text(),
                (4, Value::Str(name)) if valid_interface(name) => message.error_name = text(),
                (5, Value::U32(serial)) => message.reply_serial = Some(*serial),
                (6, Value::Str(name)) if valid_bus_name(name) => message.destination = text(),
                (7, Value::Str(name)) if valid_bus_name(name) => message.sender = text(),
                (8, Value::Signature(body)) => signature = body.clone(),
                (9, Value::U32(0)) => {}
                (9, Value::U32(_)) => {
                    return Err(String::from("file descriptors never negotiated"));
                }
                (1..=9, _) => return Err(wrong()),
                // Unknown fields are ignored.
                _ => {}
            }
        }
        reader.align(8)?;
        let start = reader.at;
        if data.len() != start.checked_add(body_length).ok_or("body too long")? {
            return Err(String::from("body length doesn't match"));
        }
        message.body = Reader::new(&data[start..], big_endian).body(&signature)?;
        let required = match kind {
            Kind::Call => message.path.is_some() && message.member.is_some(),
            Kind::Signal => {
                message.path.is_some() && message.interface.is_some() && message.member.is_some()
            }
            Kind::Error => message.error_name.is_some() && message.reply_serial.is_some(),
            Kind::Return => message.reply_serial.is_some(),
            Kind::Unknown(_) => true,
        };
        if !required {
            return Err(format!("a {kind:?} without its required header fields"));
        }
        Ok(message)
    }
}

fn byte_order(flag: u8) -> Result<bool, String> {
    match flag {
        b'l' => Ok(false),
        b'B' => Ok(true),
        other => Err(format!("byte order {other:#x}")),
    }
}

fn name_element(element: &str, extra: &[u8], digit_first: bool) -> bool {
    !element.is_empty()
        && (digit_first || !element.as_bytes()[0].is_ascii_digit())
        && element
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || extra.contains(&b))
}

/// Interface and error names: two or more elements of `[A-Za-z0-9_]`,
/// none starting with a digit.
pub fn valid_interface(name: &str) -> bool {
    name.len() <= MAX_NAME
        && name.contains('.')
        && name
            .split('.')
            .all(|element| name_element(element, b"", false))
}

pub fn valid_member(name: &str) -> bool {
    name.len() <= MAX_NAME && name_element(name, b"", false)
}

/// Unique names (`:1.42`) and well-known names (`org.kde.Foo-1`).
pub fn valid_bus_name(name: &str) -> bool {
    let (unique, rest) = match name.strip_prefix(':') {
        Some(rest) => (true, rest),
        None => (false, name),
    };
    name.len() <= MAX_NAME
        && rest.contains('.')
        && rest
            .split('.')
            .all(|element| name_element(element, b"-", unique))
}

/// Paths and signatures are checked as they are read; this checks a path
/// before RubyChat sends one.
pub fn valid_object_path(path: &str) -> bool {
    valid_path(path)
}

/// Whether `signature` is valid, for tests and callers building bodies.
pub fn valid_signature(signature: &str) -> bool {
    split_signature(signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_encodes_byte_for_byte() {
        let hello = Message::bus_call("Hello", Vec::new());
        let bytes = hello.encode(1).unwrap();
        let mut expected = vec![b'l', 1, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0x6e, 0, 0, 0];
        // PATH: code 1, variant 'o'.
        expected.extend_from_slice(&[1, 1, b'o', 0, 21, 0, 0, 0]);
        expected.extend_from_slice(b"/org/freedesktop/DBus\0");
        expected.extend_from_slice(&[0, 0]);
        // INTERFACE, MEMBER and DESTINATION: variants of 's'.
        for (code, text) in [
            (2u8, "org.freedesktop.DBus"),
            (3, "Hello"),
            (6, "org.freedesktop.DBus"),
        ] {
            while expected.len() % 8 != 0 {
                expected.push(0);
            }
            expected.extend_from_slice(&[code, 1, b's', 0]);
            expected.extend_from_slice(&(text.len() as u32).to_le_bytes());
            expected.extend_from_slice(text.as_bytes());
            expected.push(0);
        }
        let fields = expected.len() - 16;
        expected[12..16].copy_from_slice(&(fields as u32).to_le_bytes());
        while expected.len() % 8 != 0 {
            expected.push(0);
        }
        assert_eq!(bytes, expected);
        assert_eq!(Message::length(&bytes, 1 << 20), Ok(Some(bytes.len())));
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded, Message { serial: 1, ..hello });
    }

    #[test]
    fn messages_round_trip_with_bodies() {
        let mut call = Message::call(
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "Notify",
            vec![
                Value::str("RubyChat"),
                Value::U32(0),
                Value::str(""),
                Value::str("#rust · Libera.Chat"),
                Value::str("ferris: ruby, look"),
                Value::strings(["default", "Open"]),
                Value::dict([("urgency", Value::Byte(1))]),
                Value::I32(-1),
            ],
        );
        call.sender = Some(String::from(":1.7"));
        let bytes = call.encode(42).unwrap();
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded.serial, 42);
        assert_eq!(decoded.body, call.body);
        let reply = Message::reply(&decoded, vec![Value::U32(9)]);
        let reply = Message::decode(&reply.encode(43).unwrap()).unwrap();
        assert_eq!(
            (reply.kind, reply.reply_serial, reply.destination.as_deref()),
            (Kind::Return, Some(42), Some(":1.7"))
        );
        let error = Message::error(&decoded, "org.freedesktop.DBus.Error.Failed", "no");
        let error = Message::decode(&error.encode(44).unwrap()).unwrap();
        assert_eq!(error.arg_str(0), Some("no"));
    }

    #[test]
    fn big_endian_messages_are_read() {
        // A method return assembled by hand: reply serial 3, body (u) 42.
        let bytes = [
            b'B', 2, 0, 1, 0, 0, 0, 4, 0, 0, 0, 7, 0, 0, 0, 15, // fixed part
            5, 1, b'u', 0, 0, 0, 0, 3, // REPLY_SERIAL
            8, 1, b'g', 0, 1, b'u', 0, // SIGNATURE
            0, // header padding
            0, 0, 0, 42, // body
        ];
        assert_eq!(Message::length(&bytes, 1 << 20), Ok(Some(bytes.len())));
        let message = Message::decode(&bytes).unwrap();
        assert_eq!(
            (message.kind, message.serial, message.reply_serial),
            (Kind::Return, 7, Some(3))
        );
        assert_eq!(message.body, [Value::U32(42)]);
    }

    #[test]
    fn invalid_messages_are_refused() {
        let good = Message::bus_call("Hello", Vec::new()).encode(1).unwrap();
        let mut bad = good.clone();
        bad[3] = 2;
        assert!(Message::decode(&bad).is_err(), "protocol version 2");
        let mut bad = good.clone();
        bad[8..12].copy_from_slice(&[0; 4]);
        assert!(Message::decode(&bad).is_err(), "serial 0");
        let mut bad = good.clone();
        bad.push(0);
        assert!(Message::decode(&bad).is_err(), "trailing bytes");
        // A signal without an interface.
        let mut signal = Message::signal("/a", "a.b", "C", Vec::new());
        signal.interface = None;
        assert!(Message::decode(&signal.encode(1).unwrap()).is_err());
        // Too long for the limit.
        assert!(Message::length(&good, 16).is_err());
        assert_eq!(Message::length(&good[..15], 1 << 20), Ok(None));
    }

    #[test]
    fn mutated_and_truncated_messages_never_panic() {
        let mut layout = Message::reply(
            &Message::bus_call("Hello", Vec::new()),
            vec![
                Value::U32(3),
                Value::Struct(vec![
                    Value::I32(0),
                    Value::dict([("label", Value::str("Quit"))]),
                    Value::Array(
                        String::from("v"),
                        vec![Value::variant(Value::Bytes(vec![1]))],
                    ),
                ]),
            ],
        );
        layout.destination = Some(String::from(":1.2"));
        let samples = [
            Message::bus_call("Hello", Vec::new()).encode(1).unwrap(),
            layout.encode(2).unwrap(),
        ];
        for bytes in &samples {
            for cut in 0..bytes.len() {
                let _ = Message::length(&bytes[..cut], 1 << 20);
                let _ = Message::decode(&bytes[..cut]);
            }
            for at in 0..bytes.len() {
                for value in [0x00, 0x01, 0x7f, 0x80, 0xff, b'a', b'(', b'{', b'v'] {
                    let mut mutated = bytes.clone();
                    mutated[at] = value;
                    let _ = Message::length(&mutated, 1 << 20);
                    let _ = Message::decode(&mutated);
                }
            }
        }
    }

    #[test]
    fn names_follow_the_rules() {
        assert!(valid_interface("org.kde.StatusNotifierItem"));
        assert!(!valid_interface("org"));
        assert!(!valid_interface("org.1kde"));
        assert!(!valid_interface("org..kde"));
        assert!(valid_member("NewIcon"));
        assert!(!valid_member("New.Icon"));
        assert!(!valid_member("1Icon"));
        assert!(valid_bus_name(":1.42"));
        assert!(valid_bus_name("org.kde.StatusNotifierItem-4077-1"));
        assert!(!valid_bus_name("org.kde.1Item"));
        assert!(!valid_bus_name("org"));
        assert!(!valid_bus_name(".org.kde"));
        assert!(valid_signature("a{sv}"));
        assert!(valid_object_path("/MenuBar"));
    }
}
