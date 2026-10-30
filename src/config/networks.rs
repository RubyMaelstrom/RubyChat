//! Saved networks (`networks.toml`) and their secrets (`secrets.toml`), as
//! DESIGN.md §11 lays them out. Passwords and channel keys live apart, in an
//! owner-only file, so `networks.toml` can be shared or backed up; in memory
//! a network carries both.

use crate::irc::encoding::Fallback;

use super::toml::{self, Table, Value};

/// How a server's certificate is checked (DESIGN.md §6.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CertificatePolicy {
    /// Against the WebPKI roots; a certificate pinned after a prompt also
    /// passes.
    #[default]
    Verify,
    /// Only the certificate pinned in `known_hosts`.
    Pinned,
    /// Anything: encrypted, but the server's identity unchecked.
    None,
}

impl CertificatePolicy {
    pub const ALL: [Self; 3] = [Self::Verify, Self::Pinned, Self::None];

    pub fn token(self) -> &'static str {
        match self {
            Self::Verify => "verify",
            Self::Pinned => "pinned",
            Self::None => "none",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Verify => "Verify",
            Self::Pinned => "Pinned",
            Self::None => "No verification",
        }
    }
}

/// How RubyChat logs in (DESIGN.md §6.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoginMethod {
    #[default]
    None,
    ServerPassword,
    SaslPlain,
    SaslScramSha256,
    SaslExternal,
    NickServ,
}

impl LoginMethod {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::ServerPassword,
        Self::SaslPlain,
        Self::SaslScramSha256,
        Self::SaslExternal,
        Self::NickServ,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ServerPassword => "server-password",
            Self::SaslPlain => "sasl-plain",
            Self::SaslScramSha256 => "sasl-scram-sha-256",
            Self::SaslExternal => "sasl-external",
            Self::NickServ => "nickserv",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::ServerPassword => "Server password",
            Self::SaslPlain => "SASL PLAIN",
            Self::SaslScramSha256 => "SASL SCRAM-SHA-256",
            Self::SaslExternal => "SASL EXTERNAL",
            Self::NickServ => "NickServ",
        }
    }

    /// The method takes an account name.
    pub fn uses_account(self) -> bool {
        matches!(
            self,
            Self::SaslPlain | Self::SaslScramSha256 | Self::NickServ
        )
    }

    /// The method takes a password.
    pub fn uses_password(self) -> bool {
        !matches!(self, Self::None | Self::SaslExternal)
    }

    pub fn is_sasl(self) -> bool {
        matches!(
            self,
            Self::SaslPlain | Self::SaslScramSha256 | Self::SaslExternal
        )
    }
}

/// How fast lines may go out (RFC 1459 §8.10's penalty timer).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FloodRate {
    /// Five lines at once, then one every two seconds.
    #[default]
    Standard,
    /// Three, then one every three seconds, for strict servers.
    Gentle,
    /// Ten, then one a second, for bouncers that pace on their own.
    Fast,
}

impl FloodRate {
    pub const ALL: [Self; 3] = [Self::Standard, Self::Gentle, Self::Fast];

    pub fn token(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Gentle => "gentle",
            Self::Fast => "fast",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard (5, then 1 per 2 s)",
            Self::Gentle => "Gentle (3, then 1 per 3 s)",
            Self::Fast => "Fast (10, then 1 per s)",
        }
    }

    /// The penalty per line and the window the timer may run ahead.
    pub fn timing(self) -> (std::time::Duration, std::time::Duration) {
        use std::time::Duration;
        match self {
            Self::Standard => (Duration::from_secs(2), Duration::from_secs(10)),
            Self::Gentle => (Duration::from_secs(3), Duration::from_secs(9)),
            Self::Fast => (Duration::from_secs(1), Duration::from_secs(10)),
        }
    }
}

pub fn fallback_token(fallback: Fallback) -> &'static str {
    match fallback {
        Fallback::Windows1252 => "cp1252",
        Fallback::Latin1 => "latin1",
    }
}

pub fn fallback_label(fallback: Fallback) -> &'static str {
    match fallback {
        Fallback::Windows1252 => "Windows-1252",
        Fallback::Latin1 => "Latin-1",
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedServer {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub certificate: CertificatePolicy,
}

impl SavedServer {
    pub fn new(host: impl Into<String>, port: u16, tls: bool) -> Self {
        Self {
            host: host.into(),
            port,
            tls,
            certificate: CertificatePolicy::Verify,
        }
    }

    /// `host:port`, with brackets around IPv6 addresses.
    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Login {
    pub method: LoginMethod,
    pub account: String,
    /// The login or server password, from `secrets.toml`.
    pub password: String,
    /// Disconnect rather than continue unidentified when login fails.
    pub require: bool,
    /// A PEM file with a certificate chain and its key, for SASL EXTERNAL
    /// and CertFP.
    pub client_certificate: String,
}

/// A channel to join, with its key from `secrets.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoJoin {
    pub channel: String,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedNetwork {
    pub name: String,
    pub connect_at_startup: bool,
    pub reconnect: bool,
    /// Tried in order.
    pub servers: Vec<SavedServer>,
    pub nick: String,
    pub alt_nicks: Vec<String>,
    pub username: String,
    pub realname: String,
    pub login: Login,
    pub autojoin: Vec<AutoJoin>,
    /// Lines run after connecting, as typed in the server buffer.
    pub commands: Vec<String>,
    pub fallback_encoding: Fallback,
    pub flood: FloodRate,
    pub ctcp_replies: bool,
    pub quit_message: String,
    pub part_message: String,
}

impl Default for SavedNetwork {
    fn default() -> Self {
        Self {
            name: String::new(),
            connect_at_startup: false,
            reconnect: true,
            servers: Vec::new(),
            nick: String::new(),
            alt_nicks: Vec::new(),
            username: String::new(),
            realname: String::new(),
            login: Login::default(),
            autojoin: Vec::new(),
            commands: Vec::new(),
            fallback_encoding: Fallback::Windows1252,
            flood: FloodRate::Standard,
            ctcp_replies: true,
            quit_message: String::new(),
            part_message: String::new(),
        }
    }
}

/// The saved networks, in their order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Networks {
    pub list: Vec<SavedNetwork>,
}

/// Networks offered on first launch, with their TLS addresses
/// (DESIGN.md §8.9).
pub const PRESETS: &[(&str, &str, u16)] = &[
    ("Libera.Chat", "irc.libera.chat", 6697),
    ("OFTC", "irc.oftc.net", 6697),
    ("hackint", "irc.hackint.org", 6697),
    ("Rizon", "irc.rizon.net", 6697),
    ("EFnet", "irc.efnet.org", 6697),
];

fn strings(table: &Table, key: &str, network: &str) -> Result<Vec<String>, String> {
    match table.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => Ok(text.clone()),
                _ => Err(format!(
                    "{network}: {key} holds something other than strings"
                )),
            })
            .collect(),
        Some(_) => Err(format!("{network}: {key} must be a list of strings")),
    }
}

fn choice<T: Copy>(
    table: &Table,
    key: &str,
    network: &str,
    all: &[T],
    token: impl Fn(T) -> &'static str,
) -> Result<Option<T>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => all
            .iter()
            .copied()
            .find(|item| token(*item) == text)
            .map(Some)
            .ok_or_else(|| format!("{network}: {key} can't be \"{text}\"")),
        Some(_) => Err(format!("{network}: {key} must be a string")),
    }
}

fn string_list(items: &[String]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|item| Value::String(item.clone()))
            .collect(),
    )
}

impl Networks {
    /// Reads `networks.toml`, then `secrets.toml` into it.
    pub fn parse(networks: &str, secrets: Option<&str>) -> Result<Self, String> {
        let root = toml::parse(networks).map_err(|error| format!("networks.toml: {error}"))?;
        let mut list = Vec::new();
        for table in root.tables("network") {
            let name = table.string("name").unwrap_or("").to_string();
            if name.is_empty() {
                return Err(String::from("networks.toml: a network has no name"));
            }
            if list.iter().any(|n: &SavedNetwork| n.name == name) {
                return Err(format!("networks.toml: two networks are named {name}"));
            }
            let mut network = SavedNetwork {
                connect_at_startup: table.boolean("connect_at_startup").unwrap_or(false),
                reconnect: table.boolean("reconnect").unwrap_or(true),
                nick: table.string("nick").unwrap_or("").to_string(),
                alt_nicks: strings(table, "alt_nicks", &name)?,
                username: table.string("username").unwrap_or("").to_string(),
                realname: table.string("realname").unwrap_or("").to_string(),
                autojoin: strings(table, "autojoin", &name)?
                    .into_iter()
                    .map(|channel| AutoJoin {
                        channel,
                        key: String::new(),
                    })
                    .collect(),
                commands: strings(table, "commands", &name)?,
                fallback_encoding: choice(
                    table,
                    "fallback_encoding",
                    &name,
                    &[Fallback::Windows1252, Fallback::Latin1],
                    fallback_token,
                )?
                .unwrap_or_default(),
                flood: choice(table, "flood", &name, &FloodRate::ALL, FloodRate::token)?
                    .unwrap_or_default(),
                ctcp_replies: table.boolean("ctcp_replies").unwrap_or(true),
                quit_message: table.string("quit_message").unwrap_or("").to_string(),
                part_message: table.string("part_message").unwrap_or("").to_string(),
                ..SavedNetwork::default()
            };
            for server in table.tables("server") {
                let host = server.string("host").unwrap_or("").to_string();
                if host.is_empty() {
                    return Err(format!("{name}: a server has no host"));
                }
                let tls = server.boolean("tls").unwrap_or(true);
                let port = match server.integer("port") {
                    Some(port) => u16::try_from(port)
                        .ok()
                        .filter(|&port| port != 0)
                        .ok_or_else(|| format!("{name}: {port} isn't a port"))?,
                    None if tls => 6697,
                    None => 6667,
                };
                let certificate = choice(
                    server,
                    "certificate",
                    &name,
                    &CertificatePolicy::ALL,
                    CertificatePolicy::token,
                )?
                .unwrap_or_default();
                network.servers.push(SavedServer {
                    host,
                    port,
                    tls,
                    certificate,
                });
            }
            if let Some(Value::Table(login)) = table.get("login") {
                network.login = Login {
                    method: choice(
                        login,
                        "method",
                        &name,
                        &LoginMethod::ALL,
                        LoginMethod::token,
                    )?
                    .unwrap_or_default(),
                    account: login.string("account").unwrap_or("").to_string(),
                    password: String::new(),
                    require: login.boolean("require").unwrap_or(false),
                    client_certificate: login
                        .string("client_certificate")
                        .unwrap_or("")
                        .to_string(),
                };
            }
            network.name = name;
            list.push(network);
        }
        let mut networks = Self { list };
        if let Some(secrets) = secrets {
            networks.read_secrets(secrets)?;
        }
        Ok(networks)
    }

    fn read_secrets(&mut self, text: &str) -> Result<(), String> {
        let root = toml::parse(text).map_err(|error| format!("secrets.toml: {error}"))?;
        for table in root.tables("network") {
            let name = table.string("name").unwrap_or("");
            // Secrets for a network no longer saved are dropped on the next
            // save.
            let Some(network) = self.list.iter_mut().find(|n| n.name == name) else {
                continue;
            };
            network.login.password = table.string("password").unwrap_or("").to_string();
            for entry in strings(table, "channel_keys", name)? {
                let (channel, key) = entry.split_once(' ').unwrap_or((&entry, ""));
                if let Some(join) = network
                    .autojoin
                    .iter_mut()
                    .find(|join| join.channel == channel)
                {
                    join.key = key.to_string();
                }
            }
        }
        Ok(())
    }

    /// `networks.toml`, without secrets.
    pub fn to_toml(&self) -> String {
        let mut root = Table::default();
        let tables = self
            .list
            .iter()
            .map(|network| {
                let mut table = Table::default();
                table.insert("name", Value::String(network.name.clone()));
                table.insert(
                    "connect_at_startup",
                    Value::Boolean(network.connect_at_startup),
                );
                table.insert("reconnect", Value::Boolean(network.reconnect));
                table.insert("nick", Value::String(network.nick.clone()));
                table.insert("alt_nicks", string_list(&network.alt_nicks));
                table.insert("username", Value::String(network.username.clone()));
                table.insert("realname", Value::String(network.realname.clone()));
                let channels: Vec<String> = network
                    .autojoin
                    .iter()
                    .map(|join| join.channel.clone())
                    .collect();
                table.insert("autojoin", string_list(&channels));
                table.insert("commands", string_list(&network.commands));
                table.insert(
                    "fallback_encoding",
                    Value::String(fallback_token(network.fallback_encoding).to_string()),
                );
                table.insert("flood", Value::String(network.flood.token().to_string()));
                table.insert("ctcp_replies", Value::Boolean(network.ctcp_replies));
                table.insert("quit_message", Value::String(network.quit_message.clone()));
                table.insert("part_message", Value::String(network.part_message.clone()));
                let servers = network
                    .servers
                    .iter()
                    .map(|server| {
                        let mut table = Table::default();
                        table.insert("host", Value::String(server.host.clone()));
                        table.insert("port", Value::Integer(i64::from(server.port)));
                        table.insert("tls", Value::Boolean(server.tls));
                        table.insert(
                            "certificate",
                            Value::String(server.certificate.token().to_string()),
                        );
                        table
                    })
                    .collect();
                table.insert("server", Value::Tables(servers));
                let mut login = Table::default();
                login.insert(
                    "method",
                    Value::String(network.login.method.token().to_string()),
                );
                login.insert("account", Value::String(network.login.account.clone()));
                login.insert("require", Value::Boolean(network.login.require));
                login.insert(
                    "client_certificate",
                    Value::String(network.login.client_certificate.clone()),
                );
                table.insert("login", Value::Table(login));
                table
            })
            .collect();
        root.insert("network", Value::Tables(tables));
        toml::to_string(
            &root,
            "RubyChat's saved networks. Passwords and channel keys are in\nsecrets.toml, beside this file.",
        )
    }

    /// `secrets.toml`: each network's password and channel keys.
    pub fn secrets_toml(&self) -> String {
        let mut root = Table::default();
        let tables = self
            .list
            .iter()
            .filter(|network| {
                !network.login.password.is_empty()
                    || network.autojoin.iter().any(|join| !join.key.is_empty())
            })
            .map(|network| {
                let mut table = Table::default();
                table.insert("name", Value::String(network.name.clone()));
                table.insert("password", Value::String(network.login.password.clone()));
                let keys: Vec<String> = network
                    .autojoin
                    .iter()
                    .filter(|join| !join.key.is_empty())
                    .map(|join| format!("{} {}", join.channel, join.key))
                    .collect();
                table.insert("channel_keys", string_list(&keys));
                table
            })
            .collect();
        root.insert("network", Value::Tables(tables));
        toml::to_string(
            &root,
            "RubyChat's passwords and channel keys. Keep this file private:\nRubyChat writes it readable by you alone.",
        )
    }

    pub fn find(&self, name: &str) -> Option<&SavedNetwork> {
        self.list.iter().find(|network| network.name == name)
    }

    /// A name not yet taken: `base`, else `base 2`, `base 3`…
    pub fn unique_name(&self, base: &str) -> String {
        if self.find(base).is_none() {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|name| self.find(name).is_none())
            .expect("names run out only past usize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NETWORKS: &str = r##"
[[network]]
name = "Libera.Chat"
connect_at_startup = true
nick = "ruby"
alt_nicks = ["ruby_", "ruby__"]
username = "ruby"
realname = "Ruby"
autojoin = ["#rust", "#secret"]
fallback_encoding = "cp1252"

[[network.server]]
host = "irc.libera.chat"
port = 6697
tls = true
certificate = "verify"

[network.login]
method = "sasl-scram-sha-256"
account = "ruby"
require = true

[[network]]
name = "Bouncer"
[[network.server]]
host = "bnc.example"
tls = false
"##;

    const SECRETS: &str = r##"
[[network]]
name = "Libera.Chat"
password = "hunter2"
channel_keys = ["#secret sesame"]

[[network]]
name = "Gone"
password = "dropped"
"##;

    #[test]
    fn reads_the_documented_format() {
        let networks = Networks::parse(NETWORKS, Some(SECRETS)).unwrap();
        let libera = networks.find("Libera.Chat").unwrap();
        assert!(libera.connect_at_startup && libera.reconnect);
        assert_eq!(libera.alt_nicks, ["ruby_", "ruby__"]);
        assert_eq!(
            libera.servers,
            [SavedServer::new("irc.libera.chat", 6697, true)]
        );
        assert_eq!(libera.login.method, LoginMethod::SaslScramSha256);
        assert_eq!(
            (
                libera.login.account.as_str(),
                libera.login.password.as_str(),
                libera.login.require
            ),
            ("ruby", "hunter2", true)
        );
        assert_eq!(
            libera.autojoin[1],
            AutoJoin {
                channel: String::from("#secret"),
                key: String::from("sesame")
            }
        );
        let bouncer = networks.find("Bouncer").unwrap();
        assert_eq!(bouncer.servers[0].port, 6667, "plain text defaults to 6667");
        assert_eq!(bouncer.login.method, LoginMethod::None);
    }

    #[test]
    fn writing_round_trips_and_keeps_secrets_apart() {
        let networks = Networks::parse(NETWORKS, Some(SECRETS)).unwrap();
        let text = networks.to_toml();
        assert!(
            !text.contains("hunter2") && !text.contains("sesame"),
            "no secrets in networks.toml"
        );
        let secrets = networks.secrets_toml();
        assert!(secrets.contains("hunter2") && secrets.contains("#secret sesame"));
        assert!(
            !secrets.contains("dropped"),
            "secrets of networks no longer saved go"
        );
        assert_eq!(Networks::parse(&text, Some(&secrets)).unwrap(), networks);
    }

    #[test]
    fn mistakes_are_named() {
        let error = Networks::parse(
            "[[network]]\nname = \"X\"\n[network.login]\nmethod = \"telepathy\"\n",
            None,
        )
        .unwrap_err();
        assert!(
            error.contains("X") && error.contains("telepathy"),
            "{error}"
        );
        assert!(
            Networks::parse("[[network]]\nnick = \"ruby\"\n", None).is_err(),
            "networks need names"
        );
        assert!(
            Networks::parse(
                "[[network]]\nname = \"A\"\n[[network]]\nname = \"A\"\n",
                None
            )
            .is_err()
        );
        assert!(
            Networks::parse(
                "[[network]]\nname = \"A\"\n[[network.server]]\nhost = \"h\"\nport = 70000\n",
                None
            )
            .is_err()
        );
        assert_eq!(Networks::default().unique_name("OFTC"), "OFTC");
        let networks = Networks::parse(NETWORKS, None).unwrap();
        assert_eq!(networks.unique_name("Libera.Chat"), "Libera.Chat 2");
    }
}
