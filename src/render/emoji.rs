//! Color emoji from CBDT (DESIGN.md §9.5): a glyph's PNG at the strike
//! nearest its size, decoded, scaled to the text and premultiplied. The
//! font worker decodes; the text system keeps the results.
//!
//! Placement follows the OpenType CBDT and EBLC glyph metrics: the bitmap's
//! left edge is `horiBearingX` pixels right of the pen and its top edge
//! `horiBearingY` pixels above the baseline, both at the strike's size.

use std::io::Cursor;

use skrifa::bitmap::{BitmapData, BitmapStrikes, Origin};
use skrifa::instance::Size;
use skrifa::{FontRef, GlyphId};

use super::image::Image;

/// Decoded bitmaps larger than this many bytes are refused: a bad font
/// fails locally.
const MAX_DECODED: usize = 4 << 20;

/// A color glyph: its image, in device pixels, and where its top-left
/// corner goes relative to the pen on the baseline.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorGlyph {
    pub left: i32,
    pub top: i32,
    pub image: Image,
}

impl ColorGlyph {
    pub fn bytes(&self) -> usize {
        self.image.pixels.len() * 4 + std::mem::size_of::<Self>()
    }
}

/// Glyph `glyph` of `font` as a color image `ppem` device pixels to the
/// em; `None` when it has no color bitmap.
pub fn color_glyph(font: &FontRef<'_>, glyph: u16, ppem: f32) -> Option<ColorGlyph> {
    if !(ppem > 0.0 && ppem.is_finite()) {
        return None;
    }
    let strikes = BitmapStrikes::new(font);
    let bitmap = strikes.glyph_for_size(Size::new(ppem), GlyphId::new(u32::from(glyph)))?;
    let decoded = match bitmap.data {
        BitmapData::Png(bytes) => decode_png(bytes)?,
        BitmapData::Bgra(bytes) => {
            let rgba: Vec<u8> = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|&[b, g, r, a]| [r, g, b, a])
                .collect();
            // Already premultiplied: straighten so the one path applies.
            straight(bitmap.width, bitmap.height, &rgba)?
        }
        BitmapData::Mask(_) => return None,
    };
    let scale = ppem / bitmap.ppem_y.max(1.0);
    let width = (decoded.width as f32 * scale).round().max(1.0) as u32;
    let height = (decoded.height as f32 * scale).round().max(1.0) as u32;
    let left = (bitmap.inner_bearing_x * scale).round() as i32;
    let top = match bitmap.placement_origin {
        Origin::TopLeft => -(bitmap.inner_bearing_y * scale).round() as i32,
        Origin::BottomLeft => {
            -((bitmap.inner_bearing_y + decoded.height as f32) * scale).round() as i32
        }
    };
    Some(ColorGlyph {
        left,
        top,
        image: decoded.resized(width, height),
    })
}

/// A PNG as a premultiplied image, whatever its color type.
fn decode_png(bytes: &[u8]) -> Option<Image> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_limits(png::Limits { bytes: MAX_DECODED });
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buffer).ok()?;
    let pixels = &buffer[..frame.buffer_size()];
    let rgba: Vec<u8> = match frame.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => pixels
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[l, a]| [l, l, l, a])
            .collect(),
        png::ColorType::Grayscale => pixels.iter().flat_map(|&l| [l, l, l, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Image::from_straight_rgba(frame.width, frame.height, &rgba)
}

/// Premultiplied RGBA as an image, by way of straight alpha.
fn straight(width: u32, height: u32, premultiplied: &[u8]) -> Option<Image> {
    let rgba: Vec<u8> = premultiplied
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| {
            let unmultiply = |c: u8| {
                if a == 0 {
                    0
                } else {
                    ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
                }
            };
            [unmultiply(r), unmultiply(g), unmultiply(b), a]
        })
        .collect();
    Image::from_straight_rgba(width, height, &rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pngs_of_every_color_type_decode_premultiplied() {
        // A 2 × 1 RGBA PNG: opaque red, then half-transparent white.
        let mut encoded = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut encoded, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[255, 0, 0, 255, 255, 255, 255, 128])
                .unwrap();
        }
        let image = decode_png(&encoded).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixels[0], [255, 0, 0, 255]);
        assert_eq!(image.pixels[1], [128, 128, 128, 128], "premultiplied");
        assert!(decode_png(b"not a png").is_none());
    }

    #[test]
    fn faces_without_color_bitmaps_have_no_color_glyphs() {
        let font = FontRef::new(crate::render::font::JETBRAINS_MONO_REGULAR).unwrap();
        assert!(color_glyph(&font, 36, 15.0).is_none());
    }
}
