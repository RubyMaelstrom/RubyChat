//! Colors for chat text: nick colors, and IRC colors kept readable on the
//! dark canvas (DESIGN.md §8.2).

use crate::irc::format::IrcColor;
use crate::render::Color;

use super::theme::CANVAS;

/// Sixteen nick colors with at least 4.5:1 contrast on the canvas; pink is
/// left out, as highlights own it.
const NICK_COLORS: [u32; 16] = [
    0x5ec8ff, 0x7ee787, 0xffa657, 0xd2a8ff, 0x79c0ff, 0x56d4dd, 0xe3b341, 0xf78166, 0xa5d6ff,
    0xb4f2b0, 0xffc6a8, 0xc9a0ff, 0x8ddbe0, 0xf0e68c, 0x9fb3ff, 0xc4e17f,
];

/// A nick's color, from its casefolded form so case changes keep it.
pub fn nick_color(folded: &str) -> Color {
    // FNV-1a.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in folded.bytes() {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    Color::hex(NICK_COLORS[(hash % NICK_COLORS.len() as u32) as usize])
}

fn linear(channel: u8) -> f32 {
    let c = f32::from(channel) / 255.0;
    if c <= 0.039_28 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance.
fn luminance(color: Color) -> f32 {
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

pub fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `color` lightened (or, on a light background, darkened) until it reads
/// on `background` at 3:1 or better.
pub fn readable(color: Color, background: Color) -> Color {
    const MINIMUM: f32 = 3.0;
    let target = if luminance(background) < 0.5 {
        255.0
    } else {
        0.0
    };
    let mut current = color;
    for step in 1..=10 {
        if contrast(current, background) >= MINIMUM {
            break;
        }
        let mix = step as f32 / 10.0;
        let blend = |c: u8| (f32::from(c) + (target - f32::from(c)) * mix).round() as u8;
        current = Color::rgb(blend(color.r), blend(color.g), blend(color.b));
    }
    current
}

/// An IRC color as RubyChat draws it on `background`.
pub fn irc_color(color: IrcColor, background: Color) -> Color {
    readable(Color::hex(color.rgb()), background)
}

/// The canvas, as backgrounds for IRC colors default to it.
pub const DEFAULT_BACKGROUND: Color = CANVAS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nick_colors_are_stable_and_readable() {
        assert_eq!(nick_color("ruby"), nick_color("ruby"));
        for color in NICK_COLORS {
            assert!(contrast(Color::hex(color), CANVAS) >= 4.5, "{color:06x}");
        }
        let distinct: std::collections::HashSet<u32> =
            ["alice", "bob", "carol", "dave", "eve", "ruby"]
                .iter()
                .map(|n| nick_color(n).pixel())
                .collect();
        assert!(distinct.len() >= 4);
    }

    #[test]
    fn unreadable_irc_colors_are_lifted() {
        let black = irc_color(IrcColor::Palette(1), CANVAS);
        assert!(contrast(black, CANVAS) >= 3.0);
        let white = irc_color(IrcColor::Palette(0), CANVAS);
        assert_eq!(white, Color::rgb(255, 255, 255), "already readable");
        let on_white = readable(Color::rgb(255, 255, 0), Color::rgb(255, 255, 255));
        assert!(contrast(on_white, Color::rgb(255, 255, 255)) >= 3.0);
    }
}
