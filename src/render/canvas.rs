//! The window's backing store: one `0x00RRGGBB` pixel per device pixel. It
//! always holds the complete current frame; presentation copies only the
//! damaged parts of it to the window (`app::present`).

use super::color::{Color, blend, div_255};
use super::geom::IRect;
use super::image::Image;

/// An alpha mask to blend through, borrowed from a rasterized path or glyph.
#[derive(Clone, Copy, Debug)]
pub struct MaskRef<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

#[derive(Clone, Debug, Default)]
pub struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize],
        }
    }

    /// Changes the size; every pixel becomes black.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.pixels.resize(width as usize * height as usize, 0);
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn bounds(&self) -> IRect {
        IRect::from_size(self.width, self.height)
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        self.pixels[y as usize * self.width as usize + x as usize]
    }

    /// Fills `rect`, clipped to the canvas, blending when `color` is
    /// translucent.
    pub fn fill(&mut self, rect: IRect, color: Color) {
        let rect = rect.intersect(self.bounds());
        if rect.is_empty() || color.a == 0 {
            return;
        }
        let width = self.width as usize;
        let (x0, x1) = (rect.x0 as usize, rect.x1 as usize);
        for y in rect.y0 as usize..rect.y1 as usize {
            let row = &mut self.pixels[y * width + x0..y * width + x1];
            if color.a == 255 {
                row.fill(color.pixel());
            } else {
                for pixel in row {
                    *pixel = blend(*pixel, color, 255);
                }
            }
        }
    }

    /// Blends `color` through `mask`, whose top-left pixel lands at (`x`,
    /// `y`), inside `clip`.
    pub fn blend_mask(&mut self, x: i32, y: i32, mask: MaskRef<'_>, color: Color, clip: IRect) {
        let target = IRect::new(x, y, x + mask.width as i32, y + mask.height as i32)
            .intersect(clip)
            .intersect(self.bounds());
        if target.is_empty() || color.a == 0 {
            return;
        }
        let width = self.width as usize;
        let mask_width = mask.width as usize;
        for py in target.y0..target.y1 {
            let mask_row = (py - y) as usize * mask_width;
            let canvas_row = py as usize * width;
            for px in target.x0..target.x1 {
                let coverage = mask.data[mask_row + (px - x) as usize];
                if coverage != 0 {
                    let pixel = &mut self.pixels[canvas_row + px as usize];
                    *pixel = blend(*pixel, color, u32::from(coverage));
                }
            }
        }
    }

    /// Composites a premultiplied image with its top-left pixel at (`x`, `y`),
    /// inside `clip` (source-over).
    pub fn blend_image(&mut self, x: i32, y: i32, image: &Image, clip: IRect) {
        let target = IRect::new(x, y, x + image.width as i32, y + image.height as i32)
            .intersect(clip)
            .intersect(self.bounds());
        if target.is_empty() {
            return;
        }
        let width = self.width as usize;
        for py in target.y0..target.y1 {
            let source_row = (py - y) as usize * image.width as usize;
            let canvas_row = py as usize * width;
            for px in target.x0..target.x1 {
                let [r, g, b, a] = image.pixels[source_row + (px - x) as usize];
                if a == 0 {
                    continue;
                }
                let pixel = &mut self.pixels[canvas_row + px as usize];
                if a == 255 {
                    *pixel = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
                    continue;
                }
                let keep = 255 - u32::from(a);
                let channel = |source: u8, shift: u32| {
                    (u32::from(source) + div_255(((*pixel >> shift) & 0xff) * keep)).min(255)
                };
                *pixel = (channel(r, 16) << 16) | (channel(g, 8) << 8) | channel(b, 0);
            }
        }
    }

    /// Moves the rows of `rect` by `dy` (negative is up), staying inside
    /// `rect`. Rows the move uncovers keep their old pixels for the caller to
    /// repaint. Chat views scroll this way and paint only the new rows.
    pub fn shift_rows(&mut self, rect: IRect, dy: i32) {
        let rect = rect.intersect(self.bounds());
        if rect.is_empty() || dy == 0 || dy.abs() >= rect.height() {
            return;
        }
        let width = self.width as usize;
        let (x0, x1) = (rect.x0 as usize, rect.x1 as usize);
        let copy_row = |pixels: &mut Vec<u32>, from: i32| {
            let src = from as usize * width;
            let dst = (from + dy) as usize * width;
            pixels.copy_within(src + x0..src + x1, dst + x0);
        };
        if dy < 0 {
            for from in rect.y0 - dy..rect.y1 {
                copy_row(&mut self.pixels, from);
            }
        } else {
            for from in (rect.y0..rect.y1 - dy).rev() {
                copy_row(&mut self.pixels, from);
            }
        }
    }

    /// Opaque RGBA bytes, row by row, for snapshots.
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(self.pixels.len() * 4);
        for &pixel in &self.pixels {
            rgba.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8, 255]);
        }
        rgba
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_clip_to_the_canvas_and_blend_when_translucent() {
        let mut canvas = Canvas::new(4, 3);
        canvas.fill(IRect::new(-5, -5, 2, 2), Color::rgb(255, 0, 0));
        assert_eq!(canvas.pixel(0, 0), 0xff0000);
        assert_eq!(canvas.pixel(1, 1), 0xff0000);
        assert_eq!(canvas.pixel(2, 1), 0);
        canvas.fill(IRect::new(0, 0, 4, 3), Color::rgba(0, 0, 255, 128));
        assert_eq!(canvas.pixel(0, 0), 0x7f0080);
        assert_eq!(canvas.pixel(3, 2), 0x000080);
    }

    #[test]
    fn masks_blend_inside_their_clip() {
        let mut canvas = Canvas::new(4, 4);
        let data = [255, 128, 0, 255];
        let mask = MaskRef {
            width: 2,
            height: 2,
            data: &data,
        };
        canvas.blend_mask(
            1,
            1,
            mask,
            Color::rgb(255, 255, 255),
            IRect::new(0, 0, 4, 2),
        );
        assert_eq!(canvas.pixel(1, 1), 0xffffff);
        assert_eq!(canvas.pixel(2, 1), 0x808080);
        assert_eq!(canvas.pixel(2, 2), 0, "clipped away");
        canvas.blend_mask(3, 3, mask, Color::rgb(255, 255, 255), canvas.bounds());
        assert_eq!(canvas.pixel(3, 3), 0xffffff, "clipped at the canvas edge");
    }

    #[test]
    fn shifting_rows_scrolls_inside_the_rectangle() {
        let mut canvas = Canvas::new(2, 4);
        for y in 0..4 {
            canvas.fill(IRect::new(0, y, 2, y + 1), Color::rgb(y as u8 + 1, 0, 0));
        }
        canvas.shift_rows(IRect::new(0, 0, 1, 4), -1);
        let column =
            |canvas: &Canvas, x| (0..4).map(|y| canvas.pixel(x, y) >> 16).collect::<Vec<_>>();
        assert_eq!(
            column(&canvas, 0),
            [2, 3, 4, 4],
            "up by one; the last row is stale"
        );
        assert_eq!(column(&canvas, 1), [1, 2, 3, 4], "outside the rectangle");
        canvas.shift_rows(IRect::new(0, 0, 1, 4), 2);
        assert_eq!(column(&canvas, 0), [2, 3, 2, 3]);
    }
}
