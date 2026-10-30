//! The input line, painted as COMMAND's input field: an inset surface with
//! the cyan prompt. Editing lives in `model::editor`.

pub use crate::model::editor::LineEditor;
use crate::render::{Align, Color, Painter, Rect, TextStyle};

use super::theme::{self, AMBER, CYAN, DIM, DISABLED, EDGE, INSET, TEXT};

/// The input field's text style: the chat face.
pub fn input_style() -> TextStyle {
    super::chat_view::chat_style()
}

/// Paints COMMAND's input field in `rect`: inset surface, etched edge, cyan
/// left edge and prompt, the text scrolled to keep the caret in view, and a
/// steady caret (no blinking, so an idle window never wakes). A `note`
/// (search's NO MATCHES) sits at the right end.
pub fn paint_input(
    painter: &mut Painter<'_>,
    rect: Rect,
    editor: &mut LineEditor,
    prompt: &str,
    placeholder: &str,
    focused: bool,
    note: Option<&str>,
) {
    painter.fill_rect(rect, INSET);
    painter.stroke_rect(rect, 1.0, EDGE);
    painter.fill_rect(
        Rect::new(rect.x, rect.y, 2.0, rect.height),
        if focused { CYAN } else { DIM },
    );
    let prompt_style = theme::chrome(theme::PROMPT_SIZE);
    let prompt_width = painter.text.width(prompt, &prompt_style);
    painter.label(
        prompt,
        Rect::new(rect.x + 12.0, rect.y, prompt_width + 1.0, rect.height),
        &prompt_style,
        CYAN,
        Align::Start,
    );
    let note_style = theme::chrome(9.0);
    let note_width = note.map_or(0.0, |note| painter.text.width(note, &note_style) + 16.0);
    if let Some(note) = note {
        painter.label(
            note,
            Rect::new(
                rect.right() - note_width,
                rect.y,
                note_width - 12.0,
                rect.height,
            ),
            &note_style,
            AMBER,
            Align::End,
        );
    }
    let field = Rect::new(
        rect.x + 22.0 + prompt_width,
        rect.y,
        (rect.width - prompt_width - 34.0 - note_width).max(0.0),
        rect.height,
    );
    let style = input_style();
    if editor.text().is_empty() {
        painter.label(placeholder, field, &style, DISABLED, Align::Start);
    }
    let shaped = painter.text.shape(editor.text(), &style);
    let offset_of = |byte: usize| -> f32 {
        shaped
            .glyphs
            .iter()
            .find(|glyph| glyph.cluster as usize >= byte)
            .map_or(shaped.advance, |glyph| glyph.x)
    };
    let caret = offset_of(editor.cursor());
    // Keep the caret inside the field with a little room to its right.
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
    if let Some(range) = editor.selection() {
        let (start, end) = (offset_of(range.start), offset_of(range.end));
        painter.fill_rect(
            Rect::new(x + start, top, end - start, shaped.line_height),
            Color::rgba(0x6e, 0x4e, 0x9e, 140),
        );
    }
    painter.draw_text(&shaped, x, shaped.baseline(top).round(), TEXT);
    if focused {
        painter.fill_rect(
            Rect::new((x + caret).round(), top, 2.0, shaped.line_height),
            CYAN,
        );
    }
    painter.restore();
}
