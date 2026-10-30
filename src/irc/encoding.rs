//! Decoding received lines. Modern IRC ("Character Encodings") recommends
//! UTF-8 with a legacy fallback, the "hybrid" decoding. The fallback here
//! defaults to windows-1252, which differs from Latin-1 only in 0x80–0x9F,
//! where Latin-1 has unprintable C1 controls; the WHATWG Encoding Standard
//! maps the "latin1" label to windows-1252 for the same reason.
//!
//! Tag values must be UTF-8 (IRCv3 message-tags), and a value that isn't is
//! dropped rather than replaced, so decoding never invents tag data.

use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fallback {
    #[default]
    Windows1252,
    Latin1,
}

/// windows-1252's 0x80–0x9F (WHATWG index-windows-1252). The five
/// unassigned bytes map to their C1 code points, as the index does.
const WINDOWS_1252_HIGH: [char; 32] = [
    '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
];

fn decode_fallback(bytes: &[u8], fallback: Fallback, out: &mut String) {
    for &byte in bytes {
        out.push(match (fallback, byte) {
            (Fallback::Windows1252, 0x80..=0x9f) => WINDOWS_1252_HIGH[usize::from(byte - 0x80)],
            _ => char::from(byte),
        });
    }
}

/// Decodes a received line, without its line ending.
pub fn decode_line(bytes: &[u8], fallback: Fallback) -> Cow<'_, str> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 2);
    let mut rest = bytes;
    if bytes.first() == Some(&b'@') {
        let end = bytes.iter().position(|&b| b == b' ').unwrap_or(bytes.len());
        out.push('@');
        let mut first = true;
        for tag in bytes[1..end].split(|&b| b == b';') {
            let (key, value) = match tag.iter().position(|&b| b == b'=') {
                Some(at) => (&tag[..at], Some(&tag[at + 1..])),
                None => (tag, None),
            };
            let Ok(key) = std::str::from_utf8(key) else {
                continue;
            };
            if !first {
                out.push(';');
            }
            first = false;
            out.push_str(key);
            if let Some(Ok(value)) = value.map(std::str::from_utf8) {
                out.push('=');
                out.push_str(value);
            }
        }
        rest = &bytes[end..];
    }
    match std::str::from_utf8(rest) {
        Ok(text) => out.push_str(text),
        Err(_) => decode_fallback(rest, fallback, &mut out),
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_passes_through_and_legacy_text_falls_back() {
        assert!(matches!(
            decode_line("caf\u{e9} \u{1f496}".as_bytes(), Fallback::Windows1252),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            decode_line(b"PRIVMSG #a :caf\xe9", Fallback::Windows1252),
            "PRIVMSG #a :caf\u{e9}"
        );
        assert_eq!(
            decode_line(b"PRIVMSG #a :\x93quoted\x94 \x80", Fallback::Windows1252),
            "PRIVMSG #a :\u{201c}quoted\u{201d} \u{20ac}"
        );
        assert_eq!(
            decode_line(b"PRIVMSG #a :\x93", Fallback::Latin1),
            "PRIVMSG #a :\u{93}"
        );
    }

    #[test]
    fn tag_values_that_are_not_utf8_are_dropped() {
        assert_eq!(
            decode_line(
                b"@a=ok;b=\xff\xfe;c :n PRIVMSG #a :caf\xe9",
                Fallback::Windows1252
            ),
            "@a=ok;b;c :n PRIVMSG #a :caf\u{e9}"
        );
        // Tags that are fine keep the rest's own decoding.
        assert_eq!(
            decode_line(
                b"@a=\xff :n PRIVMSG #a :\xe2\x9c\x93",
                Fallback::Windows1252
            ),
            "@a :n PRIVMSG #a :\u{2713}"
        );
    }
}
