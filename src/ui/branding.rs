//! RubyChat's logo (logo-a, generated with the local Ideogram service): a
//! magenta faceted ruby shaped like a speech bubble with cyan edges. The
//! embedded asset is 256 px, keyed to transparency by
//! `tools/gen_logo.py`; it is resized once per device size.

use crate::render::Image;

/// 256 × 256 straight RGBA.
static LOGO_256: &[u8] = include_bytes!("../../assets/branding/logo-256.rgba");
const SOURCE_SIZE: u32 = 256;

pub struct Logo {
    source: Image,
    sized: Option<Image>,
}

impl Default for Logo {
    fn default() -> Self {
        Self {
            source: Image::from_straight_rgba(SOURCE_SIZE, SOURCE_SIZE, LOGO_256)
                .expect("the embedded logo is 256 × 256 RGBA"),
            sized: None,
        }
    }
}

impl Logo {
    /// The logo `pixels` square, resized on first use at that size.
    pub fn at(&mut self, pixels: u32) -> &Image {
        let pixels = pixels.max(1);
        if self
            .sized
            .as_ref()
            .is_none_or(|image| image.width != pixels)
        {
            self.sized = Some(self.source.resized(pixels, pixels));
        }
        self.sized.as_ref().expect("just sized")
    }

    /// The window icon: straight RGBA at `pixels` square.
    pub fn icon(&self, pixels: u32) -> (Vec<u8>, u32) {
        (
            self.source.resized(pixels, pixels).to_straight_rgba(),
            pixels,
        )
    }
}

/// The logo at `pixels` square, straight RGBA, for notifications. It runs
/// on the desktop thread.
pub fn logo_rgba(pixels: u32) -> Vec<u8> {
    Logo::default().icon(pixels).0
}

/// The tray icon's gem at 16 px, pixel by pixel: the logo's cut gem and
/// colors with a cyan edge, which stays legible where the logo's thin lines
/// would blur. 16 px is what sfwbar draws.
const GEM_16: [&str; 16] = [
    "................",
    "................",
    "....CCCCCCC.....",
    "...CLWLLLLLC....",
    "..ClLLLLLLLlC...",
    ".ClllllllllllC..",
    "CmmmmmmmddddddC.",
    "CmmmmmmmddddddC.",
    ".CmmmmmmdddddC..",
    "..CmmmmmddddC...",
    "...CmmmmdddC....",
    "....CmmmddC.....",
    ".....CmmdC......",
    "......CmC.......",
    ".......C........",
    "................",
];

/// The 16 px dot, at the lower right: a ring in the strip's color.
const DOT_16: [&str; 5] = [".kkk.", "kXXXk", "kXXXk", "kXXXk", ".kkk."];

/// The gem's colors, from the logo's facets.
const EDGE: [u8; 4] = [0x00, 0xff, 0xf9, 255];
const SPARKLE: [u8; 4] = [0xff, 0xd6, 0xf4, 255];
const TABLE: [u8; 4] = [0xff, 0x68, 0xde, 255];
const CROWN: [u8; 4] = [0xff, 0x36, 0xc8, 255];
const LIT: [u8; 4] = [0xf4, 0x08, 0xa3, 255];
const SHADE: [u8; 4] = [0xbe, 0x00, 0x80, 255];

/// The tray icon at `pixels` square, straight RGBA: the gem, with a dot at
/// its lower right for unread messages (cyan) or highlights (pink), ringed
/// in the strip's color so it reads on light and dark panels alike. 16 px
/// is drawn from `GEM_16`; other sizes trace the same gem as vectors, with
/// its edges on whole pixels. It runs on the desktop thread.
pub fn tray_icon(pixels: u32, attention: crate::platform::desktop::Attention) -> Vec<u8> {
    use crate::platform::desktop::Attention;
    use crate::ui::theme::{CYAN, PINK, STRIP};
    let dot = match attention {
        Attention::None => None,
        Attention::Unread => Some(CYAN),
        Attention::Highlight => Some(PINK),
    };
    if pixels == 16 {
        let strip = [STRIP.r, STRIP.g, STRIP.b, 255];
        let mut rgba = vec![0u8; 16 * 16 * 4];
        let mut put = |x: usize, y: usize, color: [u8; 4]| {
            rgba[(y * 16 + x) * 4..][..4].copy_from_slice(&color);
        };
        for (y, row) in GEM_16.iter().enumerate() {
            for (x, cell) in row.bytes().enumerate() {
                let color = match cell {
                    b'C' => EDGE,
                    b'W' => SPARKLE,
                    b'L' => TABLE,
                    b'l' => CROWN,
                    b'm' => LIT,
                    b'd' => SHADE,
                    _ => continue,
                };
                put(x, y, color);
            }
        }
        if let Some(color) = dot {
            for (dy, row) in DOT_16.iter().enumerate() {
                for (dx, cell) in row.bytes().enumerate() {
                    match cell {
                        b'k' => put(11 + dx, 11 + dy, strip),
                        b'X' => put(11 + dx, 11 + dy, [color.r, color.g, color.b, 255]),
                        _ => {}
                    }
                }
            }
        }
        return rgba;
    }
    let mut rgba = vec![0u8; (pixels * pixels * 4) as usize];
    let size = pixels as f32;
    let k = size / 16.0;
    // The edge is as many whole pixels wide as fit the scale; vertices sit
    // on pixel centers for odd widths and on pixel edges for even ones, so
    // straight runs stay crisp.
    let width = (k * 0.8).floor().max(1.0);
    let snap = |grid: f32| {
        let device = (grid + 0.5) * k;
        if width as u32 % 2 == 1 {
            device.floor() + 0.5
        } else {
            device.round()
        }
    };
    let point = |x: f32, y: f32| crate::render::Point::new(snap(x), snap(y));
    // The outline through the edge pixels' centers of `GEM_16`.
    let outline = crate::render::Path::polygon(&[
        point(4.0, 2.0),
        point(10.0, 2.0),
        point(14.0, 6.0),
        point(14.0, 7.0),
        point(7.0, 14.0),
        point(0.0, 7.0),
        point(0.0, 6.0),
    ]);
    let cut = |grid: f32| (grid * k).round() as i32;
    let everywhere = crate::render::IRect::new(0, 0, pixels as i32, pixels as i32);
    let regions = [
        (everywhere, LIT),
        (
            crate::render::IRect::new(cut(8.0), 0, pixels as i32, pixels as i32),
            SHADE,
        ),
        (
            crate::render::IRect::new(0, 0, pixels as i32, cut(6.0)),
            CROWN,
        ),
        (
            crate::render::IRect::new(cut(4.0), cut(3.0), cut(11.0), cut(5.0)),
            TABLE,
        ),
    ];
    for (clip, color) in regions {
        blend(&mut rgba, pixels, &outline, clip, color);
    }
    let sparkle = crate::render::Path::polygon(&[
        crate::render::Point::new(cut(5.0) as f32, cut(3.0) as f32),
        crate::render::Point::new(cut(6.0) as f32, cut(3.0) as f32),
        crate::render::Point::new(cut(6.0) as f32, cut(4.0) as f32),
        crate::render::Point::new(cut(5.0) as f32, cut(4.0) as f32),
    ]);
    blend(&mut rgba, pixels, &sparkle, everywhere, SPARKLE);
    let edge = crate::render::stroke::stroke(&outline, width, crate::render::Cap::Butt);
    blend(&mut rgba, pixels, &edge, everywhere, EDGE);
    if let Some(color) = dot {
        // About a third of the icon across, as the 16 px dot is.
        let radius = (size * 0.12).max(2.5);
        let ring = (size * 0.04).max(1.0);
        let center = size - radius - ring;
        let disc = |r: f32| {
            let mut path = crate::render::Path::new();
            path.move_to(center + r, center);
            for step in 1..=32 {
                let angle = step as f32 / 32.0 * std::f32::consts::TAU;
                path.line_to(center + r * angle.cos(), center + r * angle.sin());
            }
            path.close();
            path
        };
        blend(
            &mut rgba,
            pixels,
            &disc(radius + ring),
            everywhere,
            [STRIP.r, STRIP.g, STRIP.b, 255],
        );
        blend(
            &mut rgba,
            pixels,
            &disc(radius),
            everywhere,
            [color.r, color.g, color.b, 255],
        );
    }
    rgba
}

/// Composites `color` over straight RGBA wherever `path` covers it within
/// `clip`.
fn blend(
    rgba: &mut [u8],
    pixels: u32,
    path: &crate::render::Path,
    clip: crate::render::IRect,
    color: [u8; 4],
) {
    let Some(mask) = crate::render::raster::fill(path, clip) else {
        return;
    };
    for row in 0..mask.height {
        for column in 0..mask.width {
            let coverage = mask.data[(row * mask.width + column) as usize];
            if coverage == 0 {
                continue;
            }
            let (x, y) = (mask.left + column as i32, mask.top + row as i32);
            if x < 0 || y < 0 || x >= pixels as i32 || y >= pixels as i32 {
                continue;
            }
            let at = ((y as u32 * pixels + x as u32) * 4) as usize;
            let pixel = &mut rgba[at..at + 4];
            let alpha = f32::from(coverage) / 255.0 * f32::from(color[3]) / 255.0;
            let below = f32::from(pixel[3]) / 255.0;
            let out = alpha + below * (1.0 - alpha);
            if out > 0.0 {
                for channel in 0..3 {
                    let blended = (f32::from(color[channel]) * alpha
                        + f32::from(pixel[channel]) * below * (1.0 - alpha))
                        / out;
                    pixel[channel] = blended.round() as u8;
                }
            }
            pixel[3] = (out * 255.0).round() as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::desktop::Attention;

    #[test]
    fn the_small_gem_is_drawn_pixel_for_pixel() {
        assert!(GEM_16.iter().all(|row| row.len() == 16));
        let icon = tray_icon(16, Attention::None);
        let at = |x: usize, y: usize| &icon[(y * 16 + x) * 4..][..4];
        assert_eq!(at(4, 2), EDGE, "the table's top edge");
        assert_eq!(at(7, 14), EDGE, "the tip");
        assert_eq!(at(0, 0)[3], 0, "transparent around");
        let unread = tray_icon(16, Attention::Unread);
        assert_eq!(&unread[(13 * 16 + 13) * 4..][..3], [0x00, 0xff, 0xf9]);
        // Every size is square RGBA with a cyan edge somewhere.
        for size in [18, 20, 22, 24, 32, 48, 64] {
            for attention in [Attention::None, Attention::Unread, Attention::Highlight] {
                let icon = tray_icon(size, attention);
                assert_eq!(icon.len(), (size * size * 4) as usize);
                assert!(icon.as_chunks::<4>().0.contains(&EDGE), "{size}");
            }
        }
    }

    /// `RUBYCHAT_SNAPSHOTS=DIR cargo test --lib tray_icon_snapshots -- --ignored`
    /// writes the tray icon at each size and state, 1× and magnified 8×, on
    /// sfwbar's panel gray.
    #[test]
    #[ignore = "writes snapshot images; set RUBYCHAT_SNAPSHOTS"]
    fn tray_icon_snapshots() {
        let Some(dir) = std::env::var_os("RUBYCHAT_SNAPSHOTS") else {
            return;
        };
        let sizes = [16u32, 18, 20, 22, 24, 32, 48, 64];
        let states = [Attention::None, Attention::Unread, Attention::Highlight];
        let magnify = 8u32;
        let cell = 64 * magnify + 8;
        let (width, height) = (sizes.len() as u32 * cell, states.len() as u32 * (cell + 72));
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for pixel in rgba.as_chunks_mut::<4>().0 {
            *pixel = [42, 46, 50, 255];
        }
        for (row, attention) in states.iter().enumerate() {
            for (column, &size) in sizes.iter().enumerate() {
                let icon = tray_icon(size, *attention);
                let (left, top) = (column as u32 * cell, row as u32 * (cell + 72));
                for y in 0..size {
                    for x in 0..size {
                        let source = &icon[((y * size + x) * 4) as usize..][..4];
                        let alpha = u32::from(source[3]);
                        let mut put = |px: u32, py: u32| {
                            let at = ((py * width + px) * 4) as usize;
                            for channel in 0..3 {
                                let under = u32::from(rgba[at + channel]);
                                rgba[at + channel] =
                                    ((u32::from(source[channel]) * alpha + under * (255 - alpha))
                                        / 255) as u8;
                            }
                        };
                        for dy in 0..magnify {
                            for dx in 0..magnify {
                                put(left + x * magnify + dx, top + y * magnify + dy);
                            }
                        }
                        put(left + x, top + 64 * magnify + 8 + y);
                    }
                }
            }
        }
        let path = std::path::PathBuf::from(dir).join("tray-icons.png");
        crate::render::png::write_rgba(&path, width, height, &rgba).unwrap();
    }
}
