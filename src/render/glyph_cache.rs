//! Rasterized glyphs, keyed by face, glyph, device size and quarter-pixel
//! horizontal position.
//!
//! Hinting follows trust-desktop's "light" policy (its vendored glifo's
//! `HintingMode::Light`, after FreeType's `FT_LOAD_TARGET_LIGHT`): TrueType
//! outlines are fitted vertically by the autohinter, PostScript outlines by
//! their own hints, and advances stay linear so shaping and layout never
//! depend on rasterization. Native TrueType instructions would round 11 pt
//! to 15 ppem and stretch JetBrains Mono's capitals.

use std::collections::HashMap;

use skrifa::GlyphId;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{
    DrawSettings, Engine, HintingInstance, HintingOptions, OutlineGlyphCollection,
    OutlineGlyphFormat, OutlinePen, SmoothMode, Target,
};

use super::font::{Face, FaceId};
use super::geom::IRect;
use super::raster::{self, Mask, Path};

/// Horizontal glyph positions are kept to this fraction of a device pixel.
pub const SUBPIXEL_STEPS: u8 = 4;

/// The cache empties when its masks pass this size; rasterizing again is
/// cheap and only visible text asks.
const BUDGET_BYTES: usize = 32 << 20;

/// Glyphs larger than this many device pixels are clipped.
const MAX_EXTENT: i32 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub face: FaceId,
    pub glyph: u16,
    /// Size in 1/64 device pixels per em.
    pub ppem: u32,
    /// Horizontal offset in `1 / SUBPIXEL_STEPS` device pixels.
    pub subpixel: u8,
    /// Slanted, for italics the face lacks.
    pub oblique: bool,
}

/// How far a synthetic oblique leans: the 12° slant of typical italics.
const OBLIQUE_SHEAR: f32 = 0.21;

#[derive(Default)]
pub struct GlyphCache {
    /// `None` for glyphs with no ink, such as spaces.
    glyphs: HashMap<GlyphKey, Option<Mask>>,
    hinters: HashMap<(FaceId, u32), Option<HintingInstance>>,
    bytes: usize,
}

impl GlyphCache {
    /// The glyph's coverage, its `left` and `top` relative to the pen
    /// position on the baseline, rasterized on first use.
    pub fn glyph(&mut self, faces: &[Face], key: GlyphKey) -> Option<&Mask> {
        if !self.glyphs.contains_key(&key) {
            if self.bytes > BUDGET_BYTES {
                self.glyphs.clear();
                self.bytes = 0;
            }
            let mask = rasterize(faces, &mut self.hinters, key);
            self.bytes +=
                mask.as_ref().map_or(0, Mask::bytes) + std::mem::size_of::<(GlyphKey, Mask)>();
            self.glyphs.insert(key, mask);
        }
        self.glyphs.get(&key).and_then(Option::as_ref)
    }

    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

fn hinting_options(outlines: &OutlineGlyphCollection) -> HintingOptions {
    // Hint-reliant ("tricky") fonts need their interpreter, in monochrome.
    if outlines.require_interpreter() {
        return HintingOptions {
            engine: Engine::Interpreter,
            target: Target::Mono,
        };
    }
    HintingOptions {
        engine: match outlines.format() {
            Some(OutlineGlyphFormat::Cff | OutlineGlyphFormat::Cff2) => Engine::Interpreter,
            _ => Engine::Auto(None),
        },
        target: Target::Smooth {
            mode: SmoothMode::Light,
            symmetric_rendering: false,
            preserve_linear_metrics: true,
        },
    }
}

fn rasterize(
    faces: &[Face],
    hinters: &mut HashMap<(FaceId, u32), Option<HintingInstance>>,
    key: GlyphKey,
) -> Option<Mask> {
    let face = faces.get(usize::from(key.face.0))?;
    let glyph = face.outlines.get(GlyphId::new(u32::from(key.glyph)))?;
    let ppem = key.ppem as f32 / 64.0;
    let hinter = hinters.entry((key.face, key.ppem)).or_insert_with(|| {
        HintingInstance::new(
            &face.outlines,
            Size::new(ppem),
            LocationRef::default(),
            hinting_options(&face.outlines),
        )
        .ok()
    });
    let mut pen = PathPen {
        path: Path::new(),
        dx: f32::from(key.subpixel) / f32::from(SUBPIXEL_STEPS),
        shear: if key.oblique { OBLIQUE_SHEAR } else { 0.0 },
    };
    match hinter {
        Some(hinter) => glyph.draw(DrawSettings::hinted(hinter, false), &mut pen),
        None => glyph.draw(
            DrawSettings::unhinted(Size::new(ppem), LocationRef::default()),
            &mut pen,
        ),
    }
    .ok()?;
    raster::fill(
        &pen.path,
        IRect::new(-MAX_EXTENT, -MAX_EXTENT, MAX_EXTENT, MAX_EXTENT),
    )
}

/// Collects an outline in device orientation (y down), shifted by the
/// subpixel offset.
struct PathPen {
    path: Path,
    dx: f32,
    /// Horizontal shift per unit of height above the baseline.
    shear: f32,
}

impl PathPen {
    fn x(&self, x: f32, y: f32) -> f32 {
        x + y * self.shear + self.dx
    }
}

impl OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to(self.x(x, y), -y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(self.x(x, y), -y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.path.quad_to(self.x(cx0, cy0), -cy0, self.x(x, y), -y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.path.curve_to(
            self.x(cx0, cy0),
            -cy0,
            self.x(cx1, cy1),
            -cy1,
            self.x(x, y),
            -y,
        );
    }

    fn close(&mut self) {
        self.path.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::font::JETBRAINS_MONO_REGULAR;

    #[test]
    fn glyphs_rasterize_at_their_size_and_subpixel_position() {
        let faces = [Face::new(JETBRAINS_MONO_REGULAR).unwrap()];
        let mut cache = GlyphCache::default();
        let (h, _) = faces[0].ascii(b'H');
        let key = |subpixel| GlyphKey {
            face: FaceId(0),
            glyph: h,
            ppem: 15 * 64,
            subpixel,
            oblique: false,
        };
        let mask = cache.glyph(&faces, key(0)).cloned().expect("H has ink");
        // Cap height is 730 units: about 11 pixels at 15 ppem, above the baseline.
        assert!((10..=13).contains(&mask.height), "{}", mask.height);
        assert!(
            mask.top <= -10 && mask.top + mask.height as i32 >= 0,
            "{}",
            mask.top
        );
        assert!(mask.data.contains(&255), "light hinting keeps solid stems");
        let shifted = cache.glyph(&faces, key(2)).cloned().unwrap();
        assert_ne!(
            mask.data, shifted.data,
            "a half-pixel shift changes coverage"
        );
        let (space, _) = faces[0].ascii(b' ');
        assert!(
            cache
                .glyph(
                    &faces,
                    GlyphKey {
                        glyph: space,
                        ..key(0)
                    }
                )
                .is_none()
        );
        assert_eq!(cache.len(), 3);
        assert!(cache.bytes() > 0);
    }
}
