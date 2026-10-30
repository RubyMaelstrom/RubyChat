//! Colors: 8-bit sRGB with straight (non-premultiplied) alpha, blended in
//! sRGB space as trust-desktop's renderer does.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// An opaque color from `0xRRGGBB`.
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// The `0x00RRGGBB` pixel softbuffer presents; alpha is dropped.
    pub const fn pixel(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }

    /// The color of an opaque pixel.
    pub const fn from_pixel(pixel: u32) -> Self {
        Self::rgb((pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8)
    }
}

/// `value / 255`, rounded, for `value <= 255 * 255`.
#[inline]
pub(crate) fn div_255(value: u32) -> u32 {
    let t = value + 128;
    (t + (t >> 8)) >> 8
}

/// `color` at `coverage` (0–255, scaled by its alpha) over the opaque pixel
/// `dst`.
#[inline]
pub fn blend(dst: u32, color: Color, coverage: u32) -> u32 {
    let alpha = div_255(u32::from(color.a) * coverage);
    if alpha == 0 {
        return dst;
    }
    if alpha == 255 {
        return color.pixel();
    }
    let keep = 255 - alpha;
    let channel =
        |src: u8, shift: u32| div_255(u32::from(src) * alpha + ((dst >> shift) & 0xff) * keep);
    (channel(color.r, 16) << 16) | (channel(color.g, 8) << 8) | channel(color.b, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn division_by_255_rounds_exactly() {
        for value in 0..=255 * 255 {
            assert_eq!(
                div_255(value),
                (value as f32 / 255.0).round() as u32,
                "{value}"
            );
        }
    }

    #[test]
    fn blending_reaches_both_ends_and_rounds_between() {
        let white = Color::rgb(255, 255, 255);
        assert_eq!(blend(0, white, 255), 0xffffff);
        assert_eq!(blend(0x123456, white, 0), 0x123456);
        assert_eq!(blend(0, white.with_alpha(0), 255), 0);
        assert_eq!(blend(0, white, 128), 0x808080);
        // Half-transparent white at half coverage is a quarter of the way.
        assert_eq!(blend(0, white.with_alpha(128), 128), 0x404040);
        assert_eq!(Color::hex(0xff2bd6), Color::rgb(0xff, 0x2b, 0xd6));
        assert_eq!(
            Color::from_pixel(Color::hex(0x0b0221).pixel()),
            Color::hex(0x0b0221)
        );
    }
}
