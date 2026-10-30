//! What Enter does: quick connect on the start page; in a buffer, text for
//! the channel or query, and `/commands` (DESIGN.md §6.8). What you send
//! shows at once (local echo); the network thread sends it behind flood
//! control. Tab completes nicks, channels and commands.

use crate::irc::serialize;
use crate::model::LineKind;
use crate::model::buffer::BufferKind;
use crate::net::{ConnectConfig, NetCommand, redact};
use crate::platform::time::now_ms;

use super::App;

/// Commands Tab completes, without their slash.
const COMMANDS: &[&str] = &[
    "away",
    "clear",
    "close",
    "connect",
    "ctcp",
    "deop",
    "devoice",
    "disconnect",
    "help",
    "invite",
    "join",
    "kick",
    "me",
    "mode",
    "msg",
    "names",
    "nick",
    "notice",
    "op",
    "part",
    "query",
    "quit",
    "quote",
    "raw",
    "reconnect",
    "server",
    "topic",
    "voice",
    "whois",
];

const HELP: &[&str] = &[
    "Text goes to the channel or query; start it with // to send a line that begins with /.",
    "/join <#channel>[,<#channel>…] [keys], /part [#channel] [reason], /close",
    "/query <nick> [text], /msg <target> <text>, /me <action>, /notice <target> <text>",
    "/topic [text], /mode [target] <modes>, /kick <nick> [reason], /invite <nick> [#channel]",
    "/op, /deop, /voice, /devoice <nick>…, /whois <nick>, /names, /away [message], /nick <nick>",
    "/ctcp <nick> <command>, /quote <line> (also /raw), /clear",
    "/connect <server>, /reconnect, /disconnect [reason]",
    "Alt+Up/Down or Alt+1–9 switch buffers, Ctrl+Tab networks; Ctrl+J joins, Ctrl+W closes; Tab completes; F7 and F8 hide the nick list and sidebar.",
    "Any other /command is sent as a raw line: /whowas ruby sends WHOWAS ruby.",
];

/// Room left in a relayed line for the prefix the server adds,
/// `:nick!user@host ` (Modern IRC, "Message format"): user names are at
/// most 10 bytes and host names 63.
pub(super) const RELAY_PREFIX: usize = 1 + 1 + 10 + 1 + 63 + 1;
/// A line's limit, with its CR LF.
const LINE_BYTES: usize = 512;
/// Mode changes per MODE line when the server doesn't say (Modern IRC,
/// RPL_ISUPPORT `MODES`).
const MODES_PER_LINE: usize = 3;

/// A server address from quick connect, and what it names there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Server {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    /// A server or bouncer password, from `irc://:password@host`.
    pub password: Option<String>,
    pub entity: Option<Entity>,
}

/// What an IRC URL names on its server (draft-butcher-irc-url-04, §2.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Entity {
    /// A channel to join, with its key (the `key` option, §2.6.1).
    Channel { name: String, key: Option<String> },
    /// Someone to talk to (`,isuser`); nothing is sent to them.
    Nick(String),
}

/// `%XX` escapes decoded (RFC 3986, §2.1); malformed ones stay as they are.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|b| (*b as char).to_digit(16));
        match (bytes[index], hex(index + 1), hex(index + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                index += 3;
            }
            (byte, ..) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The entity, flags and options after an IRC URL's host
/// (draft-butcher-irc-url-04, §2.1 and §2.5–2.6).
fn parse_entity(path: &str) -> Option<Entity> {
    let (path, options) = path.split_once('?').unwrap_or((path, ""));
    let mut parts = path.split(',');
    let name = percent_decode(parts.next().unwrap_or(""));
    if name.is_empty() {
        return None;
    }
    if parts.any(|flag| flag.eq_ignore_ascii_case("isuser")) {
        return Some(Entity::Nick(name));
    }
    let key = options
        .split('&')
        .filter_map(|option| option.split_once('='))
        .find(|(option, _)| option.eq_ignore_ascii_case("key"))
        .map(|(_, key)| percent_decode(key));
    // Before the server names its channel prefixes, Modern IRC's default
    // CHANTYPES apply; a name without one gets "#" (§2.5.1).
    let name = if name.starts_with(['#', '&']) {
        name
    } else {
        format!("#{name}")
    };
    Some(Entity::Channel { name, key })
}

/// Reads `host`, `host:port`, `[ipv6]:port`, or an IRC URL:
/// `ircs://[:password@]host[:port][/entity[,flags][?key=…]]`
/// (draft-butcher-irc-url-04). TLS unless the address says `irc://` or port
/// 6667; `+port` asks for TLS on any port.
pub(super) fn parse_server(input: &str) -> Result<Server, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(String::from(
            "Type a server address, such as irc.libera.chat.",
        ));
    }
    let lower = input.to_ascii_lowercase();
    let (scheme_tls, rest) = if lower.starts_with("ircs://") {
        (Some(true), &input[7..])
    } else if lower.starts_with("irc://") {
        (Some(false), &input[6..])
    } else {
        (None, input)
    };
    let location = rest.split(['/', '?', '#']).next().unwrap_or("");
    let entity = rest[location.len()..]
        .strip_prefix('/')
        .and_then(parse_entity);
    let (authinfo, authority) = match location.rsplit_once('@') {
        Some((authinfo, authority)) => (Some(authinfo), authority),
        None => (None, location),
    };
    let password = authinfo
        .and_then(|authinfo| authinfo.split_once(':'))
        .map(|(_, password)| percent_decode(password.split(';').next().unwrap_or("")))
        .filter(|password| !password.is_empty());
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6
            .split_once(']')
            .ok_or_else(|| format!("{authority} is missing its closing ]"))?;
        (host, after.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, Some(port)),
            _ => (authority, None),
        }
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(format!("“{input}” isn't a server address."));
    }
    let (plus, port) = match port {
        Some(port) => match port.strip_prefix('+') {
            Some(port) => (true, Some(port)),
            None => (false, Some(port)),
        },
        None => (false, None),
    };
    let port = match port {
        Some(port) => port
            .parse::<u16>()
            .ok()
            .filter(|&port| port != 0)
            .ok_or_else(|| format!("“{port}” isn't a port number."))?,
        None if scheme_tls == Some(false) => 6667,
        None => 6697,
    };
    Ok(Server {
        host: host.to_string(),
        port,
        tls: plus || scheme_tls.unwrap_or(port != 6667),
        password,
        entity,
    })
}

/// A nickname from the login name, kept to the characters IRC allows.
fn default_nick() -> String {
    let login = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let nick: String = login
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_[]\\`^{}|".contains(*c))
        .take(16)
        .collect();
    if nick.is_empty() || nick.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        String::from("rubychat")
    } else {
        nick
    }
}

/// `text` in pieces of at most `budget` bytes, breaking after a space when
/// one is near the end of a piece, and never inside a character.
pub(super) fn split_message(text: &str, budget: usize) -> Vec<&str> {
    let budget = budget.max(32);
    let mut pieces = Vec::new();
    let mut rest = text;
    while rest.len() > budget {
        let mut end = budget;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        if let Some(space) = rest[..end].rfind(' ').filter(|&space| space >= end * 3 / 4) {
            end = space + 1;
        }
        pieces.push(rest[..end].trim_end());
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        pieces.push(rest);
    }
    pieces
}

/// Tab completion in progress: Tab again offers the next candidate.
pub(super) struct Completion {
    /// Where the completed word starts and ends in the input.
    start: usize,
    end: usize,
    candidates: Vec<String>,
    index: usize,
    suffix: &'static str,
}

impl App {
    pub(super) fn submit(&mut self) {
        if self.welcome_visible() {
            let Some(address) = self.prompt.submit() else {
                return;
            };
            match parse_server(&address) {
                Ok(server) => self.connect(server),
                Err(message) => {
                    self.prompt_message = Some(message);
                    self.mark_all();
                }
            }
            return;
        }
        let Some(line) = self.networks[self.active].buffer_mut().input.submit() else {
            return;
        };
        self.mark_input();
        self.run_command(&line);
    }

    pub(super) fn connect(&mut self, server: Server) {
        let mut config =
            ConnectConfig::new(server.host.clone(), server.port, server.tls, default_nick());
        config.password = server.password;
        let title = server
            .host
            .strip_prefix("irc.")
            .unwrap_or(&server.host)
            .to_string();
        let id = self.add_network(title, config.clone());
        // What the address names opens at once; a channel joins on
        // registering.
        let network = self.networks.last_mut().expect("just added");
        match server.entity {
            Some(Entity::Channel { name, key }) => {
                let (index, _) = network.state.open(BufferKind::Channel, &name);
                let buffer = &mut network.state.buffers[index];
                buffer.rejoin = true;
                // Where rejoining looks for the key, until the server says.
                buffer.modes.extend(key.map(|key| ('k', Some(key))));
                network.active = index;
            }
            Some(Entity::Nick(nick)) => {
                network.active = network.state.open(BufferKind::Query, &nick).0
            }
            None => {}
        }
        self.send(NetCommand::Connect(id, config));
        self.prompt_open = false;
        self.activate(self.networks.len() - 1);
        self.mark_all();
    }

    pub(super) fn run_command(&mut self, line: &str) {
        let (name, args) = match line.strip_prefix('/') {
            Some(rest) if !rest.starts_with('/') => rest
                .split_once(' ')
                .map_or((rest, ""), |(name, args)| (name, args.trim())),
            // `//text` sends `/text`.
            Some(_) => return self.say_here(&line[1..]),
            None => return self.say_here(line),
        };
        let index = self.active;
        let id = self.networks[index].id;
        let (kind, here) = {
            let buffer = self.networks[index].buffer();
            (buffer.kind, buffer.name.clone())
        };
        let channel = (kind == BufferKind::Channel).then(|| here.clone());
        match name.to_ascii_lowercase().as_str() {
            "me" => {
                if kind == BufferKind::Server {
                    self.say(LineKind::Error, "!", "/me needs a channel or query.");
                } else if args.is_empty() {
                    self.say(LineKind::Error, "!", "Usage: /me <action>");
                } else {
                    self.send_text(LineKind::Action, &here, args);
                }
            }
            command @ ("msg" | "notice") => match args.split_once(' ') {
                Some((target, text)) if !text.trim().is_empty() => {
                    let kind = if command == "msg" {
                        LineKind::Message
                    } else {
                        LineKind::Notice
                    };
                    self.send_text(kind, target, text);
                }
                _ => self.say(
                    LineKind::Error,
                    "!",
                    format!("Usage: /{command} <target> <text>"),
                ),
            },
            "query" => {
                let (nick, text) = args.split_once(' ').unwrap_or((args, ""));
                if nick.is_empty() || self.networks[index].state.isupport.is_channel(nick) {
                    self.say(LineKind::Error, "!", "Usage: /query <nick> [text]");
                } else {
                    self.open_query(nick);
                    if !text.trim().is_empty() {
                        self.send_text(LineKind::Message, nick, text);
                    }
                }
            }
            "join" | "j" => self.join(args),
            "part" | "leave" => self.part(args),
            "close" => {
                let active = self.networks[index].active;
                self.close_buffer(active);
            }
            "topic" => match channel {
                None => self.say(LineKind::Error, "!", "/topic works in a channel."),
                Some(channel) if args.is_empty() => {
                    let buffer = self.networks[index].buffer();
                    let text = match (&buffer.topic, &buffer.topic_by) {
                        (Some(topic), Some(by)) => {
                            format!("Topic: {} (set by {by})", crate::irc::format::strip(topic))
                        }
                        (Some(topic), None) => {
                            format!("Topic: {}", crate::irc::format::strip(topic))
                        }
                        (None, _) => format!("{channel} has no topic."),
                    };
                    self.say(LineKind::Event, "•", text);
                }
                Some(channel) => self.send_built("TOPIC", &[&channel, args]),
            },
            "mode" => {
                let words: Vec<&str> = args.split_whitespace().collect();
                // Without a target: the channel here, or your own modes.
                let target = match (&channel, words.first()) {
                    (_, Some(first)) if !first.starts_with(['+', '-']) => None,
                    (Some(channel), None) => {
                        let modes = self.networks[index].buffer().mode_string();
                        let text = if modes.is_empty() {
                            format!("{channel} has no modes set.")
                        } else {
                            format!("Modes: {modes}")
                        };
                        return self.say(LineKind::Event, "•", text);
                    }
                    (Some(channel), Some(_)) => Some(channel.clone()),
                    (None, _) => Some(self.networks[index].state.nick.clone()),
                };
                let mut params: Vec<&str> = target.as_deref().into_iter().collect();
                params.extend(&words);
                self.send_built("MODE", &params);
            }
            command @ ("op" | "deop" | "voice" | "devoice") => match channel {
                None => self.say(
                    LineKind::Error,
                    "!",
                    format!("/{command} works in a channel."),
                ),
                Some(channel) => {
                    let nicks: Vec<&str> = args.split_whitespace().collect();
                    if nicks.is_empty() {
                        return self.say(
                            LineKind::Error,
                            "!",
                            format!("Usage: /{command} <nick>…"),
                        );
                    }
                    let (sign, letter) = match command {
                        "op" => ('+', 'o'),
                        "deop" => ('-', 'o'),
                        "voice" => ('+', 'v'),
                        _ => ('-', 'v'),
                    };
                    for batch in nicks.chunks(MODES_PER_LINE) {
                        let modes = format!("{sign}{}", letter.to_string().repeat(batch.len()));
                        let mut params = vec![channel.as_str(), modes.as_str()];
                        params.extend(batch);
                        self.send_built("MODE", &params);
                    }
                }
            },
            "kick" => match (channel, args.split_once(' ').unwrap_or((args, ""))) {
                (Some(channel), (nick, reason)) if !nick.is_empty() => {
                    let mut params = vec![channel.as_str(), nick];
                    if !reason.trim().is_empty() {
                        params.push(reason.trim());
                    }
                    self.send_built("KICK", &params);
                }
                _ => self.say(
                    LineKind::Error,
                    "!",
                    "Usage, in a channel: /kick <nick> [reason]",
                ),
            },
            "invite" => {
                let words: Vec<&str> = args.split_whitespace().collect();
                match (words.as_slice(), &channel) {
                    ([nick, target], _) => self.send_built("INVITE", &[nick, target]),
                    ([nick], Some(channel)) => self.send_built("INVITE", &[nick, channel]),
                    _ => self.say(LineKind::Error, "!", "Usage: /invite <nick> [#channel]"),
                }
            }
            "names" => match (args.split_whitespace().next(), channel) {
                (Some(target), _) => self.send_built("NAMES", &[target]),
                (None, Some(channel)) => self.send_built("NAMES", &[&channel]),
                (None, None) => self.say(LineKind::Error, "!", "Usage: /names <#channel>"),
            },
            "whois" => {
                let Some(nick) = args.split_whitespace().next() else {
                    return self.say(LineKind::Error, "!", "Usage: /whois <nick>");
                };
                // The replies come here.
                self.networks[index].state.whois_to =
                    (kind != BufferKind::Server).then(|| here.clone());
                self.send_built("WHOIS", &[nick]);
            }
            "away" => {
                if args.is_empty() {
                    self.send_raw_quietly(String::from("AWAY"));
                } else {
                    self.send_built("AWAY", &[args]);
                }
            }
            "nick" => self.send_built("NICK", &[args]),
            "ctcp" => match args.split_once(' ') {
                Some((target, request)) if !request.trim().is_empty() => {
                    let body = format!("\u{1}{}\u{1}", request.trim().to_ascii_uppercase());
                    if let Ok(line) = serialize(&[], None, "PRIVMSG", &[target, &body]) {
                        self.say(
                            LineKind::Sent,
                            "→",
                            format!("CTCP {} to {target}", request.trim().to_ascii_uppercase()),
                        );
                        self.send_line(line);
                    }
                }
                _ => self.say(LineKind::Error, "!", "Usage: /ctcp <nick> <command>"),
            },
            "quote" | "raw" => self.send_raw(args.to_string()),
            "connect" | "server" => match parse_server(args) {
                Ok(server) => self.connect(server),
                Err(message) => self.say(LineKind::Error, "!", message),
            },
            "disconnect" | "quit" => {
                let reason = (!args.is_empty()).then(|| args.to_string());
                self.send(NetCommand::Disconnect(id, reason));
            }
            "reconnect" => {
                let config = self.networks[index].config.clone();
                self.send(NetCommand::Connect(id, config));
            }
            "clear" => {
                self.networks[index].buffer_mut().clear();
                self.selection = None;
                self.mark_chat();
            }
            "help" => {
                for line in HELP {
                    self.say(LineKind::Status, "•", *line);
                }
            }
            other => {
                let line = format!("{} {args}", other.to_ascii_uppercase());
                self.send_raw(line.trim_end().to_string());
            }
        }
    }

    /// Text typed in the shown buffer: a message to its channel or query.
    fn say_here(&mut self, text: &str) {
        let buffer = self.networks[self.active].buffer();
        if buffer.kind == BufferKind::Server {
            self.say(
                LineKind::Error,
                "!",
                "The server buffer has no one to send text to: /join a channel, /query someone, or /quote a raw line.",
            );
            return;
        }
        let target = buffer.name.clone();
        self.send_text(LineKind::Message, &target, text);
    }

    /// Sends a message, action or notice to `target`, split to fit, and
    /// shows it in the target's buffer if one is open, else here.
    fn send_text(&mut self, kind: LineKind, target: &str, text: &str) {
        if !self.can_send() {
            return;
        }
        let network = &self.networks[self.active];
        let (verb, wrap) = match kind {
            LineKind::Notice => ("NOTICE", ("", "")),
            LineKind::Action => ("PRIVMSG", ("\u{1}ACTION ", "\u{1}")),
            _ => ("PRIVMSG", ("", "")),
        };
        let overhead = RELAY_PREFIX
            + network.state.nick.len()
            + verb.len()
            + 1
            + target.len()
            + 2
            + wrap.0.len()
            + wrap.1.len()
            + 2;
        let budget = LINE_BYTES.saturating_sub(overhead);
        let buffer = network.state.find(target);
        let id = network.id;
        let now = now_ms();
        for piece in split_message(text, budget) {
            let body = format!("{}{piece}{}", wrap.0, wrap.1);
            let Ok(line) = serialize(&[], None, verb, &[target, &body]) else {
                self.say(LineKind::Error, "!", "That can't be sent.");
                return;
            };
            self.send(NetCommand::Send(id, line));
            let network = &mut self.networks[self.active];
            match buffer {
                Some(index) => {
                    network.state.echo(index, kind, piece, now);
                    network.state.buffers[index].anchor = None;
                }
                None => {
                    let shown = network.active;
                    let what = if kind == LineKind::Notice {
                        format!("-{target}- {piece}")
                    } else {
                        format!("*{target}* {piece}")
                    };
                    network.push(shown, LineKind::Sent, "→", what);
                }
            }
        }
        let shown = self.networks[self.active].active;
        if buffer.is_none_or(|index| index == shown) {
            self.return_to_present();
            self.mark_chat();
        } else {
            self.mark_sidebar();
        }
    }

    /// Opens a query with `nick` and shows it.
    pub(super) fn open_query(&mut self, nick: &str) {
        let network = &mut self.networks[self.active];
        let (index, opened) = network.state.open(BufferKind::Query, nick);
        if opened {
            self.mark_sidebar();
        }
        self.activate_buffer(index);
    }

    /// Joins channels at once: their buffers open (DESIGN.md §10) and join
    /// when the server agrees, or on registering if not yet connected.
    pub(super) fn join(&mut self, args: &str) {
        let mut words = args.split_whitespace();
        let Some(list) = words.next() else {
            return self.say(
                LineKind::Error,
                "!",
                "Usage: /join <#channel>[,<#channel>…] [keys]",
            );
        };
        let keys = words.next();
        let network = &mut self.networks[self.active];
        let names: Vec<String> = list
            .split(',')
            .filter(|name| !name.is_empty())
            .map(|name| {
                if network.state.isupport.is_channel(name) {
                    name.to_string()
                } else {
                    format!("#{name}")
                }
            })
            .collect();
        let mut first = None;
        let mut to_join = Vec::new();
        for name in &names {
            let (index, _) = network.state.open(BufferKind::Channel, name);
            let buffer = &mut network.state.buffers[index];
            buffer.rejoin = true;
            if !buffer.joined {
                to_join.push(name.as_str());
            }
            first.get_or_insert(index);
        }
        let connected = network.registered() && !network.demo;
        let id = network.id;
        if !to_join.is_empty() && connected {
            let channels = to_join.join(",");
            let mut params = vec![channels.as_str()];
            params.extend(keys);
            if let Ok(line) = serialize(&[], None, "JOIN", &params) {
                self.send(NetCommand::Send(id, line));
            }
        }
        self.mark_sidebar();
        if let Some(index) = first {
            self.activate_buffer(index);
            if !to_join.is_empty() && !connected {
                self.say(
                    LineKind::Status,
                    "•",
                    "Not connected yet: joins once connected.",
                );
            }
        }
    }

    /// Parts a channel and closes its buffer at once (DESIGN.md §10).
    fn part(&mut self, args: &str) {
        let network = &self.networks[self.active];
        let first = args.split_whitespace().next().unwrap_or("");
        let (name, reason) = if network.state.isupport.is_channel(first) {
            (first.to_string(), args[first.len()..].trim())
        } else if network.buffer().kind == BufferKind::Channel {
            (network.buffer().name.clone(), args)
        } else {
            return self.say(LineKind::Error, "!", "Usage: /part [#channel] [reason]");
        };
        let Some(index) = network.state.find(&name) else {
            return self.say(LineKind::Error, "!", format!("{name} isn't open."));
        };
        let buffer = &network.state.buffers[index];
        if buffer.joined && network.connected() && !network.demo {
            let mut params = vec![name.as_str()];
            if !reason.is_empty() {
                params.push(reason);
            }
            if let Ok(line) = serialize(&[], None, "PART", &params) {
                let id = network.id;
                self.send(NetCommand::Send(id, line));
            }
        }
        // Closing sends no second PART: the buffer is no longer joined.
        self.networks[self.active].state.buffers[index].joined = false;
        self.close_buffer(index);
    }

    /// Adds a line to the shown buffer and brings it into view.
    pub(super) fn say(&mut self, kind: LineKind, prefix: &str, text: impl Into<String>) {
        if let Some(network) = self.networks.get_mut(self.active) {
            let index = network.active;
            network.push(index, kind, prefix, text);
            network.buffer_mut().anchor = None;
            self.mark_chat();
        }
    }

    /// Whether the active network can send, saying why not.
    fn can_send(&mut self) -> bool {
        let network = &self.networks[self.active];
        if network.demo {
            self.say(LineKind::Error, "!", "Demo networks never connect.");
            false
        } else if !network.connected() {
            self.say(
                LineKind::Error,
                "!",
                "Not connected; nothing was sent. /reconnect connects again.",
            );
            false
        } else {
            true
        }
    }

    fn send_built(&mut self, command: &str, params: &[&str]) {
        if params.first().is_none_or(|first| first.is_empty()) {
            self.say(
                LineKind::Error,
                "!",
                format!("{command} needs a target; type /help for usage."),
            );
            return;
        }
        match serialize(&[], None, command, params) {
            Ok(line) => self.send_raw(line),
            Err(error) => self.say(LineKind::Error, "!", format!("Can't send that: {error}")),
        }
    }

    /// Sends a line, showing it (with passwords redacted).
    fn send_raw(&mut self, line: String) {
        if line.trim().is_empty() {
            self.say(LineKind::Error, "!", "Usage: /quote <line>");
        } else if line.contains(['\r', '\n', '\0']) {
            self.say(LineKind::Error, "!", "A line can't contain line breaks.");
        } else if self.can_send() {
            self.say(LineKind::Sent, "→", redact(&line));
            let id = self.networks[self.active].id;
            self.send(NetCommand::Send(id, line));
        }
    }

    /// Sends a line without showing it.
    fn send_raw_quietly(&mut self, line: String) {
        if self.can_send() {
            let id = self.networks[self.active].id;
            self.send(NetCommand::Send(id, line));
        }
    }

    fn send_line(&mut self, line: String) {
        self.send_raw_quietly(line);
    }

    /// Tab: completes the word before the caret—a command at the start of
    /// the line, a channel, or a nick (recent speakers first, with `: `
    /// after it at the start of a line)—and Tab again offers the next.
    pub(super) fn complete(&mut self, backward: bool) {
        let network = &mut self.networks[self.active];
        if let Some(completion) = &mut self.completion {
            let count = completion.candidates.len();
            completion.index = if backward {
                (completion.index + count - 1) % count
            } else {
                (completion.index + 1) % count
            };
            let replacement = format!(
                "{}{}",
                completion.candidates[completion.index], completion.suffix
            );
            network
                .buffer_mut()
                .input
                .replace(completion.start..completion.end, &replacement);
            completion.end = completion.start + replacement.len();
            self.mark_input();
            return;
        }
        let state = &mut network.state;
        let buffer = &state.buffers[network.active];
        let text = buffer.input.text();
        let cursor = buffer.input.cursor();
        let start = text[..cursor].rfind(' ').map_or(0, |space| space + 1);
        let word = &text[start..cursor];
        if word.is_empty() {
            return;
        }
        let (candidates, suffix): (Vec<String>, &'static str) = if start == 0
            && word.starts_with('/')
        {
            let typed = word[1..].to_ascii_lowercase();
            let commands = COMMANDS
                .iter()
                .filter(|c| c.starts_with(&typed))
                .map(|c| format!("/{c}"))
                .collect();
            (commands, " ")
        } else if state.isupport.is_channel(word) {
            let folded = state.fold(word);
            let channels = state
                .buffers
                .iter()
                .filter(|b| b.kind == BufferKind::Channel && b.key.starts_with(&folded))
                .map(|b| b.name.clone())
                .collect();
            (channels, " ")
        } else {
            let nicks: Vec<String> = match buffer.kind {
                BufferKind::Channel => buffer
                    .members
                    .completions(word, &state.isupport)
                    .into_iter()
                    .filter(|nick| !state.isupport.casemap.eq(nick, &state.nick))
                    .map(str::to_string)
                    .collect(),
                BufferKind::Query if state.fold(&buffer.name).starts_with(&state.fold(word)) => {
                    vec![buffer.name.clone()]
                }
                _ => Vec::new(),
            };
            (nicks, if start == 0 { ": " } else { " " })
        };
        if candidates.is_empty() {
            return;
        }
        let replacement = format!("{}{suffix}", candidates[0]);
        network
            .buffer_mut()
            .input
            .replace(start..cursor, &replacement);
        self.completion = Some(Completion {
            start,
            end: start + replacement.len(),
            candidates,
            index: 0,
            suffix,
        });
        self.mark_input();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(host: &str, port: u16, tls: bool) -> Result<Server, String> {
        Ok(Server {
            host: host.to_string(),
            port,
            tls,
            password: None,
            entity: None,
        })
    }

    #[test]
    fn quick_connect_reads_addresses() {
        assert_eq!(
            parse_server("irc.libera.chat"),
            server("irc.libera.chat", 6697, true)
        );
        assert_eq!(
            parse_server(" irc.example.net:6667 "),
            server("irc.example.net", 6667, false)
        );
        assert_eq!(
            parse_server("irc.example.net:+7000"),
            server("irc.example.net", 7000, true)
        );
        assert_eq!(
            parse_server("irc.example.net:7000"),
            server("irc.example.net", 7000, true)
        );
        assert_eq!(
            parse_server("IRCS://irc.example.net:6697"),
            server("irc.example.net", 6697, true)
        );
        assert_eq!(
            parse_server("[2001:db8::1]:6697"),
            server("2001:db8::1", 6697, true)
        );
        assert_eq!(
            parse_server("2001:db8::1"),
            server("2001:db8::1", 6697, true)
        );
        assert!(parse_server("").is_err());
        assert!(parse_server("irc.example.net:port").is_err());
        assert!(parse_server("irc.example.net:0").is_err());
        assert!(parse_server("two words").is_err());
    }

    #[test]
    fn irc_urls_name_a_channel_or_a_nick() {
        // draft-butcher-irc-url-04, §3: four ways to name #worldchat.
        let channel = |name: &str, key: Option<&str>| {
            Some(Entity::Channel {
                name: name.to_string(),
                key: key.map(str::to_string),
            })
        };
        for url in [
            "irc://irc.ircnet.net/#worldchat,ischannel",
            "irc://irc.ircnet.net/%23worldchat,ischannel",
            "irc://irc.ircnet.net/#worldchat",
            "irc://irc.ircnet.net:6667/%23worldchat",
        ] {
            let server = parse_server(url).unwrap();
            assert_eq!(
                (server.host.as_str(), server.port, server.tls),
                ("irc.ircnet.net", 6667, false)
            );
            assert_eq!(server.entity, channel("#worldchat", None), "{url}");
        }
        let entity = |url: &str| parse_server(url).unwrap().entity;
        assert_eq!(
            entity("irc://irc.austnet.org/%23foobar?key=bazqux"),
            channel("#foobar", Some("bazqux"))
        );
        assert_eq!(
            entity("irc://irc.alien.net.au/pickle,isuser"),
            Some(Entity::Nick(String::from("pickle")))
        );
        assert_eq!(
            entity("irc.libera.chat/rust"),
            channel("#rust", None),
            "a missing prefix becomes #"
        );
        assert_eq!(
            entity("ircs://irc.libera.chat/%23caf%C3%A9"),
            channel("#café", None)
        );
        assert_eq!(entity("irc.libera.chat/"), None);
        let bouncer = parse_server("ircs://ruby:hunter2@bouncer.example:6697/").unwrap();
        assert_eq!(
            (bouncer.host.as_str(), bouncer.password.as_deref()),
            ("bouncer.example", Some("hunter2"))
        );
        assert_eq!(
            parse_server("irc://ruby@irc.example.net").unwrap().host,
            "irc.example.net"
        );
    }

    #[test]
    fn long_messages_split_at_spaces() {
        let text = "word ".repeat(100);
        let pieces = split_message(text.trim_end(), 100);
        assert!(
            pieces
                .iter()
                .all(|piece| piece.len() <= 100 && !piece.ends_with(' '))
        );
        assert_eq!(pieces.concat().replace(' ', ""), "word".repeat(100));
        let unbroken = "é".repeat(100);
        let pieces = split_message(&unbroken, 51);
        assert_eq!(pieces[0].len(), 50, "never inside a character");
        assert_eq!(pieces.concat(), unbroken);
        assert_eq!(split_message("short", 400), ["short"]);
    }

    #[test]
    fn default_nicks_are_valid() {
        let nick = default_nick();
        assert!(!nick.is_empty() && nick.len() <= 16);
        assert!(!nick.starts_with(|c: char| c.is_ascii_digit() || c == '-'));
    }
}
