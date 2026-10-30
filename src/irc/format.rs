//! mIRC-style formatting (Modern IRC "IRC Formatting"): toggles for bold
//! (0x02), italics (0x1D), underline (0x1F), strikethrough (0x1E),
//! monospace (0x11) and reverse (0x16); reset (0x0F); palette colors (0x03)
//! and hex colors (0x04). Parsing yields the plain text and the styled
//! ranges over it.

use std::borrow::Cow;
use std::ops::Range;

const BOLD: char = '\u{2}';
const COLOR: char = '\u{3}';
const HEX_COLOR: char = '\u{4}';
const RESET: char = '\u{f}';
const MONOSPACE: char = '\u{11}';
const REVERSE: char = '\u{16}';
const ITALICS: char = '\u{1d}';
const STRIKETHROUGH: char = '\u{1e}';
const UNDERLINE: char = '\u{1f}';

/// Colors 00–98 as the specification lists them: 0–15 from its stylesheet,
/// 16–98 from its "Colors 16-98" table. 99 means the default color.
pub const PALETTE: [u32; 99] = [
    0xffffff, 0x000000, 0x00007f, 0x009300, 0xff0000, 0x7f0000, 0x9c009c, 0xfc7f00, 0xffff00,
    0x00fc00, 0x009393, 0x00ffff, 0x0000fc, 0xff00ff, 0x7f7f7f, 0xd2d2d2, 0x470000, 0x472100,
    0x474700, 0x324700, 0x004700, 0x00472c, 0x004747, 0x002747, 0x000047, 0x2e0047, 0x470047,
    0x47002a, 0x740000, 0x743a00, 0x747400, 0x517400, 0x007400, 0x007449, 0x007474, 0x004074,
    0x000074, 0x4b0074, 0x740074, 0x740045, 0xb50000, 0xb56300, 0xb5b500, 0x7db500, 0x00b500,
    0x00b571, 0x00b5b5, 0x0063b5, 0x0000b5, 0x7500b5, 0xb500b5, 0xb5006b, 0xff0000, 0xff8c00,
    0xffff00, 0xb2ff00, 0x00ff00, 0x00ffa0, 0x00ffff, 0x008cff, 0x0000ff, 0xa500ff, 0xff00ff,
    0xff0098, 0xff5959, 0xffb459, 0xffff71, 0xcfff60, 0x6fff6f, 0x65ffc9, 0x6dffff, 0x59b4ff,
    0x5959ff, 0xc459ff, 0xff66ff, 0xff59bc, 0xff9c9c, 0xffd39c, 0xffff9c, 0xe2ff9c, 0x9cff9c,
    0x9cffdb, 0x9cffff, 0x9cd3ff, 0x9c9cff, 0xdc9cff, 0xff9cff, 0xff94d3, 0x000000, 0x131313,
    0x282828, 0x363636, 0x4d4d4d, 0x656565, 0x818181, 0x9f9f9f, 0xbcbcbc, 0xe2e2e2, 0xffffff,
];

/// A color a message asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrcColor {
    /// 00–98.
    Palette(u8),
    /// `0xRRGGBB`, from the hex color code.
    Rgb(u32),
}

impl IrcColor {
    pub fn rgb(self) -> u32 {
        match self {
            IrcColor::Palette(index) => PALETTE[usize::from(index).min(98)],
            IrcColor::Rgb(rgb) => rgb,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub monospace: bool,
    pub reverse: bool,
    pub foreground: Option<IrcColor>,
    pub background: Option<IrcColor>,
}

/// Text with its formatting codes removed, and the styles over it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Formatted {
    pub text: String,
    /// Styled byte ranges of `text`, in order; unstyled text has none.
    pub spans: Vec<(Range<usize>, Style)>,
}

fn is_code(c: char) -> bool {
    matches!(
        c,
        BOLD | COLOR
            | HEX_COLOR
            | RESET
            | MONOSPACE
            | REVERSE
            | ITALICS
            | STRIKETHROUGH
            | UNDERLINE
    )
}

/// The text without formatting codes.
pub fn strip(text: &str) -> Cow<'_, str> {
    if text.chars().any(is_code) {
        Cow::Owned(parse(text).text)
    } else {
        Cow::Borrowed(text)
    }
}

/// Reads up to `max` characters satisfying `accept` at the start of `rest`.
fn digits(rest: &str, max: usize, accept: impl Fn(char) -> bool) -> &str {
    let end = rest
        .char_indices()
        .take(max)
        .take_while(|&(_, c)| accept(c))
        .last()
        .map_or(0, |(index, c)| index + c.len_utf8());
    &rest[..end]
}

pub fn parse(source: &str) -> Formatted {
    let mut out = Formatted {
        text: String::with_capacity(source.len()),
        spans: Vec::new(),
    };
    let mut style = Style::default();
    let mut span_start = 0;
    let close = |out: &mut Formatted, style: Style, start: usize| {
        let end = out.text.len();
        if end > start && style != Style::default() {
            match out.spans.last_mut() {
                Some((range, last)) if range.end == start && *last == style => range.end = end,
                _ => out.spans.push((start..end, style)),
            }
        }
    };
    let mut rest = source;
    while let Some(c) = rest.chars().next() {
        rest = &rest[c.len_utf8()..];
        if !is_code(c) {
            out.text.push(c);
            continue;
        }
        close(&mut out, style, span_start);
        match c {
            BOLD => style.bold = !style.bold,
            ITALICS => style.italic = !style.italic,
            UNDERLINE => style.underline = !style.underline,
            STRIKETHROUGH => style.strikethrough = !style.strikethrough,
            MONOSPACE => style.monospace = !style.monospace,
            REVERSE => style.reverse = !style.reverse,
            RESET => style = Style::default(),
            COLOR | HEX_COLOR => {
                let (max, accept): (usize, fn(char) -> bool) = if c == COLOR {
                    (2, |c| c.is_ascii_digit())
                } else {
                    (6, |c| c.is_ascii_hexdigit())
                };
                let read = |text: &str| -> Option<IrcColor> {
                    if c == COLOR {
                        let value: u8 = text.parse().ok()?;
                        // 99 is the default color.
                        (value < 99).then_some(IrcColor::Palette(value))
                    } else {
                        u32::from_str_radix(text, 16).ok().map(IrcColor::Rgb)
                    }
                };
                let foreground = digits(rest, max, accept);
                let complete = |text: &str| {
                    if c == COLOR {
                        !text.is_empty()
                    } else {
                        text.len() == 6
                    }
                };
                if !complete(foreground) {
                    // A bare code resets both colors.
                    style.foreground = None;
                    style.background = None;
                } else {
                    rest = &rest[foreground.len()..];
                    style.foreground = read(foreground);
                    if let Some(after) = rest.strip_prefix(',') {
                        let background = digits(after, max, accept);
                        // A comma without a color is text.
                        if complete(background) {
                            rest = &after[background.len()..];
                            style.background = read(background);
                        }
                    }
                }
            }
            _ => unreachable!(),
        }
        span_start = out.text.len();
    }
    close(&mut out, style, span_start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(formatted: &Formatted) -> Vec<(&str, Style)> {
        formatted
            .spans
            .iter()
            .map(|(range, style)| (&formatted.text[range.clone()], *style))
            .collect()
    }

    #[test]
    fn toggles_and_reset() {
        let parsed = parse("plain \u{2}bold\u{1d} both\u{2} italic\u{f} plain");
        assert_eq!(parsed.text, "plain bold both italic plain");
        let bold = Style {
            bold: true,
            ..Default::default()
        };
        let both = Style {
            bold: true,
            italic: true,
            ..Default::default()
        };
        let italic = Style {
            italic: true,
            ..Default::default()
        };
        assert_eq!(
            styled(&parsed),
            [("bold", bold), (" both", both), (" italic", italic)]
        );
        assert_eq!(strip("no codes"), "no codes");
        assert!(matches!(strip("no codes"), Cow::Borrowed(_)));
    }

    #[test]
    fn color_code_forms_from_the_specification() {
        let fg = |n| Style {
            foreground: Some(IrcColor::Palette(n)),
            ..Default::default()
        };
        // <CODE><COLOR>: one or two digits, never more.
        let parsed = parse("\u{3}4red\u{3}12345");
        assert_eq!(parsed.text, "red345");
        assert_eq!(styled(&parsed), [("red", fg(4)), ("345", fg(12))]);
        // <CODE><COLOR>,<COLOR> sets both; <CODE><COLOR>, keeps the comma as text.
        let parsed = parse("\u{3}03,13Test!\u{3}5,text");
        assert_eq!(parsed.text, "Test!,text");
        assert_eq!(
            styled(&parsed)[0],
            (
                "Test!",
                Style {
                    foreground: Some(IrcColor::Palette(3)),
                    background: Some(IrcColor::Palette(13)),
                    ..Default::default()
                }
            )
        );
        assert_eq!(styled(&parsed)[1].0, ",text");
        // <CODE> alone and <CODE>, reset colors; the comma is text.
        let parsed = parse("\u{3}4x\u{3}y\u{3}4z\u{3},w");
        assert_eq!(parsed.text, "xyz,w");
        assert_eq!(styled(&parsed), [("x", fg(4)), ("z", fg(4))]);
        // 99 is the default color.
        assert!(parse("\u{3}99text").spans.is_empty());
        // Hex colors take exactly six digits.
        let parsed = parse("\u{4}FF0000,00ff00rg\u{4}12x");
        assert_eq!(parsed.text, "rg12x");
        assert_eq!(
            styled(&parsed)[0].1,
            Style {
                foreground: Some(IrcColor::Rgb(0xff0000)),
                background: Some(IrcColor::Rgb(0x00ff00)),
                ..Default::default()
            }
        );
        assert_eq!(IrcColor::Palette(52).rgb(), 0xff0000);
        assert_eq!(PALETTE[98], 0xffffff);
    }
}
