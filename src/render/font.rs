//! Font faces. JetBrains Mono Regular and Bold are embedded (OFL; see
//! `assets/fonts/`) so RubyChat looks the same on every machine; the Latin
//! glyphs match the JetBrainsMono Nerd Font that trust-desktop's chrome uses.
//! Fallback faces from the system arrive with the fallback index (DESIGN.md
//! §9.5).

use harfrust::{Shaper, ShaperData};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::OutlineGlyphCollection;
use skrifa::{FontRef, GlyphId, MetadataProvider};

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
    /// Glyph and advance (font units) of each printable ASCII character.
    ascii: [(u16, f32); 95],
}

impl Face {
    /// A face from font data that lives as long as the process.
    pub fn new(data: &'static [u8]) -> Option<Self> {
        let font = FontRef::new(data).ok()?;
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
        Some(Self {
            outlines,
            shaper,
            units_per_em: f32::from(metrics.units_per_em),
            ascent: metrics.ascent,
            descent: -metrics.descent,
            line_gap: metrics.leading,
            monospace: metrics.is_monospace,
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
        }
    }
}
