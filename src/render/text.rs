//! Text: shaping with harfrust (and an ASCII fast path), line metrics, and
//! single-line labels fitted with an ellipsis.

use std::collections::HashMap;
use std::rc::Rc;

use harfrust::{Feature, ShapeOptions, Tag, UnicodeBuffer};

use super::font::{
    ASCII_PRINTABLE, Face, FaceId, JETBRAINS_MONO_BOLD, JETBRAINS_MONO_REGULAR, Weight,
};
use super::glyph_cache::{GlyphCache, GlyphKey};
use super::raster::Mask;

const ELLIPSIS: &str = "\u{2026}";

/// Fitted labels are remembered: chrome repaints the same titles every frame.
const FIT_CACHE_ENTRIES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    /// Font size in logical pixels.
    pub size: f32,
    pub weight: Weight,
    /// Contextual alternates and ligatures. Chat text turns them off, so
    /// `->` stays two characters and ASCII takes the fast path.
    pub ligatures: bool,
}

impl TextStyle {
    pub const fn new(size: f32, weight: Weight) -> Self {
        Self {
            size,
            weight,
            ligatures: true,
        }
    }

    pub const fn without_ligatures(self) -> Self {
        Self {
            ligatures: false,
            ..self
        }
    }
}

/// A positioned glyph. Positions are logical pixels from the run's origin
/// on the baseline; `y` grows downward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub face: FaceId,
    pub id: u16,
    pub x: f32,
    pub y: f32,
    pub advance: f32,
    /// Byte offset of the cluster this glyph belongs to.
    pub cluster: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapedText {
    pub text: String,
    pub glyphs: Vec<Glyph>,
    /// Font size in logical pixels.
    pub size: f32,
    pub advance: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_height: f32,
}

impl ShapedText {
    /// The baseline of a line box whose top is `top`.
    pub fn baseline(&self, top: f32) -> f32 {
        top + (self.line_height - self.ascent - self.descent) / 2.0 + self.ascent
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub line_height: f32,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct FitKey {
    text: String,
    size: u32,
    weight: Weight,
    ligatures: bool,
    width: u32,
}

pub struct TextSystem {
    faces: Vec<Face>,
    regular: FaceId,
    bold: FaceId,
    glyphs: GlyphCache,
    fitted: HashMap<FitKey, Rc<ShapedText>>,
    buffer: Option<UnicodeBuffer>,
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl TextSystem {
    pub fn new() -> Self {
        let faces = vec![
            Face::new(JETBRAINS_MONO_REGULAR).expect("embedded JetBrains Mono Regular parses"),
            Face::new(JETBRAINS_MONO_BOLD).expect("embedded JetBrains Mono Bold parses"),
        ];
        Self {
            faces,
            regular: FaceId(0),
            bold: FaceId(1),
            glyphs: GlyphCache::default(),
            fitted: HashMap::new(),
            buffer: None,
        }
    }

    pub fn glyph_cache(&self) -> &GlyphCache {
        &self.glyphs
    }

    /// A rasterized glyph (see `GlyphCache::glyph`).
    pub fn glyph(&mut self, key: GlyphKey) -> Option<&Mask> {
        self.glyphs.glyph(&self.faces, key)
    }

    fn face_id(&self, style: &TextStyle) -> FaceId {
        match style.weight {
            Weight::Regular => self.regular,
            Weight::Bold => self.bold,
        }
    }

    pub fn metrics(&self, style: &TextStyle) -> LineMetrics {
        let face = &self.faces[usize::from(self.face_id(style).0)];
        let scale = style.size / face.units_per_em;
        let ascent = face.ascent * scale;
        let descent = face.descent * scale;
        LineMetrics {
            ascent,
            descent,
            line_height: ascent + descent + face.line_gap * scale,
        }
    }

    pub fn shape(&mut self, text: &str, style: &TextStyle) -> ShapedText {
        let face_id = self.face_id(style);
        let metrics = self.metrics(style);
        let face = &self.faces[usize::from(face_id.0)];
        let scale = style.size / face.units_per_em;
        let mut glyphs = Vec::with_capacity(text.len());
        let mut pen = 0.0;
        if !style.ligatures && face.monospace && text.bytes().all(|b| ASCII_PRINTABLE.contains(&b))
        {
            // A monospaced face without ligatures maps printable ASCII one
            // glyph per byte with its default advance: no shaping needed.
            for (index, byte) in text.bytes().enumerate() {
                let (id, advance) = face.ascii(byte);
                let advance = advance * scale;
                glyphs.push(Glyph {
                    face: face_id,
                    id,
                    x: pen,
                    y: 0.0,
                    advance,
                    cluster: index as u32,
                });
                pen += advance;
            }
        } else {
            let mut buffer = self.buffer.take().unwrap_or_default();
            buffer.push_str(text);
            buffer.guess_segment_properties();
            let off = [
                Feature::new(Tag::new(b"calt"), 0, ..),
                Feature::new(Tag::new(b"clig"), 0, ..),
                Feature::new(Tag::new(b"liga"), 0, ..),
            ];
            let features: &[Feature] = if style.ligatures { &[] } else { &off };
            let output = face
                .shaper
                .shape(buffer, ShapeOptions::new().features(features));
            for (info, position) in output.glyph_infos().iter().zip(output.glyph_positions()) {
                let advance = position.x_advance as f32 * scale;
                glyphs.push(Glyph {
                    face: face_id,
                    id: info.glyph_id as u16,
                    x: pen + position.x_offset as f32 * scale,
                    y: -(position.y_offset as f32) * scale,
                    advance,
                    cluster: info.cluster,
                });
                pen += advance;
            }
            self.buffer = Some(output.clear());
        }
        ShapedText {
            text: text.to_owned(),
            glyphs,
            size: style.size,
            advance: pen,
            ascent: metrics.ascent,
            descent: metrics.descent,
            line_height: metrics.line_height,
        }
    }

    pub fn width(&mut self, text: &str, style: &TextStyle) -> f32 {
        self.shape(text, style).advance
    }

    /// `text` on one line, ending in an ellipsis after the longest prefix of
    /// whole clusters (in logical order) that fits `width` beside it.
    /// Control characters become spaces: a line break would start another
    /// line.
    pub fn fit(&mut self, text: &str, style: &TextStyle, width: f32) -> Rc<ShapedText> {
        let key = FitKey {
            text: text.to_owned(),
            size: style.size.to_bits(),
            weight: style.weight,
            ligatures: style.ligatures,
            width: width.to_bits(),
        };
        if let Some(fitted) = self.fitted.get(&key) {
            return fitted.clone();
        }
        let line: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let full = self.shape(&line, style);
        let shaped = if full.advance <= width + 0.01 {
            full
        } else {
            self.ellipsize(&line, &full, style, width)
        };
        if self.fitted.len() >= FIT_CACHE_ENTRIES {
            self.fitted.clear();
        }
        let shaped = Rc::new(shaped);
        self.fitted.insert(key, shaped.clone());
        shaped
    }

    fn ellipsize(
        &mut self,
        line: &str,
        full: &ShapedText,
        style: &TextStyle,
        width: f32,
    ) -> ShapedText {
        let room = width - self.shape(ELLIPSIS, style).advance;
        let mut clusters: Vec<(u32, f32)> =
            full.glyphs.iter().map(|g| (g.cluster, g.advance)).collect();
        clusters.sort_by_key(|&(cluster, _)| cluster);
        clusters.dedup_by(|next, kept| {
            let same = next.0 == kept.0;
            if same {
                kept.1 += next.1;
            }
            same
        });
        let mut used = 0.0;
        let mut end = 0;
        for (index, &(_, advance)) in clusters.iter().enumerate() {
            if used + advance > room {
                break;
            }
            used += advance;
            end = clusters
                .get(index + 1)
                .map_or(line.len(), |&(next, _)| next as usize);
        }
        let mut prefix = line[..end].trim_end().to_owned();
        prefix.push_str(ELLIPSIS);
        self.shape(&prefix, style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ascii_fast_path_matches_harfrust() {
        let mut text = TextSystem::new();
        for weight in [Weight::Regular, Weight::Bold] {
            let style = TextStyle::new(14.0, weight).without_ligatures();
            let sample = "Hello, World! -> != === www <ruby> #rust ~/.config 0O1lI|";
            let fast = text.shape(sample, &style);
            // Force the general path with a character outside printable ASCII.
            let general = text.shape(&format!("{sample}\u{e9}"), &style);
            assert_eq!(fast.glyphs.len(), sample.len());
            for (a, b) in fast.glyphs.iter().zip(&general.glyphs) {
                assert_eq!((a.id, a.cluster), (b.id, b.cluster));
                assert!((a.x - b.x).abs() < 1e-3 && (a.advance - b.advance).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn ligatures_apply_only_when_asked() {
        let mut text = TextSystem::new();
        let style = TextStyle::new(14.0, Weight::Regular);
        let on = text.shape("->", &style);
        let off = text.shape("->", &style.without_ligatures());
        assert_ne!(
            on.glyphs.iter().map(|g| g.id).collect::<Vec<_>>(),
            off.glyphs.iter().map(|g| g.id).collect::<Vec<_>>(),
            "JetBrains Mono draws -> as an arrow through calt"
        );
        assert!(
            (on.advance - off.advance).abs() < 1e-3,
            "monospaced either way"
        );
    }

    #[test]
    fn metrics_scale_with_size() {
        let text = TextSystem::new();
        let small = text.metrics(&TextStyle::new(10.0, Weight::Regular));
        let large = text.metrics(&TextStyle::new(20.0, Weight::Regular));
        assert!((large.line_height - 2.0 * small.line_height).abs() < 1e-4);
        assert!(
            (small.ascent + small.descent - small.line_height).abs() < 1e-4,
            "no line gap"
        );
    }

    #[test]
    fn long_labels_end_in_an_ellipsis_that_fits() {
        let mut text = TextSystem::new();
        let style = TextStyle::new(11.5, Weight::Bold);
        let short = text.fit("Short", &style, 400.0);
        assert_eq!(short.text, "Short");
        let long = text.fit(
            "A very long network name that cannot possibly fit",
            &style,
            90.0,
        );
        assert!(long.text.ends_with(ELLIPSIS), "{}", long.text);
        assert!(long.advance <= 90.0 + 0.5, "{}", long.advance);
        assert!(long.text.len() > ELLIPSIS.len(), "keeps a prefix");
        let control = text.fit("line\nbreak", &style, 400.0);
        assert_eq!(control.text, "line break");
        let unicode = text.fit("caf\u{e9} au lait sur la terrasse", &style, 60.0);
        assert!(unicode.text.ends_with(ELLIPSIS) && unicode.advance <= 60.5);
        assert!(
            Rc::ptr_eq(&text.fit("Short", &style, 400.0), &short),
            "remembered"
        );
    }
}
