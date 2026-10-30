//! A buffer: one conversation view inside a network—the server buffer, a
//! channel or a query—with its lines, its members and topic (channels), its
//! unread state and its own input draft (DESIGN.md §7).

use std::collections::VecDeque;

use super::editor::LineEditor;
use super::members::Members;
use super::{Line, LineKind};

/// Lines kept per buffer (DESIGN.md §7).
pub const SERVER_LINES: usize = 2000;
pub const CONVERSATION_LINES: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferKind {
    Server,
    Channel,
    Query,
}

/// How much happened since the buffer was last seen, ranked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Activity {
    #[default]
    None,
    /// Joins, parts and the like.
    Events,
    Messages,
    /// Someone mentioned you, or wrote to you privately.
    Highlight,
}

/// What a query knows about the person on the other end (DESIGN.md
/// §8.5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Peer {
    /// Online or not, from MONITOR; `None` until the server says.
    pub online: Option<bool>,
    pub account: Option<String>,
    /// Their away message, while away.
    pub away: Option<String>,
    pub realname: Option<String>,
}

/// Where a scrolled-up view rests: the bottom of line `id` is `below`
/// pixels under the view's bottom edge. Anchoring to a line rather than a
/// pixel offset keeps the view still as lines arrive and the width changes
/// (DESIGN.md §10).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anchor {
    pub id: u64,
    pub below: f32,
}

#[derive(Clone, Debug)]
pub struct Buffer {
    pub kind: BufferKind,
    pub name: String,
    /// The casefolded name, for finding the buffer.
    pub key: String,
    lines: VecDeque<Line>,
    capacity: usize,
    next_id: u64,
    pub topic: Option<String>,
    pub topic_by: Option<String>,
    /// Channel settings (CHANMODES types B, C and D) and their parameters.
    pub modes: Vec<(char, Option<String>)>,
    pub members: Members,
    /// A channel is joined; false after parting, being kicked or a lost
    /// connection.
    pub joined: bool,
    /// Join the channel whenever the network registers: you joined it and
    /// haven't parted or been kicked since.
    pub rejoin: bool,
    /// Someone is looking at it (shown while the window has focus), so its
    /// lines arrive read.
    pub seen: bool,
    pub unread: u32,
    pub highlights: u32,
    pub activity: Activity,
    /// `None` follows the newest line.
    pub anchor: Option<Anchor>,
    /// The nick list's scroll, in pixels from its top.
    pub nick_scroll: f32,
    pub input: LineEditor,
    /// The first line that arrived unseen: where the NEW MESSAGES rule goes.
    pub marker: Option<u64>,
    /// The longest nick column label among its lines, in characters.
    pub label_chars: usize,
    /// A query's person.
    pub peer: Peer,
}

impl Buffer {
    pub fn new(kind: BufferKind, name: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
            key: key.into(),
            lines: VecDeque::new(),
            capacity: if kind == BufferKind::Server {
                SERVER_LINES
            } else {
                CONVERSATION_LINES
            },
            next_id: 1,
            topic: None,
            topic_by: None,
            modes: Vec::new(),
            members: Members::default(),
            joined: kind != BufferKind::Channel,
            rejoin: false,
            seen: false,
            unread: 0,
            highlights: 0,
            activity: Activity::None,
            anchor: None,
            nick_scroll: 0.0,
            input: LineEditor::default(),
            marker: None,
            label_chars: 0,
            peer: Peer::default(),
        }
    }

    /// Adds a line, counting it as unread unless the buffer is seen.
    pub fn push(&mut self, mut line: Line) {
        line.id = self.next_id;
        self.next_id += 1;
        let message = matches!(
            line.kind,
            LineKind::Message | LineKind::Action | LineKind::Notice
        ) && !line.own;
        if self.seen {
            // Read as it arrives.
        } else if message {
            self.unread += 1;
            self.marker.get_or_insert(line.id);
            let private = self.kind == BufferKind::Query && line.kind != LineKind::Notice;
            if line.highlight || private {
                self.highlights += 1;
                self.activity = Activity::Highlight;
            } else {
                self.activity = self.activity.max(Activity::Messages);
            }
        } else if line.kind == LineKind::Event {
            self.activity = self.activity.max(Activity::Events);
        }
        self.label_chars = self.label_chars.max(line.label_chars());
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn lines(&self) -> &VecDeque<Line> {
        &self.lines
    }

    /// The line with serial number `id`, by index: serial numbers are
    /// consecutive.
    pub fn index_of(&self, id: u64) -> Option<usize> {
        let first = self.lines.front()?.id;
        let index = usize::try_from(id.checked_sub(first)?).ok()?;
        (index < self.lines.len()).then_some(index)
    }

    /// The line with serial number `id`, to change it in place.
    pub fn line_mut(&mut self, id: u64) -> Option<&mut Line> {
        let index = self.index_of(id)?;
        self.lines.get_mut(index)
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.anchor = None;
        self.marker = None;
        self.label_chars = 0;
    }

    /// Seen: the counts reset, but the NEW MESSAGES rule stays until the
    /// buffer is left.
    pub fn mark_read(&mut self) {
        self.unread = 0;
        self.highlights = 0;
        self.activity = Activity::None;
    }

    /// Shown with the window focused, or no longer.
    pub fn set_seen(&mut self, seen: bool) {
        self.seen = seen;
        if seen {
            self.mark_read();
        }
    }

    /// Left for another buffer: the rule goes.
    pub fn leave(&mut self) {
        self.seen = false;
        self.marker = None;
    }

    /// `+knt`, with parameters left out (keys stay private).
    pub fn mode_string(&self) -> String {
        if self.modes.is_empty() {
            return String::new();
        }
        let mut letters: Vec<char> = self.modes.iter().map(|(mode, _)| *mode).collect();
        letters.sort_unstable();
        std::iter::once('+').chain(letters).collect()
    }

    /// The channel key, if it has one (mode `k`).
    pub fn key_mode(&self) -> Option<&str> {
        self.modes
            .iter()
            .find(|(mode, _)| *mode == 'k')
            .and_then(|(_, key)| key.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str, highlight: bool) -> Line {
        let mut line = Line::message(0, LineKind::Message, "bob", text);
        line.highlight = highlight;
        line
    }

    #[test]
    fn counts_unread_messages_and_highlights() {
        let mut channel = Buffer::new(BufferKind::Channel, "#rust", "#rust");
        channel.push(Line::new(0, LineKind::Event, "→", "alice joined"));
        assert_eq!((channel.unread, channel.activity), (0, Activity::Events));
        channel.push(message("hello", false));
        channel.push(message("ruby: hi", true));
        assert_eq!(
            (channel.unread, channel.highlights, channel.activity),
            (2, 1, Activity::Highlight)
        );
        assert_eq!(channel.marker, Some(2));
        let mut own = message("mine", false);
        own.own = true;
        channel.push(own);
        assert_eq!(channel.unread, 2, "your own lines are never unread");
        channel.set_seen(true);
        assert_eq!(
            (channel.unread, channel.highlights, channel.activity),
            (0, 0, Activity::None)
        );
        assert_eq!(
            channel.marker,
            Some(2),
            "the rule stays while the buffer is shown"
        );
        channel.push(message("seen at once", true));
        assert_eq!(
            (channel.unread, channel.highlights),
            (0, 0),
            "lines arrive read while seen"
        );
        channel.leave();
        assert_eq!((channel.marker, channel.seen), (None, false));
        let mut query = Buffer::new(BufferKind::Query, "bob", "bob");
        query.push(message("psst", false));
        assert_eq!(query.highlights, 1, "private messages always count");
    }

    #[test]
    fn lines_are_found_by_serial_number() {
        let mut server = Buffer::new(BufferKind::Server, "srv", "");
        server.capacity = 3;
        for n in 0..5 {
            server.push(Line::new(0, LineKind::Server, "", format!("{n}")));
        }
        assert_eq!(server.lines().front().unwrap().id, 3);
        assert_eq!(
            (
                server.index_of(2),
                server.index_of(3),
                server.index_of(5),
                server.index_of(6)
            ),
            (None, Some(0), Some(2), None)
        );
    }

    #[test]
    fn modes_read_compactly() {
        let mut channel = Buffer::new(BufferKind::Channel, "#rust", "#rust");
        assert!(!channel.joined);
        channel.modes = vec![
            ('t', None),
            ('n', None),
            ('k', Some(String::from("secret"))),
        ];
        assert_eq!(channel.mode_string(), "+knt");
        assert_eq!(channel.key_mode(), Some("secret"));
    }
}
