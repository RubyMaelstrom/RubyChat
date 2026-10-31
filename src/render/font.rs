//! Font faces. JetBrains Mono Regular and Bold are embedded (OFL; see
//! `assets/fonts/`) so RubyChat looks the same on every machine; the Latin
//! glyphs match the JetBrainsMono Nerd Font that trust-desktop's chrome uses.
//! Fallback faces come from the system, through the fallback index (DESIGN.md
//! §9.5), and are loaded on the font worker.

use harfrust::{Shaper, ShaperData};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::OutlineGlyphCollection;
use skrifa::{FontRef, GlyphId, MetadataProvider, Tag};

use super::fallback::Coverage;

pub static JETBRAINS_MONO_REGULAR: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
pub static JETBRAINS_MONO_BOLD: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Bold.ttf");

/// Printable ASCII, which the shaping fast path covers.
pub const ASCII_PRINTABLE: std::ops::RangeInclusive<u8> = 0x20..=0x7e;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceId(pub u16);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Weight {
    #[default]
    Regular,
    Bold,
}

pub struct Face {
    pub(crate) outlines: OutlineGlyphCollection<'static>,
    pub(crate) shaper: Shaper<'static>,
    pub units_per_em: f32,
    /// Distance above the baseline, in font units.
    pub ascent: f32,
    /// Distance below the baseline, in font units (positive).
    pub descent: f32,
    /// Extra space between lines, in font units.
    pub line_gap: f32,
    pub monospace: bool,
    /// Draws from color bitmaps (CBDT) rather than outlines.
    pub color: bool,
    /// The characters it maps.
    pub coverage: Coverage,
    /// Glyph and advance (font units) of each printable ASCII character.
    ascii: [(u16, f32); 95],
}

impl Face {
    /// A face from font data that lives as long as the process.
    pub fn new(data: &'static [u8]) -> Option<Self> {
        Self::from_index(data, 0)
    }

    /// Face `index` of a font file or collection that lives as long as the
    /// process.
    pub fn from_index(data: &'static [u8], index: u32) -> Option<Self> {
        let font = FontRef::from_index(data, index).ok()?;
        let outlines = font.outline_glyphs();
        // Shaping tables live as long as the face, which is the process.
        let shaper_data: &'static ShaperData = Box::leak(Box::new(ShaperData::new(&font)));
        let shaper = shaper_data.shaper(&font).build();
        let metrics = font.metrics(Size::unscaled(), LocationRef::default());
        let glyph_metrics = font.glyph_metrics(Size::unscaled(), LocationRef::default());
        let charmap = font.charmap();
        let mut ascii = [(0u16, 0.0f32); 95];
        for (byte, slot) in ASCII_PRINTABLE.zip(ascii.iter_mut()) {
            let glyph = charmap.map(char::from(byte)).unwrap_or(GlyphId::NOTDEF);
            *slot = (
                glyph.to_u32() as u16,
                glyph_metrics.advance_width(glyph).unwrap_or(0.0),
            );
        }
        let coverage = Coverage::from_codepoints(
            charmap
                .mappings()
                .filter(|(_, glyph)| *glyph != GlyphId::NOTDEF)
                .map(|(codepoint, _)| codepoint),
        );
        let color = font.table_data(Tag::new(b"CBDT")).is_some();
        Some(Self {
            outlines,
            shaper,
            units_per_em: f32::from(metrics.units_per_em),
            ascent: metrics.ascent,
            descent: -metrics.descent,
            line_gap: metrics.leading,
            monospace: metrics.is_monospace,
            color,
            coverage,
            ascii,
        })
    }

    /// The glyph and advance (font units) of a printable ASCII byte.
    pub fn ascii(&self, byte: u8) -> (u16, f32) {
        self.ascii[usize::from(byte - ASCII_PRINTABLE.start())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_faces_load_with_sane_metrics() {
        for data in [JETBRAINS_MONO_REGULAR, JETBRAINS_MONO_BOLD] {
            let face = Face::new(data).expect("embedded face parses");
            assert_eq!(face.units_per_em, 1000.0);
            assert!(
                face.ascent > 900.0 && face.descent > 200.0,
                "{} {}",
                face.ascent,
                face.descent
            );
            assert!(face.monospace);
            let (m, m_advance) = face.ascii(b'M');
            let (i, i_advance) = face.ascii(b'i');
            assert!(m != 0 && i != 0 && m != i);
            assert_eq!(m_advance, i_advance, "monospaced");
            assert!(face.coverage.contains('M') && face.coverage.contains('\u{e9}'));
            assert!(!face.coverage.contains('\u{4e00}'));
            assert!(!face.color);
        }
    }

    /// Faces must cross to the UI thread from the font worker.
    #[test]
    fn faces_travel_between_threads() {
        fn send<T: Send>() {}
        send::<Face>();
    }
}
