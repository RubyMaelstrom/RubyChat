//! Paragraphs: styled spans set on one line, then broken into rows that fit
//! a width. Rows break after spaces and between CJK ideographs; a word wider
//! than a whole row breaks between its glyphs. Spaces at a row's end hang
//! past it rather than forcing a break.

use std::ops::Range;

use super::color::Color;
use super::text::{Glyph, TextStyle, TextSystem};

/// Text in one style and color.
#[derive(Clone, Copy, Debug)]
pub struct Span<'a> {
    pub text: &'a str,
    pub style: TextStyle,
    pub color: Color,
    pub decoration: Decoration,
}

impl<'a> Span<'a> {
    pub fn new(text: &'a str, style: TextStyle, color: Color) -> Self {
        Self {
            text,
            style,
            color,
            decoration: Decoration::default(),
        }
    }
}

/// What a span carries besides its glyphs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Decoration {
    pub underline: bool,
    pub strikethrough: bool,
    /// Slanted: italics.
    pub oblique: bool,
    pub background: Option<Color>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedGlyph {
    /// `x` is the glyph's position on the unbroken line, and `cluster` its
    /// byte offset in the spans' texts laid end to end.
    pub glyph: Glyph,
    pub size: f32,
    pub color: Color,
    pub decoration: Decoration,
    /// A row may end after this glyph.
    pub break_after: bool,
    pub space: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Paragraph {
    pub glyphs: Vec<PlacedGlyph>,
    /// Each row's glyphs, and the unbroken line's x where the row starts.
    pub rows: Vec<(Range<usize>, f32)>,
    pub ascent: f32,
    pub descent: f32,
    pub line_height: f32,
}

impl Paragraph {
    /// Rows, at least one even when empty.
    pub fn row_count(&self) -> usize {
        self.rows.len().max(1)
    }
}

/// Ideographs, kana and fullwidth forms, between which rows may break.
fn breaks_around(c: char) -> bool {
    matches!(c,
        '\u{2e80}'..='\u{303f}' | '\u{3040}'..='\u{30ff}' | '\u{3100}'..='\u{31ff}'
        | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'
        | '\u{ff00}'..='\u{ffef}' | '\u{20000}'..='\u{2fa1f}')
}

pub fn layout(text: &mut TextSystem, spans: &[Span<'_>], width: f32) -> Paragraph {
    let mut glyphs = Vec::new();
    let mut x = 0.0;
    let mut offset = 0;
    let (mut ascent, mut descent, mut line_height) = (0.0f32, 0.0f32, 0.0f32);
    for span in spans {
        let metrics = text.metrics(&span.style);
        ascent = ascent.max(metrics.ascent);
        descent = descent.max(metrics.descent);
        line_height = line_height.max(metrics.line_height);
        if span.text.is_empty() {
            continue;
        }
        let shaped = text.shape(span.text, &span.style);
        for glyph in &shaped.glyphs {
            let c = span.text[glyph.cluster as usize..]
                .chars()
                .next()
                .unwrap_or(' ');
            let space = c == ' ' || c == '\u{3000}';
            glyphs.push(PlacedGlyph {
                glyph: Glyph {
                    x: x + glyph.x,
                    cluster: offset + glyph.cluster,
                    ..*glyph
                },
                size: span.style.size,
                color: span.color,
                decoration: span.decoration,
                break_after: space || breaks_around(c),
                space,
            });
        }
        // A row may also break before an ideograph.
        if let Some(first) = span.text.chars().next()
            && breaks_around(first)
            && let Some(previous) = glyphs.len().checked_sub(shaped.glyphs.len() + 1)
        {
            glyphs[previous].break_after = true;
        }
        x += shaped.advance;
        offset += span.text.len() as u32;
    }
    let rows = wrap(&glyphs, width);
    Paragraph {
        glyphs,
        rows,
        ascent,
        descent,
        line_height,
    }
}

fn wrap(glyphs: &[PlacedGlyph], width: f32) -> Vec<(Range<usize>, f32)> {
    let mut rows = Vec::new();
    let mut start = 0;
    while start < glyphs.len() {
        let origin = glyphs[start].glyph.x;
        let mut last_break = None;
        let mut index = start;
        while index < glyphs.len() {
            let glyph = &glyphs[index];
            let right = glyph.glyph.x + glyph.glyph.advance - origin;
            if right > width + 0.01 && !glyph.space && index > start {
                break;
            }
            if glyph.break_after {
                last_break = Some(index);
            }
            index += 1;
        }
        if index == glyphs.len() {
            rows.push((start..index, origin));
            break;
        }
        let mut end = match last_break {
            Some(at) => at + 1,
            None => index,
        };
        // Never split one cluster's glyphs between rows.
        while end > start + 1 && glyphs[end].glyph.cluster == glyphs[end - 1].glyph.cluster {
            end -= 1;
        }
        rows.push((start..end, origin));
        start = end;
        while start < glyphs.len() && glyphs[start].space {
            start += 1;
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Weight;

    fn rows(paragraph: &Paragraph, text: &str) -> Vec<String> {
        // Monospaced ASCII here: glyph i is byte i of `text`.
        paragraph
            .rows
            .iter()
            .map(|(range, _)| {
                let first = paragraph.glyphs[range.start].glyph.cluster as usize;
                let last = paragraph.glyphs[range.end - 1].glyph.cluster as usize;
                text[first..=last].to_string()
            })
            .collect()
    }

    fn span(text: &str) -> Span<'_> {
        Span::new(
            text,
            TextStyle::new(10.0, Weight::Regular).without_ligatures(),
            Color::rgb(255, 255, 255),
        )
    }

    #[test]
    fn rows_break_after_spaces_and_split_only_overlong_words() {
        let mut text = TextSystem::new();
        let advance = text.width("x", &span("x").style);
        let sample = "the quick brown fox jumps";
        // Ten characters fit a row.
        let paragraph = layout(&mut text, &[span(sample)], advance * 10.0);
        assert_eq!(
            rows(&paragraph, sample),
            ["the quick ", "brown fox ", "jumps"]
        );
        let long = "abcdefghijklmnopqrstuvwxyz";
        let paragraph = layout(&mut text, &[span(long)], advance * 10.0);
        assert_eq!(
            rows(&paragraph, long),
            ["abcdefghij", "klmnopqrst", "uvwxyz"]
        );
        // Each row's x is where it starts on the unbroken line.
        assert!((paragraph.rows[1].1 - advance * 10.0).abs() < 1e-3);
        let narrow = layout(&mut text, &[span("ab")], 0.0);
        assert_eq!(narrow.rows.len(), 2, "at least one glyph per row");
        let empty = layout(&mut text, &[span("")], 100.0);
        assert_eq!((empty.rows.len(), empty.row_count()), (0, 1));
    }

    #[test]
    fn spans_join_into_one_line_with_their_colors() {
        let mut text = TextSystem::new();
        let red = Span {
            color: Color::rgb(255, 0, 0),
            ..span("<ruby> ")
        };
        let paragraph = layout(&mut text, &[red, span("hello there")], 1000.0);
        assert_eq!(paragraph.rows.len(), 1);
        assert_eq!(paragraph.glyphs[0].color, Color::rgb(255, 0, 0));
        assert_eq!(
            paragraph.glyphs.last().unwrap().color,
            Color::rgb(255, 255, 255)
        );
        let first_of_second = paragraph.glyphs[7].glyph.x;
        assert!((first_of_second - text.width("<ruby> ", &span("").style)).abs() < 1e-3);
        assert_eq!(
            paragraph.glyphs[8].glyph.cluster, 8,
            "clusters count from the first span"
        );
    }
}
