//! Certificate pins (`known_hosts`): one line per server, its `host:port`
//! and the SHA-256 of its certificate in hex—the format trust-desktop uses
//! for Telnet (DESIGN.md §6.2). Comments and lines that don't parse are
//! ignored, and dropped on the next write.

/// The SHA-256 of a certificate's DER encoding.
pub type Fingerprint = [u8; 32];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnownHosts {
    /// `host:port` and its pin, sorted by key.
    pins: Vec<(String, Fingerprint)>,
}

/// The key a server is pinned under: `host:port`, IPv6 hosts in brackets.
pub fn key(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{}:{port}", host.to_ascii_lowercase())
    }
}

pub fn hex(fingerprint: &Fingerprint) -> String {
    fingerprint
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn from_hex(text: &str) -> Option<Fingerprint> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut fingerprint = [0u8; 32];
    for (index, byte) in fingerprint.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(fingerprint)
}

/// For people: upper-case pairs joined by colons, as certificate viewers
/// show them.
pub fn display(fingerprint: &Fingerprint) -> String {
    fingerprint
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

impl KnownHosts {
    pub fn parse(text: &str) -> Self {
        let mut hosts = Self::default();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            if let (Some(key), Some(fingerprint)) =
                (fields.next(), fields.next().and_then(from_hex))
            {
                hosts.insert(key.to_string(), fingerprint);
            }
        }
        hosts
    }

    fn insert(&mut self, key: String, fingerprint: Fingerprint) {
        match self.pins.binary_search_by(|(k, _)| k.as_str().cmp(&key)) {
            Ok(index) => self.pins[index].1 = fingerprint,
            Err(index) => self.pins.insert(index, (key, fingerprint)),
        }
    }

    pub fn get(&self, host: &str, port: u16) -> Option<Fingerprint> {
        let key = key(host, port);
        self.pins
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, fingerprint)| *fingerprint)
    }

    pub fn pin(&mut self, host: &str, port: u16, fingerprint: Fingerprint) {
        self.insert(key(host, port), fingerprint);
    }

    /// Forgets a server's pin; true if it had one.
    pub fn forget(&mut self, host: &str, port: u16) -> bool {
        let key = key(host, port);
        let before = self.pins.len();
        self.pins.retain(|(k, _)| *k != key);
        self.pins.len() != before
    }

    pub fn to_text(&self) -> String {
        let mut text = String::from(
            "# RubyChat's certificate pins: <host:port> <sha256 of the server certificate>\n\
             # Remove a line to be asked again about a server whose certificate changed.\n",
        );
        for (key, fingerprint) in &self.pins {
            text.push_str(&format!("{key} {}\n", hex(fingerprint)));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_round_trip_in_trust_desktops_format() {
        let one = [0xab; 32];
        let mut hosts = KnownHosts::default();
        hosts.pin("IRC.Example.net", 6697, one);
        hosts.pin("2001:db8::1", 6697, [1; 32]);
        let text = hosts.to_text();
        assert!(text.contains(&format!("irc.example.net:6697 {}", "ab".repeat(32))));
        assert!(text.contains("[2001:db8::1]:6697 0101"));
        let read = KnownHosts::parse(&format!("{text}\ngarbage line\nhost:1 not-hex\n"));
        assert_eq!(read, hosts);
        assert_eq!(read.get("irc.example.net", 6697), Some(one));
        assert_eq!(read.get("irc.example.net", 6698), None);
        let mut read = read;
        assert!(read.forget("irc.example.net", 6697) && !read.forget("irc.example.net", 6697));
        assert_eq!(display(&[0x0f; 32])[..8], *"0F:0F:0F");
        assert_eq!(from_hex(&hex(&one)), Some(one));
    }
}
