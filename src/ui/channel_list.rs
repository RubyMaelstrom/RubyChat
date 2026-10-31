//! The channel list (DESIGN.md §8.10): LIST's channels as a table in the
//! chat view's place—name, users and topic—sorted by users or name from
//! the column titles and filtered from the input row. Only the rows in view
//! are drawn, so a hundred thousand channels cost what ten do.

use crate::model::channels::{ChannelList, Order};
use crate::render::{Align, Painter, Point, Rect};

use super::header::thousands;
use super::theme::{self, AMBER, CANVAS, CYAN, DIM, DISABLED, EDGE, HOVER, MUTED, SURFACE, TEXT};

pub const ROW_HEIGHT: f32 = 24.0;
const HEAD_HEIGHT: f32 = 30.0;
const PADDING_X: f32 = 14.0;
const GAP: f32 = 16.0;
const USERS_WIDTH: f32 = 56.0;
/// The name column's share of the width, within bounds.
const NAME_MIN: f32 = 140.0;
const NAME_MAX: f32 = 280.0;

/// Something in the table under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TablePart {
    /// A column title, which sorts by it.
    Heading(Order),
    /// A row, by its place among the rows.
    Row(usize),
}

/// Where the table's parts go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableLayout {
    pub rect: Rect,
    head: Rect,
    body: Rect,
    name_x: f32,
    name_width: f32,
    users_x: f32,
    topic_x: f32,
}

impl TableLayout {
    pub fn new(rect: Rect) -> Self {
        let head = Rect::new(rect.x, rect.y, rect.width, HEAD_HEIGHT.min(rect.height));
        let body = Rect::new(
            rect.x,
            head.bottom(),
            rect.width,
            (rect.bottom() - head.bottom()).max(0.0),
        );
        let name_x = rect.x + PADDING_X;
        let name_width = (rect.width * 0.3).clamp(NAME_MIN, NAME_MAX);
        let users_x = name_x + name_width + GAP;
        Self {
            rect,
            head,
            body,
            name_x,
            name_width,
            users_x,
            topic_x: users_x + USERS_WIDTH + GAP,
        }
    }

    /// Whole rows that fit.
    pub fn page(&self) -> usize {
        ((self.body.height / ROW_HEIGHT).floor() as usize).max(1)
    }

    /// The furthest `rows` rows scroll, in pixels.
    pub fn max_scroll(&self, rows: usize) -> f32 {
        (rows as f32 * ROW_HEIGHT - self.body.height).max(0.0)
    }

    /// The scroll that brings row `row` into view from `scroll`.
    pub fn reveal(&self, row: usize, scroll: f32) -> f32 {
        let top = row as f32 * ROW_HEIGHT;
        if top < scroll {
            top
        } else if top + ROW_HEIGHT > scroll + self.body.height {
            top + ROW_HEIGHT - self.body.height
        } else {
            scroll
        }
    }

    pub fn part_at(&self, point: Point, scroll: f32, rows: usize) -> Option<TablePart> {
        if self.head.contains(point) {
            return if point.x < self.users_x - GAP / 2.0 {
                Some(TablePart::Heading(Order::Name))
            } else if point.x < self.topic_x - GAP / 2.0 {
                Some(TablePart::Heading(Order::Users))
            } else {
                None
            };
        }
        if !self.body.contains(point) {
            return None;
        }
        let row = ((point.y - self.body.y + scroll) / ROW_HEIGHT).floor() as usize;
        (row < rows).then_some(TablePart::Row(row))
    }
}

/// What to paint.
pub struct TableView<'a> {
    pub list: &'a ChannelList,
    pub scroll: f32,
    /// The row Enter joins, by its place.
    pub selected: Option<usize>,
    pub hovered: Option<TablePart>,
    /// Whether a filter is typed.
    pub filtered: bool,
    pub focused: bool,
}

/// Paints the table and returns the scroll it was painted at.
pub fn paint_table(painter: &mut Painter<'_>, layout: &TableLayout, view: &TableView<'_>) -> f32 {
    let rows = view.list.rows();
    let scroll = view.scroll.clamp(0.0, layout.max_scroll(rows.len()));
    painter.fill_rect(layout.rect, CANVAS);
    // Column titles: the sorting one in cyan, and any can be clicked.
    painter.fill_rect(layout.head, SURFACE);
    painter.fill_rect(
        Rect::new(
            layout.head.x,
            layout.head.bottom() - 1.0,
            layout.head.width,
            1.0,
        ),
        EDGE,
    );
    let title_style = theme::chrome(9.0);
    let order = view.list.order();
    let titles = [
        (
            "CHANNEL",
            Some(Order::Name),
            Rect::new(
                layout.name_x,
                layout.head.y,
                layout.name_width,
                layout.head.height,
            ),
            Align::Start,
        ),
        (
            "USERS",
            Some(Order::Users),
            Rect::new(
                layout.users_x,
                layout.head.y,
                USERS_WIDTH,
                layout.head.height,
            ),
            Align::End,
        ),
        (
            "TOPIC",
            None,
            Rect::new(
                layout.topic_x,
                layout.head.y,
                (layout.rect.right() - PADDING_X - layout.topic_x).max(0.0),
                layout.head.height,
            ),
            Align::Start,
        ),
    ];
    for (title, sorts, rect, align) in titles {
        let color = match sorts {
            Some(by) if by == order => CYAN,
            Some(by) if view.hovered == Some(TablePart::Heading(by)) => TEXT,
            _ => MUTED,
        };
        let title = if sorts == Some(order) {
            format!("{title} \u{25be}")
        } else {
            title.to_string()
        };
        painter.label(&title, rect, &title_style, color, align);
    }
    painter.save_clip(layout.body);
    let name_style = theme::body(12.0);
    let topic_style = theme::body(12.0);
    let count_style = theme::chrome(10.0);
    let first = (scroll / ROW_HEIGHT).floor() as usize;
    let mut y = layout.body.y - (scroll - first as f32 * ROW_HEIGHT);
    for (place, &index) in rows.iter().enumerate().skip(first) {
        if y >= layout.body.bottom() {
            break;
        }
        let Some(channel) = view.list.channel(index) else {
            break;
        };
        let row = Rect::new(layout.rect.x, y, layout.rect.width, ROW_HEIGHT);
        let selected = view.selected == Some(place);
        if selected || view.hovered == Some(TablePart::Row(place)) {
            painter.fill_rect(row, HOVER);
        }
        if selected {
            painter.fill_rect(
                Rect::new(row.x, row.y, 2.0, row.height),
                if view.focused { CYAN } else { DIM },
            );
        }
        painter.label(
            channel.name(),
            Rect::new(layout.name_x, y, layout.name_width, ROW_HEIGHT),
            &name_style,
            if selected { CYAN } else { TEXT },
            Align::Start,
        );
        painter.label(
            &thousands(channel.users as usize),
            Rect::new(layout.users_x, y, USERS_WIDTH, ROW_HEIGHT),
            &count_style,
            AMBER,
            Align::End,
        );
        painter.label(
            channel.topic(),
            Rect::new(
                layout.topic_x,
                y,
                (layout.rect.right() - PADDING_X - layout.topic_x).max(0.0),
                ROW_HEIGHT,
            ),
            &topic_style,
            MUTED,
            Align::Start,
        );
        y += ROW_HEIGHT;
    }
    painter.restore();
    // Nothing to show: say why.
    if rows.is_empty() && !view.list.settling() {
        let note = if view.list.loading {
            "LOADING CHANNELS"
        } else if let Some(error) = &view.list.error {
            error.as_str()
        } else if view.filtered && !view.list.is_empty() {
            "NO CHANNELS MATCH"
        } else {
            "NO CHANNELS"
        };
        painter.label(
            note,
            Rect::new(
                layout.body.x + PADDING_X,
                layout.body.y + 16.0,
                (layout.body.width - 2.0 * PADDING_X).max(0.0),
                20.0,
            ),
            &theme::chrome(10.0),
            DISABLED,
            Align::Center,
        );
    }
    scroll
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RUBYCHAT_SNAPSHOTS=DIR cargo test --lib list_and_find_snapshots --
    /// --ignored` writes the channel list as `/list` shows it, and search
    /// matches highlighted in a chat view, with the input row of each.
    #[test]
    #[ignore = "writes snapshot images; set RUBYCHAT_SNAPSHOTS"]
    fn list_and_find_snapshots() {
        use std::collections::VecDeque;
        use std::time::{Duration, Instant};

        use crate::irc::casemap::Casemap;
        use crate::model::editor::LineEditor;
        use crate::model::search::Query;
        use crate::model::{Line, LineKind};
        use crate::render::{Canvas, TextSystem};
        use crate::ui::chat_view::{ChatContext, ChatFrame, ChatView, Found, paint_chat};
        use crate::ui::header::{HeaderView, paint_header};
        use crate::ui::input::paint_input;

        let Some(dir) = std::env::var_os("RUBYCHAT_SNAPSHOTS") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let (width, height) = (900.0, 420.0);
        let header = Rect::new(0.0, 0.0, width, 50.0);
        let input = Rect::new(0.0, height - 52.0, width, 52.0);
        let body = Rect::new(0.0, header.bottom(), width, input.y - header.bottom());
        let field = Rect::new(12.0, input.y + 8.0, width - 24.0, 36.0);
        let mut text = TextSystem::new();

        let mut list = ChannelList::loading();
        for (name, users, topic) in [
            (
                "#rust",
                1834,
                "Rust programming | https://www.rust-lang.org | Be kind",
            ),
            (
                "#rust-beginners",
                412,
                "Questions welcome, \u{2}no\u{2} question too small",
            ),
            ("##rust-offtopic", 96, ""),
            ("#rustls", 41, "A modern TLS library in Rust"),
            (
                "#rust-embedded",
                233,
                "Rust on microcontrollers · see #rust-embedded-space",
            ),
            ("#trust", 12, "Not about Rust at all, mostly"),
            ("#rust-gamedev", 158, "Making games with Rust"),
        ] {
            list.add(name, users, topic);
        }
        list.finish(None);
        list.set_filter("rust");
        while !list.step(Instant::now() + Duration::from_secs(1)) {}
        let mut canvas = Canvas::new(width as u32, height as u32);
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        paint_header(
            &mut painter,
            header,
            &HeaderView {
                name: "Channels",
                detail: "7 of 48,213 channels",
                topic: None,
                placeholder: "Type to filter by name or topic · Enter joins · Tab sorts · Esc closes",
            },
        );
        let layout = TableLayout::new(body);
        let view = TableView {
            list: &list,
            scroll: 0.0,
            selected: Some(1),
            hovered: Some(TablePart::Row(3)),
            filtered: true,
            focused: true,
        };
        paint_table(&mut painter, &layout, &view);
        let mut editor = LineEditor::default();
        editor.insert("rust");
        paint_input(
            &mut painter,
            field,
            &mut editor,
            "FILTER\u{203a}",
            "Channel name or topic",
            true,
            None,
        );
        crate::render::png::write_rgba(
            &dir.join("channel-list.png"),
            canvas.width(),
            canvas.height(),
            &canvas.to_rgba(),
        )
        .unwrap();

        let lines: VecDeque<Line> = [
            ("alice", "has anyone tried the new borrow checker?"),
            (
                "bob",
                "the \u{2}borrow\u{2} checker is fine, the docs aren't",
            ),
            ("carol", "borrowing twice: borrow, then borrow again"),
            ("dave", "lunch?"),
        ]
        .iter()
        .enumerate()
        .map(|(index, (nick, said))| {
            let mut line = Line::message(0, LineKind::Message, *nick, *said);
            line.id = index as u64 + 1;
            line
        })
        .collect();
        let query = Query::new("borrow").unwrap();
        let mut canvas = Canvas::new(width as u32, height as u32);
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        let view = ChatView {
            lines: &lines,
            context: ChatContext {
                casemap: Casemap::Rfc1459,
                chantypes: "#",
                marker: None,
                label_chars: 5,
                filter: false,
                loading: false,
                seconds: false,
            },
            anchor: None,
            selection: None,
            hovered: None,
            found: Some(Found {
                query: &query,
                current: Some((3, 30..36)),
            }),
            focused: true,
        };
        paint_chat(
            &mut painter,
            Rect::new(0.0, 0.0, width, input.y),
            &view,
            ChatFrame::default(),
            false,
        );
        let mut editor = LineEditor::default();
        editor.insert("borrow");
        paint_input(
            &mut painter,
            field,
            &mut editor,
            "FIND\u{203a}",
            "",
            true,
            Some("NO EARLIER MATCHES"),
        );
        crate::render::png::write_rgba(
            &dir.join("find.png"),
            canvas.width(),
            canvas.height(),
            &canvas.to_rgba(),
        )
        .unwrap();
    }

    #[test]
    fn rows_and_titles_are_found_under_the_pointer() {
        let layout = TableLayout::new(Rect::new(200.0, 100.0, 800.0, 400.0));
        let first = Point::new(400.0, 100.0 + HEAD_HEIGHT + ROW_HEIGHT / 2.0);
        assert_eq!(layout.part_at(first, 0.0, 50), Some(TablePart::Row(0)));
        assert_eq!(
            layout.part_at(first, ROW_HEIGHT * 3.0, 50),
            Some(TablePart::Row(3))
        );
        assert_eq!(layout.part_at(first, 0.0, 0), None, "no rows");
        assert_eq!(
            layout.part_at(Point::new(220.0, 110.0), 0.0, 50),
            Some(TablePart::Heading(Order::Name))
        );
        assert_eq!(
            layout.part_at(Point::new(layout.users_x + 10.0, 110.0), 0.0, 50),
            Some(TablePart::Heading(Order::Users))
        );
        // Keeping a row in view scrolls only as far as needed.
        assert_eq!(layout.reveal(0, 100.0), 0.0);
        let page = layout.page();
        assert_eq!(layout.reveal(page - 1, 0.0), 0.0);
        assert!(layout.reveal(page + 5, 0.0) > 0.0);
        assert_eq!(layout.max_scroll(2), 0.0);
    }
}
