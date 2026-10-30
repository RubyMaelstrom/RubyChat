//! The header above the chat view (DESIGN.md §8.5): the buffer's name, its
//! modes and member count, and below them the topic on one line. A query
//! names who you're talking to; the server buffer names the server.

use crate::irc::format::strip;
use crate::render::{Align, Painter, Rect};

use super::theme::{self, DISABLED, EDGE, MUTED, SURFACE, TEXT};

pub const HEADER_HEIGHT: f32 = 50.0;

pub struct HeaderView<'a> {
    pub name: &'a str,
    /// Modes and members, or the server's address.
    pub detail: &'a str,
    /// The topic, with any formatting codes, or a description.
    pub topic: Option<&'a str>,
    /// Shown dimmed when there's no topic.
    pub placeholder: &'a str,
}

/// `1,203`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub fn paint_header(painter: &mut Painter<'_>, rect: Rect, view: &HeaderView<'_>) {
    painter.fill_rect(rect, SURFACE);
    painter.fill_rect(
        Rect::new(rect.x, rect.bottom() - 1.0, rect.width, 1.0),
        EDGE,
    );
    let name_style = theme::chrome(13.0);
    let x = rect.x + 16.0;
    let room = (rect.width - 32.0).max(0.0);
    let name_width = painter.text.width(view.name, &name_style).min(room);
    painter.label(
        view.name,
        Rect::new(x, rect.y + 7.0, name_width + 1.0, 18.0),
        &name_style,
        TEXT,
        Align::Start,
    );
    if !view.detail.is_empty() {
        let detail_x = x + name_width + 12.0;
        painter.label(
            view.detail,
            Rect::new(
                detail_x,
                rect.y + 8.0,
                (rect.right() - 16.0 - detail_x).max(0.0),
                18.0,
            ),
            &theme::chrome(10.0),
            MUTED,
            Align::Start,
        );
    }
    let topic_rect = Rect::new(x, rect.y + 27.0, room, 16.0);
    match view.topic {
        Some(topic) if !topic.trim().is_empty() => {
            painter.label(
                &strip(topic),
                topic_rect,
                &theme::body(12.0),
                TEXT,
                Align::Start,
            );
        }
        _ => painter.label(
            view.placeholder,
            topic_rect,
            &theme::body(12.0),
            DISABLED,
            Align::Start,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_with_separators() {
        assert_eq!(
            (thousands(7), thousands(1203), thousands(1_000_000)),
            (
                String::from("7"),
                String::from("1,203"),
                String::from("1,000,000")
            )
        );
    }
}
