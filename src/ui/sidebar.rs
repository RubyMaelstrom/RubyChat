//! The sidebar: the active network's buffers (DESIGN.md §8.4). A header
//! names the network above its connection status; then come the server
//! buffer, CHANNELS and QUERIES, each row with an icon, its name and an
//! activity badge; `+ JOIN` sits at the foot.

use crate::model::buffer::{Activity, Buffer, BufferKind};
use crate::render::{Align, Cap, Color, Painter, Path, Point, Rect};

use super::theme::{
    self, AMBER, CYAN, DIM, DISABLED, EDGE, HEART, HOVER, KEY, MUTED, PINK, RULE, SURFACE, TEXT,
};

pub const SIDEBAR_WIDTH: f32 = 216.0;
const HEADER_HEIGHT: f32 = 54.0;
const ROW_HEIGHT: f32 = 26.0;
const SECTION_HEIGHT: f32 = 28.0;
const FOOTER_HEIGHT: f32 = 38.0;

/// A network's connection, as the header's status line shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub text: String,
    pub color: Color,
}

/// Something in the sidebar the pointer can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarPart {
    /// A buffer, by its index in the network.
    Buffer(usize),
    Join,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarItem {
    Section(&'static str),
    Buffer(usize),
}

/// Buffer indices in sidebar order: the server buffer, then channels, then
/// queries, each in the order they were opened.
pub fn order(buffers: &[Buffer]) -> Vec<usize> {
    let of =
        |kind: BufferKind| (0..buffers.len()).filter(move |&index| buffers[index].kind == kind);
    of(BufferKind::Server)
        .chain(of(BufferKind::Channel))
        .chain(of(BufferKind::Query))
        .collect()
}

/// Where the sidebar's parts are.
#[derive(Clone, Debug, PartialEq)]
pub struct SidebarLayout {
    pub rect: Rect,
    pub header: Rect,
    /// The scrolling list between the header and the footer.
    pub list: Rect,
    /// Every item, placed after scrolling; some may lie outside `list`.
    pub items: Vec<(SidebarItem, Rect)>,
    pub footer: Rect,
    pub scroll: f32,
    pub max_scroll: f32,
}

impl SidebarLayout {
    pub fn new(rect: Rect, buffers: &[Buffer], scroll: f32) -> Self {
        let header = Rect::new(rect.x, rect.y, rect.width, HEADER_HEIGHT.min(rect.height));
        let footer_height = FOOTER_HEIGHT.min((rect.height - header.height).max(0.0));
        let footer = Rect::new(
            rect.x,
            rect.bottom() - footer_height,
            rect.width,
            footer_height,
        );
        let list = Rect::new(
            rect.x,
            header.bottom(),
            rect.width,
            (footer.y - header.bottom()).max(0.0),
        );
        let mut entries: Vec<(SidebarItem, f32)> = Vec::new();
        let mut section = None;
        for index in order(buffers) {
            let kind = buffers[index].kind;
            let name = match kind {
                BufferKind::Server => None,
                BufferKind::Channel => Some("CHANNELS"),
                BufferKind::Query => Some("QUERIES"),
            };
            if name.is_some() && name != section {
                section = name;
                entries.push((
                    SidebarItem::Section(name.unwrap_or_default()),
                    SECTION_HEIGHT,
                ));
            }
            entries.push((SidebarItem::Buffer(index), ROW_HEIGHT));
        }
        let content: f32 = entries.iter().map(|(_, height)| height).sum::<f32>() + 6.0;
        let max_scroll = (content - list.height).max(0.0);
        let scroll = scroll.clamp(0.0, max_scroll);
        let mut y = list.y + 6.0 - scroll;
        let items = entries
            .into_iter()
            .map(|(item, height)| {
                let rect = Rect::new(rect.x, y, rect.width - 1.0, height);
                y += height;
                (item, rect)
            })
            .collect();
        Self {
            rect,
            header,
            list,
            items,
            footer,
            scroll,
            max_scroll,
        }
    }

    pub fn part_at(&self, point: Point) -> Option<SidebarPart> {
        if self.footer.contains(point) {
            return Some(SidebarPart::Join);
        }
        if !self.list.contains(point) {
            return None;
        }
        self.items.iter().find_map(|(item, rect)| match item {
            SidebarItem::Buffer(index) if rect.contains(point) => Some(SidebarPart::Buffer(*index)),
            _ => None,
        })
    }

    /// The scroll that brings buffer `index`'s row into view.
    pub fn reveal(&self, index: usize) -> f32 {
        let Some((_, row)) = self
            .items
            .iter()
            .find(|(item, _)| *item == SidebarItem::Buffer(index))
        else {
            return self.scroll;
        };
        if row.y < self.list.y {
            self.scroll - (self.list.y - row.y)
        } else if row.bottom() > self.list.bottom() {
            self.scroll + (row.bottom() - self.list.bottom())
        } else {
            self.scroll
        }
        .clamp(0.0, self.max_scroll)
    }
}

/// What the sidebar shows.
pub struct SidebarView<'a> {
    pub title: &'a str,
    pub status: &'a Status,
    pub buffers: &'a [Buffer],
    /// The server buffer's row: the server's address.
    pub server: &'a str,
    pub active: usize,
    pub hovered: Option<SidebarPart>,
    pub focused: bool,
}

/// trust-desktop's terminal glyph, a channel's `#`, or a query's diamond.
fn paint_icon(painter: &mut Painter<'_>, kind: BufferKind, rect: Rect, color: Color) {
    match kind {
        BufferKind::Server => painter.label(">_", rect, &theme::chrome(10.0), color, Align::Center),
        BufferKind::Channel => painter.label("#", rect, &theme::chrome(12.0), color, Align::Center),
        BufferKind::Query => {
            let center = rect.center();
            let r = 4.0;
            let path = Path::polygon(&[
                Point::new(center.x, center.y - r),
                Point::new(center.x + r, center.y),
                Point::new(center.x, center.y + r),
                Point::new(center.x - r, center.y),
            ]);
            painter.stroke_path(&path, 1.2, Cap::Butt, color);
        }
    }
}

/// A count as the badges show it: 1 to 999, then 999+.
fn count(n: u32) -> String {
    if n > 999 {
        String::from("999+")
    } else {
        n.to_string()
    }
}

pub fn paint_sidebar(painter: &mut Painter<'_>, layout: &SidebarLayout, view: &SidebarView<'_>) {
    let rect = layout.rect;
    painter.fill_rect(rect, SURFACE);
    painter.fill_rect(
        Rect::new(rect.right() - 1.0, rect.y, 1.0, rect.height),
        EDGE,
    );
    let accent = if view.focused { PINK } else { DIM };
    // The network, in small caps, over its connection.
    let header = layout.header;
    painter.label(
        &view.title.to_uppercase(),
        Rect::new(header.x + 14.0, header.y + 10.0, header.width - 28.0, 16.0),
        &theme::chrome(11.5),
        TEXT,
        Align::Start,
    );
    let status_row = Rect::new(header.x + 14.0, header.y + 29.0, header.width - 28.0, 14.0);
    painter.fill_rect(
        Rect::new(
            status_row.x,
            (status_row.center().y - 3.0).round(),
            6.0,
            6.0,
        ),
        view.status.color,
    );
    painter.label(
        &view.status.text,
        Rect::new(
            status_row.x + 12.0,
            status_row.y,
            status_row.width - 12.0,
            status_row.height,
        ),
        &theme::chrome(9.5),
        view.status.color,
        Align::Start,
    );
    painter.fill_rect(
        Rect::new(
            header.x + 14.0,
            header.bottom() - 1.0,
            header.width - 28.0,
            1.0,
        ),
        RULE,
    );
    painter.save_clip(layout.list);
    for (item, row) in &layout.items {
        if row.bottom() <= layout.list.y || row.y >= layout.list.bottom() {
            continue;
        }
        match *item {
            SidebarItem::Section(name) => {
                painter.label(
                    name,
                    Rect::new(row.x + 14.0, row.y + 10.0, row.width - 28.0, 14.0),
                    &theme::chrome(9.0),
                    MUTED,
                    Align::Start,
                );
            }
            SidebarItem::Buffer(index) => {
                let buffer = &view.buffers[index];
                let active = index == view.active;
                let hovered = view.hovered == Some(SidebarPart::Buffer(index));
                if active {
                    painter.fill_rect(*row, HOVER);
                    painter.fill_rect(Rect::new(row.x, row.y, 2.0, row.height), accent);
                } else if hovered {
                    painter.fill_rect(*row, HOVER.with_alpha(150));
                    painter.fill_rect(Rect::new(row.x, row.y, 1.0, row.height), CYAN);
                }
                // Channels you've left, and people MONITOR says are offline.
                let left = (buffer.kind == BufferKind::Channel && !buffer.joined)
                    || (buffer.kind == BufferKind::Query && buffer.peer.online == Some(false));
                let color = if left {
                    DISABLED
                } else if active || buffer.activity >= Activity::Messages {
                    TEXT
                } else {
                    MUTED
                };
                paint_icon(
                    painter,
                    buffer.kind,
                    Rect::new(row.x + 12.0, row.y, 16.0, row.height),
                    if active { CYAN } else { color },
                );
                let name = if buffer.kind == BufferKind::Server {
                    view.server
                } else {
                    buffer
                        .name
                        .strip_prefix('#')
                        .filter(|_| buffer.kind == BufferKind::Channel)
                        .unwrap_or(&buffer.name)
                };
                // The badge, right-aligned, then the name in what is left.
                let style = theme::chrome(9.5);
                let mut right = row.right() - 12.0;
                if buffer.highlights > 0 {
                    let text = format!("@{}", count(buffer.highlights));
                    let width = painter.text.width(&text, &style) + 10.0;
                    let pill = Rect::new(
                        (right - width).round(),
                        (row.center().y - 8.0).round(),
                        width.round(),
                        16.0,
                    );
                    painter.fill_rect(pill, if view.focused { PINK } else { DIM });
                    painter.label(&text, pill, &style, HEART, Align::Center);
                    right = pill.x - 6.0;
                } else if buffer.unread > 0 {
                    let text = count(buffer.unread);
                    let width = painter.text.width(&text, &style);
                    painter.label(
                        &text,
                        Rect::new(right - width, row.y, width + 1.0, row.height),
                        &style,
                        CYAN,
                        Align::End,
                    );
                    right -= width + 8.0;
                } else if buffer.activity == Activity::Events {
                    painter.fill_rect(
                        Rect::new(right - 4.0, (row.center().y - 2.0).round(), 4.0, 4.0),
                        DIM,
                    );
                    right -= 12.0;
                }
                let label_x = row.x + 34.0;
                painter.label(
                    name,
                    Rect::new(label_x, row.y, (right - label_x).max(0.0), row.height),
                    &theme::chrome(11.5),
                    color,
                    Align::Start,
                );
            }
        }
    }
    painter.restore();
    // + JOIN, with its keycap.
    let footer = layout.footer;
    if footer.height > 0.0 {
        painter.fill_rect(
            Rect::new(footer.x + 14.0, footer.y, footer.width - 28.0, 1.0),
            RULE,
        );
        let hovered = view.hovered == Some(SidebarPart::Join);
        let color = if hovered { CYAN } else { MUTED };
        painter.label(
            "+ JOIN",
            Rect::new(footer.x + 14.0, footer.y + 1.0, 80.0, footer.height - 1.0),
            &theme::chrome(10.5),
            color,
            Align::Start,
        );
        let style = theme::chrome(9.0);
        let width = painter.text.width("Ctrl+J", &style) + 10.0;
        let key = Rect::new(
            (footer.right() - 14.0 - width).round(),
            (footer.center().y - 8.0).round(),
            width.round(),
            16.0,
        );
        painter.fill_rect(key, KEY);
        painter.label("Ctrl+J", key, &style, AMBER, Align::Center);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Canvas, TextSystem};

    fn buffers() -> Vec<Buffer> {
        vec![
            Buffer::new(BufferKind::Server, "Libera.Chat", ""),
            Buffer::new(BufferKind::Query, "alice", "alice"),
            Buffer::new(BufferKind::Channel, "#rust", "#rust"),
            Buffer::new(BufferKind::Channel, "#labwc", "#labwc"),
        ]
    }

    #[test]
    fn rows_group_channels_before_queries() {
        let buffers = buffers();
        assert_eq!(order(&buffers), [0, 2, 3, 1]);
        let layout = SidebarLayout::new(Rect::new(0.0, 26.0, SIDEBAR_WIDTH, 600.0), &buffers, 0.0);
        let items: Vec<SidebarItem> = layout.items.iter().map(|(item, _)| *item).collect();
        assert_eq!(
            items,
            [
                SidebarItem::Buffer(0),
                SidebarItem::Section("CHANNELS"),
                SidebarItem::Buffer(2),
                SidebarItem::Buffer(3),
                SidebarItem::Section("QUERIES"),
                SidebarItem::Buffer(1),
            ]
        );
        let rust = layout.items[2].1;
        assert_eq!(layout.part_at(rust.center()), Some(SidebarPart::Buffer(2)));
        assert_eq!(
            layout.part_at(layout.footer.center()),
            Some(SidebarPart::Join)
        );
        assert_eq!(layout.part_at(layout.header.center()), None);
        assert_eq!(layout.max_scroll, 0.0);
    }

    #[test]
    fn long_lists_scroll_to_reveal_a_row() {
        let mut buffers = buffers();
        for n in 0..40 {
            buffers.push(Buffer::new(
                BufferKind::Channel,
                format!("#c{n}"),
                format!("#c{n}"),
            ));
        }
        let layout = SidebarLayout::new(Rect::new(0.0, 26.0, SIDEBAR_WIDTH, 400.0), &buffers, 0.0);
        assert!(layout.max_scroll > 0.0);
        let scroll = layout.reveal(43);
        let revealed = SidebarLayout::new(layout.rect, &buffers, scroll);
        let row = revealed
            .items
            .iter()
            .find(|(item, _)| *item == SidebarItem::Buffer(43))
            .unwrap()
            .1;
        assert!(row.y >= revealed.list.y && row.bottom() <= revealed.list.bottom() + 0.01);
    }

    #[test]
    fn paints_inside_its_rectangle() {
        let mut buffers = buffers();
        buffers[2].push(crate::model::Line::message(
            0,
            crate::model::LineKind::Message,
            "bob",
            "hi",
        ));
        let rect = Rect::new(10.0, 20.0, SIDEBAR_WIDTH, 300.0);
        let layout = SidebarLayout::new(rect, &buffers, 0.0);
        let mut canvas = Canvas::new(300, 340);
        let mut text = TextSystem::new();
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        let status = Status {
            text: String::from("TLS · 38 MS"),
            color: CYAN,
        };
        let view = SidebarView {
            title: "Libera.Chat",
            status: &status,
            buffers: &buffers,
            server: "irc.libera.chat",
            active: 0,
            hovered: Some(SidebarPart::Buffer(3)),
            focused: true,
        };
        paint_sidebar(&mut painter, &layout, &view);
        assert_eq!(canvas.pixel(9, 100), 0);
        assert_eq!(canvas.pixel(10 + SIDEBAR_WIDTH as u32, 100), 0);
        assert_eq!(
            canvas.pixel(12 + SIDEBAR_WIDTH as u32 - 4, 319),
            SURFACE.pixel()
        );
        assert_eq!(count(1500), "999+");
    }
}
