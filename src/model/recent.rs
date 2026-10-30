//! The servers RubyChat connected to most recently, newest first, for the
//! start page's quick access. Kept in `recent.toml` in the state directory.

use crate::config::toml::{self, Table, Value};

/// How many servers the start page offers.
pub const RECENT_SERVERS: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentServer {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    /// The network's name (RPL_ISUPPORT `NETWORK`), or the host until known.
    pub name: String,
    /// When RubyChat last registered there, in milliseconds since the epoch.
    pub last: i64,
}

impl RecentServer {
    fn same_server(&self, host: &str, port: u16, tls: bool) -> bool {
        self.host.eq_ignore_ascii_case(host) && self.port == port && self.tls == tls
    }

    /// `irc.libera.chat:6697`, as quick connect would read it back.
    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recent {
    servers: Vec<RecentServer>,
}

impl Recent {
    /// Reads `recent.toml`, skipping entries that don't make sense.
    pub fn parse(text: &str) -> Result<Self, toml::Error> {
        let root = toml::parse(text)?;
        let servers = root
            .tables("server")
            .iter()
            .filter_map(|table| {
                let host = table.string("host")?.trim();
                let port = u16::try_from(table.integer("port")?)
                    .ok()
                    .filter(|&p| p != 0)?;
                (!host.is_empty() && !host.contains(char::is_whitespace)).then(|| RecentServer {
                    host: host.to_string(),
                    port,
                    tls: table.boolean("tls").unwrap_or(true),
                    name: table.string("name").unwrap_or(host).to_string(),
                    last: table.integer("last").unwrap_or(0),
                })
            })
            .take(RECENT_SERVERS)
            .collect();
        Ok(Self { servers })
    }

    pub fn to_toml(&self) -> String {
        let mut root = Table::default();
        let tables = self
            .servers
            .iter()
            .map(|server| {
                let mut table = Table::default();
                table.insert("name", Value::String(server.name.clone()));
                table.insert("host", Value::String(server.host.clone()));
                table.insert("port", Value::Integer(i64::from(server.port)));
                table.insert("tls", Value::Boolean(server.tls));
                table.insert("last", Value::Integer(server.last));
                table
            })
            .collect();
        root.insert("server", Value::Tables(tables));
        toml::to_string(
            &root,
            "Servers RubyChat connected to, newest first. RubyChat rewrites this file.",
        )
    }

    pub fn servers(&self) -> &[RecentServer] {
        &self.servers
    }

    /// Puts `server` first, replacing an older entry for the same server.
    pub fn record(&mut self, server: RecentServer) {
        self.servers
            .retain(|kept| !kept.same_server(&server.host, server.port, server.tls));
        self.servers.insert(0, server);
        self.servers.truncate(RECENT_SERVERS);
    }

    /// Updates a server's network name; true if anything changed.
    pub fn rename(&mut self, host: &str, port: u16, tls: bool, name: &str) -> bool {
        match self
            .servers
            .iter_mut()
            .find(|s| s.same_server(host, port, tls))
        {
            Some(server) if server.name != name => {
                server.name = name.to_string();
                true
            }
            _ => false,
        }
    }
}

/// "just now", "5 min ago", "3 h ago", "yesterday", "4 days ago" or the
/// date.
pub fn ago(then: i64, now: i64, date: impl Fn(i64) -> String) -> String {
    let minutes = (now - then).max(0) / 60_000;
    match minutes {
        0 => String::from("just now"),
        1..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        1440..2880 => String::from("yesterday"),
        2880..10_080 => format!("{} days ago", minutes / 1440),
        _ => date(then),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str, last: i64) -> RecentServer {
        RecentServer {
            host: host.to_string(),
            port: 6697,
            tls: true,
            name: host.to_string(),
            last,
        }
    }

    #[test]
    fn keeps_the_newest_five_without_duplicates() {
        let mut recent = Recent::default();
        for (index, host) in [
            "a.example",
            "b.example",
            "c.example",
            "d.example",
            "e.example",
            "f.example",
        ]
        .iter()
        .enumerate()
        {
            recent.record(server(host, index as i64));
        }
        let hosts: Vec<&str> = recent.servers().iter().map(|s| s.host.as_str()).collect();
        assert_eq!(
            hosts,
            [
                "f.example",
                "e.example",
                "d.example",
                "c.example",
                "b.example"
            ]
        );
        recent.record(server("C.EXAMPLE", 99));
        assert_eq!(recent.servers()[0].host, "C.EXAMPLE");
        assert_eq!(recent.servers().len(), 5);
        assert!(recent.rename("c.example", 6697, true, "Example Net"));
        assert!(!recent.rename("c.example", 6697, true, "Example Net"));
        assert!(!recent.rename("c.example", 6667, false, "Other"));
    }

    #[test]
    fn round_trips_through_toml_and_skips_damage() {
        let mut recent = Recent::default();
        recent.record(server("irc.oftc.net", 1));
        recent.record(RecentServer {
            name: String::from("Libera.Chat"),
            ..server("irc.libera.chat", 2)
        });
        let text = recent.to_toml();
        assert!(text.contains("[[server]]") && text.contains("name = \"Libera.Chat\""));
        assert_eq!(Recent::parse(&text).unwrap(), recent);
        let damaged = "[[server]]\nhost = \"ok.example\"\nport = 6697\n[[server]]\nhost = \"bad port\"\nport = 70000\n";
        let parsed = Recent::parse(damaged).unwrap();
        assert_eq!(parsed.servers().len(), 1);
        assert_eq!(parsed.servers()[0].name, "ok.example");
        assert!(Recent::parse("[[server]\n").is_err());
    }

    #[test]
    fn ages_read_naturally() {
        let date = |_| String::from("2026-10-01");
        let minute = 60_000;
        assert_eq!(ago(0, 30_000, date), "just now");
        assert_eq!(ago(0, 5 * minute, date), "5 min ago");
        assert_eq!(ago(0, 3 * 60 * minute, date), "3 h ago");
        assert_eq!(ago(0, 30 * 60 * minute, date), "yesterday");
        assert_eq!(ago(0, 4 * 1440 * minute, date), "4 days ago");
        assert_eq!(ago(0, 30 * 1440 * minute, date), "2026-10-01");
    }

    #[test]
    fn addresses_bracket_ipv6() {
        assert_eq!(
            server("irc.libera.chat", 0).address(),
            "irc.libera.chat:6697"
        );
        assert_eq!(server("2001:db8::1", 0).address(), "[2001:db8::1]:6697");
    }
}
