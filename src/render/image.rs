//! Images: premultiplied RGBA, resized by area averaging so any scale stays
//! smooth, and composited onto the canvas with source-over.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA, row by row.
    pub pixels: Vec<[u8; 4]>,
}

impl Image {
    /// From straight (non-premultiplied) RGBA bytes.
    pub fn from_straight_rgba(width: u32, height: u32, bytes: &[u8]) -> Option<Self> {
        if bytes.len() != width as usize * height as usize * 4 {
            return None;
        }
        let pixels = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| {
                let a = u32::from(p[3]);
                let premultiply = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
                [
                    premultiply(p[0]),
                    premultiply(p[1]),
                    premultiply(p[2]),
                    p[3],
                ]
            })
            .collect();
        Some(Self {
            width,
            height,
            pixels,
        })
    }

    /// Straight RGBA bytes, as window-icon APIs take them.
    pub fn to_straight_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for &[r, g, b, a] in &self.pixels {
            let unpremultiply = |c: u8| {
                if a == 0 {
                    0
                } else {
                    ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
                }
            };
            out.extend_from_slice(&[unpremultiply(r), unpremultiply(g), unpremultiply(b), a]);
        }
        out
    }

    /// The image scaled to `width` × `height`: each output pixel averages the
    /// source area it covers, in premultiplied space so edges don't darken.
    pub fn resized(&self, width: u32, height: u32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return self.clone();
        }
        let source: Vec<[f32; 4]> = self.pixels.iter().map(|p| p.map(f32::from)).collect();
        let rows = resample(
            &source,
            self.width as usize,
            self.height as usize,
            width as usize,
            true,
        );
        let both = resample(
            &rows,
            width as usize,
            self.height as usize,
            height as usize,
            false,
        );
        Self {
            width,
            height,
            pixels: both
                .into_iter()
                .map(|p| p.map(|c| c.round().clamp(0.0, 255.0) as u8))
                .collect(),
        }
    }
}

/// Area-averages one axis: horizontally (`across`) from `from` columns to
/// `to`, or vertically from `from` rows to `to`, of an image whose other
/// dimension is `other`.
fn resample(
    pixels: &[[f32; 4]],
    first: usize,
    second: usize,
    to: usize,
    across: bool,
) -> Vec<[f32; 4]> {
    let (from, other) = if across {
        (first, second)
    } else {
        (second, first)
    };
    let ratio = from as f32 / to as f32;
    let mut out = vec![[0.0f32; 4]; to * other];
    for line in 0..other {
        for target in 0..to {
            let (start, end) = (target as f32 * ratio, (target + 1) as f32 * ratio);
            let mut sum = [0.0f32; 4];
            let mut weight = 0.0;
            let mut index = start.floor() as usize;
            while (index as f32) < end && index < from {
                let covered = (end.min(index as f32 + 1.0) - start.max(index as f32)).max(0.0);
                let pixel = if across {
                    pixels[line * first + index]
                } else {
                    pixels[index * first + line]
                };
                for channel in 0..4 {
                    sum[channel] += pixel[channel] * covered;
                }
                weight += covered;
                index += 1;
            }
            let average = if weight > 0.0 {
                sum.map(|c| c / weight)
            } else {
                sum
            };
            if across {
                out[line * to + target] = average;
            } else {
                out[target * first + line] = average;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplies_and_round_trips() {
        let image = Image::from_straight_rgba(2, 1, &[255, 0, 0, 128, 10, 20, 30, 255]).unwrap();
        assert_eq!(image.pixels, [[128, 0, 0, 128], [10, 20, 30, 255]]);
        assert_eq!(image.to_straight_rgba(), [255, 0, 0, 128, 10, 20, 30, 255]);
        assert!(Image::from_straight_rgba(2, 2, &[0; 4]).is_none());
    }

    #[test]
    fn downscaling_averages_areas() {
        // A 4×4 checkerboard of opaque white and transparent averages to
        // half-covered white everywhere.
        let mut bytes = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                let on = (x + y) % 2 == 0;
                bytes.extend_from_slice(if on {
                    &[255, 255, 255, 255]
                } else {
                    &[0, 0, 0, 0]
                });
            }
        }
        let image = Image::from_straight_rgba(4, 4, &bytes).unwrap();
        let half = image.resized(2, 2);
        assert!(
            half.pixels.iter().all(|&p| p == [128, 128, 128, 128]),
            "{:?}",
            half.pixels
        );
        // Uneven ratios keep their total coverage.
        let third = image.resized(3, 3);
        let total: u32 = third.pixels.iter().map(|p| u32::from(p[3])).sum();
        assert!((total as f32 / 9.0 - 127.5).abs() < 2.0, "{total}");
    }
}
