//! Finding text as it shows, ignoring case: search through a buffer and the
//! channel list's filter (DESIGN.md §8.10).

use std::ops::Range;

/// What to look for, in lower case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    folded: String,
}

/// Lower case, a character at a time, as text is compared: the same
/// mapping for the query and the text searched, whatever their context.
fn fold(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

impl Query {
    /// `None` for nothing to look for.
    pub fn new(text: &str) -> Option<Self> {
        (!text.is_empty()).then(|| Self { folded: fold(text) })
    }

    /// Where the query occurs in `text`, in order and without overlapping.
    pub fn find(&self, text: &str) -> Vec<Range<usize>> {
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(range) = self.find_from(text, from) {
            from = range.end;
            found.push(range);
        }
        found
    }

    /// Whether `text` contains the query.
    pub fn is_in(&self, text: &str) -> bool {
        self.find_from(text, 0).is_some()
    }

    /// Whether text derived from `source` by removing characters (as
    /// formatting codes are) could contain the query: false only when it
    /// can't, so a line without it is passed over before the work of
    /// showing it.
    pub fn may_be_in(&self, source: &str) -> bool {
        if !self.folded.is_ascii() {
            return true;
        }
        let mut wanted = self.folded.bytes().peekable();
        for byte in source.bytes() {
            if wanted.peek() == Some(&byte.to_ascii_lowercase()) {
                wanted.next();
            }
        }
        wanted.peek().is_none()
    }

    /// The first occurrence starting at or after byte `from`, a character
    /// boundary.
    fn find_from(&self, text: &str, from: usize) -> Option<Range<usize>> {
        let needle = self.folded.as_bytes();
        if self.folded.is_ascii() {
            // ASCII bytes never occur inside another character, so every
            // match starts and ends on a boundary.
            let bytes = text.as_bytes();
            let last = bytes.len().checked_sub(needle.len())?;
            // Candidates start with the first byte in either case.
            let (lower, upper) = (needle[0], needle[0].to_ascii_uppercase());
            let mut start = from;
            while start <= last {
                let at = start
                    + bytes[start..=last]
                        .iter()
                        .position(|&byte| byte == lower || byte == upper)?;
                let rest = &bytes[at + 1..at + needle.len()];
                if rest
                    .iter()
                    .zip(&needle[1..])
                    .all(|(byte, wanted)| byte.to_ascii_lowercase() == *wanted)
                {
                    return Some(at..at + needle.len());
                }
                start = at + 1;
            }
            return None;
        }
        text.get(from..)?.char_indices().find_map(|(offset, _)| {
            let start = from + offset;
            self.match_at(text, start).map(|end| start..end)
        })
    }

    /// The end of an occurrence starting at `start`, comparing lower cases;
    /// a character whose lower case runs past the query's end isn't one.
    fn match_at(&self, text: &str, start: usize) -> Option<usize> {
        let mut wanted = self.folded.chars().peekable();
        for (offset, c) in text[start..].char_indices() {
            for lower in c.to_lowercase() {
                if wanted.next() != Some(lower) {
                    return None;
                }
            }
            if wanted.peek().is_none() {
                return Some(start + offset + c.len_utf8());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_occurrence_whatever_its_case() {
        let query = Query::new("Rust").unwrap();
        assert_eq!(query.find("rust is RUST, trust me"), [0..4, 8..12, 15..19]);
        assert!(query.is_in("Rustacean"));
        assert!(!query.is_in("rus"));
        assert_eq!(Query::new(""), None);
        // Non-overlapping, left to right.
        assert_eq!(Query::new("aa").unwrap().find("aaaa"), [0..2, 2..4]);
    }

    #[test]
    fn non_ascii_text_matches_on_character_boundaries() {
        let query = Query::new("ÉTÉ").unwrap();
        let text = "un été chaud, ÉTÉ";
        let found = query.find(text);
        assert_eq!(found.len(), 2);
        assert_eq!(&text[found[0].clone()], "été");
        assert_eq!(&text[found[1].clone()], "ÉTÉ");
        // An ASCII query inside text with other characters around it.
        let found = Query::new("chat").unwrap().find("ça · chat ✓ CHAT");
        assert_eq!(found.len(), 2);
        // A character whose lower case is longer than what's left of the
        // query doesn't match part-way.
        assert!(!Query::new("i").unwrap().is_in("\u{130}"));
        assert!(Query::new("i\u{307}").unwrap().is_in("\u{130}"));
    }

    #[test]
    fn text_that_cannot_contain_the_query_is_known_before_it_is_shown() {
        let query = Query::new("Bold").unwrap();
        // Formatting codes split the word; what shows still has it.
        assert!(query.may_be_in("so \u{2}bo\u{2}\u{3}04ld\u{3} it is"));
        assert!(!query.may_be_in("so dlob it is"));
        assert!(Query::new("été").unwrap().may_be_in("anything"));
    }
}
