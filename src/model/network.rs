//! A network as the interface sees it: your nick, what the server
//! advertised, and the buffers. Each message is routed to the buffer it
//! belongs in and updates membership, modes and topics (Modern IRC's client
//! messages and numerics, with IRCv3 multi-prefix and userhost-in-names),
//! and CTCP queries are answered at a bounded rate (DESIGN.md §6.8).

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};

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

/// Whether someone who last spoke at `spoke` (0: never) is quiet at
/// `time`.
fn quiet(spoke: i64, time: i64) -> bool {
    spoke == 0 || time.saturating_sub(spoke) >= QUIET_MS
}

/// Whether `text` matches `pattern`, where `*` stands for any run of
/// characters and `?` for one.
fn glob(pattern: &str, text: &str) -> bool {
    let (pattern, text): (Vec<char>, Vec<char>) =
        (pattern.chars().collect(), text.chars().collect());
    let (mut p, mut t) = (0, 0);
    // The last `*` seen, and where in `text` it was tried up to.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((at, tried)) => {
                    p = at + 1;
                    t = tried + 1;
                    star = Some((at, tried + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// JOIN lines stay this short, however many channels they carry.
const JOIN_LINE_BYTES: usize = 400;
/// Batches open at once, and messages one holds, at most; past these,
/// messages are applied as they come (IRCv3 batch lets clients process
/// them one by one).
const MAX_BATCHES: usize = 32;
const MAX_BATCH_MESSAGES: usize = 20_000;
/// Lines awaiting their echo, and labels awaiting their response, at most.
const MAX_PENDING: usize = 256;
const MAX_LABELS: usize = 256;
/// Someone who hasn't spoken for this long is quiet: the smart filter
/// hides their joins, parts, quits and nick changes (DESIGN.md §8.6).
pub const QUIET_MS: i64 = 15 * 60 * 1000;
/// Channels larger than this aren't asked WHO after joining.
const WHO_LIMIT: usize = 1000;
/// The WHOX token on RubyChat's own WHO queries (IRCv3 whox: up to three
/// digits).
const WHO_TOKEN: &str = "152";
/// Names a collapsed netsplit or netjoin line lists before counting.
const COLLAPSED_NAMES: usize = 8;
/// MONITOR lines stay this short.
const MONITOR_LINE_BYTES: usize = 400;

/// What came with a PRIVMSG or NOTICE besides its parameters.
struct Incoming<'a> {
    time: i64,
    /// Its labeled-response label.
    label: Option<&'a str>,
    /// The sender's account (IRCv3 account-tag).
    account: Option<String>,
}

/// A batch being received (IRCv3 batch).
struct Batch {
    kind: String,
    params: Vec<String>,
    /// The batch it is nested in.
    parent: Option<String>,
    /// The label its start carried (IRCv3 labeled-response).
    label: Option<String>,
    items: Vec<Item>,
}

enum Item {
    Message(Message),
    Batch(Batch),
}

/// A line sent while `echo-message` is on, until its echo confirms it.
struct Pending {
    label: Option<String>,
    /// The target's casefolded name.
    target: String,
    kind: LineKind,
    text: String,
    /// The key of the buffer showing it, and its line there.
    buffer: String,
    line: u64,
}

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
    /// The capabilities enabled on the connection.
    pub caps: Vec<String>,
    batches: HashMap<String, Batch>,
    pending: VecDeque<Pending>,
    /// Labels awaiting their response, with the key of the buffer that
    /// asked (empty for the server buffer).
    labels: VecDeque<(String, String)>,
    next_label: u64,
    /// Where replies go while a labeled response is applied.
    route: Option<usize>,
    /// Channels to ask WHO about after joining, and the one being asked.
    who_queue: VecDeque<String>,
    who_running: Option<String>,
    /// Nicks under MONITOR, casefolded.
    monitored: Vec<String>,
    /// Masks whose messages are dropped, casefolded (`nick!user@host`
    /// with `*` and `?`).
    pub ignores: Vec<String>,
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
            caps: Vec::new(),
            batches: HashMap::new(),
            pending: VecDeque::new(),
            labels: VecDeque::new(),
            next_label: 0,
            route: None,
            who_queue: VecDeque::new(),
            who_running: None,
            monitored: Vec::new(),
            ignores: Vec::new(),
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

    /// The connection ended: channels are no longer joined, lines still
    /// waiting for their echo weren't delivered, and what the server kept
    /// for this connection is gone.
    pub fn disconnected(&mut self) {
        for buffer in &mut self.buffers {
            match buffer.kind {
                BufferKind::Channel => {
                    buffer.joined = false;
                    buffer.members.clear();
                }
                BufferKind::Query => buffer.peer.online = None,
                BufferKind::Server => {}
            }
        }
        for pending in std::mem::take(&mut self.pending) {
            if let Some(index) = self.buffer_by_key(&pending.buffer)
                && let Some(line) = self.buffers[index].line_mut(pending.line)
            {
                line.pending = false;
                line.failed = true;
            }
        }
        self.names.clear();
        self.caps.clear();
        self.batches.clear();
        self.labels.clear();
        self.route = None;
        self.who_queue.clear();
        self.who_running = None;
        self.monitored.clear();
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
    /// in `index`, as yours. With `echo-message` it waits, dimmed, for the
    /// server's echo, found by `label` or else by target and text.
    pub fn echo(
        &mut self,
        index: usize,
        kind: LineKind,
        text: &str,
        now: i64,
        label: Option<String>,
    ) {
        let waits = self.has("echo-message");
        let mut line = Line::message(now, kind, self.nick.clone(), text);
        line.own = true;
        line.pending = waits;
        let Some(buffer) = self.buffers.get_mut(index) else {
            return;
        };
        buffer.push(line);
        if !waits {
            return;
        }
        let pending = Pending {
            label,
            target: buffer.key.clone(),
            kind,
            text: text.to_string(),
            buffer: buffer.key.clone(),
            line: buffer.lines().back().map_or(0, |line| line.id),
        };
        if self.pending.len() == MAX_PENDING
            && let Some(oldest) = self.pending.pop_front()
            && let Some(index) = self.buffer_by_key(&oldest.buffer)
            && let Some(line) = self.buffers[index].line_mut(oldest.line)
        {
            // Too many unconfirmed: the oldest stops waiting.
            line.pending = false;
        }
        self.pending.push_back(pending);
    }

    pub fn has(&self, cap: &str) -> bool {
        self.caps.iter().any(|enabled| enabled == cap)
    }

    /// Applies a message from the server: what changed, and anything to
    /// send in reply.
    pub fn apply(&mut self, message: &Message, now: i64) -> Applied {
        let mut applied = Applied::default();
        self.receive(message, now, &mut applied);
        applied
    }

    /// A message in an open batch waits for the batch's end; others apply
    /// now.
    fn receive(&mut self, message: &Message, now: i64, applied: &mut Applied) {
        if message.command().eq_ignore_ascii_case("BATCH") {
            return self.on_batch(message, now, applied);
        }
        if let Some(reference) = message.tag("batch")
            && let Some(batch) = self.batches.get_mut(reference.as_ref())
            && batch.items.len() < MAX_BATCH_MESSAGES
        {
            batch.items.push(Item::Message(message.clone()));
            return;
        }
        self.labeled(message, now, applied);
    }

    /// Applies a message, its replies routed to the buffer that asked when
    /// it carries a label (IRCv3 labeled-response): one labeled message is
    /// the whole response.
    fn labeled(&mut self, message: &Message, now: i64, applied: &mut Applied) {
        let label = message.tag("label").map(Cow::into_owned);
        let route = label.as_deref().and_then(|label| self.take_label(label));
        let saved = self.route;
        if route.is_some() {
            self.route = route;
        }
        self.apply_one(message, now, label.as_deref(), applied);
        self.route = saved;
    }

    /// `BATCH +reference type params…` opens a batch; `BATCH -reference`
    /// ends it, applying what it held, or hands it to the batch it is
    /// nested in.
    fn on_batch(&mut self, message: &Message, now: i64, applied: &mut Applied) {
        let Some(reference) = message.param(0) else {
            return;
        };
        if let Some(reference) = reference.strip_prefix('+') {
            if reference.is_empty() || self.batches.len() >= MAX_BATCHES {
                return;
            }
            let mut params = message.params().skip(1).map(str::to_string);
            let kind = params.next().unwrap_or_default();
            self.batches.insert(
                reference.to_string(),
                Batch {
                    kind,
                    params: params.collect(),
                    parent: message.tag("batch").map(Cow::into_owned),
                    label: message.tag("label").map(Cow::into_owned),
                    items: Vec::new(),
                },
            );
        } else if let Some(reference) = reference.strip_prefix('-')
            && let Some(batch) = self.batches.remove(reference)
        {
            if let Some(parent) = batch.parent.as_deref()
                && let Some(outer) = self.batches.get_mut(parent)
            {
                outer.items.push(Item::Batch(batch));
                return;
            }
            self.finish(batch, now, applied);
        }
    }

    /// Applies a batch that ended: netsplits and netjoins collapse into a
    /// line per buffer (IRCv3 netsplit batches); other types apply their
    /// messages in order.
    fn finish(&mut self, batch: Batch, now: i64, applied: &mut Applied) {
        let route = batch
            .label
            .as_deref()
            .and_then(|label| self.take_label(label));
        let saved = self.route;
        if route.is_some() {
            self.route = route;
        }
        if matches!(batch.kind.as_str(), "netsplit" | "netjoin") {
            self.collapse(batch, now, applied);
        } else {
            for item in batch.items {
                match item {
                    Item::Message(message) => self.labeled(&message, now, applied),
                    Item::Batch(inner) => self.finish(inner, now, applied),
                }
            }
        }
        self.route = saved;
    }

    fn collapse(&mut self, batch: Batch, now: i64, applied: &mut Applied) {
        let split = batch.kind == "netsplit";
        let mut affected: Vec<(usize, Vec<String>)> = Vec::new();
        let mut time = now;
        for item in batch.items {
            let Item::Message(message) = item else {
                continue;
            };
            time = message_time(&message, now);
            let Some(nick) = message.nick().map(str::to_string) else {
                continue;
            };
            let buffers = match (split, message.command()) {
                (true, command) if command.eq_ignore_ascii_case("QUIT") => self
                    .remove_everywhere(&nick, applied)
                    .into_iter()
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>(),
                (false, command) if command.eq_ignore_ascii_case("JOIN") => {
                    let channel = message.param(0).unwrap_or("");
                    self.add_member(&nick, channel, None, applied)
                        .into_iter()
                        .collect::<Vec<_>>()
                }
                // Anything else in the batch applies as usual.
                _ => {
                    self.labeled(&message, now, applied);
                    continue;
                }
            };
            for index in buffers {
                match affected.iter_mut().find(|(at, _)| *at == index) {
                    Some((_, nicks)) => nicks.push(nick.clone()),
                    None => affected.push((index, vec![nick.clone()])),
                }
            }
        }
        let servers = batch.params.join(" ↔ ");
        for (index, nicks) in affected {
            let mut names = nicks
                .iter()
                .take(COLLAPSED_NAMES)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if nicks.len() > COLLAPSED_NAMES {
                names.push_str(&format!(" and {} more", nicks.len() - COLLAPSED_NAMES));
            }
            let (prefix, verb) = if split {
                ("←", "Netsplit")
            } else {
                ("→", "Netjoin")
            };
            let text = format!("{verb} {servers}: {names}");
            self.event(index, time, prefix, text, applied);
        }
    }

    /// A new label for a command sent from buffer `index`, while
    /// labeled-response is on (it needs `batch`); the response comes back
    /// to that buffer.
    pub fn label(&mut self, index: usize) -> Option<String> {
        if !(self.has("labeled-response") && self.has("batch")) {
            return None;
        }
        self.next_label += 1;
        let label = format!("rc{}", self.next_label);
        let key = self
            .buffers
            .get(index)
            .map(|buffer| buffer.key.clone())
            .unwrap_or_default();
        if self.labels.len() == MAX_LABELS {
            self.labels.pop_front();
        }
        self.labels.push_back((label.clone(), key));
        Some(label)
    }

    /// The buffer awaiting `label`'s response, which is now complete.
    fn take_label(&mut self, label: &str) -> Option<usize> {
        let position = self.labels.iter().position(|(sent, _)| sent == label)?;
        let (_, key) = self.labels.remove(position)?;
        if key.is_empty() {
            return Some(0);
        }
        self.buffers
            .iter()
            .position(|buffer| buffer.kind != BufferKind::Server && buffer.key == key)
            .or(Some(0))
    }

    /// Where a reply goes: the buffer that asked, when known, else the
    /// server buffer.
    fn reply_buffer(&self) -> usize {
        self.route.unwrap_or(0)
    }

    /// The server confirmed a line you sent: it stops waiting, showing the
    /// text as the server passed it on.
    fn confirm(&mut self, pending: Pending, text: &str, applied: &mut Applied) {
        if let Some(index) = self.buffer_by_key(&pending.buffer)
            && let Some(line) = self.buffers[index].line_mut(pending.line)
        {
            line.pending = false;
            if line.text != text {
                line.text = text.to_string();
            }
            applied.touch(index);
        }
    }

    /// The server refused a line you sent: the one with `label`, else the
    /// oldest to `target`.
    fn refuse(&mut self, label: Option<&str>, target: Option<&str>, applied: &mut Applied) {
        let target = target.map(|target| self.fold(target));
        let position = self
            .pending
            .iter()
            .position(|pending| match (label, &target) {
                (Some(label), _) => pending.label.as_deref() == Some(label),
                (None, Some(target)) => pending.target == *target,
                (None, None) => false,
            });
        if let Some(pending) = position.and_then(|at| self.pending.remove(at)) {
            self.fail(pending, applied);
        }
    }

    fn fail(&mut self, pending: Pending, applied: &mut Applied) {
        if let Some(index) = self.buffer_by_key(&pending.buffer)
            && let Some(line) = self.buffers[index].line_mut(pending.line)
        {
            line.pending = false;
            line.failed = true;
            applied.touch(index);
        }
    }

    fn buffer_by_key(&self, key: &str) -> Option<usize> {
        self.buffers
            .iter()
            .position(|buffer| buffer.kind != BufferKind::Server && buffer.key == key)
    }

    /// IRCv3 away-notify: `nick` went away with `message`, or came back.
    fn set_away(&mut self, nick: &str, message: Option<&str>, applied: &mut Applied) {
        let key = self.fold(nick);
        for index in 1..self.buffers.len() {
            let buffer = &mut self.buffers[index];
            match buffer.kind {
                BufferKind::Channel => {
                    if buffer
                        .members
                        .set_away(nick, message.is_some(), &self.isupport)
                    {
                        applied.detail(index);
                    }
                }
                BufferKind::Query if buffer.key == key => {
                    buffer.peer.away = message.map(str::to_string);
                    applied.detail(index);
                }
                _ => {}
            }
        }
    }

    /// IRCv3 account-notify: `nick` logged in to `account`, or out.
    fn set_account(&mut self, nick: &str, account: Option<String>, applied: &mut Applied) {
        let key = self.fold(nick);
        for index in 1..self.buffers.len() {
            let buffer = &mut self.buffers[index];
            match buffer.kind {
                BufferKind::Channel => {
                    if let Some(member) = buffer.members.get_mut(nick, &self.isupport) {
                        member.account = account.clone();
                    }
                }
                BufferKind::Query if buffer.key == key => {
                    buffer.peer.account = account.clone();
                    applied.detail(index);
                }
                _ => {}
            }
        }
    }

    /// IRCv3 setname: `nick` has a new realname.
    fn set_realname(&mut self, nick: &str, realname: &str, applied: &mut Applied) {
        if let Some(index) = self.find(nick)
            && self.buffers[index].kind == BufferKind::Query
        {
            self.buffers[index].peer.realname = Some(realname.to_string());
            applied.detail(index);
        }
    }

    /// A reply to RubyChat's own WHO about `channel`: `nick` is away or
    /// not, and with WHOX, logged in to an account or not.
    fn whoreply(
        &mut self,
        channel: &str,
        nick: &str,
        away: bool,
        account: Option<Option<String>>,
        applied: &mut Applied,
    ) {
        let Some(index) = self.find(channel) else {
            return;
        };
        if let Some(member) = self.buffers[index].members.get_mut(nick, &self.isupport) {
            member.away = away;
            if let Some(account) = account {
                member.account = account;
            }
            applied.detail(index);
        }
    }

    /// Asks WHO about the next channel waiting, one at a time; WHOX asks
    /// for flags and accounts (IRCv3 whox).
    fn next_who(&mut self, applied: &mut Applied) {
        while self.who_running.is_none() {
            let Some(key) = self.who_queue.pop_front() else {
                return;
            };
            let Some(buffer) = self.buffers.iter().find(|buffer| {
                buffer.kind == BufferKind::Channel && buffer.key == key && buffer.joined
            }) else {
                continue;
            };
            let query = if self.isupport.whox {
                format!("WHO {} %tcnfa,{WHO_TOKEN}", buffer.name)
            } else {
                format!("WHO {}", buffer.name)
            };
            applied.replies.push(query);
            self.who_running = Some(key);
        }
    }

    /// RPL_MONONLINE and RPL_MONOFFLINE: who of `targets` is on.
    fn monitored_status(&mut self, online: bool, targets: &str, applied: &mut Applied) {
        for target in targets.split(',').filter(|target| !target.is_empty()) {
            let nick = split_source(target).nick;
            if let Some(index) = self.find(nick)
                && self.buffers[index].kind == BufferKind::Query
            {
                self.buffers[index].peer.online = Some(online);
                applied.detail(index);
                applied.list = true;
            }
        }
    }

    /// MONITOR lines that make the server watch exactly the open queries'
    /// nicks, within its limit (IRCv3 monitor). Call it while registered.
    pub fn monitor_lines(&mut self) -> Vec<String> {
        let Some(limit) = self.isupport.monitor else {
            return Vec::new();
        };
        let wanted: Vec<(&str, &str)> = self
            .buffers
            .iter()
            .filter(|buffer| buffer.kind == BufferKind::Query)
            .map(|buffer| (buffer.name.as_str(), buffer.key.as_str()))
            .take(limit)
            .collect();
        let added: Vec<&str> = wanted
            .iter()
            .filter(|(_, key)| !self.monitored.iter().any(|monitored| monitored == key))
            .map(|(name, _)| *name)
            .collect();
        let removed: Vec<&str> = self
            .monitored
            .iter()
            .filter(|monitored| !wanted.iter().any(|(_, key)| key == monitored))
            .map(String::as_str)
            .collect();
        let mut lines = Vec::new();
        for (sign, nicks) in [("-", removed), ("+", added)] {
            let mut line = String::new();
            for nick in nicks {
                if !line.is_empty() && line.len() + nick.len() + 1 > MONITOR_LINE_BYTES {
                    lines.push(std::mem::take(&mut line));
                }
                if line.is_empty() {
                    line = format!("MONITOR {sign} {nick}");
                } else {
                    line.push(',');
                    line.push_str(nick);
                }
            }
            if !line.is_empty() {
                lines.push(line);
            }
        }
        self.monitored = wanted.iter().map(|(_, key)| key.to_string()).collect();
        lines
    }

    /// Whether `source` matches an ignore mask.
    pub fn ignored(&self, source: &str) -> bool {
        if self.ignores.is_empty() || !source.contains('!') {
            return false;
        }
        let subject = self.fold(source);
        self.ignores.iter().any(|mask| glob(mask, &subject))
    }

    /// The mask `/ignore` keeps for `pattern`: a bare nick ignores that nick
    /// anywhere.
    pub fn ignore_mask(&self, pattern: &str) -> String {
        let mask = if pattern.contains(['!', '@']) {
            pattern.to_string()
        } else {
            format!("{pattern}!*@*")
        };
        self.fold(&mask)
    }

    /// Applies one message, outside any batch; `label` is its
    /// labeled-response label.
    fn apply_one(
        &mut self,
        message: &Message,
        now: i64,
        label: Option<&str>,
        applied: &mut Applied,
    ) {
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
                self.server_line(message, now, applied);
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
                self.server_line(message, now, applied);
            }
            "JOIN" => {
                let userhost = match (parts.user, parts.host) {
                    (Some(user), Some(host)) => format!(" ({user}@{host})"),
                    _ => String::new(),
                };
                // IRCv3 extended-join: the account (`*` for none), then the
                // realname.
                let account = (params.len() >= 3 && self.has("extended-join"))
                    .then(|| param(1))
                    .filter(|account| *account != "*")
                    .map(str::to_string);
                self.on_join(from, &userhost, param(0), account, time, applied);
            }
            "PART" => {
                let reason = params
                    .get(1)
                    .map(|r| format!(" ({})", strip(r)))
                    .unwrap_or_default();
                self.on_leave(from, param(0), time, false, applied, |who| {
                    format!("{who} left{reason}")
                });
            }
            "KICK" => {
                let (victim, reason) = (param(1), strip(param(2)).into_owned());
                let kicker = from.to_string();
                self.on_leave(victim, param(0), time, true, applied, |who| {
                    format!("{kicker} kicked {who} ({reason})")
                });
            }
            "QUIT" => self.on_quit(from, &strip(param(0)), time, applied),
            "NICK" => self.on_nick(from, param(0), time, applied),
            "MODE" => self.on_mode(from, &params, time, applied),
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
                    self.event(index, time, "•", text, applied);
                }
            }
            "PRIVMSG" | "NOTICE" => self.on_message(
                command == "NOTICE",
                from,
                source,
                &params,
                Incoming {
                    time,
                    label,
                    account: message.tag("account").map(Cow::into_owned),
                },
                applied,
            ),
            "INVITE" if self.ignored(source) => {}
            // IRCv3 invite-notify tells of invitations to others too.
            "INVITE" if !self.is_me(param(0)) => {
                let text = format!("{from} invited {} to {}", param(0), param(1));
                let index = self.find(param(1)).unwrap_or(0);
                self.event(index, time, "•", text, applied);
            }
            "INVITE" => {
                let text = format!("{from} invites you to {}", param(1));
                self.event(0, time, "•", text, applied);
            }
            // IRCv3 away-notify, account-notify, chghost and setname: state
            // that shows in the nick list and query headers, not as lines.
            "AWAY" => self.set_away(from, params.first().copied(), applied),
            "ACCOUNT" => {
                let account = Some(param(0))
                    .filter(|account| !account.is_empty() && *account != "*")
                    .map(str::to_string);
                self.set_account(from, account, applied);
            }
            "SETNAME" => self.set_realname(from, param(0), applied),
            "CHGHOST" | "ACK" => {}
            // IRCv3 standard replies.
            "FAIL" | "WARN" | "NOTE" => {
                if command == "FAIL" {
                    self.refuse(label, None, applied);
                }
                let description = strip(params.last().copied().unwrap_or("")).into_owned();
                let text = match param(0) {
                    "*" | "" => description,
                    about => format!("{description} ({about})"),
                };
                let (kind, prefix) = match command.as_str() {
                    "FAIL" => (LineKind::Error, "!"),
                    "WARN" => (LineKind::Notice, "!"),
                    _ => (LineKind::Server, "•"),
                };
                let index = self.reply_buffer();
                self.add(index, Line::new(time, kind, prefix, text), applied);
            }
            // RubyChat's own WHO after joining is answered quietly: away
            // flags (G) and, with WHOX, accounts.
            "352" if self.who_running == Some(self.fold(param(1))) => {
                let away = param(6).starts_with('G');
                self.whoreply(param(1), param(5), away, None, applied);
            }
            "354" if param(1) == WHO_TOKEN => {
                let account = Some(param(5))
                    .filter(|account| *account != "0")
                    .map(str::to_string);
                let away = param(4).starts_with('G');
                self.whoreply(param(2), param(3), away, Some(account), applied);
            }
            "315" if self.who_running == Some(self.fold(param(1))) => {
                self.who_running = None;
                self.next_who(applied);
            }
            // MONITOR (IRCv3 monitor): who among your queries is online.
            "730" | "731" => self.monitored_status(command == "730", param(1), applied),
            "732" | "733" => {}
            // RPL_TOPIC and RPL_TOPICWHOTIME.
            "332" => {
                if let Some(index) = self.find(param(1)) {
                    self.buffers[index].topic = Some(param(2).to_string());
                    applied.detail(index);
                    let text = format!("Topic: {}", strip(param(2)));
                    self.event(index, time, "•", text, applied);
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
                    self.event(index, time, "•", format!("Set by {setter}{when}"), applied);
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
                    // Away and account state comes from WHO (IRCv3
                    // away-notify, account-notify).
                    let (joined, count, key) =
                        (buffer.joined, buffer.members.len(), buffer.key.clone());
                    if joined
                        && count <= WHO_LIMIT
                        && (self.has("away-notify") || self.has("account-notify"))
                    {
                        if !self.who_queue.contains(&key) {
                            self.who_queue.push_back(key);
                        }
                        self.next_who(applied);
                    }
                }
            }
            // WHOIS replies go to the buffer that asked.
            "301" | "311" | "312" | "313" | "317" | "318" | "319" | "320" | "330" | "338"
            | "378" | "379" | "671" | "276" => {
                let index = self
                    .route
                    .or_else(|| self.whois_to.as_deref().and_then(|name| self.find(name)))
                    .unwrap_or(0);
                if command == "318" {
                    self.whois_to = None;
                }
                // What WHOIS says of someone you have a query with shows in
                // its header.
                if let Some(query) = self.find(param(1))
                    && self.buffers[query].kind == BufferKind::Query
                {
                    let peer = &mut self.buffers[query].peer;
                    match command.as_str() {
                        "330" => peer.account = Some(param(2).to_string()),
                        "301" => peer.away = Some(param(2).to_string()),
                        "311" => peer.realname = Some(param(5).to_string()),
                        _ => {}
                    }
                    applied.detail(query);
                }
                let text = whois_text(&command, &params);
                self.event(index, time, "•", text, applied);
            }
            // Errors about a channel or nick show where they apply.
            "401" | "403" | "404" | "405" | "406" | "442" | "467" | "471" | "473" | "474"
            | "475" | "477" | "478" | "482" => {
                // A message that can't be delivered stops waiting for its
                // echo.
                if label.is_some() {
                    self.refuse(label, None, applied);
                } else if matches!(command.as_str(), "401" | "404") {
                    self.refuse(None, Some(param(1)), applied);
                }
                let index = self.find(param(1)).unwrap_or_else(|| self.reply_buffer());
                if let Some(mut line) = describe(message, now) {
                    line.kind = LineKind::Error;
                    line.prefix = String::from("!");
                    self.add(index, line, applied);
                }
            }
            _ => self.server_line(message, now, applied),
        }
    }

    fn server_line(&mut self, message: &Message, now: i64, applied: &mut Applied) {
        if let Some(line) = describe(message, now) {
            let index = self.reply_buffer();
            self.add(index, line, applied);
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
        account: Option<String>,
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
        } else if let Some(index) = self.add_member(from, channel, account, applied) {
            // Quiet until they speak (the smart filter).
            let line = self.member_event(
                index,
                time,
                "→",
                format!("{from}{userhost} joined"),
                true,
                applied,
            );
            if let Some(member) = self.buffers[index].members.get_mut(from, &self.isupport) {
                member.join_line = Some((line, time));
            }
        }
    }

    /// Adds `nick` to `channel`'s members; the channel's buffer, if open.
    fn add_member(
        &mut self,
        nick: &str,
        channel: &str,
        account: Option<String>,
        applied: &mut Applied,
    ) -> Option<usize> {
        let index = self.find(channel)?;
        let mut member = Member::new(nick, "");
        member.account = account;
        self.buffers[index].members.insert(member, &self.isupport);
        applied.detail(index);
        Some(index)
    }

    /// Takes `nick` out of every channel; the buffers they were in (their
    /// query too), with when they last spoke there.
    fn remove_everywhere(&mut self, nick: &str, applied: &mut Applied) -> Vec<(usize, i64)> {
        let key = self.fold(nick);
        let mut found = Vec::new();
        for index in 1..self.buffers.len() {
            let buffer = &mut self.buffers[index];
            let spoke = match buffer.kind {
                BufferKind::Channel => buffer
                    .members
                    .remove(nick, &self.isupport)
                    .map(|member| member.spoke),
                // A query's quit always shows.
                BufferKind::Query if buffer.key == key => Some(i64::MAX),
                _ => None,
            };
            if let Some(spoke) = spoke {
                applied.detail(index);
                found.push((index, spoke));
            }
        }
        found
    }

    /// An event about someone else; `quiet` ones the smart filter hides.
    /// Its line's serial number.
    fn member_event(
        &mut self,
        index: usize,
        time: i64,
        prefix: &str,
        text: String,
        quiet: bool,
        applied: &mut Applied,
    ) -> u64 {
        let mut line = Line::new(time, LineKind::Event, prefix, text);
        line.quiet = quiet;
        self.add(index, line, applied);
        self.buffers[index].lines().back().map_or(0, |line| line.id)
    }

    /// PART and KICK: `who` leaves `channel`. A part of someone quiet is
    /// quiet; a kick always shows.
    fn on_leave(
        &mut self,
        who: &str,
        channel: &str,
        time: i64,
        kick: bool,
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
            let spoke = self.buffers[index]
                .members
                .remove(who, &self.isupport)
                .map_or(0, |member| member.spoke);
            let quiet = !kick && quiet(spoke, time);
            self.member_event(index, time, "←", text(who), quiet, applied);
        }
        applied.detail(index);
    }

    fn on_quit(&mut self, from: &str, reason: &str, time: i64, applied: &mut Applied) {
        for (index, spoke) in self.remove_everywhere(from, applied) {
            let quiet = quiet(spoke, time);
            let text = format!("{from} quit ({reason})");
            self.member_event(index, time, "←", text, quiet, applied);
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
            // When they last spoke here, if they're here; a query's rename
            // always shows.
            let spoke = match buffer.kind {
                BufferKind::Channel => {
                    let spoke = buffer
                        .members
                        .get(from, &self.isupport)
                        .map(|member| member.spoke);
                    buffer.members.rename(from, new, &self.isupport);
                    spoke
                }
                BufferKind::Query if buffer.key == old_key => {
                    buffer.name = new.to_string();
                    buffer.key = self.isupport.casemap.fold(new);
                    applied.list = true;
                    Some(i64::MAX)
                }
                _ => None,
            };
            if let Some(spoke) = spoke {
                applied.detail(index);
                let quiet = !mine && quiet(spoke, time);
                self.member_event(index, time, "•", text.clone(), quiet, applied);
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
        incoming: Incoming<'_>,
        applied: &mut Applied,
    ) {
        let time = incoming.time;
        let (Some(&target), Some(&text)) = (params.first(), params.get(1)) else {
            return;
        };
        let own = self.is_me(from);
        if !own && self.ignored(source) {
            return;
        }
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
        // IRCv3 echo-message: your own line coming back confirms the one
        // waiting, found by label, else by target and text. One that
        // matches nothing was sent elsewhere (another client on a bouncer).
        if own && self.has("echo-message") {
            let target_key = self.fold(target);
            let position = match incoming.label {
                Some(label) => self
                    .pending
                    .iter()
                    .position(|pending| pending.label.as_deref() == Some(label)),
                None => self.pending.iter().position(|pending| {
                    pending.target == target_key && pending.kind == kind && pending.text == text
                }),
            };
            if let Some(pending) = position.and_then(|at| self.pending.remove(at)) {
                self.confirm(pending, text, applied);
                return;
            }
        }
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
            let buffer = &mut self.buffers[index];
            let mut shown_join = None;
            if let Some(member) = buffer.members.get_mut(from, &self.isupport) {
                member.spoke = time;
                if let Some(account) = &incoming.account {
                    member.account = Some(account.clone());
                }
                // Speaking soon after joining shows the join the smart
                // filter hid.
                if let Some((line, joined)) = member.join_line.take()
                    && time.saturating_sub(joined) < QUIET_MS
                {
                    shown_join = Some(line);
                }
            }
            if let Some(id) = shown_join
                && let Some(line) = buffer.line_mut(id)
            {
                line.quiet = false;
            }
        } else if index > 0
            && !own
            && let Some(account) = incoming.account
        {
            // IRCv3 account-tag: who is writing to you.
            self.buffers[index].peer.account = Some(account);
            applied.detail(index);
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

    /// A network with these capabilities enabled, in #rust with alice
    /// and bob.
    fn with_caps(caps: &[&str]) -> (NetworkState, usize) {
        let mut network = network();
        network.caps = caps.iter().map(|cap| cap.to_string()).collect();
        apply(&mut network, ":ruby!u@h JOIN #rust");
        apply(&mut network, ":srv 353 ruby = #rust :ruby alice bob");
        apply(&mut network, ":srv 366 ruby #rust :End of /NAMES list.");
        let index = network.find("#rust").unwrap();
        (network, index)
    }

    #[test]
    fn netsplits_and_netjoins_collapse_into_one_line() {
        let (mut network, rust) = with_caps(&["batch"]);
        apply(&mut network, ":srv BATCH +sp netsplit irc.hub other.host");
        let held = apply(
            &mut network,
            "@batch=sp :alice!a@h QUIT :irc.hub other.host",
        );
        assert!(held.buffers.is_empty(), "held until the batch ends");
        apply(&mut network, "@batch=sp :bob!b@h QUIT :irc.hub other.host");
        let applied = apply(&mut network, ":srv BATCH -sp");
        assert!(applied.buffers.contains(&rust));
        assert_eq!(
            texts(&network, rust).last().unwrap(),
            "Netsplit irc.hub ↔ other.host: alice, bob"
        );
        assert!(
            !network.buffers[rust]
                .members
                .contains("alice", &network.isupport)
        );
        apply(&mut network, ":srv BATCH +j netjoin irc.hub other.host");
        apply(&mut network, "@batch=j :alice!a@h JOIN #rust");
        apply(&mut network, ":srv BATCH -j");
        assert_eq!(
            texts(&network, rust).last().unwrap(),
            "Netjoin irc.hub ↔ other.host: alice"
        );
        assert!(
            network.buffers[rust]
                .members
                .contains("alice", &network.isupport)
        );
        // Unknown types and nested batches apply their messages in order.
        apply(&mut network, ":srv BATCH +outer example.com/foo");
        apply(
            &mut network,
            "@batch=outer :srv BATCH +inner example.com/bar",
        );
        apply(&mut network, "@batch=inner :alice!a@h PRIVMSG #rust :inner");
        apply(&mut network, "@batch=outer :alice!a@h PRIVMSG #rust :outer");
        apply(&mut network, "@batch=outer :srv BATCH -inner");
        assert_eq!(
            texts(&network, rust).last().unwrap(),
            "Netjoin irc.hub ↔ other.host: alice"
        );
        apply(&mut network, ":srv BATCH -outer");
        let last: Vec<String> = texts(&network, rust).into_iter().rev().take(2).collect();
        assert_eq!(last, ["inner", "outer"]);
    }

    #[test]
    fn echoes_confirm_waiting_lines_and_refusals_fail_them() {
        let (mut network, rust) = with_caps(&["echo-message", "labeled-response", "batch"]);
        let label = network.label(rust).unwrap();
        network.echo(rust, LineKind::Message, "hello", 500, Some(label.clone()));
        let line = network.buffers[rust].lines().back().unwrap();
        assert!(line.pending && line.own);
        // The server may change the text; the echo carries the label.
        apply(
            &mut network,
            &format!("@label={label} :ruby!u@h PRIVMSG #rust :hello!"),
        );
        let lines = network.buffers[rust].lines();
        assert_eq!(lines.back().unwrap().text, "hello!");
        assert!(!lines.back().unwrap().pending);
        assert_eq!(
            lines.iter().filter(|line| line.own).count(),
            1,
            "the echo is not a second line"
        );
        // Without labels: matched by target and text.
        network.caps = vec![String::from("echo-message")];
        network.echo(rust, LineKind::Action, "waves", 600, None);
        apply(
            &mut network,
            ":ruby!u@h PRIVMSG #rust :\u{1}ACTION waves\u{1}",
        );
        assert!(!network.buffers[rust].lines().back().unwrap().pending);
        // Refused: 404 to that channel.
        network.echo(rust, LineKind::Message, "shh", 700, None);
        apply(&mut network, ":srv 404 ruby #rust :Cannot send to channel");
        let refused = network.buffers[rust]
            .lines()
            .iter()
            .find(|line| line.text == "shh")
            .unwrap();
        assert!(refused.failed && !refused.pending);
        // Lost with the connection.
        network.echo(rust, LineKind::Message, "bye", 800, None);
        network.disconnected();
        assert!(
            network.buffers[rust]
                .lines()
                .iter()
                .any(|line| line.text == "bye" && line.failed)
        );
        // An echo of a line sent elsewhere (another client) is a new line.
        network.caps = vec![String::from("echo-message")];
        apply(&mut network, ":ruby!u@h PRIVMSG #rust :from my phone");
        assert_eq!(texts(&network, rust).last().unwrap(), "from my phone");
    }

    #[test]
    fn labeled_replies_go_to_the_buffer_that_asked() {
        let (mut network, rust) = with_caps(&["labeled-response", "batch"]);
        let label = network.label(rust).unwrap();
        apply(
            &mut network,
            &format!("@label={label} :srv BATCH +w labeled-response"),
        );
        apply(&mut network, "@batch=w :srv 311 ruby alice a host * :Alice");
        apply(
            &mut network,
            "@batch=w :srv 318 ruby alice :End of /WHOIS list.",
        );
        apply(&mut network, ":srv BATCH -w");
        let text = texts(&network, rust);
        assert!(
            text.iter().any(|line| line == "alice is a@host (Alice)"),
            "{text:?}"
        );
        // A labeled ACK says nothing; a labeled FAIL shows where it was asked.
        let ack = network.label(rust).unwrap();
        let before = network.buffers[rust].lines().len();
        apply(&mut network, &format!("@label={ack} :srv ACK"));
        assert_eq!(network.buffers[rust].lines().len(), before);
        let fail = network.label(rust).unwrap();
        apply(
            &mut network,
            &format!("@label={fail} :srv FAIL CHATHISTORY INVALID_TARGET :No such target"),
        );
        let last = network.buffers[rust].lines().back().unwrap().clone();
        assert_eq!(
            (last.kind, last.text.as_str()),
            (LineKind::Error, "No such target (CHATHISTORY)")
        );
        // Unlabeled notes go to the server buffer.
        apply(&mut network, ":srv NOTE * OPER_MESSAGE :Be nice");
        assert_eq!(texts(&network, 0).last().unwrap(), "Be nice");
        // No labels without the capability.
        network.caps.clear();
        assert!(network.label(rust).is_none());
    }

    #[test]
    fn away_account_and_names_follow_the_capabilities() {
        let mut network = network();
        network.caps = [
            "away-notify",
            "account-notify",
            "extended-join",
            "account-tag",
        ]
        .iter()
        .map(|cap| cap.to_string())
        .collect();
        // WHO after joining: WHOX when the server has it, answered quietly.
        apply(&mut network, ":srv 005 ruby WHOX :are supported");
        apply(&mut network, ":ruby!u@h JOIN #rust");
        apply(&mut network, ":srv 353 ruby = #rust :ruby alice bob");
        let applied = apply(&mut network, ":srv 366 ruby #rust :End of /NAMES list.");
        assert_eq!(applied.replies, ["WHO #rust %tcnfa,152"]);
        let rust = network.find("#rust").unwrap();
        apply(&mut network, ":ruby!u@h JOIN #other");
        let applied = apply(&mut network, ":srv 366 ruby #other :End of /NAMES list.");
        assert!(applied.replies.is_empty(), "one WHO at a time");
        let before = texts(&network, 0).len();
        apply(&mut network, ":srv 354 ruby 152 #rust alice G alicea");
        apply(&mut network, ":srv 354 ruby 152 #rust bob H 0");
        let applied = apply(&mut network, ":srv 315 ruby #rust :End of WHO list.");
        assert_eq!(applied.replies, ["WHO #other %tcnfa,152"], "then the next");
        assert_eq!(texts(&network, 0).len(), before, "nothing shown");
        let alice = network.buffers[rust]
            .members
            .get("alice", &network.isupport)
            .unwrap();
        assert!(alice.away);
        assert_eq!(alice.account.as_deref(), Some("alicea"));
        apply(&mut network, ":alice!a@h AWAY");
        apply(&mut network, ":bob!b@h ACCOUNT bobb");
        let members = &network.buffers[rust].members;
        assert!(!members.get("alice", &network.isupport).unwrap().away);
        assert_eq!(
            members
                .get("bob", &network.isupport)
                .unwrap()
                .account
                .as_deref(),
            Some("bobb")
        );
        apply(&mut network, ":carol!c@h JOIN #rust carolc :Carol C");
        let carol = network.buffers[rust]
            .members
            .get("carol", &network.isupport)
            .unwrap();
        assert_eq!(carol.account.as_deref(), Some("carolc"));
        // A query learns from account-tag, AWAY and SETNAME.
        apply(&mut network, "@account=dd :dave!d@h PRIVMSG ruby :hi");
        let dave = network.find("dave").unwrap();
        apply(&mut network, ":dave!d@h AWAY :lunch");
        apply(&mut network, ":dave!d@h SETNAME :Dave D");
        let peer = &network.buffers[dave].peer;
        assert_eq!(peer.account.as_deref(), Some("dd"));
        assert_eq!(peer.away.as_deref(), Some("lunch"));
        assert_eq!(peer.realname.as_deref(), Some("Dave D"));
        // CHGHOST changes nothing shown.
        let before = texts(&network, 0).len();
        apply(&mut network, ":alice!a@h CHGHOST b ckyard");
        assert_eq!(texts(&network, 0).len(), before);
        // invite-notify: someone else invited, in the channel.
        apply(&mut network, ":alice!a@h INVITE erin #rust");
        assert_eq!(
            texts(&network, rust).last().unwrap(),
            "alice invited erin to #rust"
        );
    }

    #[test]
    fn monitor_watches_open_queries() {
        let mut network = network();
        apply(&mut network, ":srv 005 ruby MONITOR=100 :are supported");
        assert!(network.monitor_lines().is_empty());
        apply(&mut network, ":alice!a@h PRIVMSG ruby :hi");
        apply(&mut network, ":bob!b@h PRIVMSG ruby :hi");
        assert_eq!(network.monitor_lines(), ["MONITOR + alice,bob"]);
        assert!(network.monitor_lines().is_empty(), "already watched");
        let bob = network.find("bob").unwrap();
        network.close(bob);
        assert_eq!(network.monitor_lines(), ["MONITOR - bob"]);
        let applied = apply(&mut network, ":srv 731 ruby :alice");
        let alice = network.find("alice").unwrap();
        assert_eq!(network.buffers[alice].peer.online, Some(false));
        assert!(applied.list);
        apply(&mut network, ":srv 730 ruby :alice!a@h");
        assert_eq!(network.buffers[alice].peer.online, Some(true));
    }

    #[test]
    fn the_smart_filter_marks_quiet_comings_and_goings() {
        let (mut network, rust) = with_caps(&[]);
        let at = |network: &mut NetworkState, line: &str, now: i64| {
            network.apply(&Message::parse(line).unwrap(), now);
        };
        at(&mut network, ":carol!c@h JOIN #rust", 10_000);
        assert!(network.buffers[rust].lines().back().unwrap().quiet);
        let join = network.buffers[rust].lines().back().unwrap().id;
        // Speaking soon after shows the join.
        at(&mut network, ":carol!c@h PRIVMSG #rust :hi all", 20_000);
        let lines = network.buffers[rust].lines();
        assert!(!lines[network.buffers[rust].index_of(join).unwrap()].quiet);
        // Leaving soon after speaking shows; someone silent leaves quietly.
        at(&mut network, ":carol!c@h PART #rust", 30_000);
        assert!(!network.buffers[rust].lines().back().unwrap().quiet);
        at(&mut network, ":alice!a@h QUIT :bye", 40_000);
        assert!(network.buffers[rust].lines().back().unwrap().quiet);
        // A kick always shows.
        at(&mut network, ":op!o@h KICK #rust bob :out", 50_000);
        assert!(!network.buffers[rust].lines().back().unwrap().quiet);
    }

    #[test]
    fn ignored_people_say_nothing() {
        let (mut network, rust) = with_caps(&[]);
        let mask = network.ignore_mask("Alice");
        assert_eq!(mask, "alice!*@*");
        network.ignores.push(mask);
        network
            .ignores
            .push(network.ignore_mask("*!*@spam.example"));
        let before = texts(&network, rust).len();
        apply(&mut network, ":ALICE!a@h PRIVMSG #rust :hello");
        apply(&mut network, ":mallory!m@spam.example PRIVMSG #rust :buy");
        apply(&mut network, ":mallory!m@spam.example PRIVMSG ruby :buy");
        assert_eq!(texts(&network, rust).len(), before);
        assert!(network.find("mallory").is_none(), "no query opened");
        apply(&mut network, ":bob!b@h PRIVMSG #rust :still here");
        assert_eq!(texts(&network, rust).len(), before + 1);
        assert!(glob("a*c?e", "abcde"));
        assert!(!glob("a*c?e", "abcd"));
        assert!(glob("*", ""));
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
