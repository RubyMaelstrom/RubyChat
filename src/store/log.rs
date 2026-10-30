//! Chat logs (DESIGN.md §11): a file per buffer per UTC day,
//! `$XDG_DATA_HOME/rubychat/logs/<network>/<buffer>/<YYYY-MM-DD>.log`, one
//! line per event, tab-separated: the UTC time, the kind with its flags,
//! the nick (or the event's prefix), the text with its formatting codes,
//! and the server's message ID when there was one (⇥ is a tab):
//!
//! ```text
//! 2026-10-30T12:02:17.123Z⇥msg,own⇥ruby⇥hello there⇥ABC123
//! ```
//!
//! Backslashes and tabs in fields are escaped (`\\`, `\t`), so the files
//! stay greppable and restore faithfully. Writes go through small buffers,
//! flushed a second after the first unwritten line and on exit; a few
//! files stay open.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::irc::time::{format_server_time, parse_server_time};
use crate::model::{Line, LineKind};

/// Files kept open at once.
const OPEN_FILES: usize = 16;
/// Day files a backlog read looks through, newest first, at most.
const DAYS_BACK: usize = 400;
/// Bytes read from a file's end at a time when reading it backwards.
const CHUNK: u64 = 64 * 1024;
/// The longest line read back; longer ones are skipped as damaged.
const MAX_LINE: usize = 64 * 1024;

/// A line to append to a buffer's log.
#[derive(Clone, Debug)]
pub struct LogLine {
    /// The network's name, as its directory is called.
    pub network: String,
    /// The buffer's casefolded name.
    pub buffer: String,
    pub line: Line,
}

/// Where a buffer's logs are.
pub fn directory(root: &Path, network: &str, buffer: &str) -> PathBuf {
    root.join(file_name(network)).join(file_name(buffer))
}

/// A name made safe as one path element on Linux and Windows: characters
/// that can't appear in file names, `%`, and a leading dot become `%XX`.
fn file_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (index, c) in name.chars().enumerate() {
        let unsafe_char = c.is_control()
            || matches!(
                c,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '%'
            )
            || (index == 0 && c == '.');
        if unsafe_char {
            let mut bytes = [0u8; 4];
            for byte in c.encode_utf8(&mut bytes).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

fn day_file(directory: &Path, unix_ms: i64) -> PathBuf {
    let day = crate::platform::time::utc(unix_ms);
    directory.join(format!(
        "{:04}-{:02}-{:02}.log",
        day.year, day.month, day.day
    ))
}

fn escape(field: &str) -> String {
    if !field.contains(['\\', '\t', '\n', '\r']) {
        return field.to_string();
    }
    let mut out = String::with_capacity(field.len() + 8);
    for c in field.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(field: &str) -> String {
    if !field.contains('\\') {
        return field.to_string();
    }
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// A line as its log keeps it.
pub fn encode(line: &Line) -> String {
    let kind = match line.kind {
        LineKind::Message => "msg",
        LineKind::Action => "action",
        LineKind::Notice => "notice",
        LineKind::Event => "event",
        LineKind::Error => "error",
        LineKind::Sent => "sent",
        LineKind::Status | LineKind::Server => "info",
    };
    let mut flags = String::from(kind);
    if line.own {
        flags.push_str(",own");
    }
    if line.highlight {
        flags.push_str(",hl");
    }
    let who = if line.nick.is_empty() {
        &line.prefix
    } else {
        &line.nick
    };
    let mut text = format!(
        "{}\t{flags}\t{}\t{}",
        format_server_time(line.time),
        escape(who),
        escape(&line.text)
    );
    if let Some(msgid) = &line.msgid {
        text.push('\t');
        text.push_str(&escape(msgid));
    }
    text
}

/// A log line read back, as history; `None` if it doesn't parse.
pub fn decode(text: &str) -> Option<Line> {
    let mut fields = text.split('\t');
    let time = parse_server_time(fields.next()?)?;
    let mut flags = fields.next()?.split(',');
    let kind = match flags.next()? {
        "msg" => LineKind::Message,
        "action" => LineKind::Action,
        "notice" => LineKind::Notice,
        "event" => LineKind::Event,
        "error" => LineKind::Error,
        "sent" => LineKind::Sent,
        "info" => LineKind::Status,
        _ => return None,
    };
    let who = unescape(fields.next()?);
    let body = unescape(fields.next()?);
    let named = matches!(
        kind,
        LineKind::Message | LineKind::Action | LineKind::Notice
    );
    let mut line = if named {
        Line::message(time, kind, who, body)
    } else {
        Line::new(time, kind, who, body)
    };
    for flag in flags {
        match flag {
            "own" => line.own = true,
            "hl" => line.highlight = true,
            _ => {}
        }
    }
    line.msgid = fields
        .next()
        .filter(|msgid| !msgid.is_empty())
        .map(|msgid| unescape(msgid).into_boxed_str());
    line.history = true;
    Some(line)
}

/// Appends lines to logs, keeping a few files open.
pub struct Writer {
    root: PathBuf,
    /// Open files, most recently used last.
    open: Vec<(PathBuf, BufWriter<File>)>,
    /// Lines written but not yet flushed.
    pub dirty: bool,
}

impl Writer {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            open: Vec::new(),
            dirty: false,
        }
    }

    pub fn append(&mut self, log: &LogLine) -> io::Result<()> {
        let directory = directory(&self.root, &log.network, &log.buffer);
        let path = day_file(&directory, log.line.time);
        let index = match self.open.iter().position(|(open, _)| *open == path) {
            Some(index) => index,
            None => {
                create_dir(&directory)?;
                let file = OpenOptions::new().create(true).append(true).open(&path)?;
                if self.open.len() == OPEN_FILES {
                    let (_, mut oldest) = self.open.remove(0);
                    oldest.flush()?;
                }
                self.open.push((path, BufWriter::new(file)));
                self.open.len() - 1
            }
        };
        let entry = self.open.remove(index);
        self.open.push(entry);
        let (_, writer) = self.open.last_mut().expect("just pushed");
        writer.write_all(encode(&log.line).as_bytes())?;
        writer.write_all(b"\n")?;
        self.dirty = true;
        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        for (_, writer) in &mut self.open {
            writer.flush()?;
        }
        self.dirty = false;
        Ok(())
    }
}

fn create_dir(directory: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)
}

/// Up to `limit` of a buffer's logged lines from before `before` (all when
/// `None`), oldest first, as history. Day files are read newest first and
/// each from its end, so a long log costs only what is returned.
pub fn backlog(
    root: &Path,
    network: &str,
    buffer: &str,
    before: Option<i64>,
    limit: usize,
) -> io::Result<Vec<Line>> {
    let directory = directory(root, network, buffer);
    let mut days: Vec<PathBuf> = match fs::read_dir(&directory) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "log"))
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    // `YYYY-MM-DD.log` sorts by date.
    days.sort();
    if let Some(before) = before {
        let last_day = day_file(&directory, before);
        days.retain(|day| *day <= last_day);
    }
    let mut found: Vec<Line> = Vec::new();
    for day in days.iter().rev().take(DAYS_BACK) {
        let wanted = limit - found.len();
        let mut lines = tail(day, before, wanted)?;
        lines.append(&mut found);
        found = lines;
        if found.len() >= limit {
            break;
        }
    }
    Ok(found)
}

/// Up to `limit` lines from the end of `path` that are before `before`,
/// oldest first.
fn tail(path: &Path, before: Option<i64>, limit: usize) -> io::Result<Vec<Line>> {
    let mut file = File::open(path)?;
    let mut end = file.seek(SeekFrom::End(0))?;
    let mut carry: Vec<u8> = Vec::new();
    let mut lines: Vec<Line> = Vec::new();
    while end > 0 && lines.len() < limit {
        let start = end.saturating_sub(CHUNK);
        file.seek(SeekFrom::Start(start))?;
        let mut chunk = vec![0u8; (end - start) as usize];
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&carry);
        end = start;
        // The first piece may be the end of a line that starts earlier.
        let mut pieces: Vec<&[u8]> = chunk.split(|&b| b == b'\n').collect();
        carry = if start > 0 {
            pieces.remove(0).to_vec()
        } else {
            Vec::new()
        };
        if carry.len() > MAX_LINE {
            carry.clear();
        }
        for piece in pieces.into_iter().rev() {
            let Some(line) = std::str::from_utf8(piece).ok().and_then(decode) else {
                continue;
            };
            if before.is_some_and(|before| line.time >= before) {
                continue;
            }
            lines.push(line);
            if lines.len() == limit {
                break;
            }
        }
    }
    lines.reverse();
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(time: i64, nick: &str, text: &str) -> Line {
        Line::message(time, LineKind::Message, nick, text)
    }

    #[test]
    fn lines_round_trip_with_flags_escapes_and_ids() {
        let mut line = message(1_793_000_000_123, "ruby", "a\ttab \\ and \x02bold\x02");
        line.own = true;
        line.highlight = true;
        line.msgid = Some("ABC\t1".into());
        let text = encode(&line);
        assert_eq!(
            text,
            "2026-10-26T07:33:20.123Z\tmsg,own,hl\truby\ta\\ttab \\\\ and \x02bold\x02\tABC\\t1"
        );
        let back = decode(&text).unwrap();
        assert_eq!(
            (back.text.as_str(), back.own, back.highlight, back.history),
            (line.text.as_str(), true, true, true)
        );
        assert_eq!(back.msgid.as_deref(), Some("ABC\t1"));
        let event = Line::new(5_000, LineKind::Event, "→", "alice joined");
        let back = decode(&encode(&event)).unwrap();
        assert_eq!((back.prefix.as_str(), back.kind), ("→", LineKind::Event));
        assert!(decode("garbage").is_none());
        assert_eq!(file_name("#a/b"), "#a%2Fb");
        assert_eq!(file_name(".hidden"), "%2Ehidden");
    }

    #[test]
    fn logs_append_by_day_and_read_back_from_the_end() {
        let root = std::env::temp_dir().join(format!("rubychat-logs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut writer = Writer::new(root.clone());
        const DAY: i64 = 86_400_000;
        let base = 20_000 * DAY;
        for i in 0..300 {
            // Two days, 150 lines each.
            let time = base + (i / 150) * DAY + i * 1000;
            writer
                .append(&LogLine {
                    network: String::from("Libera.Chat"),
                    buffer: String::from("#rust"),
                    line: message(time, "alice", &format!("line {i}")),
                })
                .unwrap();
        }
        writer.flush().unwrap();
        let days: Vec<_> = fs::read_dir(directory(&root, "Libera.Chat", "#rust"))
            .unwrap()
            .collect();
        assert_eq!(days.len(), 2);
        let last = backlog(&root, "Libera.Chat", "#rust", None, 10).unwrap();
        let texts: Vec<&str> = last.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts.first(), Some(&"line 290"));
        assert_eq!(texts.last(), Some(&"line 299"));
        // Before the 160th line: across the day boundary.
        let before = last[0].time - 130 * 1000;
        let earlier = backlog(&root, "Libera.Chat", "#rust", Some(before), 20).unwrap();
        let texts: Vec<&str> = earlier.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts.first(), Some(&"line 140"));
        assert_eq!(texts.last(), Some(&"line 159"));
        assert!(earlier.iter().all(|line| line.history));
        assert!(
            backlog(&root, "Libera.Chat", "#none", None, 10)
                .unwrap()
                .is_empty()
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
