//! A network as the interface sees it: your nick, what the server
//! advertised, and the buffers. Each message is routed to the buffer it
//! belongs in and updates membership, modes and topics (Modern IRC's client
//! messages and numerics, with IRCv3 multi-prefix and userhost-in-names),
//! and CTCP queries are answered at a bounded rate (DESIGN.md §6.8).

use std::collections::HashMap;

use crate::irc::format::strip;
use crate::irc::isupport::Isupport;
use crate::irc::{Message, serialize, split_source};
use crate::platform::time;

use super::buffer::{Buffer, BufferKind};
use super::members::Member;
use super::{Line, LineKind, describe, message_time};

/// What RubyChat answers CTCP CLIENTINFO with.
const CLIENTINFO: &str = "ACTION CLIENTINFO PING TIME VERSION";
/// CTCP replies: a burst of three, then one per three seconds.
const CTCP_BURST: f64 = 3.0;
const CTCP_REFILL_MS: f64 = 3000.0;

/// What a message changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Buffers that changed, by index.
    pub buffers: Vec<usize>,
    /// Buffers whose topic, modes or members changed, by index.
    pub details: Vec<usize>,
    /// Buffers were opened or renamed, or a channel was joined or left.
    pub list: bool,
    /// Lines to send in reply (CTCP).
    pub replies: Vec<String>,
    /// Buffers that got a line that notifies: a highlight, or a private
    /// message, by index.
    pub alerts: Vec<usize>,
}

impl Applied {
    fn touch(&mut self, index: usize) {
        if !self.buffers.contains(&index) {
            self.buffers.push(index);
        }
    }

    fn detail(&mut self, index: usize) {
        if !self.details.contains(&index) {
            self.details.push(index);
        }
    }
}

/// JOIN lines stay this short, however many channels they carry.
const JOIN_LINE_BYTES: usize = 400;

pub struct NetworkState {
    /// Your nick as the server last confirmed it.
    pub nick: String,
    pub isupport: Isupport,
    /// The server buffer first, then channels and queries in opening order.
    pub buffers: Vec<Buffer>,
    /// NAMES replies being collected, by casefolded channel.
    names: HashMap<String, Vec<Member>>,
    /// The buffer WHOIS replies go to: the one that asked.
    pub whois_to: Option<String>,
    ctcp_tokens: f64,
    ctcp_time: i64,
    /// The CTCP VERSION answer.
    pub version: String,
    /// Answer CTCP queries (VERSION, PING, TIME, CLIENTINFO).
    pub ctcp_replies: bool,
    /// PART's reason when none is given.
    pub part_message: String,
}

impl NetworkState {
    pub fn new(nick: impl Into<String>, server: impl Into<String>) -> Self {
        Self {
            nick: nick.into(),
            isupport: Isupport::default(),
            buffers: vec![Buffer::new(BufferKind::Server, server, "")],
            names: HashMap::new(),
            whois_to: None,
            ctcp_tokens: CTCP_BURST,
            ctcp_time: 0,
            version: format!("RubyChat {}", env!("CARGO_PKG_VERSION")),
            ctcp_replies: true,
            part_message: String::new(),
        }
    }

    pub fn fold(&self, name: &str) -> String {
        self.isupport.casemap.fold(name)
    }

    pub fn is_me(&self, nick: &str) -> bool {
        self.isupport.casemap.eq(nick, &self.nick)
    }

    /// The channel or query named `name`.
    pub fn find(&self, name: &str) -> Option<usize> {
        let key = self.fold(name);
        self.buffers
            .iter()
            .position(|buffer| buffer.kind != BufferKind::Server && buffer.key == key)
    }

    /// The channel or query named `name`, opened at the end if needed; true
    /// when it was opened.
    pub fn open(&mut self, kind: BufferKind, name: &str) -> (usize, bool) {
        if let Some(index) = self.find(name) {
            return (index, false);
        }
        let key = self.fold(name);
        self.buffers.push(Buffer::new(kind, name, key));
        (self.buffers.len() - 1, true)
    }

    /// Closes a channel or query; the server buffer stays.
    pub fn close(&mut self, index: usize) -> Option<Buffer> {
        (index > 0 && index < self.buffers.len()).then(|| self.buffers.remove(index))
    }

    /// JOIN lines for the channels to join again after registering, those
    /// with keys first in each line as JOIN requires, several channels to a
    /// line (Modern IRC, JOIN).
    pub fn rejoin_lines(&self) -> Vec<String> {
        let mut channels: Vec<(&str, Option<&str>)> = self
            .buffers
            .iter()
            .filter(|buffer| buffer.kind == BufferKind::Channel && buffer.rejoin && !buffer.joined)
            .map(|buffer| (buffer.name.as_str(), buffer.key_mode()))
            .collect();
        channels.sort_by_key(|(_, key)| key.is_none());
        let mut lines = Vec::new();
        let mut batch: Vec<(&str, Option<&str>)> = Vec::new();
        let line = |batch: &[(&str, Option<&str>)]| {
            let names: Vec<&str> = batch.iter().map(|(name, _)| *name).collect();
            let keys: Vec<&str> = batch.iter().filter_map(|(_, key)| *key).collect();
            let mut line = format!("JOIN {}", names.join(","));
            if !keys.is_empty() {
                line.push(' ');
                line.push_str(&keys.join(","));
            }
            line
        };
        for channel in channels {
            batch.push(channel);
            if batch.len() > 1 && line(&batch).len() > JOIN_LINE_BYTES {
                batch.pop();
                lines.push(line(&batch));
                batch.clear();
                batch.push(channel);
            }
        }
        if !batch.is_empty() {
            lines.push(line(&batch));
        }
        lines
    }

    /// The connection ended: channels are no longer joined.
    pub fn disconnected(&mut self) {
        for buffer in &mut self.buffers {
            if buffer.kind == BufferKind::Channel {
                buffer.joined = false;
                buffer.members.clear();
            }
        }
        self.names.clear();
    }

    /// Whether `text` mentions your nick as a whole word.
    pub fn mentions(&self, text: &str) -> bool {
        let text = self.fold(&strip(text));
        let nick = self.fold(&self.nick);
        if nick.is_empty() {
            return false;
        }
        let nick_char = |c: char| c.is_alphanumeric() || "-_[]{}\\|`^".contains(c);
        let mut start = 0;
        while let Some(found) = text[start..].find(&nick) {
            let at = start + found;
            let end = at + nick.len();
            let before = text[..at].chars().next_back();
            let after = text[end..].chars().next();
            if !before.is_some_and(nick_char) && !after.is_some_and(nick_char) {
                return true;
            }
            start = at + nick.chars().next().map_or(1, char::len_utf8);
        }
        false
    }

    fn add(&mut self, index: usize, line: Line, applied: &mut Applied) {
        let private = self.buffers[index].kind == BufferKind::Query
            && matches!(line.kind, LineKind::Message | LineKind::Action);
        if !line.own && (line.highlight || private) && !applied.alerts.contains(&index) {
            applied.alerts.push(index);
        }
        self.buffers[index].push(line);
        applied.touch(index);
    }

    fn event(
        &mut self,
        index: usize,
        time: i64,
        prefix: &str,
        text: String,
        applied: &mut Applied,
    ) {
        self.add(
            index,
            Line::new(time, LineKind::Event, prefix, text),
            applied,
        );
    }

    /// Shows something you sent (local echo): a message, action or notice
    /// in `index`, as yours.
    pub fn echo(&mut self, index: usize, kind: LineKind, text: &str, now: i64) {
        let mut line = Line::message(now, kind, self.nick.clone(), text);
        line.own = true;
        if let Some(buffer) = self.buffers.get_mut(index) {
            buffer.push(line);
        }
    }

    pub fn apply(&mut self, message: &Message, now: i64) -> Applied {
        let mut applied = Applied::default();
        let time = message_time(message, now);
        let command = message.command().to_ascii_uppercase();
        let source = message.source().unwrap_or("");
        let parts = split_source(source);
        let from = parts.nick;
        let params: Vec<&str> = message.params().collect();
        let param = |index: usize| params.get(index).copied().unwrap_or("");
        match command.as_str() {
            "001" => {
                if !param(0).is_empty() {
                    self.nick = param(0).to_string();
                }
                self.server_line(message, now, &mut applied);
            }
            "005" => {
                let casemap = self.isupport.casemap;
                let tokens = params
                    .get(1..params.len().saturating_sub(1))
                    .unwrap_or_default();
                self.isupport.apply(tokens.iter().copied());
                if self.isupport.casemap != casemap {
                    self.rekey();
                }
                self.server_line(message, now, &mut applied);
            }
            "JOIN" => {
                let userhost = match (parts.user, parts.host) {
                    (Some(user), Some(host)) => format!(" ({user}@{host})"),
                    _ => String::new(),
                };
                self.on_join(from, &userhost, param(0), time, &mut applied);
            }
            "PART" => {
                let reason = params
                    .get(1)
                    .map(|r| format!(" ({})", strip(r)))
                    .unwrap_or_default();
                self.on_leave(from, param(0), time, &mut applied, |who| {
                    format!("{who} left{reason}")
                });
            }
            "KICK" => {
                let (victim, reason) = (param(1), strip(param(2)).into_owned());
                let kicker = from.to_string();
                self.on_leave(victim, param(0), time, &mut applied, |who| {
                    format!("{kicker} kicked {who} ({reason})")
                });
            }
            "QUIT" => self.on_quit(from, &strip(param(0)), time, &mut applied),
            "NICK" => self.on_nick(from, param(0), time, &mut applied),
            "MODE" => self.on_mode(from, &params, time, &mut applied),
            "TOPIC" => {
                if let Some(index) = self.find(param(0)) {
                    let topic = param(1).to_string();
                    let text = if topic.is_empty() {
                        format!("{from} cleared the topic")
                    } else {
                        format!("{from} changed the topic to: {}", strip(&topic))
                    };
                    let buffer = &mut self.buffers[index];
                    buffer.topic = (!topic.is_empty()).then_some(topic);
                    buffer.topic_by = Some(from.to_string());
                    applied.detail(index);
                    self.event(index, time, "•", text, &mut applied);
                }
            }
            "PRIVMSG" | "NOTICE" => self.on_message(
                command == "NOTICE",
                from,
                source,
                &params,
                time,
                &mut applied,
            ),
            "INVITE" => {
                let text = format!("{from} invites you to {}", param(1));
                self.event(0, time, "•", text, &mut applied);
            }
            // RPL_TOPIC and RPL_TOPICWHOTIME.
            "332" => {
                if let Some(index) = self.find(param(1)) {
                    self.buffers[index].topic = Some(param(2).to_string());
                    applied.detail(index);
                    let text = format!("Topic: {}", strip(param(2)));
                    self.event(index, time, "•", text, &mut applied);
                }
            }
            "333" => {
                if let Some(index) = self.find(param(1)) {
                    let setter = split_source(param(2)).nick.to_string();
                    let when = param(3)
                        .parse::<i64>()
                        .map(|seconds| {
                            let local = time::local(seconds * 1000);
                            format!(
                                " on {:04}-{:02}-{:02} {:02}:{:02}",
                                local.year, local.month, local.day, local.hour, local.minute
                            )
                        })
                        .unwrap_or_default();
                    self.buffers[index].topic_by = Some(setter.clone());
                    applied.detail(index);
                    self.event(
                        index,
                        time,
                        "•",
                        format!("Set by {setter}{when}"),
                        &mut applied,
                    );
                }
            }
            // RPL_CHANNELMODEIS.
            "324" => {
                if let Some(index) = self.find(param(1)) {
                    let changes = self
                        .isupport
                        .channel_modes(param(2), params.get(3..).unwrap_or_default());
                    self.buffers[index].modes = changes
                        .into_iter()
                        .filter(|change| {
                            change.adding && self.isupport.prefix_for_mode(change.mode).is_none()
                        })
                        .map(|change| (change.mode, change.param))
                        .collect();
                    applied.detail(index);
                }
            }
            "329" => {}
            // RPL_NAMREPLY, then RPL_ENDOFNAMES replaces the member list.
            "353" => {
                let key = self.fold(param(2));
                let entries = self.names.entry(key).or_default();
                for name in param(3).split(' ').filter(|n| !n.is_empty()) {
                    let (prefixes, rest) = self.isupport.split_prefixes(name);
                    // With userhost-in-names, names come as nick!user@host.
                    let nick = split_source(rest).nick;
                    entries.push(Member::new(nick, prefixes));
                }
            }
            "366" => {
                let key = self.fold(param(1));
                let members = self.names.remove(&key).unwrap_or_default();
                if let Some(index) = self.find(param(1)) {
                    let buffer = &mut self.buffers[index];
                    buffer.members.clear();
                    for member in members {
                        buffer.members.insert(member, &self.isupport);
                    }
                    applied.detail(index);
                }
            }
            // WHOIS replies go to the buffer that asked.
            "301" | "311" | "312" | "313" | "317" | "318" | "319" | "320" | "330" | "338"
            | "378" | "379" | "671" | "276" => {
                let index = self
                    .whois_to
                    .as_deref()
                    .and_then(|name| self.find(name))
                    .unwrap_or(0);
                if command == "318" {
                    self.whois_to = None;
                }
                let text = whois_text(&command, &params);
                self.event(index, time, "•", text, &mut applied);
            }
            // Errors about a channel or nick show where they apply.
            "401" | "403" | "404" | "405" | "406" | "442" | "467" | "471" | "473" | "474"
            | "475" | "477" | "478" | "482" => {
                let index = self.find(param(1)).unwrap_or(0);
                if let Some(mut line) = describe(message, now) {
                    line.kind = LineKind::Error;
                    line.prefix = String::from("!");
                    self.add(index, line, &mut applied);
                }
            }
            _ => self.server_line(message, now, &mut applied),
        }
        applied
    }

    fn server_line(&mut self, message: &Message, now: i64, applied: &mut Applied) {
        if let Some(line) = describe(message, now) {
            self.add(0, line, applied);
        }
    }

    /// The casemapping changed: buffers and members are found anew.
    fn rekey(&mut self) {
        let casemap = self.isupport.casemap;
        for buffer in &mut self.buffers {
            if buffer.kind != BufferKind::Server {
                buffer.key = casemap.fold(&buffer.name);
            }
            buffer.members.rekey(&self.isupport);
        }
    }

    fn on_join(
        &mut self,
        from: &str,
        userhost: &str,
        channel: &str,
        time: i64,
        applied: &mut Applied,
    ) {
        if self.is_me(from) {
            let (index, _) = self.open(BufferKind::Channel, channel);
            let buffer = &mut self.buffers[index];
            buffer.joined = true;
            buffer.rejoin = true;
            buffer.members.clear();
            applied.list = true;
            applied.detail(index);
            self.event(index, time, "→", format!("You joined {channel}"), applied);
        } else if let Some(index) = self.find(channel) {
            self.buffers[index]
                .members
                .insert(Member::new(from, ""), &self.isupport);
            applied.detail(index);
            self.event(
                index,
                time,
                "→",
                format!("{from}{userhost} joined"),
                applied,
            );
        }
    }

    /// PART and KICK: `who` leaves `channel`.
    fn on_leave(
        &mut self,
        who: &str,
        channel: &str,
        time: i64,
        applied: &mut Applied,
        text: impl Fn(&str) -> String,
    ) {
        let Some(index) = self.find(channel) else {
            return;
        };
        if self.is_me(who) {
            let buffer = &mut self.buffers[index];
            buffer.joined = false;
            buffer.rejoin = false;
            buffer.members.clear();
            applied.list = true;
            self.event(index, time, "←", text("You"), applied);
        } else {
            self.buffers[index].members.remove(who, &self.isupport);
            self.event(index, time, "←", text(who), applied);
        }
        applied.detail(index);
    }

    fn on_quit(&mut self, from: &str, reason: &str, time: i64, applied: &mut Applied) {
        let key = self.fold(from);
        for index in 1..self.buffers.len() {
            let buffer = &mut self.buffers[index];
            let present = match buffer.kind {
                BufferKind::Channel => buffer.members.remove(from, &self.isupport).is_some(),
                BufferKind::Query => buffer.key == key,
                BufferKind::Server => false,
            };
            if present {
                applied.detail(index);
                self.event(index, time, "←", format!("{from} quit ({reason})"), applied);
            }
        }
    }

    fn on_nick(&mut self, from: &str, new: &str, time: i64, applied: &mut Applied) {
        if new.is_empty() {
            return;
        }
        let mine = self.is_me(from);
        if mine {
            self.nick = new.to_string();
            self.event(0, time, "•", format!("You are now known as {new}"), applied);
        }
        let old_key = self.isupport.casemap.fold(from);
        let text = if mine {
            format!("You are now known as {new}")
        } else {
            format!("{from} is now known as {new}")
        };
        for index in 1..self.buffers.len() {
            let buffer = &mut self.buffers[index];
            let present = match buffer.kind {
                BufferKind::Channel => buffer.members.rename(from, new, &self.isupport),
                BufferKind::Query if buffer.key == old_key => {
                    buffer.name = new.to_string();
                    buffer.key = self.isupport.casemap.fold(new);
                    applied.list = true;
                    true
                }
                _ => false,
            };
            if present {
                applied.detail(index);
                self.event(index, time, "•", text.clone(), applied);
            }
        }
    }

    fn on_mode(&mut self, from: &str, params: &[&str], time: i64, applied: &mut Applied) {
        let Some((&target, rest)) = params.split_first() else {
            return;
        };
        let summary = rest.join(" ");
        if !self.isupport.is_channel(target) {
            self.event(
                0,
                time,
                "•",
                format!("{from} sets mode {summary} on {target}"),
                applied,
            );
            return;
        }
        let Some(index) = self.find(target) else {
            return;
        };
        let modestring = rest.first().copied().unwrap_or("");
        let changes = self
            .isupport
            .channel_modes(modestring, rest.get(1..).unwrap_or_default());
        for change in changes {
            let buffer = &mut self.buffers[index];
            if let Some(prefix) = self.isupport.prefix_for_mode(change.mode) {
                if let Some(nick) = &change.param {
                    buffer
                        .members
                        .set_prefix(nick, prefix, change.adding, &self.isupport);
                }
            } else if !self.isupport.chanmodes[0].contains(change.mode) {
                buffer.modes.retain(|(mode, _)| *mode != change.mode);
                if change.adding {
                    buffer.modes.push((change.mode, change.param));
                }
            }
        }
        applied.detail(index);
        self.event(
            index,
            time,
            "•",
            format!("{from} sets mode {summary}"),
            applied,
        );
    }

    fn on_message(
        &mut self,
        notice: bool,
        from: &str,
        source: &str,
        params: &[&str],
        time: i64,
        applied: &mut Applied,
    ) {
        let (Some(&target), Some(&text)) = (params.first(), params.get(1)) else {
            return;
        };
        let target = self.isupport.strip_statusmsg(target);
        let mut kind = if notice {
            LineKind::Notice
        } else {
            LineKind::Message
        };
        let mut text = text;
        if let Some(ctcp) = text.strip_prefix('\u{1}') {
            // The closing \x01 may be missing (Modern IRC, CTCP).
            let body = ctcp.strip_suffix('\u{1}').unwrap_or(ctcp);
            let (verb, args) = body.split_once(' ').unwrap_or((body, ""));
            if notice || !verb.eq_ignore_ascii_case("ACTION") {
                self.on_ctcp(
                    notice,
                    from,
                    &verb.to_ascii_uppercase(),
                    args,
                    time,
                    applied,
                );
                return;
            }
            kind = LineKind::Action;
            text = args;
        }
        let own = self.is_me(from);
        let channel = self.isupport.is_channel(target);
        // A server's notices, and notices before registration, have no nick!user@host source.
        let from_server = from.is_empty() || (!source.contains('!') && from.contains('.'));
        let (index, opened) = if channel {
            self.open(BufferKind::Channel, target)
        } else if from_server {
            (0, false)
        } else {
            let other = if own { target } else { from };
            match self.find(other) {
                Some(index) => (index, false),
                // Private notices (services, for one) go to the server buffer.
                None if notice => (0, false),
                None => self.open(BufferKind::Query, other),
            }
        };
        applied.list |= opened;
        if channel {
            self.buffers[index]
                .members
                .touch(from, time, &self.isupport);
        }
        let mut line = Line::message(time, kind, from, text);
        line.own = own;
        line.highlight = !own && channel && self.mentions(text);
        self.add(index, line, applied);
    }

    fn on_ctcp(
        &mut self,
        reply: bool,
        from: &str,
        verb: &str,
        args: &str,
        time: i64,
        applied: &mut Applied,
    ) {
        if reply {
            let text = format!("CTCP {verb} reply from {from}: {}", strip(args));
            self.event(0, time, "•", text, applied);
            return;
        }
        let answer = match verb {
            "VERSION" => Some(format!("VERSION {}", self.version)),
            "PING" => Some(format!("PING {args}")),
            "TIME" => {
                let local = time::local(time);
                Some(format!(
                    "TIME {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    local.year, local.month, local.day, local.hour, local.minute, local.second
                ))
            }
            "CLIENTINFO" => Some(format!("CLIENTINFO {CLIENTINFO}")),
            _ => None,
        };
        let mut text = match verb {
            "DCC" => format!("{from} offered a DCC transfer, which RubyChat doesn't support"),
            _ => format!("CTCP {verb} from {from}"),
        };
        if let Some(answer) = answer.filter(|_| self.ctcp_replies) {
            if self.ctcp_allowed(time) {
                if let Ok(line) =
                    serialize(&[], None, "NOTICE", &[from, &format!("\u{1}{answer}\u{1}")])
                {
                    applied.replies.push(line);
                }
            } else {
                text.push_str(" (not answered: too many requests)");
            }
        }
        self.event(0, time, "•", text, applied);
    }

    fn ctcp_allowed(&mut self, now: i64) -> bool {
        let elapsed = (now - self.ctcp_time).max(0) as f64;
        self.ctcp_tokens = (self.ctcp_tokens + elapsed / CTCP_REFILL_MS).min(CTCP_BURST);
        self.ctcp_time = now;
        if self.ctcp_tokens >= 1.0 {
            self.ctcp_tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// The common WHOIS replies as sentences; others as their parameters.
fn whois_text(command: &str, params: &[&str]) -> String {
    let param = |index: usize| params.get(index).copied().unwrap_or("");
    let nick = param(1);
    match command {
        "311" => format!("{nick} is {}@{} ({})", param(2), param(3), strip(param(5))),
        "312" => format!("{nick} is on {} ({})", param(2), param(3)),
        "313" => format!("{nick} is an IRC operator"),
        "317" => format!("{nick} has been idle {}", duration(param(2))),
        "318" => String::from("End of WHOIS"),
        "319" => format!("{nick} is on {}", param(2)),
        "330" => format!("{nick} is logged in as {}", param(2)),
        "301" => format!("{nick} is away: {}", strip(param(2))),
        "671" => format!("{nick} is using a secure connection"),
        _ => params.get(1..).unwrap_or_default().join(" "),
    }
}

fn duration(seconds: &str) -> String {
    let Ok(seconds) = seconds.parse::<u64>() else {
        return seconds.to_string();
    };
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3600 => format!("{} min", seconds / 60),
        3600..86_400 => format!("{} h {} min", seconds / 3600, seconds % 3600 / 60),
        _ => format!("{} days", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network() -> NetworkState {
        let mut network = NetworkState::new("ruby", "Libera.Chat");
        for line in [
            ":srv 001 ruby :Welcome",
            ":srv 005 ruby CHANTYPES=# PREFIX=(ov)@+ STATUSMSG=@+ CASEMAPPING=rfc1459 CHANMODES=eIbq,k,flj,imnpst :are supported",
        ] {
            network.apply(&Message::parse(line).unwrap(), 0);
        }
        network
    }

    fn apply(network: &mut NetworkState, line: &str) -> Applied {
        network.apply(&Message::parse(line).unwrap(), 1000)
    }

    fn texts(network: &NetworkState, index: usize) -> Vec<String> {
        network.buffers[index]
            .lines()
            .iter()
            .map(|line| line.text.clone())
            .collect()
    }

    #[test]
    fn joining_fills_members_from_names() {
        let mut network = network();
        let applied = apply(&mut network, ":Ruby!u@h JOIN #Rust");
        assert!(applied.list);
        let index = network.find("#rust").unwrap();
        assert!(network.buffers[index].joined);
        apply(&mut network, ":srv 353 ruby = #rust :@alice +bob ruby");
        apply(&mut network, ":srv 353 ruby = #rust :carol!c@host");
        assert!(
            network.buffers[index].members.is_empty(),
            "filled at the end of NAMES"
        );
        apply(&mut network, ":srv 366 ruby #rust :End of /NAMES list.");
        let members = &mut network.buffers[index].members;
        let order: Vec<String> = members
            .sorted(&network.isupport)
            .map(|m| format!("{}{}", m.prefixes, m.nick))
            .collect();
        assert_eq!(order, ["@alice", "+bob", "carol", "ruby"]);
        apply(&mut network, ":dave!d@h JOIN #rust");
        apply(&mut network, ":alice!a@h MODE #rust +v-o dave alice");
        let isupport = network.isupport.clone();
        let members = &mut network.buffers[index].members;
        assert_eq!(members.get("dave", &isupport).unwrap().prefixes, "+");
        assert_eq!(members.get("alice", &isupport).unwrap().prefixes, "");
        apply(&mut network, ":bob!b@h PART #rust :bye");
        apply(&mut network, ":carol!c@h QUIT :Ping timeout");
        apply(&mut network, ":dave!d@h NICK Dave2");
        let members = &mut network.buffers[index].members;
        let order: Vec<String> = members.sorted(&isupport).map(|m| m.nick.clone()).collect();
        assert_eq!(order, ["Dave2", "alice", "ruby"]);
        assert_eq!(
            texts(&network, index)[1..],
            [
                "dave (d@h) joined",
                "alice sets mode +v-o dave alice",
                "bob left (bye)",
                "carol quit (Ping timeout)",
                "dave is now known as Dave2",
            ]
        );
    }

    #[test]
    fn messages_route_to_channels_queries_and_the_server_buffer() {
        let mut network = network();
        apply(&mut network, ":ruby!u@h JOIN #rust");
        let channel = network.find("#rust").unwrap();
        let applied = apply(
            &mut network,
            ":alice!a@h PRIVMSG #rust :hello Ruby, how are you?",
        );
        assert_eq!(applied.alerts, [channel], "a highlight notifies");
        let applied = apply(&mut network, ":alice!a@h PRIVMSG @#rust :ops only");
        assert!(applied.alerts.is_empty());
        apply(&mut network, ":alice!a@h PRIVMSG #rust :rubygems is great");
        let lines = network.buffers[channel].lines();
        assert_eq!(lines.len(), 4);
        assert!(lines[1].highlight, "mentions ruby");
        assert_eq!(lines[2].text, "ops only");
        assert!(!lines[3].highlight, "rubygems is not ruby");
        let applied = apply(&mut network, ":bob!b@h PRIVMSG ruby :psst");
        assert!(applied.list, "a query opened");
        let query = network.find("BOB").unwrap();
        assert_eq!(applied.alerts, [query], "a private message notifies");
        assert_eq!(network.buffers[query].kind, BufferKind::Query);
        assert_eq!(network.buffers[query].highlights, 1);
        apply(
            &mut network,
            ":NickServ!s@services NOTICE ruby :This nickname is registered",
        );
        apply(
            &mut network,
            ":irc.example.net NOTICE * :*** Looking up your hostname",
        );
        assert!(
            network.find("nickserv").is_none(),
            "private notices don't open queries"
        );
        let server = texts(&network, 0);
        assert!(server.iter().any(|t| t == "This nickname is registered"));
        apply(
            &mut network,
            ":alice!a@h PRIVMSG #rust :\u{1}ACTION waves\u{1}",
        );
        let last = network.buffers[channel].lines().back().unwrap().clone();
        assert_eq!(
            (last.kind, last.text.as_str(), last.nick.as_str()),
            (LineKind::Action, "waves", "alice")
        );
        // Messages you sent from elsewhere (a bouncer) show as yours.
        apply(&mut network, ":ruby!u@h PRIVMSG bob :from my phone");
        assert!(network.buffers[query].lines().back().unwrap().own);
    }

    #[test]
    fn ctcp_queries_are_answered_at_a_bounded_rate() {
        let mut network = network();
        let mut answered = 0;
        for _ in 0..5 {
            let applied = network.apply(
                &Message::parse(":eve!e@h PRIVMSG ruby :\u{1}VERSION\u{1}").unwrap(),
                5000,
            );
            answered += applied.replies.len();
        }
        assert_eq!(answered, 3, "a burst of three");
        let applied = network.apply(
            &Message::parse(":eve!e@h PRIVMSG ruby :\u{1}PING 12345").unwrap(),
            9000,
        );
        assert_eq!(
            applied.replies,
            ["NOTICE eve :\u{1}PING 12345\u{1}"],
            "a token refilled; the closing \\x01 is optional"
        );
        assert!(
            network.find("eve").is_none(),
            "CTCP queries don't open queries"
        );
        let applied = network.apply(
            &Message::parse(":eve!e@h NOTICE ruby :\u{1}VERSION Fancy 1.0\u{1}").unwrap(),
            9000,
        );
        assert!(applied.replies.is_empty(), "replies are never answered");
    }

    #[test]
    fn rejoins_what_you_had_joined() {
        let mut network = network();
        for channel in ["#a", "#b", "#c"] {
            apply(&mut network, &format!(":ruby!u@h JOIN {channel}"));
        }
        apply(&mut network, ":srv 324 ruby #c +k sesame");
        apply(&mut network, ":op!o@h KICK #b ruby :out");
        network.disconnected();
        assert_eq!(
            network.rejoin_lines(),
            ["JOIN #c,#a sesame"],
            "keyed channels first; not where you were kicked"
        );
        for n in 0..60 {
            let (index, _) = network.open(BufferKind::Channel, &format!("#channel-number-{n:02}"));
            network.buffers[index].rejoin = true;
        }
        let lines = network.rejoin_lines();
        assert!(lines.len() > 1 && lines.iter().all(|line| line.len() <= JOIN_LINE_BYTES));
        assert_eq!(
            lines
                .iter()
                .map(|line| line.matches(',').count() + 1)
                .sum::<usize>(),
            62
        );
    }

    #[test]
    fn topics_kicks_and_your_own_nick() {
        let mut network = network();
        apply(&mut network, ":ruby!u@h JOIN #rust");
        let index = network.find("#rust").unwrap();
        apply(&mut network, ":srv 332 ruby #rust :Welcome to #rust");
        apply(&mut network, ":srv 324 ruby #rust +ntk secret");
        assert_eq!(
            network.buffers[index].topic.as_deref(),
            Some("Welcome to #rust")
        );
        assert_eq!(network.buffers[index].mode_string(), "+knt");
        apply(&mut network, ":alice!a@h TOPIC #rust :New topic");
        assert_eq!(network.buffers[index].topic_by.as_deref(), Some("alice"));
        apply(&mut network, ":ruby!u@h NICK :Ruby_");
        assert_eq!(network.nick, "Ruby_");
        assert!(network.is_me("ruby_"));
        let applied = apply(&mut network, ":alice!a@h KICK #rust Ruby_ :behave");
        assert!(applied.list && !network.buffers[index].joined);
        assert_eq!(
            network.buffers[index].lines().back().unwrap().text,
            "alice kicked You (behave)"
        );
        network.whois_to = Some(String::from("#rust"));
        apply(&mut network, ":srv 311 Ruby_ bob b host * :Bob Builder");
        apply(&mut network, ":srv 318 Ruby_ bob :End of /WHOIS list.");
        let lines = texts(&network, index);
        assert_eq!(
            lines[lines.len() - 2..],
            ["bob is b@host (Bob Builder)", "End of WHOIS"]
        );
        assert!(network.whois_to.is_none());
    }
}
