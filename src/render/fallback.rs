//! Font fallback (DESIGN.md §9.5): which installed face draws a character
//! the chat face lacks. The font worker builds the index of installed
//! faces; the text system asks it a character at a time and remembers the
//! answers.

use std::path::PathBuf;
use std::sync::Arc;

/// The characters a face maps, as sorted, disjoint, inclusive ranges.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Coverage {
    ranges: Vec<(u32, u32)>,
}

impl Coverage {
    /// Coverage of `codepoints`, in any order.
    pub fn from_codepoints(codepoints: impl IntoIterator<Item = u32>) -> Self {
        let mut codepoints: Vec<u32> = codepoints.into_iter().collect();
        codepoints.sort_unstable();
        codepoints.dedup();
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for codepoint in codepoints {
            match ranges.last_mut() {
                Some((_, end)) if *end + 1 == codepoint => *end = codepoint,
                _ => ranges.push((codepoint, codepoint)),
            }
        }
        Self { ranges }
    }

    /// Coverage from ranges as `ranges` gives them; `None` unless sorted,
    /// disjoint and each in order.
    pub fn from_ranges(ranges: Vec<(u32, u32)>) -> Option<Self> {
        let ordered = ranges.iter().all(|(start, end)| start <= end)
            && ranges.windows(2).all(|pair| pair[0].1 < pair[1].0);
        ordered.then_some(Self { ranges })
    }

    pub fn ranges(&self) -> &[(u32, u32)] {
        &self.ranges
    }

    pub fn contains(&self, c: char) -> bool {
        let c = u32::from(c);
        self.ranges
            .binary_search_by(|&(start, end)| {
                if end < c {
                    std::cmp::Ordering::Less
                } else if start > c {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }
}

/// A face that can be loaded: a file, and its place in a collection.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceKey {
    pub path: PathBuf,
    pub index: u32,
}

/// An installed face, as the index knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceInfo {
    pub key: FaceKey,
    pub family: String,
    /// 100 to 900, 400 regular and 700 bold.
    pub weight: u16,
    pub italic: bool,
    pub monospace: bool,
    /// Draws from color bitmaps (CBDT), as emoji fonts do.
    pub color: bool,
    pub coverage: Arc<Coverage>,
}

/// The installed faces, and the families tried first.
#[derive(Clone, Debug, Default)]
pub struct FontIndex {
    pub faces: Vec<FaceInfo>,
    /// Families tried before others, in order, compared without case.
    pub preferred: Vec<String>,
}

impl FontIndex {
    /// The face to draw `c`: among those that have it, an emoji in color
    /// (and other text not), then the preferred families in order, upright
    /// faces, and the weight nearest the text's.
    pub fn choose(&self, c: char, bold: bool, emoji: bool) -> Option<&FaceInfo> {
        let wanted = if bold { 700 } else { 400 };
        self.faces
            .iter()
            .filter(|face| face.coverage.contains(c))
            .min_by_key(|face| {
                let preference = self
                    .preferred
                    .iter()
                    .position(|family| family.eq_ignore_ascii_case(&face.family))
                    .unwrap_or(self.preferred.len());
                (
                    face.color != emoji,
                    preference,
                    face.italic,
                    face.weight.abs_diff(wanted),
                )
            })
    }
}

/// Families tried first: the Nerd Font build of the chat face for its
/// icons, color emoji, the CJK face for the locale's region, then common
/// broad faces.
pub fn preferred_families(language: &str) -> Vec<String> {
    let cjk = match language.split(['.', '@']).next().unwrap_or("") {
        lang if lang.starts_with("ja") => "Noto Sans CJK JP",
        lang if lang.starts_with("ko") => "Noto Sans CJK KR",
        "zh_TW" => "Noto Sans CJK TC",
        "zh_HK" | "zh_MO" => "Noto Sans CJK HK",
        _ => "Noto Sans CJK SC",
    };
    [
        "JetBrainsMono Nerd Font",
        "Noto Color Emoji",
        cjk,
        "DejaVu Sans Mono",
        "DejaVu Sans",
        "Noto Sans",
        "Noto Sans Symbols",
        "Noto Sans Symbols 2",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Whether a grapheme cluster asks for an emoji's color presentation: a
/// variation selector 16, a zero-width joiner or keycap, a skin tone, a
/// flag's regional indicators, or a character from the emoji blocks, and
/// no text presentation selector (Unicode emoji presentation sequences).
pub fn emoji_cluster(cluster: &str) -> bool {
    let mut emoji = false;
    for c in cluster.chars() {
        match c {
            '\u{fe0e}' => return false,
            '\u{fe0f}' | '\u{200d}' | '\u{20e3}' | '\u{1f3fb}'..='\u{1f3ff}' => emoji = true,
            '\u{1f1e6}'..='\u{1f1ff}' | '\u{1f300}'..='\u{1faff}' => emoji = true,
            _ => {}
        }
    }
    emoji
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(family: &str, weight: u16, color: bool, codepoints: &[u32]) -> FaceInfo {
        FaceInfo {
            key: FaceKey {
                path: PathBuf::from(format!("/fonts/{family}-{weight}.ttf")),
                index: 0,
            },
            family: family.to_string(),
            weight,
            italic: false,
            monospace: false,
            color,
            coverage: Arc::new(Coverage::from_codepoints(codepoints.iter().copied())),
        }
    }

    #[test]
    fn coverage_is_kept_as_ranges() {
        let coverage = Coverage::from_codepoints([0x42, 0x41, 0x43, 0x100, 0x41]);
        assert_eq!(coverage.ranges(), [(0x41, 0x43), (0x100, 0x100)]);
        assert!(coverage.contains('B') && coverage.contains('\u{100}'));
        assert!(!coverage.contains('D') && !coverage.contains('@'));
        assert!(Coverage::from_ranges(vec![(5, 1)]).is_none());
        assert!(Coverage::from_ranges(vec![(1, 5), (5, 9)]).is_none());
        assert!(Coverage::from_ranges(vec![(1, 5), (7, 9)]).is_some());
    }

    #[test]
    fn the_preferred_face_with_the_character_draws_it() {
        let index = FontIndex {
            faces: vec![
                face("Anything Sans", 400, false, &[0x4e00, 0x2764]),
                face("Noto Sans CJK SC", 700, false, &[0x4e00]),
                face("Noto Sans CJK SC", 400, false, &[0x4e00]),
                face("Noto Color Emoji", 400, true, &[0x2764, 0x1f600]),
            ],
            preferred: preferred_families("en_GB.UTF-8"),
        };
        let family = |c, bold, emoji| {
            index
                .choose(c, bold, emoji)
                .map(|f| (f.family.as_str(), f.weight))
        };
        assert_eq!(
            family('\u{4e00}', false, false),
            Some(("Noto Sans CJK SC", 400))
        );
        assert_eq!(
            family('\u{4e00}', true, false),
            Some(("Noto Sans CJK SC", 700))
        );
        // A heart is text unless it asks to be an emoji.
        assert_eq!(
            family('\u{2764}', false, false),
            Some(("Anything Sans", 400))
        );
        assert_eq!(
            family('\u{2764}', false, true),
            Some(("Noto Color Emoji", 400))
        );
        assert_eq!(
            family('\u{1f600}', false, false),
            Some(("Noto Color Emoji", 400))
        );
        assert_eq!(family('\u{10ffff}', false, false), None);
        assert_eq!(preferred_families("ja_JP.UTF-8")[2], "Noto Sans CJK JP");
    }

    #[test]
    fn emoji_clusters_are_recognized() {
        assert!(emoji_cluster("\u{1f600}"));
        assert!(emoji_cluster("\u{2764}\u{fe0f}"));
        assert!(!emoji_cluster("\u{2764}"));
        assert!(!emoji_cluster("\u{1f600}\u{fe0e}"));
        assert!(emoji_cluster("1\u{fe0f}\u{20e3}"));
        assert!(emoji_cluster("\u{1f1ec}\u{1f1e7}"));
        assert!(!emoji_cluster("a"));
    }
}
