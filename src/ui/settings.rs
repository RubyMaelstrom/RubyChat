//! The settings panel (DESIGN.md §8.10): every setting in COMMAND's panel
//! above the input row, with its value. Switches flip in place; words are
//! edited in the input row.

use crate::config::settings::{Key, Settings};
use crate::render::{Align, Painter, Point, Rect};

use super::context_menu::{MenuIcon, SHADOW, paint_icon, paint_panel};
use super::theme::{self, CYAN, DISABLED, EDGE, HEART, INSET, MUTED, PINK, TEXT};

const WIDTH: f32 = 520.0;
const HEADER: f32 = 32.0;
const ROW: f32 = 30.0;
const FOOT: f32 = 34.0;

/// A setting's name as the panel shows it.
pub fn label(key: Key) -> &'static str {
    match key {
        Key::SmartFilter => "Smart filter",
        Key::Seconds => "Seconds in times",
        Key::HighlightWords => "Highlight words",
        Key::Notifications => "Notifications",
        Key::CloseToTray => "Close to the tray",
        Key::Logging => "Chat logs",
        Key::FallbackFonts => "Fallback fonts",
        Key::LinkCommand => "Open links with",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsGeometry {
    pub panel: Rect,
    pub rows: Vec<Rect>,
}

impl SettingsGeometry {
    /// Above the input field `field` and at its left, inside `bounds`.
    pub fn new(field: Rect, bounds: Rect) -> Self {
        let count = Key::ALL.len();
        let height = HEADER + ROW * count as f32 + FOOT;
        let width = WIDTH.min((bounds.width - 8.0).max(0.0));
        let x = field
            .x
            .min(bounds.right() - width - 4.0)
            .max(bounds.x + 4.0);
        let y = (field.y - 10.0 - height).max(bounds.y + 4.0);
        let panel = Rect::new(x.round(), y.round(), width, height);
        let rows = (0..count)
            .map(|index| {
                Rect::new(
                    panel.x + 6.0,
                    panel.y + HEADER + ROW * index as f32,
                    width - 12.0,
                    ROW,
                )
            })
            .collect();
        Self { panel, rows }
    }

    pub fn row_at(&self, point: Point) -> Option<usize> {
        self.rows.iter().position(|row| row.contains(point))
    }

    /// The panel with its shadow: what showing or hiding it changes.
    pub fn damage(&self) -> Rect {
        let panel = self.panel;
        Rect::new(
            panel.x - SHADOW,
            panel.y - SHADOW / 2.0,
            panel.width + 2.0 * SHADOW,
            panel.height + 2.0 * SHADOW,
        )
    }
}

pub fn paint_settings(
    painter: &mut Painter<'_>,
    geometry: &SettingsGeometry,
    settings: &Settings,
    selected: usize,
    editing: bool,
) {
    paint_panel(painter, geometry.panel, "SETTINGS", "settings.toml");
    for (index, (key, row)) in Key::ALL.into_iter().zip(&geometry.rows).enumerate() {
        let active = index == selected;
        if active {
            painter.fill_rect(*row, INSET);
            painter.stroke_rect(*row, 1.0, EDGE);
            painter.fill_rect(Rect::new(row.x, row.y, 2.0, row.height), CYAN);
        }
        let icon = if key.is_switch() {
            MenuIcon::Check
        } else {
            MenuIcon::Command
        };
        let on = key.is_switch() && settings.switch(key);
        paint_icon(
            painter,
            icon,
            Rect::new(row.x + 11.0, row.y + 8.0, 14.0, 14.0),
            match (key.is_switch(), on) {
                (true, true) => CYAN,
                (true, false) => DISABLED,
                (false, _) => PINK,
            },
        );
        painter.label(
            label(key),
            Rect::new(row.x + 36.0, row.y, 170.0, row.height),
            &theme::body(12.5),
            if active { HEART } else { TEXT },
            Align::Start,
        );
        let value = if key.is_switch() {
            String::from(if on { "ON" } else { "OFF" })
        } else {
            settings.value(key)
        };
        let (style, color) = if key.is_switch() {
            (theme::chrome(10.0), if on { CYAN } else { DISABLED })
        } else {
            (theme::body(11.5), MUTED)
        };
        painter.label(
            &value,
            Rect::new(row.x + 214.0, row.y, row.width - 222.0, row.height),
            &style,
            color,
            Align::End,
        );
    }
    // What the chosen setting does; the input row says how to change it.
    let key = Key::ALL[selected.min(Key::ALL.len() - 1)];
    let foot = Rect::new(
        geometry.panel.x + 16.0,
        geometry.panel.bottom() - FOOT,
        geometry.panel.width - 32.0,
        FOOT - 8.0,
    );
    let color = if editing { TEXT } else { MUTED };
    painter.label(
        key.description(),
        foot,
        &theme::body(11.0),
        color,
        Align::Start,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RUBYCHAT_SNAPSHOTS=DIR cargo test --lib settings_snapshots --
    /// --ignored` writes the panel as `/settings` opens it.
    #[test]
    #[ignore = "writes snapshot images; set RUBYCHAT_SNAPSHOTS"]
    fn settings_snapshots() {
        use crate::model::editor::LineEditor;
        use crate::render::{Canvas, TextSystem};
        use crate::ui::input::paint_input;
        use crate::ui::theme::CANVAS;

        let Some(dir) = std::env::var_os("RUBYCHAT_SNAPSHOTS") else {
            return;
        };
        let (width, height) = (900.0, 420.0);
        let mut canvas = Canvas::new(width as u32, height as u32);
        let mut text = TextSystem::new();
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        painter.fill_rect(Rect::new(0.0, 0.0, width, height), CANVAS);
        let field = Rect::new(12.0, height - 44.0, width - 24.0, 36.0);
        let settings = Settings {
            highlight_words: vec![String::from("rubychat"), String::from("ferris")],
            seconds: true,
            ..Settings::default()
        };
        paint_input(
            &mut painter,
            field,
            &mut LineEditor::default(),
            "SETTINGS\u{203a}",
            "\u{2191}\u{2193} choose · Enter or Space changes · Esc closes",
            true,
            None,
        );
        let geometry = SettingsGeometry::new(field, Rect::new(0.0, 0.0, width, height));
        paint_settings(&mut painter, &geometry, &settings, 2, false);
        crate::render::png::write_rgba(
            &std::path::PathBuf::from(dir).join("settings.png"),
            canvas.width(),
            canvas.height(),
            &canvas.to_rgba(),
        )
        .unwrap();
    }

    #[test]
    fn every_setting_has_a_row_above_the_input() {
        let bounds = Rect::new(0.0, 26.0, 1100.0, 694.0);
        let field = Rect::new(228.0, 676.0, 680.0, 36.0);
        let geometry = SettingsGeometry::new(field, bounds);
        assert_eq!(geometry.rows.len(), Key::ALL.len());
        assert!(geometry.panel.bottom() <= field.y);
        let third = geometry.rows[2];
        assert_eq!(
            geometry.row_at(Point::new(third.x + 50.0, third.y + 3.0)),
            Some(2)
        );
    }
}
