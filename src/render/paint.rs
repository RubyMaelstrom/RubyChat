//! The painter: drawing in logical pixels onto the canvas at the window's
//! scale factor, inside a clip.

use super::canvas::Canvas;
use super::color::Color;
use super::geom::{IRect, Point, Rect};
use super::glyph_cache::{GlyphKey, SUBPIXEL_STEPS};
use super::image::Image;
use super::paragraph::Paragraph;
use super::raster::{self, Path};
use super::stroke::{self, Cap};
use super::text::{Glyph, ShapedText, TextStyle, TextSystem};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

pub struct Painter<'a> {
    canvas: &'a mut Canvas,
    pub text: &'a mut TextSystem,
    scale: f32,
    clip: IRect,
    saved: Vec<IRect>,
}

impl<'a> Painter<'a> {
    pub fn new(canvas: &'a mut Canvas, text: &'a mut TextSystem, scale: f32) -> Self {
        let clip = canvas.bounds();
        Self {
            canvas,
            text,
            scale,
            clip,
            saved: Vec::new(),
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Limits painting to `rect` until the matching `restore`.
    pub fn save_clip(&mut self, rect: Rect) {
        let device = rect.snap(self.scale);
        self.save_device_clip(device);
    }

    /// Limits painting to device pixels `rect` until the matching `restore`.
    pub fn save_device_clip(&mut self, rect: IRect) {
        self.saved.push(self.clip);
        self.clip = self.clip.intersect(rect);
    }

    pub fn restore(&mut self) {
        if let Some(clip) = self.saved.pop() {
            self.clip = clip;
        }
    }

    /// Moves the pixels in `rect`, within the clip, `dy` device rows down
    /// (up when negative); scrolling repaints only what this exposes.
    pub fn shift_rows(&mut self, rect: Rect, dy: i32) {
        self.canvas
            .shift_rows(rect.snap(self.scale).intersect(self.clip), dy);
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        self.canvas
            .fill(rect.snap(self.scale).intersect(self.clip), color);
    }

    /// A border `width` wide just inside `rect`, snapped to device pixels so
    /// it stays crisp at every scale.
    pub fn stroke_rect(&mut self, rect: Rect, width: f32, color: Color) {
        let outer = rect.snap(self.scale);
        let w = (width * self.scale).round().max(1.0) as i32;
        for edge in [
            IRect::new(outer.x0, outer.y0, outer.x1, outer.y0 + w),
            IRect::new(outer.x0, outer.y1 - w, outer.x1, outer.y1),
            IRect::new(outer.x0, outer.y0 + w, outer.x0 + w, outer.y1 - w),
            IRect::new(outer.x1 - w, outer.y0 + w, outer.x1, outer.y1 - w),
        ] {
            self.canvas.fill(edge.intersect(self.clip), color);
        }
    }

    fn device(&self, path: &Path) -> Path {
        let scale = self.scale;
        path.map(|p| Point::new(p.x * scale, p.y * scale))
    }

    pub fn fill_path(&mut self, path: &Path, color: Color) {
        let device = self.device(path);
        if let Some(mask) = raster::fill(&device, self.clip) {
            self.canvas
                .blend_mask(mask.left, mask.top, mask.as_ref(), color, self.clip);
        }
    }

    pub fn stroke_path(&mut self, path: &Path, width: f32, cap: Cap, color: Color) {
        let outline = stroke::stroke(&self.device(path), width * self.scale, cap);
        if let Some(mask) = raster::fill(&outline, self.clip) {
            self.canvas
                .blend_mask(mask.left, mask.top, mask.as_ref(), color, self.clip);
        }
    }

    /// Draws `image`, already sized in device pixels, with its top-left
    /// corner at logical `origin` snapped to a device pixel.
    pub fn draw_image(&mut self, image: &Image, origin: Point) {
        let x = (origin.x * self.scale).round() as i32;
        let y = (origin.y * self.scale).round() as i32;
        self.canvas.blend_image(x, y, image, self.clip);
    }

    /// Draws `shaped` with its origin at `x` on the `baseline`. The baseline
    /// snaps to a device row, as light hinting expects; glyphs keep
    /// quarter-pixel horizontal positions.
    pub fn draw_text(&mut self, shaped: &ShapedText, x: f32, baseline: f32, color: Color) {
        let baseline = (baseline * self.scale).round() as i32;
        for glyph in &shaped.glyphs {
            self.draw_glyph(glyph, shaped.size, x + glyph.x, baseline, color, false);
        }
    }

    /// Draws row `row` of `paragraph` starting at `x` on the `baseline`.
    pub fn draw_row(&mut self, paragraph: &Paragraph, row: usize, x: f32, baseline: f32) {
        let Some((range, origin)) = paragraph.rows.get(row) else {
            return;
        };
        let glyphs = &paragraph.glyphs[range.clone()];
        let line_top = baseline - paragraph.ascent;
        // Backgrounds first, under every glyph of the row.
        for placed in glyphs {
            if let Some(background) = placed.decoration.background {
                let left = x + placed.glyph.x - origin;
                self.fill_rect(
                    Rect::new(left, line_top, placed.glyph.advance, paragraph.line_height),
                    background,
                );
            }
        }
        let device_baseline = (baseline * self.scale).round() as i32;
        for placed in glyphs {
            let glyph = &placed.glyph;
            self.draw_glyph(
                glyph,
                placed.size,
                x + glyph.x - origin,
                device_baseline,
                placed.color,
                placed.decoration.oblique,
            );
        }
        // Lines over the glyphs: one logical pixel, below the baseline or
        // through the middle of lower-case letters.
        let thickness = 1.0f32.max(1.0 / self.scale);
        for placed in glyphs {
            let left = x + placed.glyph.x - origin;
            if placed.decoration.underline {
                self.fill_rect(
                    Rect::new(left, baseline + 2.0, placed.glyph.advance, thickness),
                    placed.color,
                );
            }
            if placed.decoration.strikethrough {
                let middle = baseline - placed.size * 0.27;
                self.fill_rect(
                    Rect::new(left, middle, placed.glyph.advance, thickness),
                    placed.color,
                );
            }
        }
    }

    /// One glyph at logical `x`, its baseline on device row `baseline`.
    fn draw_glyph(
        &mut self,
        glyph: &Glyph,
        size: f32,
        x: f32,
        baseline: i32,
        color: Color,
        oblique: bool,
    ) {
        let ppem = (size * self.scale * 64.0).round() as u32;
        if ppem == 0 {
            return;
        }
        let px = x * self.scale;
        let mut column = px.floor();
        let mut subpixel = ((px - column) * f32::from(SUBPIXEL_STEPS)).round() as u8;
        if subpixel == SUBPIXEL_STEPS {
            column += 1.0;
            subpixel = 0;
        }
        let row = baseline + (glyph.y * self.scale).round() as i32;
        let key = GlyphKey {
            face: glyph.face,
            glyph: glyph.id,
            ppem,
            subpixel,
            oblique,
        };
        let clip = self.clip;
        if let Some(mask) = self.text.glyph(key) {
            self.canvas.blend_mask(
                column as i32 + mask.left,
                row + mask.top,
                mask.as_ref(),
                color,
                clip,
            );
        }
    }

    /// `text` on one line in `rect`: fitted with an ellipsis, vertically
    /// centered, and clipped to `rect`. The line box and its baseline land
    /// on whole logical pixels, as trust-desktop's quantized text layout
    /// places them, so labels sit identically at every integer scale.
    pub fn label(&mut self, text: &str, rect: Rect, style: &TextStyle, color: Color, align: Align) {
        if rect.width <= 0.0 || rect.height <= 0.0 || text.is_empty() {
            return;
        }
        let shaped = self.text.fit(text, style, rect.width);
        let x = match align {
            Align::Start => rect.x,
            Align::Center => rect.x + (rect.width - shaped.advance) / 2.0,
            Align::End => rect.right() - shaped.advance,
        };
        let top = (rect.y + (rect.height - shaped.line_height) / 2.0).round();
        self.save_clip(rect);
        self.draw_text(&shaped, x, shaped.baseline(top).round(), color);
        self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::font::Weight;

    #[test]
    fn labels_draw_inside_their_rectangle_only() {
        let mut canvas = Canvas::new(120, 40);
        let mut text = TextSystem::new();
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        painter.label(
            "RubyChat is lovely",
            Rect::new(10.0, 10.0, 60.0, 20.0),
            &TextStyle::new(12.0, Weight::Bold),
            Color::rgb(255, 255, 255),
            Align::Start,
        );
        let mut inked = 0;
        for y in 0..40 {
            for x in 0..120 {
                if canvas.pixel(x, y) != 0 {
                    inked += 1;
                    assert!(
                        (10..70).contains(&x) && (10..30).contains(&y),
                        "ink at ({x}, {y})"
                    );
                }
            }
        }
        assert!(inked > 100, "{inked}");
    }

    #[test]
    fn scaled_painting_doubles_device_geometry() {
        let mut canvas = Canvas::new(20, 20);
        let mut text = TextSystem::new();
        let mut painter = Painter::new(&mut canvas, &mut text, 2.0);
        painter.fill_rect(Rect::new(1.0, 1.0, 2.0, 2.0), Color::rgb(255, 0, 0));
        painter.save_clip(Rect::new(0.0, 0.0, 10.0, 3.0));
        painter.stroke_rect(Rect::new(5.0, 1.0, 4.0, 4.0), 1.0, Color::rgb(0, 255, 0));
        painter.restore();
        assert_eq!(canvas.pixel(2, 2), 0xff0000);
        assert_eq!(canvas.pixel(5, 5), 0xff0000);
        assert_eq!(canvas.pixel(6, 6), 0);
        assert_eq!(
            canvas.pixel(10, 2),
            0x00ff00,
            "top border, two device pixels thick"
        );
        assert_eq!(canvas.pixel(10, 3), 0x00ff00);
        assert_eq!(canvas.pixel(10, 7), 0, "below the clip");
    }
}
