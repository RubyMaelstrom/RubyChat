//! Context menus in trust-desktop's design (its `context_menu.rs`): the
//! COMMAND panel—chamfered, etched, with a pink badge naming what the menu
//! acts on—then one row per item with a line icon, its label and an amber
//! keycap for its shortcut (DESIGN.md §8.10).

use crate::render::{Align, Cap, Color, Painter, Path, Point, Rect};

use super::theme::{
    self, AMBER, CYAN, DISABLED, EDGE, HEART, INSET, KEY, MUTED, PINK, RULE, SURFACE, TEXT,
};

const WIDTH: f32 = 252.0;
const HEADER: f32 = 32.0;
const ROW: f32 = 30.0;
const RULE_SPACE: f32 = 9.0;
const FOOT: f32 = 7.0;
/// The distance kept from the area's edges.
const MARGIN: f32 = 4.0;
/// The drop shadow's reach below and around the panel.
const SHADOW: f32 = 10.0;

/// The pictogram beside an item, in the menu's line style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuIcon {
    Copy,
    /// Two chain links: open or copy an address.
    Link,
    /// A speech bubble: talk privately.
    Chat,
    /// A circled i: who someone is.
    Info,
    Check,
    /// The terminal prompt: a command.
    Command,
    Reload,
    /// A stop sign: leave, close, kick.
    Leave,
    /// A membership prefix such as `@` or `+`, drawn as text.
    Prefix(char),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub icon: MenuIcon,
    /// The keyboard shortcut doing the same, shown as a keycap.
    pub shortcut: Option<&'static str>,
    pub enabled: bool,
    /// A rule separates this item from the one before it.
    pub separated: bool,
    /// The item ends something: an amber accent.
    pub caution: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, icon: MenuIcon) -> Self {
        Self {
            label: label.into(),
            icon,
            shortcut: None,
            enabled: true,
            separated: false,
            caution: false,
        }
    }

    pub fn shortcut(self, shortcut: &'static str) -> Self {
        Self {
            shortcut: Some(shortcut),
            ..self
        }
    }

    pub fn enabled(self, enabled: bool) -> Self {
        Self { enabled, ..self }
    }

    /// A rule above, and the amber accent of something that ends.
    pub fn caution(self) -> Self {
        Self {
            separated: true,
            caution: true,
            ..self
        }
    }

    pub fn separated(self) -> Self {
        Self {
            separated: true,
            ..self
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuGeometry {
    pub panel: Rect,
    /// One row per item, in item order.
    pub rows: Vec<Rect>,
}

impl MenuGeometry {
    /// Lays the menu out from the pointer, `anchor`: below and to the right
    /// of it where it fits, otherwise flipped above or to its left, and
    /// always inside `bounds`.
    pub fn new(anchor: Point, bounds: Rect, items: &[MenuItem]) -> Self {
        let height = HEADER
            + items
                .iter()
                .map(|item| ROW + if item.separated { RULE_SPACE } else { 0.0 })
                .sum::<f32>()
            + FOOT;
        let width = WIDTH.min((bounds.width - 2.0 * MARGIN).max(0.0));
        let fitted = height.min((bounds.height - 2.0 * MARGIN).max(0.0));
        let place = |at: f32, size: f32, start: f32, extent: f32| {
            let end = start + extent - MARGIN;
            let at = if at + size > end && at - size >= start + MARGIN {
                at - size
            } else {
                at
            };
            at.min(end - size).max(start + MARGIN)
        };
        let panel = Rect::new(
            place(anchor.x, width, bounds.x, bounds.width).round(),
            place(anchor.y, fitted, bounds.y, bounds.height).round(),
            width,
            height,
        );
        let mut y = panel.y + HEADER;
        let rows = items
            .iter()
            .map(|item| {
                if item.separated {
                    y += RULE_SPACE;
                }
                let row = Rect::new(panel.x + 6.0, y, width - 12.0, ROW);
                y += ROW;
                row
            })
            .collect();
        Self { panel, rows }
    }

    /// The item whose row is under `point`.
    pub fn item_at(&self, point: Point) -> Option<usize> {
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

/// The chamfered outline: small at the top left, larger at the top right.
fn outline(panel: Rect, grow: f32) -> Path {
    let (left, top, right, bottom) = (
        panel.x - grow,
        panel.y - grow,
        panel.right() + grow,
        panel.bottom() + grow,
    );
    Path::polygon(&[
        Point::new(left + 8.0, top),
        Point::new(right - 16.0, top),
        Point::new(right, top + 16.0),
        Point::new(right, bottom - 8.0),
        Point::new(right - 8.0, bottom),
        Point::new(left, bottom),
        Point::new(left, top + 8.0),
    ])
}

/// COMMAND's panel: a soft shadow, the chamfered surface and its etched
/// edge, the pink and cyan accents, traces, and a badge naming what the
/// menu acts on.
fn paint_panel(painter: &mut Painter<'_>, panel: Rect, badge: &str, title: &str) {
    // A static shadow in a few translucent steps: no blur pass, no timer.
    for (grow, alpha) in [(6.0, 18), (3.5, 26), (1.5, 34)] {
        let shadow = outline(panel.translate(0.0, 5.0), grow);
        painter.fill_path(&shadow, Color::rgba(0, 0, 0, alpha));
    }
    let shape = outline(panel, 0.0);
    painter.fill_path(&shape, SURFACE);
    painter.stroke_path(&shape, 1.0, Cap::Butt, EDGE);
    painter.fill_rect(Rect::new(panel.x + 8.0, panel.y, 64.0, 2.0), PINK);
    painter.fill_rect(
        Rect::new(panel.right() - 84.0, panel.bottom() - 1.0, 68.0, 1.0),
        CYAN,
    );
    for i in 0..3 {
        let x = panel.right() - 44.0 + i as f32 * 8.0;
        let mut trace = Path::new();
        trace.move_to(x, panel.y + 5.0);
        trace.line_to(x + 8.0, panel.y + 13.0);
        painter.stroke_path(&trace, 2.0, Cap::Butt, EDGE);
    }
    let style = theme::chrome(10.0);
    let width = (painter.text.width(badge, &style) + 20.0).max(46.0);
    let tag = Rect::new(panel.x + 10.0, panel.y + 9.0, width.round(), 16.0);
    painter.fill_rect(tag, PINK);
    painter.label(badge, tag, &style, INSET, Align::Center);
    let title_rect = Rect::new(
        tag.right() + 9.0,
        tag.y,
        (panel.width - tag.width - 76.0).max(0.0),
        tag.height,
    );
    painter.label(title, title_rect, &theme::body(11.0), MUTED, Align::Start);
}

/// Paints the menu; `highlighted` is the item under the pointer or the
/// keyboard's choice.
pub fn paint_context_menu(
    painter: &mut Painter<'_>,
    geometry: &MenuGeometry,
    items: &[MenuItem],
    highlighted: Option<usize>,
    badge: &str,
    title: &str,
) {
    paint_panel(painter, geometry.panel, badge, title);
    for (index, (item, row)) in items.iter().zip(&geometry.rows).enumerate() {
        if item.separated {
            painter.fill_rect(
                Rect::new(
                    row.x + 6.0,
                    (row.y - RULE_SPACE / 2.0 - 0.5).round(),
                    row.width - 12.0,
                    1.0,
                ),
                RULE,
            );
        }
        let active = item.enabled && highlighted == Some(index);
        if active {
            painter.fill_rect(*row, INSET);
            painter.stroke_rect(*row, 1.0, EDGE);
            painter.fill_rect(
                Rect::new(row.x, row.y, 2.0, row.height),
                if item.caution { AMBER } else { CYAN },
            );
        }
        let (icon_color, text_color) = match (item.enabled, active, item.caution) {
            (false, ..) => (DISABLED, DISABLED),
            (true, true, true) => (AMBER, HEART),
            (true, true, false) => (PINK, HEART),
            (true, false, true) => (AMBER, TEXT),
            (true, false, false) => (CYAN, TEXT),
        };
        paint_icon(
            painter,
            item.icon,
            Rect::new(row.x + 11.0, row.y + 8.0, 14.0, 14.0),
            icon_color,
        );
        let mut label_width = row.width - 46.0;
        if let Some(shortcut) = item.shortcut {
            let style = theme::chrome(10.0);
            let key_width = painter.text.width(shortcut, &style) + 12.0;
            let key = Rect::new(
                (row.right() - key_width - 8.0).round(),
                row.y + 6.0,
                key_width.round(),
                18.0,
            );
            painter.fill_rect(key, KEY);
            painter.label(
                shortcut,
                key,
                &style,
                if item.enabled { AMBER } else { DISABLED },
                Align::Center,
            );
            label_width -= key_width + 12.0;
        }
        let label = Rect::new(row.x + 36.0, row.y, label_width.max(0.0), row.height);
        painter.label(
            &item.label,
            label,
            &theme::body(12.5),
            text_color,
            Align::Start,
        );
    }
}

/// A line through `points`, offsets in `rect`, closed when `close`.
fn polyline(
    painter: &mut Painter<'_>,
    rect: Rect,
    points: &[(f32, f32)],
    close: bool,
    color: Color,
) {
    let mut path = Path::new();
    for (index, &(x, y)) in points.iter().enumerate() {
        if index == 0 {
            path.move_to(rect.x + x, rect.y + y);
        } else {
            path.line_to(rect.x + x, rect.y + y);
        }
    }
    if close {
        path.close();
    }
    painter.stroke_path(&path, 1.5, Cap::Round, color);
}

/// A path of cubic curves from `start` through each `(control, control,
/// end)`, offsets in `rect`.
fn curves(rect: Rect, start: (f32, f32), segments: &[[(f32, f32); 3]]) -> Path {
    let mut path = Path::new();
    path.move_to(rect.x + start.0, rect.y + start.1);
    for [a, b, c] in segments {
        path.curve_to(
            rect.x + a.0,
            rect.y + a.1,
            rect.x + b.0,
            rect.y + b.1,
            rect.x + c.0,
            rect.y + c.1,
        );
    }
    path
}

const OCTAGON: [(f32, f32); 8] = [
    (4.5, 1.0),
    (9.5, 1.0),
    (13.0, 4.5),
    (13.0, 9.5),
    (9.5, 13.0),
    (4.5, 13.0),
    (1.0, 9.5),
    (1.0, 4.5),
];

fn paint_icon(painter: &mut Painter<'_>, icon: MenuIcon, rect: Rect, color: Color) {
    let line = |painter: &mut Painter<'_>, points: &[(f32, f32)], close: bool| {
        polyline(painter, rect, points, close, color)
    };
    match icon {
        MenuIcon::Copy => {
            line(
                painter,
                &[(4.5, 4.5), (13.0, 4.5), (13.0, 13.5), (4.5, 13.5)],
                true,
            );
            line(
                painter,
                &[
                    (2.0, 10.5),
                    (1.0, 10.5),
                    (1.0, 0.75),
                    (9.5, 0.75),
                    (9.5, 2.0),
                ],
                false,
            );
        }
        MenuIcon::Link => {
            // Two rounded links of a chain, crossed on the diagonal.
            let (hw, hh, r) = (4.25, 2.25, 2.25);
            for (cx, cy) in [(4.75, 9.5), (9.25, 4.5)] {
                let (x, y) = (rect.x + cx, rect.y + cy);
                let mut link = Path::new();
                link.move_to(x - hw + r, y - hh);
                link.line_to(x + hw - r, y - hh);
                link.curve_to(
                    x + hw + 0.25,
                    y - hh,
                    x + hw + 0.25,
                    y + hh,
                    x + hw - r,
                    y + hh,
                );
                link.line_to(x - hw + r, y + hh);
                link.curve_to(
                    x - hw - 0.25,
                    y + hh,
                    x - hw - 0.25,
                    y - hh,
                    x - hw + r,
                    y - hh,
                );
                painter.stroke_path(&link, 1.5, Cap::Round, color);
            }
            line(painter, &[(5.5, 8.5), (8.5, 5.5)], false);
        }
        MenuIcon::Chat => line(
            painter,
            &[
                (1.0, 2.0),
                (13.0, 2.0),
                (13.0, 10.0),
                (6.0, 10.0),
                (3.0, 13.0),
                (3.0, 10.0),
                (1.0, 10.0),
            ],
            true,
        ),
        MenuIcon::Info => {
            // An octagon standing in for a circle, with an i inside.
            line(painter, &OCTAGON, true);
            line(painter, &[(7.0, 6.5), (7.0, 10.0)], false);
            line(painter, &[(7.0, 4.0), (7.0, 4.1)], false);
        }
        MenuIcon::Check => line(painter, &[(1.5, 7.5), (5.5, 11.5), (12.5, 3.0)], false),
        MenuIcon::Command => {
            line(painter, &[(1.5, 3.0), (6.0, 7.0), (1.5, 11.0)], false);
            line(painter, &[(7.5, 12.5), (13.0, 12.5)], false);
        }
        MenuIcon::Reload => {
            // Three quarters of a circle and its arrowhead.
            let (cx, cy, r) = (7.0, 7.5, 5.25);
            let k = 0.5523 * r;
            let mut arc = curves(
                rect,
                (cx + r, cy),
                &[
                    [(cx + r, cy + k), (cx + k, cy + r), (cx, cy + r)],
                    [(cx - k, cy + r), (cx - r, cy + k), (cx - r, cy)],
                    [(cx - r, cy - k), (cx - k, cy - r), (cx, cy - r)],
                ],
            );
            arc.line_to(rect.x + cx + 1.5, rect.y + cy - r);
            painter.stroke_path(&arc, 1.5, Cap::Round, color);
            line(
                painter,
                &[
                    (cx - 0.5, cy - r - 3.0),
                    (cx + 2.5, cy - r),
                    (cx - 0.5, cy - r + 3.0),
                ],
                false,
            );
        }
        MenuIcon::Leave => {
            line(painter, &OCTAGON, true);
            line(painter, &[(5.0, 5.0), (9.0, 9.0)], false);
            line(painter, &[(9.0, 5.0), (5.0, 9.0)], false);
        }
        MenuIcon::Prefix(prefix) => {
            let mut text = [0u8; 4];
            painter.label(
                prefix.encode_utf8(&mut text),
                rect.inset(-3.0, -3.0),
                &theme::chrome(12.0),
                color,
                Align::Center,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Canvas, TextSystem};

    fn items() -> Vec<MenuItem> {
        vec![
            MenuItem::new("Open a query", MenuIcon::Chat),
            MenuItem::new("Copy nick", MenuIcon::Copy).shortcut("Ctrl+C"),
            MenuItem::new("Kick", MenuIcon::Leave)
                .caution()
                .enabled(false),
        ]
    }

    #[test]
    fn menus_open_where_they_fit() {
        let bounds = Rect::new(0.0, 26.0, 800.0, 600.0);
        let items = items();
        let below = MenuGeometry::new(Point::new(100.0, 100.0), bounds, &items);
        assert_eq!((below.panel.x, below.panel.y), (100.0, 100.0));
        assert_eq!(below.panel.height, HEADER + 3.0 * ROW + RULE_SPACE + FOOT);
        assert_eq!(below.item_at(below.rows[1].center()), Some(1));
        assert_eq!(
            below.item_at(Point::new(below.panel.x + 2.0, below.panel.y + 4.0)),
            None,
            "the header"
        );
        let corner = MenuGeometry::new(Point::new(790.0, 620.0), bounds, &items);
        assert!(
            corner.panel.right() <= bounds.right() - MARGIN
                && corner.panel.bottom() <= bounds.bottom() - MARGIN
        );
        assert_eq!(corner.panel.right(), 790.0, "flipped to the pointer's left");
    }

    #[test]
    fn paints_within_its_damage() {
        let mut canvas = Canvas::new(400, 300);
        let mut text = TextSystem::new();
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        let items = items();
        let geometry = MenuGeometry::new(
            Point::new(40.0, 30.0),
            Rect::new(0.0, 0.0, 400.0, 300.0),
            &items,
        );
        paint_context_menu(&mut painter, &geometry, &items, Some(0), "NICK", "alice");
        let damage = geometry.damage().snap(1.0);
        for y in 0..300 {
            for x in 0..400 {
                if canvas.pixel(x, y) != 0 {
                    assert!(
                        damage.contains_rect(crate::render::IRect::new(
                            x as i32,
                            y as i32,
                            x as i32 + 1,
                            y as i32 + 1
                        )),
                        "ink at ({x}, {y})"
                    );
                }
            }
        }
        assert_eq!(
            canvas.pixel(geometry.panel.x as u32 + 30, geometry.panel.y as u32 + 1),
            PINK.pixel()
        );
    }
}

#[cfg(test)]
mod snapshots {
    use super::*;
    use crate::render::{Canvas, TextSystem};

    /// `RUBYCHAT_SNAPSHOTS=DIR cargo test --lib menu_snapshots -- --ignored`
    /// writes a nick menu at 1× and 2×.
    #[test]
    #[ignore = "writes snapshot images; set RUBYCHAT_SNAPSHOTS"]
    fn menu_snapshots() {
        let Some(dir) = std::env::var_os("RUBYCHAT_SNAPSHOTS") else {
            return;
        };
        let items = vec![
            MenuItem::new("Open a query", MenuIcon::Chat),
            MenuItem::new("Who is this?", MenuIcon::Info),
            MenuItem::new("Copy nick", MenuIcon::Copy).shortcut("Ctrl+C"),
            MenuItem::new("Make operator", MenuIcon::Prefix('@')).separated(),
            MenuItem::new("Give voice", MenuIcon::Prefix('+')).enabled(false),
            MenuItem::new("Kick from channel", MenuIcon::Leave).caution(),
        ];
        let mut text = TextSystem::new();
        for scale in [1.0f32, 2.0] {
            let (width, height) = (320.0, 300.0);
            let mut canvas = Canvas::new((width * scale) as u32, (height * scale) as u32);
            let mut painter = Painter::new(&mut canvas, &mut text, scale);
            painter.fill_rect(
                Rect::new(0.0, 0.0, width, height),
                super::super::theme::CANVAS,
            );
            let geometry = MenuGeometry::new(
                Point::new(30.0, 20.0),
                Rect::new(0.0, 0.0, width, height),
                &items,
            );
            paint_context_menu(&mut painter, &geometry, &items, Some(1), "NICK", "alice");
            let path = std::path::Path::new(&dir).join(format!("context-menu-{scale}x.png"));
            crate::render::png::write_rgba(
                &path,
                canvas.width(),
                canvas.height(),
                &canvas.to_rgba(),
            )
            .unwrap();
        }
    }
}
