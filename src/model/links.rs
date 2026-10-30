//! Links in message text: URLs with the schemes RubyChat opens, and bare
//! `www.` hosts. Trailing punctuation and unbalanced closing brackets stay
//! outside, so "(see https://example.org/a_(b))." links what was meant.

use std::ops::Range;

/// Schemes RubyChat will hand to the system's opener; nothing else is a
/// link, so a message can't make a click open a local file.
pub const SCHEMES: &[&str] = &[
    "https://",
    "http://",
    "ircs://",
    "irc://",
    "gemini://",
    "gopher://",
    "ftp://",
    "mailto:",
];

/// Byte ranges of the links in `text`.
pub fn find_links(text: &str) -> Vec<Range<usize>> {
    let lower = text.to_ascii_lowercase();
    let mut links = Vec::new();
    let mut word_start = 0;
    for (index, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if !c.is_whitespace() {
            continue;
        }
        if word_start < index
            && let Some(link) = link_in(&lower[word_start..index], &text[word_start..index])
        {
            links.push(word_start + link.start..word_start + link.end);
        }
        word_start = index + c.len_utf8();
    }
    links
}

fn link_in(lower: &str, word: &str) -> Option<Range<usize>> {
    let start = (0..word.len())
        .filter(|&i| word.is_char_boundary(i))
        .find(|&i| {
            let boundary = i == 0
                || !word[..i]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_alphanumeric);
            boundary
                && (SCHEMES
                    .iter()
                    .any(|scheme| lower[i..].starts_with(scheme) && lower.len() > i + scheme.len())
                    || (lower[i..].starts_with("www.") && lower.len() > i + 4))
        })?;
    let mut end = word.len();
    loop {
        let tail = word[start..end].chars().next_back()?;
        let link = &word[start..end];
        let unbalanced = |open: char, close: char| {
            tail == close && link.matches(open).count() < link.matches(close).count()
        };
        if ".,;:!?'\"".contains(tail)
            || unbalanced('(', ')')
            || unbalanced('[', ']')
            || unbalanced('<', '>')
            || tail == '>' && !link.contains('<')
        {
            end -= tail.len_utf8();
        } else {
            break;
        }
    }
    let scheme_only =
        SCHEMES.iter().any(|scheme| lower[start..end] == **scheme) || &lower[start..end] == "www.";
    (end > start && !scheme_only).then_some(start..end)
}

/// Byte ranges of the channel names in `text`: words starting with one of
/// `chantypes`, without trailing punctuation. Names of digits alone, such
/// as issue numbers ("fixed in #1234"), are left out.
pub fn find_channels(text: &str, chantypes: &str) -> Vec<Range<usize>> {
    let mut channels = Vec::new();
    let mut start = 0;
    for (index, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if !c.is_whitespace() {
            continue;
        }
        let word = &text[start..index];
        // An opening bracket or quote may come before the name.
        let lead = word.len() - word.trim_start_matches(['(', '[', '"', '\'']).len();
        let name = word[lead..].trim_end_matches(|c: char| ".,;:!?'\")]".contains(c));
        let mut chars = name.chars();
        if chars.next().is_some_and(|first| chantypes.contains(first))
            && !chars.as_str().is_empty()
            && !chars.as_str().chars().all(|c| c.is_ascii_digit())
            && !name.contains(['\u{7}', ','])
        {
            channels.push(start + lead..start + lead + name.len());
        }
        start = index + c.len_utf8();
    }
    channels
}

/// The address to open for a link's text: `www.` hosts get `https://`.
pub fn target(link: &str) -> String {
    if link.to_ascii_lowercase().starts_with("www.") {
        format!("https://{link}")
    } else {
        link.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str) -> Vec<&str> {
        find_links(text)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn finds_urls_without_their_punctuation() {
        assert_eq!(
            links("see https://rust-lang.org."),
            ["https://rust-lang.org"]
        );
        assert_eq!(
            links("(docs: https://example.org/a_(b))."),
            ["https://example.org/a_(b)"]
        );
        assert_eq!(links("<https://example.org/x>"), ["https://example.org/x"]);
        assert_eq!(
            links("join ircs://irc.libera.chat/#rust or gemini://kennedy.gemi.dev!"),
            ["ircs://irc.libera.chat/#rust", "gemini://kennedy.gemi.dev"]
        );
        assert_eq!(
            links("www.example.org, mailto:ruby@example.org"),
            ["www.example.org", "mailto:ruby@example.org"]
        );
        assert_eq!(target("www.example.org"), "https://www.example.org");
    }

    #[test]
    fn finds_channel_names() {
        let text = "come to #rust, or (&local) and ##chat! not #1234 or a#b";
        let found: Vec<&str> = find_channels(text, "#&")
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(found, ["#rust", "&local", "##chat"]);
        assert!(find_channels("# alone", "#").is_empty());
    }

    #[test]
    fn ignores_what_is_not_a_link() {
        assert!(links("no links here, just text: http:// and www.").is_empty());
        assert!(links("file:///etc/passwd javascript:alert(1)").is_empty());
        assert!(
            links("nothttps://example.org").is_empty(),
            "a scheme must start a word"
        );
        assert_eq!(
            links("\"https://example.org/\" said"),
            ["https://example.org/"]
        );
    }
}
