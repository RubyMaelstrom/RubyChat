//! Presentation: copying the canvas to the window's softbuffer surface.
//! Only damaged pixels are copied. A reused buffer is `age` frames old, so it
//! also lacks the damage of the frames presented since; those rectangles
//! are copied too (DESIGN.md §9.3). The compositor is told only this
//! frame's damage.

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::rc::Rc;

use winit::window::Window;

use crate::render::{Canvas, Damage};

/// Buffers older than this many frames are refreshed whole.
const HISTORY: usize = 4;

pub struct Presenter {
    // The context must outlive the surface built from it.
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    _context: softbuffer::Context<Rc<Window>>,
    size: (u32, u32),
    /// Damage of the frames presented most recently, newest first.
    history: VecDeque<Damage>,
}

/// What one presentation did, for tracing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Presented {
    pub age: u8,
    pub copied: i64,
}

impl Presenter {
    pub fn new(window: Rc<Window>) -> Result<Self, String> {
        let context =
            softbuffer::Context::new(window.clone()).map_err(|e| format!("display: {e}"))?;
        let surface =
            softbuffer::Surface::new(&context, window).map_err(|e| format!("surface: {e}"))?;
        Ok(Self {
            surface,
            _context: context,
            size: (0, 0),
            history: VecDeque::new(),
        })
    }

    pub fn present(
        &mut self,
        window: &Window,
        canvas: &Canvas,
        damage: &Damage,
    ) -> Result<Presented, String> {
        let (width, height) = (canvas.width(), canvas.height());
        let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
            return Ok(Presented::default());
        };
        if self.size != (width, height) {
            self.surface
                .resize(w, h)
                .map_err(|e| format!("resize: {e}"))?;
            self.size = (width, height);
            self.history.clear();
        }
        let mut buffer = self
            .surface
            .buffer_mut()
            .map_err(|e| format!("buffer: {e}"))?;
        let age = buffer.age();
        let bounds = canvas.bounds();
        let mut copy = damage.clone();
        if age == 0 || usize::from(age) > self.history.len() + 1 {
            copy.clear();
            copy.add(bounds);
        } else {
            for older in self.history.iter().take(usize::from(age) - 1) {
                copy.extend(older);
            }
        }
        copy.clip(bounds);
        let pixels = canvas.pixels();
        let stride = width as usize;
        for rect in copy.rects() {
            let (x0, x1) = (rect.x0 as usize, rect.x1 as usize);
            for y in rect.y0 as usize..rect.y1 as usize {
                let row = y * stride;
                buffer[row + x0..row + x1].copy_from_slice(&pixels[row + x0..row + x1]);
            }
        }
        let mut rects: Vec<softbuffer::Rect> = Vec::with_capacity(damage.rects().len());
        for rect in damage.rects() {
            let rect = rect.intersect(bounds);
            if let (Some(width), Some(height)) = (
                NonZeroU32::new(rect.width() as u32),
                NonZeroU32::new(rect.height() as u32),
            ) {
                rects.push(softbuffer::Rect {
                    x: rect.x0 as u32,
                    y: rect.y0 as u32,
                    width,
                    height,
                });
            }
        }
        window.pre_present_notify();
        buffer
            .present_with_damage(&rects)
            .map_err(|e| format!("present: {e}"))?;
        self.history.push_front(damage.clone());
        self.history.truncate(HISTORY);
        Ok(Presented {
            age,
            copied: copy.area(),
        })
    }
}
