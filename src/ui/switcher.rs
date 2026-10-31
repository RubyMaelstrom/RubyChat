//! The quick switcher (DESIGN.md §8.10): Ctrl+K lists the buffers of every
//! network in COMMAND's panel, just above the input row, which takes what
//! to look for. The best matches come first; with nothing typed, the most
//! active.

use crate::model::buffer::{Activity, BufferKind};
use crate::render::{Align, Painter, Point, Rect};

use super::context_menu::{MenuIcon, SHADOW, paint_icon, paint_panel};
use super::header::thousands;
use super::theme::{self, AMBER, CYAN, DIM, DISABLED, EDGE, HEART, INSET, MUTED, PINK, TEXT};

const WIDTH: f32 = 400.0;
const HEADER: f32 = 32.0;
const ROW: f32 = 30.0;
const FOOT: f32 = 7.0;
/// Rows shown at most; typing narrows the rest.
pub const MAX_ROWS: usize = 10;

/// A buffer as the switcher lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchRow {
    pub name: String,
    pub network: String,
    pub kind: BufferKind,
    pub highlights: u32,
    pub unread: u32,
    pub activity: Activity,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwitcherGeometry {
    pub panel: Rect,
    pub rows: Vec<Rect>,
}

impl SwitcherGeometry {
    /// Rows for `count` buffers (one, saying there are none, for none),
    /// above the input field `field` and at its left, inside `bounds`.
    pub fn new(field: Rect, bounds: Rect, count: usize) -> Self {
        let count = count.clamp(1, MAX_ROWS);
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

/// Paints the switcher: `rows` in order, `selected` the one Enter shows,
/// out of `total` buffers.
pub fn paint_switcher(
    painter: &mut Painter<'_>,
    geometry: &SwitcherGeometry,
    rows: &[SwitchRow],
    selected: usize,
    total: usize,
) {
    let title = format!(
        "{} buffer{}",
        thousands(total),
        if total == 1 { "" } else { "s" }
    );
    paint_panel(painter, geometry.panel, "SWITCH", &title);
    if rows.is_empty()
        && let Some(row) = geometry.rows.first()
    {
        painter.label(
            "NO BUFFER MATCHES",
            row.inset(36.0, 0.0),
            &theme::chrome(10.0),
            DISABLED,
            Align::Start,
        );
    }
    for (index, (item, row)) in rows.iter().zip(&geometry.rows).enumerate() {
        let active = index == selected;
        if active {
            painter.fill_rect(*row, INSET);
            painter.stroke_rect(*row, 1.0, EDGE);
            painter.fill_rect(Rect::new(row.x, row.y, 2.0, row.height), CYAN);
        }
        let icon = match item.kind {
            BufferKind::Server => MenuIcon::Command,
            BufferKind::Channel => MenuIcon::Prefix('#'),
            BufferKind::Query => MenuIcon::Chat,
        };
        paint_icon(
            painter,
            icon,
            Rect::new(row.x + 11.0, row.y + 8.0, 14.0, 14.0),
            if active { PINK } else { CYAN },
        );
        // From the right: a count of what's unread, then the network.
        let mut right = row.right() - 8.0;
        let count_style = theme::chrome(10.0);
        let count = match (item.highlights, item.unread, item.activity) {
            (0, 0, Activity::Events) => Some((String::from("·"), DIM)),
            (0, 0, _) => None,
            (0, unread, _) => Some((thousands(unread as usize), AMBER)),
            (highlights, ..) => Some((thousands(highlights as usize), PINK)),
        };
        if let Some((count, color)) = count {
            let width = painter.text.width(&count, &count_style) + 4.0;
            painter.label(
                &count,
                Rect::new(right - width, row.y, width, row.height),
                &count_style,
                color,
                Align::End,
            );
            right -= width + 10.0;
        }
        let network_style = theme::chrome(9.0);
        let network_width = painter
            .text
            .width(&item.network, &network_style)
            .min(row.width * 0.4);
        painter.label(
            &item.network,
            Rect::new(right - network_width, row.y, network_width, row.height),
            &network_style,
            MUTED,
            Align::End,
        );
        right -= network_width + 12.0;
        let name = Rect::new(
            row.x + 36.0,
            row.y,
            (right - row.x - 36.0).max(0.0),
            row.height,
        );
        // As in the sidebar, the icon says it's a channel.
        let label = match item.kind {
            BufferKind::Channel => item.name.strip_prefix('#').unwrap_or(&item.name),
            _ => &item.name,
        };
        painter.label(
            label,
            name,
            &theme::body(12.5),
            if active { HEART } else { TEXT },
            Align::Start,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RUBYCHAT_SNAPSHOTS=DIR cargo test --lib switcher_snapshots --
    /// --ignored` writes the switcher over a canvas, as Ctrl+K opens it.
    #[test]
    #[ignore = "writes snapshot images; set RUBYCHAT_SNAPSHOTS"]
    fn switcher_snapshots() {
        use crate::model::editor::LineEditor;
        use crate::render::{Canvas, TextSystem};
        use crate::ui::input::paint_input;
        use crate::ui::theme::CANVAS;

        let Some(dir) = std::env::var_os("RUBYCHAT_SNAPSHOTS") else {
            return;
        };
        let (width, height) = (900.0, 480.0);
        let mut canvas = Canvas::new(width as u32, height as u32);
        let mut text = TextSystem::new();
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        painter.fill_rect(Rect::new(0.0, 0.0, width, height), CANVAS);
        let field = Rect::new(12.0, height - 44.0, width - 24.0, 36.0);
        let mut editor = LineEditor::default();
        paint_input(
            &mut painter,
            field,
            &mut editor,
            "SWITCH\u{203a}",
            "Buffer or network · Enter shows it · Esc closes",
            true,
            None,
        );
        let row = |name: &str, network: &str, kind, highlights, unread, activity| SwitchRow {
            name: name.to_string(),
            network: network.to_string(),
            kind,
            highlights,
            unread,
            activity,
        };
        let rows = [
            row(
                "#debian",
                "OFTC",
                BufferKind::Channel,
                2,
                5,
                Activity::Highlight,
            ),
            row(
                "alice",
                "Libera.Chat",
                BufferKind::Query,
                0,
                3,
                Activity::Messages,
            ),
            row(
                "#linux",
                "Libera.Chat",
                BufferKind::Channel,
                0,
                41,
                Activity::Messages,
            ),
            row(
                "#labwc",
                "Libera.Chat",
                BufferKind::Channel,
                0,
                0,
                Activity::Events,
            ),
            row(
                "Libera.Chat",
                "Libera.Chat",
                BufferKind::Server,
                0,
                0,
                Activity::None,
            ),
            row(
                "#rust",
                "Libera.Chat",
                BufferKind::Channel,
                0,
                0,
                Activity::None,
            ),
        ];
        let geometry = SwitcherGeometry::new(field, Rect::new(0.0, 0.0, width, height), rows.len());
        paint_switcher(&mut painter, &geometry, &rows, 1, 9);
        crate::render::png::write_rgba(
            &std::path::PathBuf::from(dir).join("switcher.png"),
            canvas.width(),
            canvas.height(),
            &canvas.to_rgba(),
        )
        .unwrap();
    }

    #[test]
    fn the_panel_sits_above_the_input_and_rows_are_found() {
        let bounds = Rect::new(0.0, 26.0, 1100.0, 694.0);
        let field = Rect::new(228.0, 676.0, 680.0, 36.0);
        let geometry = SwitcherGeometry::new(field, bounds, 3);
        assert!(geometry.panel.bottom() <= field.y);
        assert_eq!(geometry.panel.x, field.x);
        assert_eq!(geometry.rows.len(), 3);
        let second = geometry.rows[1];
        assert_eq!(
            geometry.row_at(Point::new(second.x + 20.0, second.y + 5.0)),
            Some(1)
        );
        assert_eq!(
            SwitcherGeometry::new(field, bounds, 0).rows.len(),
            1,
            "a row to say nothing matches"
        );
        assert_eq!(
            SwitcherGeometry::new(field, bounds, 50).rows.len(),
            MAX_ROWS
        );
    }
}
