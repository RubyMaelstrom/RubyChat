//! Notifications (DESIGN.md §12.3) through the Desktop Notifications
//! Specification's `org.freedesktop.Notifications`: one per buffer, which
//! later messages replace (`replaces_id`) rather than pile up, showing the
//! newest few lines. Clicking one invokes its "default" action; a server
//! that supports it first sends an xdg-activation token
//! (`ActivationToken`), which goes to the app with the click.

use std::collections::{HashMap, VecDeque};

use super::tray::escape_markup;
use super::{Notice, NoticeKey};
use crate::platform::dbus::{Message, Value};

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";

/// Lines a notification shows, newest last.
const LINES: usize = 3;
/// Buffers with a notification at once; past this the oldest goes.
const TRACKED: usize = 32;
/// Tokens kept while waiting for the click that follows them.
const TOKENS: usize = 8;
/// Characters kept of a title and of a line.
const TITLE_CHARS: usize = 100;
const LINE_CHARS: usize = 300;
/// The logo's size in the notification.
const IMAGE_SIZE: u32 = 64;

struct Entry {
    /// The server's ID, once `Notify` answered.
    id: Option<u32>,
    title: String,
    /// The newest lines, as (nick, text).
    lines: VecDeque<(String, String)>,
    /// Lines since the buffer was last seen.
    count: u32,
    /// A `Notify` awaits its reply; changes meanwhile go out after it.
    in_flight: bool,
    stale: bool,
    /// When it was last touched, for retiring the oldest.
    touched: u64,
}

pub struct Notices {
    entries: HashMap<NoticeKey, Entry>,
    by_id: HashMap<u32, NoticeKey>,
    tokens: VecDeque<(u32, String)>,
    /// The server renders `<b>` and escapes (capability "body-markup").
    pub markup: bool,
    image: Value,
    clock: u64,
}

/// A call to make for a notification: `Notify` (whose reply carries the
/// ID) or `CloseNotification`.
pub enum Call {
    Notify(NoticeKey, Message),
    Close(Message),
}

impl Notices {
    pub fn new(logo: fn(u32) -> Vec<u8>) -> Self {
        let rgba = logo(IMAGE_SIZE);
        let side = IMAGE_SIZE as i32;
        // "image-data": (iiibiiay), RGBA rows.
        let image = Value::Struct(vec![
            Value::I32(side),
            Value::I32(side),
            Value::I32(side * 4),
            Value::Bool(true),
            Value::I32(8),
            Value::I32(4),
            Value::Bytes(rgba),
        ]);
        Self {
            entries: HashMap::new(),
            by_id: HashMap::new(),
            tokens: VecDeque::new(),
            markup: false,
            image,
            clock: 0,
        }
    }

    /// Adds a line to its buffer's notification; the call that shows it,
    /// unless one is already on its way.
    pub fn add(&mut self, notice: Notice) -> Vec<Call> {
        self.clock += 1;
        let mut calls = Vec::new();
        if !self.entries.contains_key(&notice.key) && self.entries.len() >= TRACKED {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                calls.extend(self.withdraw(&oldest));
            }
        }
        let entry = self.entries.entry(notice.key.clone()).or_insert(Entry {
            id: None,
            title: String::new(),
            lines: VecDeque::new(),
            count: 0,
            in_flight: false,
            stale: false,
            touched: 0,
        });
        entry.title = truncate(&notice.title, TITLE_CHARS);
        if entry.lines.len() == LINES {
            entry.lines.pop_front();
        }
        entry.lines.push_back((
            truncate(&notice.nick, TITLE_CHARS),
            truncate(&notice.text, LINE_CHARS),
        ));
        entry.count = entry.count.saturating_add(1);
        entry.touched = self.clock;
        if entry.in_flight {
            entry.stale = true;
        } else {
            calls.push(self.notify_call(&notice.key));
        }
        calls
    }

    fn notify_call(&mut self, key: &NoticeKey) -> Call {
        let entry = self.entries.get_mut(key).expect("an entry");
        entry.in_flight = true;
        entry.stale = false;
        let earlier = entry.count as usize - entry.lines.len().min(entry.count as usize);
        let mut body = Vec::new();
        if earlier > 0 {
            body.push(format!("… {earlier} earlier"));
        }
        for (nick, text) in &entry.lines {
            body.push(match (nick.is_empty(), self.markup) {
                (true, true) => escape_markup(text),
                (true, false) => text.clone(),
                (false, true) => format!("<b>{}</b>: {}", escape_markup(nick), escape_markup(text)),
                (false, false) => format!("{nick}: {text}"),
            });
        }
        let message = Message::call(
            NAME,
            PATH,
            NAME,
            "Notify",
            vec![
                Value::str("RubyChat"),
                Value::U32(entry.id.unwrap_or(0)),
                Value::str(""),
                Value::str(entry.title.as_str()),
                Value::Str(body.join("\n")),
                Value::strings(["default", "Open"]),
                Value::dict([
                    ("desktop-entry", Value::str("rubychat")),
                    ("category", Value::str("im.received")),
                    ("urgency", Value::Byte(1)),
                    ("image-data", self.image.clone()),
                ]),
                Value::I32(-1),
            ],
        );
        Call::Notify(key.clone(), message)
    }

    /// `Notify` answered with `id`; a change that waited goes out now, and
    /// a notification withdrawn while on its way closes.
    pub fn shown(&mut self, key: &NoticeKey, id: u32) -> Option<Call> {
        let Some(entry) = self.entries.get_mut(key) else {
            return Some(close(id));
        };
        entry.in_flight = false;
        if let Some(old) = entry.id.replace(id)
            && old != id
        {
            self.by_id.remove(&old);
        }
        self.by_id.insert(id, key.clone());
        let stale = entry.stale;
        stale.then(|| self.notify_call(key))
    }

    /// `Notify` failed: the next line tries afresh.
    pub fn failed(&mut self, key: &NoticeKey) {
        if let Some(entry) = self.entries.remove(key)
            && let Some(id) = entry.id
        {
            self.by_id.remove(&id);
        }
    }

    /// The buffer was seen: its notification goes.
    pub fn withdraw(&mut self, key: &NoticeKey) -> Option<Call> {
        let entry = self.entries.remove(key)?;
        let id = entry.id?;
        self.by_id.remove(&id);
        Some(close(id))
    }

    /// Every notification goes, as RubyChat quits.
    pub fn withdraw_all(&mut self) -> Vec<Call> {
        let keys: Vec<NoticeKey> = self.entries.keys().cloned().collect();
        keys.iter().filter_map(|key| self.withdraw(key)).collect()
    }

    /// `NotificationClosed`: expired or dismissed, so the next line starts
    /// a new one.
    pub fn closed(&mut self, id: u32) {
        if let Some(key) = self.by_id.remove(&id) {
            self.entries.remove(&key);
        }
    }

    /// `ActivationToken`, which comes just before the click it belongs to.
    pub fn token(&mut self, id: u32, token: String) {
        if self.tokens.len() == TOKENS {
            self.tokens.pop_front();
        }
        self.tokens.push_back((id, token));
    }

    /// `ActionInvoked`: the buffer to open, with the token, if one came.
    pub fn invoked(&mut self, id: u32) -> Option<(NoticeKey, Option<String>)> {
        let key = self.by_id.get(&id)?.clone();
        let token = self
            .tokens
            .iter()
            .rposition(|(token_id, _)| *token_id == id)
            .and_then(|at| self.tokens.remove(at))
            .map(|(_, token)| token);
        Some((key, token))
    }

    /// The notification server went away or was replaced: its IDs mean
    /// nothing now.
    pub fn reset(&mut self) {
        self.entries.clear();
        self.by_id.clear();
        self.tokens.clear();
    }
}

fn close(id: u32) -> Call {
    Call::Close(Message::call(
        NAME,
        PATH,
        NAME,
        "CloseNotification",
        vec![Value::U32(id)],
    ))
}

/// At most `limit` characters, with control characters as spaces.
fn truncate(text: &str, limit: usize) -> String {
    let mut out: String = text
        .chars()
        .take(limit)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.chars().nth(limit).is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon(size: u32) -> Vec<u8> {
        vec![255; (size * size * 4) as usize]
    }

    fn notice(text: &str) -> Notice {
        Notice {
            key: (1, String::from("#rust")),
            title: String::from("#rust · Libera.Chat"),
            nick: String::from("ferris"),
            text: text.to_string(),
        }
    }

    fn body(call: &Call) -> (u32, String) {
        let Call::Notify(_, message) = call else {
            panic!("a Notify call");
        };
        assert!(message.encode(1).is_ok());
        (
            message.body[1].as_u32().unwrap(),
            message.arg_str(4).unwrap().to_string(),
        )
    }

    #[test]
    fn a_buffers_lines_coalesce_into_one_notification() {
        let mut notices = Notices::new(icon);
        notices.markup = true;
        let calls = notices.add(notice("ruby: <hello> & bye"));
        assert_eq!(
            body(&calls[0]),
            (
                0,
                String::from("<b>ferris</b>: ruby: &lt;hello&gt; &amp; bye")
            )
        );
        // Lines while Notify is on its way wait for its answer.
        assert!(notices.add(notice("two")).is_empty());
        assert!(notices.add(notice("three")).is_empty());
        let key = (1, String::from("#rust"));
        let again = notices.shown(&key, 41).expect("the waiting change");
        let (replaces, text) = body(&again);
        assert_eq!(replaces, 41);
        assert_eq!(text.lines().count(), 3);
        assert!(notices.shown(&key, 41).is_none());
        // A fourth line counts the one it pushed out.
        let calls = notices.add(notice("four"));
        let (_, text) = body(&calls[0]);
        assert!(text.starts_with("… 1 earlier\n"), "{text}");
        assert_eq!(text.lines().count(), 4);
    }

    #[test]
    fn clicks_find_their_buffer_and_token() {
        let mut notices = Notices::new(icon);
        let key = (1, String::from("#rust"));
        notices.add(notice("hi"));
        notices.shown(&key, 7);
        notices.token(7, String::from("activation-token"));
        assert_eq!(
            notices.invoked(7),
            Some((key.clone(), Some(String::from("activation-token"))))
        );
        assert_eq!(notices.invoked(7), Some((key.clone(), None)));
        assert_eq!(notices.invoked(8), None);
        // Dismissed: the next line starts afresh.
        notices.closed(7);
        let calls = notices.add(notice("again"));
        assert_eq!(body(&calls[0]).0, 0);
    }

    #[test]
    fn seen_buffers_withdraw_and_the_oldest_retire() {
        let mut notices = Notices::new(icon);
        let key = (1, String::from("#rust"));
        notices.add(notice("hi"));
        notices.shown(&key, 3);
        match notices.withdraw(&key) {
            Some(Call::Close(message)) => assert_eq!(message.body, [Value::U32(3)]),
            _ => panic!("a CloseNotification"),
        }
        assert!(notices.withdraw(&key).is_none());
        // Seen while its Notify was on the way: it closes once it has an ID.
        notices.add(notice("quick"));
        assert!(notices.withdraw(&key).is_none());
        assert!(matches!(notices.shown(&key, 4), Some(Call::Close(_))));
        for n in 0..TRACKED as u64 {
            let mut notice = notice("x");
            notice.key = (n + 10, String::from("#a"));
            notices.add(notice);
            notices.shown(&(n + 10, String::from("#a")), n as u32 + 100);
        }
        let calls = notices.add(notice("one too many"));
        assert!(matches!(calls[0], Call::Close(_)), "the oldest closes");
        assert_eq!(notices.entries.len(), TRACKED);
    }

    #[test]
    fn plain_servers_get_plain_text() {
        let mut notices = Notices::new(icon);
        let calls = notices.add(notice("a <b> c\u{7}"));
        assert_eq!(body(&calls[0]).1, "ferris: a <b> c ");
        let long = "x".repeat(LINE_CHARS + 10);
        assert_eq!(truncate(&long, LINE_CHARS).chars().count(), LINE_CHARS + 1);
    }
}
