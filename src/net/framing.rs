//! Splitting the received byte stream into lines (Modern IRC "Message
//! Format"): CR LF ends a line, a lone LF is accepted as clients and servers
//! commonly do, empty lines are ignored, and a line longer than the protocol
//! allows is dropped whole rather than split into garbage.

use crate::irc::message::{MAX_LINE, MAX_TAGS};

/// The longest line accepted: 8191 bytes of tags plus a 512-byte message.
pub const MAX_LINE_BYTES: usize = MAX_TAGS + MAX_LINE;

#[derive(Debug, Default)]
pub struct LineReader {
    buffer: Vec<u8>,
    /// Bytes of `buffer` already handed out as lines.
    consumed: usize,
    /// Inside an overlong line: drop everything up to its end.
    discarding: bool,
    /// Overlong lines dropped since the last `take_dropped`.
    dropped: usize,
}

impl LineReader {
    pub fn push(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Calls `line` with each complete line, without its line ending.
    pub fn lines(&mut self, mut line: impl FnMut(&[u8])) {
        loop {
            let pending = &self.buffer[self.consumed..];
            let Some(newline) = pending.iter().position(|&b| b == b'\n') else {
                if pending.len() > MAX_LINE_BYTES {
                    if !self.discarding {
                        self.dropped += 1;
                    }
                    self.discarding = true;
                    self.consumed = self.buffer.len();
                }
                break;
            };
            let start = self.consumed;
            self.consumed += newline + 1;
            if std::mem::take(&mut self.discarding) {
                continue;
            }
            let mut end = start + newline;
            if end > start && self.buffer[end - 1] == b'\r' {
                end -= 1;
            }
            if end - start > MAX_LINE_BYTES {
                self.dropped += 1;
            } else if end > start {
                line(&self.buffer[start..end]);
            }
        }
        // Compact once most of the buffer has been consumed.
        if self.consumed > 0 && self.consumed * 2 >= self.buffer.len() {
            self.buffer.drain(..self.consumed);
            self.consumed = 0;
        }
    }

    /// How many overlong lines were dropped since the last call.
    pub fn take_dropped(&mut self) -> usize {
        std::mem::take(&mut self.dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(reader: &mut LineReader) -> Vec<String> {
        let mut out = Vec::new();
        reader.lines(|line| out.push(String::from_utf8_lossy(line).into_owned()));
        out
    }

    #[test]
    fn splits_on_crlf_and_lone_lf_across_reads() {
        let mut reader = LineReader::default();
        reader.push(b"PING :a\r\nPRIVMSG #x :hel");
        assert_eq!(collect(&mut reader), ["PING :a"]);
        reader.push(b"lo\r\n\r\n\nNOTICE * :b\nPART");
        assert_eq!(collect(&mut reader), ["PRIVMSG #x :hello", "NOTICE * :b"]);
        reader.push(b" #x\r\n");
        assert_eq!(collect(&mut reader), ["PART #x"]);
    }

    #[test]
    fn overlong_lines_are_dropped_whole() {
        let mut reader = LineReader::default();
        reader.push(&vec![b'a'; MAX_LINE_BYTES + 10]);
        assert!(collect(&mut reader).is_empty());
        reader.push(b"tail of the long line\r\nPING :ok\r\n");
        assert_eq!(collect(&mut reader), ["PING :ok"]);
        assert_eq!(reader.take_dropped(), 1);
        let mut line = vec![b'b'; MAX_LINE_BYTES + 1];
        line.extend_from_slice(b"\r\nPING :x\r\n");
        reader.push(&line);
        assert_eq!(collect(&mut reader), ["PING :x"]);
        assert_eq!(reader.take_dropped(), 1);
    }
}
