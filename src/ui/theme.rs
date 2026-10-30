//! RubyChat's palette and type: trust-desktop's chrome (DESIGN.md §8.2).
//! Colors come from TRust's `theme.rs` and the constants of its tab strip
//! and COMMAND panel.

use crate::render::{Color, TextStyle, Weight};

/// Tab strip.
pub const STRIP: Color = Color::hex(0x070612);
/// Active tab, sidebar, header, nick list, overlays.
pub const SURFACE: Color = Color::hex(0x0c091c);
/// Chat view: TRust's `BG`.
pub const CANVAS: Color = Color::hex(0x0b0221);
/// Hovered tabs and rows.
pub const HOVER: Color = Color::hex(0x141028);
/// Input fields and pressed controls.
pub const INSET: Color = Color::hex(0x050813);
/// Etched edges.
pub const EDGE: Color = Color::hex(0x2c4350);
/// Separators.
pub const RULE: Color = Color::hex(0x243042);
pub const TEXT: Color = Color::hex(0xc8c8dc);
/// Secondary text and labels.
pub const MUTED: Color = Color::hex(0x939cb3);
pub const DISABLED: Color = Color::hex(0x585c70);
/// Keycap faces.
pub const KEY: Color = Color::hex(0x1f1b30);
/// Selection, highlights, destructive hover.
pub const PINK: Color = Color::hex(0xff2bd6);
/// Focus, hover, links, the prompt, connected.
pub const CYAN: Color = Color::hex(0x00fff9);
/// Work in progress, notices, keycap text.
pub const AMBER: Color = Color::hex(0xffb000);
/// Accents while the window is unfocused.
pub const DIM: Color = Color::hex(0x6e4e9e);
/// Glyphs drawn on pink.
pub const HEART: Color = Color::hex(0xffe8f6);

/// Chrome text: trust-desktop's chrome uses the terminal face at weight 700.
pub const fn chrome(size: f32) -> TextStyle {
    TextStyle::new(size, Weight::Bold)
}

/// Running text in the chrome, such as a topic: the same face at weight 400.
pub const fn body(size: f32) -> TextStyle {
    TextStyle::new(size, Weight::Regular)
}

/// 11 pt, the size of trust-desktop's terminal text and COMMAND prompt.
pub const PROMPT_SIZE: f32 = 11.0 * 96.0 / 72.0;
