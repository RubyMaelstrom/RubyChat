//! What the interface shows: networks of buffers of lines (DESIGN.md §7).

pub mod buffer;
pub mod channels;
pub mod editor;
pub mod links;
pub mod members;
pub mod network;
pub mod recent;
pub mod search;

use std::borrow::Cow;

use crate::irc::Message;
use crate::irc::format::strip;
use crate::irc::time::parse_server_time;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// RubyChat's own news: connection progress and problems.
    Status,
    /// A raw line the user sent.
    Sent,
    Notice,
    /// Numeric replies and other server text.
    Server,
    /// A message from someone.
    Message,
    /// `/me` and CTCP ACTION.
    Action,
    /// Joins, parts, quits, nick and mode changes.
    Event,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Milliseconds since the Unix epoch.
    pub time: i64,
    pub kind: LineKind,
    /// Who said it, for messages, actions and notices: the nick column.
    pub nick: String,
    /// Shown in the nick column for other lines, such as `→` or `!`.
    pub prefix: String,
    /// The text, with any formatting codes.
    pub text: String,
    /// It mentions you.
    pub highlight: bool,
    /// You sent it.
    pub own: bool,
    /// Sent, and waiting for the server's echo to confirm it
    /// (`echo-message`).
    pub pending: bool,
    /// The server refused it, or the connection ended before it was
    /// confirmed.
    pub failed: bool,
    /// A join, part, quit or nick change of someone who hadn't spoken
    /// lately, which the smart filter hides.
    pub quiet: bool,
    /// From history (a log or the server's), not seen live: never unread,
    /// never logged again.
    pub history: bool,
    /// The server's ID for it (IRCv3 message-ids), for spotting history
    /// already shown.
    pub msgid: Option<Box<str>>,
    /// Its buffer's serial number for it (0 until added to a buffer).
    pub id: u64,
}

impl Line {
    pub fn new(
        time: i64,
        kind: LineKind,
        prefix: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            time,
            kind,
            nick: String::new(),
            prefix: prefix.into(),
            text: text.into(),
            highlight: false,
            own: false,
            pending: false,
            failed: false,
            quiet: false,
            history: false,
            msgid: None,
            id: 0,
        }
    }

    /// A message, action or notice from `nick`.
    pub fn message(
        time: i64,
        kind: LineKind,
        nick: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            nick: nick.into(),
            ..Self::new(time, kind, "", text)
        }
    }

    /// What the nick column shows: the nick for a message, `-nick-` for a
    /// notice, `*` for an action (its nick starts the text), and otherwise
    /// the prefix.
    pub fn label(&self) -> Cow<'_, str> {
        match self.kind {
            LineKind::Message if !self.nick.is_empty() => Cow::Borrowed(&self.nick),
            LineKind::Notice if !self.nick.is_empty() => Cow::Owned(format!("-{}-", self.nick)),
            LineKind::Action => Cow::Borrowed("*"),
            _ => Cow::Borrowed(&self.prefix),
        }
    }

    /// The label's length in characters, without building it.
    pub fn label_chars(&self) -> usize {
        match self.kind {
            LineKind::Message if !self.nick.is_empty() => self.nick.chars().count(),
            LineKind::Notice if !self.nick.is_empty() => self.nick.chars().count() + 2,
            LineKind::Action => 1,
            _ => self.prefix.chars().count(),
        }
    }
}

/// When a message happened: its `time` tag (IRCv3 server-time), else `now`.
pub fn message_time(message: &Message, now: i64) -> i64 {
    message
        .tag("time")
        .and_then(|time| parse_server_time(&time))
        .unwrap_or(now)
}

/// How a received message reads in the server buffer, or `None` for
/// protocol traffic nobody needs to see.
pub fn describe(message: &Message, now: i64) -> Option<Line> {
    let time = message_time(message, now);
    let command = message.command().to_ascii_uppercase();
    let who = message.nick().unwrap_or("");
    let params: Vec<&str> = message.params().collect();
    let last = params.last().copied().unwrap_or("");
    let text = |text: &str| strip(text).into_owned();
    let line = |kind, prefix: String, body: String| Some(Line::new(time, kind, prefix, body));
    match command.as_str() {
        "PING" | "PONG" | "CAP" | "AUTHENTICATE" => None,
        "NOTICE" => line(LineKind::Notice, format!("-{who}-"), text(last)),
        "PRIVMSG" => {
            let target = params.first().copied().unwrap_or("");
            if let Some(action) = last
                .strip_prefix("\u{1}ACTION ")
                .map(|a| a.trim_end_matches('\u{1}'))
            {
                return line(
                    LineKind::Message,
                    String::from("*"),
                    format!("{who} {}", text(action)),
                );
            }
            let prefix = if target.starts_with(['#', '&']) {
                format!("<{who}:{target}>")
            } else {
                format!("<{who}>")
            };
            line(LineKind::Message, prefix, text(last))
        }
        "ERROR" => line(LineKind::Error, String::from("!"), text(last)),
        "JOIN" => line(
            LineKind::Event,
            String::from("→"),
            format!("{who} joined {}", params.first().unwrap_or(&"")),
        ),
        "PART" => {
            let channel = params.first().copied().unwrap_or("");
            let reason = params
                .get(1)
                .map(|r| format!(" ({})", text(r)))
                .unwrap_or_default();
            line(
                LineKind::Event,
                String::from("←"),
                format!("{who} left {channel}{reason}"),
            )
        }
        "QUIT" => line(
            LineKind::Event,
            String::from("←"),
            format!("{who} quit ({})", text(last)),
        ),
        "NICK" => line(
            LineKind::Event,
            String::from("•"),
            format!("{who} is now known as {last}"),
        ),
        "MODE" => line(
            LineKind::Event,
            String::from("•"),
            format!(
                "{who} sets mode {}",
                params.get(1..).unwrap_or_default().join(" ")
            ),
        ),
        "TOPIC" => line(
            LineKind::Event,
            String::from("•"),
            format!(
                "{who} changed the topic of {} to: {}",
                params.first().unwrap_or(&""),
                text(last)
            ),
        ),
        "KICK" => line(
            LineKind::Event,
            String::from("←"),
            format!(
                "{who} kicked {} from {} ({})",
                params.get(1).unwrap_or(&""),
                params.first().unwrap_or(&""),
                text(last)
            ),
        ),
        "INVITE" => line(
            LineKind::Event,
            String::from("•"),
            format!("{who} invites you to {last}"),
        ),
        numeric if numeric.len() == 3 && numeric.bytes().all(|b| b.is_ascii_digit()) => {
            // The first parameter is the client the reply is for.
            let body = params
                .get(1..)
                .unwrap_or_default()
                .iter()
                .map(|p| text(p))
                .collect::<Vec<_>>()
                .join(" ");
            let kind = if numeric.starts_with(['4', '5']) {
                LineKind::Error
            } else {
                LineKind::Server
            };
            line(kind, String::new(), body)
        }
        _ => line(
            LineKind::Server,
            message.command().to_string(),
            params.iter().map(|p| text(p)).collect::<Vec<_>>().join(" "),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(line: &str) -> Option<Line> {
        describe(&Message::parse(line).unwrap(), 42)
    }

    #[test]
    fn server_buffer_lines_read_naturally() {
        let welcome =
            read("@time=2011-10-19T16:40:51.620Z :srv 001 ruby :Welcome to \u{2}IRC\u{2} ruby")
                .unwrap();
        assert_eq!(
            (welcome.kind, welcome.text.as_str(), welcome.time),
            (LineKind::Server, "Welcome to IRC ruby", 1_319_042_451_620)
        );
        let isupport =
            read(":srv 005 ruby CHANTYPES=# NETWORK=Test :are supported by this server").unwrap();
        assert_eq!(
            isupport.text,
            "CHANTYPES=# NETWORK=Test are supported by this server"
        );
        assert_eq!(
            read(":srv 433 * ruby :Nickname is already in use")
                .unwrap()
                .kind,
            LineKind::Error
        );
        let notice = read(":NickServ!s@services NOTICE ruby :This nickname is registered").unwrap();
        assert_eq!((notice.prefix.as_str(), notice.time), ("-NickServ-", 42));
        assert_eq!(
            read(":a!b@c PRIVMSG ruby :\u{1}ACTION waves\u{1}")
                .unwrap()
                .text,
            "a waves"
        );
        assert_eq!(
            read(":a!b@c PRIVMSG #rust :hi").unwrap().prefix,
            "<a:#rust>"
        );
        assert_eq!(read(":a!b@c JOIN #rust").unwrap().text, "a joined #rust");
        assert!(read("PING :x").is_none() && read(":srv CAP * LS :a").is_none());
    }
}
