//! The session (`session.toml`, in the state directory): the networks open
//! at exit with their channels and queries in order and the buffer each
//! showed, so the next start opens them again (DESIGN.md §11). Channel keys
//! are not kept here; saved networks keep theirs in `secrets.toml`.

use super::toml::{self, Table, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    Channel,
    Query,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionBuffer {
    pub kind: SessionKind,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionNetwork {
    /// The saved network it is; empty for an address from quick connect.
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub nick: String,
    pub buffers: Vec<SessionBuffer>,
    /// The buffer shown; empty for the server buffer.
    pub shown: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub networks: Vec<SessionNetwork>,
    /// The network shown.
    pub active: usize,
    /// Closing the window to the tray has been explained once.
    pub tray_explained: bool,
}

impl Session {
    /// Reads `session.toml`; whatever doesn't make sense is left out, as a
    /// session is only a convenience.
    pub fn parse(text: &str) -> Result<Self, String> {
        let root = toml::parse(text).map_err(|error| format!("session.toml: {error}"))?;
        let networks = root
            .tables("network")
            .iter()
            .filter_map(|table| {
                let host = table.string("host")?.to_string();
                let port = u16::try_from(table.integer("port")?).ok()?;
                let buffers = table
                    .tables("buffer")
                    .iter()
                    .filter_map(|buffer| {
                        let kind = match buffer.string("kind")? {
                            "channel" => SessionKind::Channel,
                            "query" => SessionKind::Query,
                            _ => return None,
                        };
                        let name = buffer
                            .string("name")
                            .filter(|name| !name.is_empty())?
                            .to_string();
                        Some(SessionBuffer { kind, name })
                    })
                    .collect();
                Some(SessionNetwork {
                    name: table.string("name").unwrap_or("").to_string(),
                    host,
                    port,
                    tls: table.boolean("tls").unwrap_or(true),
                    nick: table.string("nick").unwrap_or("").to_string(),
                    buffers,
                    shown: table.string("shown").unwrap_or("").to_string(),
                })
            })
            .collect::<Vec<_>>();
        let active = root
            .integer("active")
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        Ok(Self {
            active: active.min(networks.len().saturating_sub(1)),
            networks,
            tray_explained: root.boolean("tray_explained").unwrap_or(false),
        })
    }

    pub fn to_toml(&self) -> String {
        let mut root = Table::default();
        root.insert("active", Value::Integer(self.active as i64));
        if self.tray_explained {
            root.insert("tray_explained", Value::Boolean(true));
        }
        let tables = self
            .networks
            .iter()
            .map(|network| {
                let mut table = Table::default();
                table.insert("name", Value::String(network.name.clone()));
                table.insert("host", Value::String(network.host.clone()));
                table.insert("port", Value::Integer(i64::from(network.port)));
                table.insert("tls", Value::Boolean(network.tls));
                table.insert("nick", Value::String(network.nick.clone()));
                table.insert("shown", Value::String(network.shown.clone()));
                let buffers = network
                    .buffers
                    .iter()
                    .map(|buffer| {
                        let mut table = Table::default();
                        let kind = match buffer.kind {
                            SessionKind::Channel => "channel",
                            SessionKind::Query => "query",
                        };
                        table.insert("kind", Value::String(kind.to_string()));
                        table.insert("name", Value::String(buffer.name.clone()));
                        table
                    })
                    .collect();
                table.insert("buffer", Value::Tables(buffers));
                table
            })
            .collect();
        root.insert("network", Value::Tables(tables));
        toml::to_string(
            &root,
            "What RubyChat had open, to open again at its next start.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_round_trip_and_skip_nonsense() {
        let session = Session {
            networks: vec![
                SessionNetwork {
                    name: String::from("Libera.Chat"),
                    host: String::from("irc.libera.chat"),
                    port: 6697,
                    tls: true,
                    nick: String::from("ruby"),
                    buffers: vec![
                        SessionBuffer {
                            kind: SessionKind::Channel,
                            name: String::from("#rust"),
                        },
                        SessionBuffer {
                            kind: SessionKind::Query,
                            name: String::from("alice"),
                        },
                    ],
                    shown: String::from("#rust"),
                },
                SessionNetwork {
                    host: String::from("irc.example.net"),
                    port: 6667,
                    tls: false,
                    ..SessionNetwork::default()
                },
            ],
            active: 1,
            tray_explained: true,
        };
        assert_eq!(Session::parse(&session.to_toml()).unwrap(), session);
        let odd = Session::parse("active = 9\n[[network]]\nhost = \"h\"\nport = 1\n[[network.buffer]]\nkind = \"mystery\"\nname = \"x\"\n[[network]]\nname = \"no host\"\n").unwrap();
        assert_eq!(
            (
                odd.networks.len(),
                odd.networks[0].buffers.len(),
                odd.active
            ),
            (1, 0, 0)
        );
    }
}
