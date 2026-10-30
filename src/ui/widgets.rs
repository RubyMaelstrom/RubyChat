//! Controls shared by RubyChat's panels, in the COMMAND panel's design
//! (DESIGN.md §8.2): the chamfered frame, text fields (masked for
//! passwords), toggles, choices that cycle, buttons and keycaps.

use crate::model::editor::LineEditor;
use crate::render::{Align, Cap, Color, Painter, Path, Point, Rect};

use super::theme::{
    self, AMBER, CYAN, DIM, DISABLED, EDGE, INSET, KEY, MUTED, PINK, SURFACE, TEXT,
};

/// A COMMAND-style frame: chamfered corners, a static shadow, the etched
/// edge, the pink bar and the cyan underline.
pub fn frame(painter: &mut Painter<'_>, panel: Rect, chamfer: f32) {
    let (right, bottom) = (panel.right(), panel.bottom());
    // A static, bounded shadow: no blur pass, no animation.
    for (spread, alpha) in [(8.0, 16), (4.0, 28), (2.0, 48)] {
        painter.fill_rect(
            Rect::new(
                panel.x - spread,
                panel.y - spread,
                panel.width + spread * 2.0,
                panel.height + spread * 2.0,
            ),
            Color::rgba(0, 0, 0, alpha),
        );
    }
    let outline = |inset: f32| {
        Path::polygon(&[
            Point::new(panel.x + 10.0, panel.y + inset),
            Point::new(right - chamfer, panel.y + inset),
            Point::new(right - inset, panel.y + chamfer),
            Point::new(right - inset, bottom - 10.0),
            Point::new(right - 10.0, bottom - inset),
            Point::new(panel.x + inset, bottom - inset),
            Point::new(panel.x + inset, panel.y + 10.0),
        ])
    };
    painter.fill_path(&outline(0.0), SURFACE);
    painter.stroke_path(&outline(0.5), 1.0, Cap::Butt, EDGE);
    painter.fill_rect(Rect::new(panel.x + 10.0, panel.y, 138.0, 2.0), PINK);
    painter.fill_rect(Rect::new(right - 132.0, bottom - 1.0, 108.0, 1.0), CYAN);
}

/// Decorative traces along a frame's top right.
pub fn traces(painter: &mut Painter<'_>, panel: Rect) {
    let mut traces = Path::new();
    for i in 0..4 {
        let x = panel.right() - 62.0 + i as f32 * 9.0;
        traces.move_to(x, panel.y + 5.0);
        traces.line_to(x + 10.0, panel.y + 15.0);
    }
    painter.stroke_path(&traces, 2.0, Cap::Butt, EDGE);
}

/// The pink tag that names a panel, as COMMAND's does.
pub fn tag(painter: &mut Painter<'_>, rect: Rect, text: &str) {
    painter.fill_rect(rect, PINK);
    painter.label(text, rect, &theme::chrome(12.0), INSET, Align::Center);
}

/// A keycap: amber text on its own small face.
pub fn keycap(painter: &mut Painter<'_>, rect: Rect, text: &str) {
    painter.fill_rect(rect, KEY);
    painter.label(text, rect, &theme::chrome(9.5), AMBER, Align::Center);
}

/// The width a keycap needs for `text`.
pub fn keycap_width(painter: &mut Painter<'_>, text: &str) -> f32 {
    (painter.text.width(text, &theme::chrome(9.5)) + 12.0).round()
}

/// Text field faces: the inset surface, the etched edge, and the cyan left
/// edge while focused.
fn field_face(painter: &mut Painter<'_>, rect: Rect, focused: bool) {
    painter.fill_rect(rect, INSET);
    painter.stroke_rect(rect, 1.0, if focused { CYAN } else { EDGE });
    if focused {
        painter.fill_rect(Rect::new(rect.x, rect.y, 2.0, rect.height), CYAN);
    }
}

/// A one-line text field. While focused it shows `editor`, with its caret
/// and selection, scrolled to keep the caret in view; otherwise `text`.
/// Masked fields show a bullet per character.
pub fn text_field(
    painter: &mut Painter<'_>,
    rect: Rect,
    text: &str,
    editor: Option<&mut LineEditor>,
    masked: bool,
    placeholder: &str,
) {
    let focused = editor.is_some();
    field_face(painter, rect, focused);
    let field = Rect::new(
        rect.x + 9.0,
        rect.y,
        (rect.width - 16.0).max(0.0),
        rect.height,
    );
    let style = theme::body(12.5);
    let shown = |text: &str| {
        if masked {
            "\u{2022}".repeat(text.chars().count())
        } else {
            text.to_string()
        }
    };
    let Some(editor) = editor else {
        if text.is_empty() {
            painter.label(placeholder, field, &style, DISABLED, Align::Start);
        } else {
            painter.label(&shown(text), field, &style, TEXT, Align::Start);
        }
        return;
    };
    let content = editor.text().to_string();
    let (cursor, selection) = (editor.cursor(), editor.selection());
    let display = shown(&content);
    if display.is_empty() {
        painter.label(placeholder, field, &style, DISABLED, Align::Start);
    }
    let shaped = painter.text.shape(&display, &style);
    // A byte offset in the editor's text, as an x in the shaped display.
    let offset_of = |byte: usize| -> f32 {
        let byte = if masked {
            let chars = content[..byte.min(content.len())].chars().count();
            display
                .char_indices()
                .nth(chars)
                .map_or(display.len(), |(index, _)| index)
        } else {
            byte
        };
        shaped
            .glyphs
            .iter()
            .find(|glyph| glyph.cluster as usize >= byte)
            .map_or(shaped.advance, |glyph| glyph.x)
    };
    let caret = offset_of(cursor);
    if caret - editor.scroll > field.width - 4.0 {
        editor.scroll = caret - field.width + 4.0;
    }
    if caret < editor.scroll {
        editor.scroll = caret;
    }
    editor.scroll = editor
        .scroll
        .min((shaped.advance - field.width + 4.0).max(0.0))
        .max(0.0);
    let top = (field.y + (field.height - shaped.line_height) / 2.0).round();
    let x = field.x - editor.scroll;
    painter.save_clip(field);
    if let Some(range) = selection {
        let (start, end) = (offset_of(range.start), offset_of(range.end));
        painter.fill_rect(
            Rect::new(x + start, top, end - start, shaped.line_height),
            Color::rgba(0x6e, 0x4e, 0x9e, 140),
        );
    }
    painter.draw_text(&shaped, x, shaped.baseline(top).round(), TEXT);
    painter.fill_rect(
        Rect::new((x + caret).round(), top, 2.0, shaped.line_height),
        CYAN,
    );
    painter.restore();
}

/// A toggle: a square that fills cyan when on, then its text.
pub fn toggle(painter: &mut Painter<'_>, rect: Rect, on: bool, text: &str, focused: bool) {
    let size = 14.0;
    let square = Rect::new(
        rect.x + 2.0,
        (rect.y + (rect.height - size) / 2.0).round(),
        size,
        size,
    );
    if on {
        painter.fill_rect(square, CYAN);
        let mut check = Path::new();
        check.move_to(square.x + 3.0, square.y + 7.5);
        check.line_to(square.x + 6.0, square.y + 10.5);
        check.line_to(square.x + 11.0, square.y + 4.0);
        painter.stroke_path(&check, 1.8, Cap::Round, INSET);
    } else {
        painter.fill_rect(square, INSET);
        painter.stroke_rect(square, 1.0, EDGE);
    }
    if focused {
        painter.stroke_rect(square.inset(-3.0, -3.0), 1.0, CYAN);
    }
    if !text.is_empty() {
        let label = Rect::new(
            square.right() + 8.0,
            rect.y,
            (rect.right() - square.right() - 8.0).max(0.0),
            rect.height,
        );
        painter.label(
            text,
            label,
            &theme::body(12.5),
            if on { TEXT } else { MUTED },
            Align::Start,
        );
    }
}

/// A choice that cycles with the arrows: `◀ value ▶`.
pub fn choice(painter: &mut Painter<'_>, rect: Rect, value: &str, focused: bool) {
    field_face(painter, rect, focused);
    let arrows = if focused { CYAN } else { DIM };
    let style = theme::chrome(11.0);
    painter.label(
        "\u{25c0}",
        Rect::new(rect.x + 6.0, rect.y, 14.0, rect.height),
        &style,
        arrows,
        Align::Center,
    );
    painter.label(
        "\u{25b6}",
        Rect::new(rect.right() - 20.0, rect.y, 14.0, rect.height),
        &style,
        arrows,
        Align::Center,
    );
    let inner = Rect::new(
        rect.x + 24.0,
        rect.y,
        (rect.width - 48.0).max(0.0),
        rect.height,
    );
    painter.label(value, inner, &theme::body(12.5), TEXT, Align::Center);
}

/// A button. `caution` ones end something: pink when lit.
pub fn button(
    painter: &mut Painter<'_>,
    rect: Rect,
    text: &str,
    lit: bool,
    caution: bool,
    enabled: bool,
) {
    painter.fill_rect(rect, if lit && enabled { INSET } else { KEY });
    let edge = match (lit && enabled, caution) {
        (true, true) => PINK,
        (true, false) => CYAN,
        (false, _) => EDGE,
    };
    painter.stroke_rect(rect, 1.0, edge);
    let color = match (enabled, caution) {
        (false, _) => DISABLED,
        (true, true) => PINK,
        (true, false) => TEXT,
    };
    painter.label(text, rect, &theme::chrome(11.0), color, Align::Center);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Canvas, TextSystem};

    #[test]
    fn masked_fields_never_draw_their_text() {
        let mut canvas = Canvas::new(300, 40);
        let mut text = TextSystem::new();
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        let mut editor = LineEditor::default();
        editor.insert("hunter2");
        let rect = Rect::new(0.0, 0.0, 300.0, 40.0);
        text_field(&mut painter, rect, "", Some(&mut editor), true, "");
        let masked = canvas.pixels().to_vec();
        let mut bullets = LineEditor::default();
        bullets.insert(&"\u{2022}".repeat(7));
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        text_field(&mut painter, rect, "", Some(&mut bullets), false, "");
        assert_eq!(masked, canvas.pixels(), "seven bullets, caret at the end");
    }
}
