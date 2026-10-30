//! IRC messages, parsed and serialized as Modern IRC's "Message Format" and
//! IRCv3 message-tags define them:
//!
//! ```text
//! message    ::= ['@' <tags> SPACE] [':' <source> SPACE] <command> <parameters> <crlf>
//! parameters ::= *( SPACE middle ) [ SPACE ":" trailing ]
//! ```
//!
//! One or more spaces separate parts (RFC 1459), and the trailing
//! parameter is an ordinary parameter once its colon is stripped.

use std::borrow::Cow;
use std::fmt;

/// The longest line, CRLF included, without tags (Modern IRC).
pub const MAX_LINE: usize = 512;
/// What tags may add to a received line, '@' and the space included
/// (IRCv3 message-tags "Size limit").
pub const MAX_TAGS: usize = 8191;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: u32,
    end: u32,
}

impl Span {
    fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }

    fn of<'a>(&self, text: &'a str) -> &'a str {
        &text[self.start as usize..self.end as usize]
    }
}

/// A parsed message. The line is kept whole; its parts are byte ranges.
#[derive(Clone, PartialEq, Eq)]
pub struct Message {
    text: Box<str>,
    /// Keys and escaped values; a later duplicate key replaces an earlier
    /// one.
    tags: Vec<(Span, Span)>,
    source: Option<Span>,
    command: Span,
    params: Vec<Span>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Nothing but spaces.
    Empty,
    /// Tags or a source with no command after them.
    NoCommand,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ParseError::Empty => "empty line",
            ParseError::NoCommand => "no command",
        })
    }
}

fn space_or_end(bytes: &[u8], from: usize) -> usize {
    bytes[from.min(bytes.len())..]
        .iter()
        .position(|&b| b == b' ')
        .map_or(bytes.len(), |offset| from + offset)
}

fn skip_spaces(bytes: &[u8], mut pos: usize) -> usize {
    while bytes.get(pos) == Some(&b' ') {
        pos += 1;
    }
    pos
}

impl Message {
    /// Parses one line, without its line ending.
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let bytes = line.as_bytes();
        let mut pos = 0;
        let mut tags = Vec::new();
        if bytes.first() == Some(&b'@') {
            let end = space_or_end(bytes, 1);
            let mut start = 1;
            while start < end {
                let stop = bytes[start..end]
                    .iter()
                    .position(|&b| b == b';')
                    .map_or(end, |offset| start + offset);
                if stop > start {
                    let equals = bytes[start..stop].iter().position(|&b| b == b'=');
                    let (key, value) = match equals {
                        Some(offset) => (
                            Span::new(start, start + offset),
                            Span::new(start + offset + 1, stop),
                        ),
                        None => (Span::new(start, stop), Span::new(stop, stop)),
                    };
                    // "SHOULD disregard all but the final occurrence."
                    let name = key.of(line);
                    tags.retain(|(k, _): &(Span, Span)| k.of(line) != name);
                    tags.push((key, value));
                }
                start = stop + 1;
            }
            pos = skip_spaces(bytes, end);
        }
        let mut source = None;
        if bytes.get(pos) == Some(&b':') {
            let end = space_or_end(bytes, pos + 1);
            source = Some(Span::new(pos + 1, end));
            pos = skip_spaces(bytes, end);
        }
        let end = space_or_end(bytes, pos);
        if end == pos {
            return Err(if bytes.iter().all(|&b| b == b' ') {
                ParseError::Empty
            } else {
                ParseError::NoCommand
            });
        }
        let command = Span::new(pos, end);
        let mut params = Vec::new();
        pos = end;
        loop {
            pos = skip_spaces(bytes, pos);
            if pos >= bytes.len() {
                break;
            }
            if bytes[pos] == b':' {
                params.push(Span::new(pos + 1, bytes.len()));
                break;
            }
            let end = space_or_end(bytes, pos);
            params.push(Span::new(pos, end));
            pos = end;
        }
        Ok(Self {
            text: line.into(),
            tags,
            source,
            command,
            params,
        })
    }

    /// The whole line as received.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Tags with unescaped values; a tag without a value has `""`.
    pub fn tags(&self) -> impl Iterator<Item = (&str, Cow<'_, str>)> + '_ {
        self.tags
            .iter()
            .map(|(key, value)| (key.of(&self.text), unescape(value.of(&self.text))))
    }

    /// A tag's unescaped value: `Some("")` when present without one.
    pub fn tag(&self, key: &str) -> Option<Cow<'_, str>> {
        self.tags
            .iter()
            .find(|(k, _)| k.of(&self.text) == key)
            .map(|(_, value)| unescape(value.of(&self.text)))
    }

    pub fn source(&self) -> Option<&str> {
        self.source.map(|span| span.of(&self.text))
    }

    /// The source's nickname (or server name).
    pub fn nick(&self) -> Option<&str> {
        self.source().map(|source| split_source(source).nick)
    }

    pub fn command(&self) -> &str {
        self.command.of(&self.text)
    }

    pub fn params(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.params.iter().map(|span| span.of(&self.text))
    }

    pub fn param(&self, index: usize) -> Option<&str> {
        self.params.get(index).map(|span| span.of(&self.text))
    }

    pub fn last_param(&self) -> Option<&str> {
        self.params.last().map(|span| span.of(&self.text))
    }
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Message").field(&self.text).finish()
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Unescapes a tag value (IRCv3 message-tags "Escaping values"). An
/// invalid escape drops its backslash; a lone trailing backslash is
/// dropped.
pub fn unescape(raw: &str) -> Cow<'_, str> {
    if !raw.contains('\\') {
        return Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    Cow::Owned(out)
}

fn escape_into(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            ';' => out.push_str("\\:"),
            ' ' => out.push_str("\\s"),
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
}

/// The parts of a message source: `nick[!user][@host]`. A server name comes
/// back as the nick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source<'a> {
    pub nick: &'a str,
    pub user: Option<&'a str>,
    pub host: Option<&'a str>,
}

pub fn split_source(source: &str) -> Source<'_> {
    let (rest, host) = match source.split_once('@') {
        Some((rest, host)) => (rest, Some(host)),
        None => (source, None),
    };
    let (nick, user) = match rest.split_once('!') {
        Some((nick, user)) => (nick, Some(user)),
        None => (rest, None),
    };
    Source { nick, user, host }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// The command is empty or not letters, digits and hyphens.
    Command,
    /// A part holds NUL, CR or LF, which no message can carry.
    ForbiddenCharacter,
    /// A parameter other than the last is empty, holds a space, or starts
    /// with a colon.
    Middle(usize),
    /// A tag key is empty or holds `=`, `;` or a space.
    TagKey,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::Command => f.write_str("invalid command"),
            EncodeError::ForbiddenCharacter => f.write_str("NUL, CR or LF in a message"),
            EncodeError::Middle(index) => {
                write!(f, "parameter {} cannot be sent before the last", index + 1)
            }
            EncodeError::TagKey => f.write_str("invalid tag key"),
        }
    }
}

fn forbidden(text: &str) -> bool {
    text.bytes().any(|b| matches!(b, b'\0' | b'\r' | b'\n'))
}

/// Builds a line, without its line ending. Only the last parameter may be
/// empty, hold spaces or start with a colon; it gets a colon only when it
/// needs one. (Clients send no source; it is here for completeness.)
pub fn serialize(
    tags: &[(&str, &str)],
    source: Option<&str>,
    command: &str,
    params: &[&str],
) -> Result<String, EncodeError> {
    if command.is_empty()
        || !command
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(EncodeError::Command);
    }
    let mut out = String::with_capacity(
        command.len() + params.iter().map(|p| p.len() + 2).sum::<usize>() + 16,
    );
    if !tags.is_empty() {
        out.push('@');
        for (index, (key, value)) in tags.iter().enumerate() {
            if key.is_empty()
                || key.bytes().any(|b| matches!(b, b'=' | b';' | b' '))
                || forbidden(key)
            {
                return Err(EncodeError::TagKey);
            }
            if value.contains('\0') {
                return Err(EncodeError::ForbiddenCharacter);
            }
            if index > 0 {
                out.push(';');
            }
            out.push_str(key);
            if !value.is_empty() {
                out.push('=');
                escape_into(value, &mut out);
            }
        }
        out.push(' ');
    }
    if let Some(source) = source {
        if source.is_empty() || source.contains(' ') || forbidden(source) {
            return Err(EncodeError::ForbiddenCharacter);
        }
        out.push(':');
        out.push_str(source);
        out.push(' ');
    }
    out.push_str(command);
    for (index, param) in params.iter().enumerate() {
        if forbidden(param) {
            return Err(EncodeError::ForbiddenCharacter);
        }
        let needs_colon = param.is_empty() || param.contains(' ') || param.starts_with(':');
        out.push(' ');
        if needs_colon {
            if index + 1 != params.len() {
                return Err(EncodeError::Middle(index));
            }
            out.push(':');
        }
        out.push_str(param);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irc::vectors::{JOIN, SPLIT, USERHOST};

    #[test]
    fn splits_every_parser_test_vector() {
        for case in SPLIT {
            let message =
                Message::parse(case.input).unwrap_or_else(|e| panic!("{e}: {}", case.input));
            assert_eq!(message.command(), case.verb, "{}", case.input);
            assert_eq!(message.source(), case.source, "{}", case.input);
            assert_eq!(
                message.params().collect::<Vec<_>>(),
                case.params,
                "{}",
                case.input
            );
            let mut tags: Vec<(&str, String)> =
                message.tags().map(|(k, v)| (k, v.into_owned())).collect();
            let mut expected: Vec<(&str, String)> =
                case.tags.iter().map(|&(k, v)| (k, v.to_string())).collect();
            tags.sort();
            expected.sort();
            assert_eq!(tags, expected, "{}", case.input);
        }
    }

    #[test]
    fn joins_every_parser_test_vector() {
        for case in JOIN {
            let line = serialize(case.tags, case.source, case.verb, case.params).unwrap();
            assert!(
                case.matches.contains(&line.as_str()),
                "{}: {line:?} not in {:?}",
                case.desc,
                case.matches
            );
            // And it parses back to the same parts.
            let message = Message::parse(&line).unwrap();
            assert_eq!(
                message.params().collect::<Vec<_>>(),
                case.params,
                "{}",
                case.desc
            );
        }
    }

    #[test]
    fn splits_every_source_vector() {
        for case in USERHOST {
            let source = split_source(case.source);
            assert_eq!(
                (source.nick, source.user, source.host),
                (case.nick, case.user, case.host),
                "{}",
                case.source
            );
        }
    }

    #[test]
    fn duplicate_tags_keep_the_final_occurrence_and_values_unescape() {
        let message = Message::parse("@a=1;b;a=2;c=x\\sy\\:z\\\\w\\q\\ :n!u@h PING :x").unwrap();
        assert_eq!(message.tag("a").as_deref(), Some("2"));
        assert_eq!(message.tag("b").as_deref(), Some(""));
        assert_eq!(message.tag("c").as_deref(), Some("x y;z\\wq"));
        assert_eq!(message.tag("d"), None);
        assert_eq!(message.tags().count(), 3);
        assert_eq!(message.nick(), Some("n"));
    }

    #[test]
    fn malformed_lines_are_errors_not_panics() {
        assert_eq!(Message::parse(""), Err(ParseError::Empty));
        assert_eq!(Message::parse("   "), Err(ParseError::Empty));
        assert_eq!(Message::parse("@a=b"), Err(ParseError::NoCommand));
        assert_eq!(Message::parse(":source"), Err(ParseError::NoCommand));
        assert_eq!(Message::parse("@a :src  "), Err(ParseError::NoCommand));
        let message = Message::parse("PING").unwrap();
        assert_eq!((message.command(), message.params().len()), ("PING", 0));
    }

    #[test]
    fn serializing_refuses_what_cannot_be_sent() {
        assert_eq!(
            serialize(&[], None, "PRIVMSG", &["#a b", "x"]),
            Err(EncodeError::Middle(0))
        );
        assert_eq!(
            serialize(&[], None, "PRIVMSG", &["#a", "x\r\nQUIT"]),
            Err(EncodeError::ForbiddenCharacter)
        );
        assert_eq!(serialize(&[], None, "", &[]), Err(EncodeError::Command));
        assert_eq!(
            serialize(&[("a b", "")], None, "TAGMSG", &["#a"]),
            Err(EncodeError::TagKey)
        );
        assert_eq!(
            serialize(&[("+typing", "active")], None, "TAGMSG", &["#a"]).unwrap(),
            "@+typing=active TAGMSG #a"
        );
        assert_eq!(
            serialize(&[], None, "PRIVMSG", &["#a", ":)"]).unwrap(),
            "PRIVMSG #a ::)"
        );
    }
}
