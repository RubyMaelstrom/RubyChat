//! RubyChat's renderer: a CPU compositor into a backing store the size of
//! the window, with damage tracking so each frame touches only what changed
//! (DESIGN.md §9). No GPU and no UI toolkit.

pub mod canvas;
pub mod color;
pub mod damage;
pub mod font;
pub mod geom;
pub mod glyph_cache;
pub mod image;
pub mod paint;
pub mod paragraph;
pub mod png;
pub mod raster;
pub mod stroke;
pub mod text;

pub use canvas::Canvas;
pub use color::Color;
pub use damage::Damage;
pub use font::Weight;
pub use geom::{IRect, Point, Rect};
pub use image::Image;
pub use paint::{Align, Painter};
pub use paragraph::{Decoration, Paragraph, Span};
pub use raster::Path;
pub use stroke::Cap;
pub use text::{ShapedText, TextStyle, TextSystem};
