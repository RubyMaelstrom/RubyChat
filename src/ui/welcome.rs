//! The start page: RubyChat's logo, then quick connect in the design of
//! trust-desktop's COMMAND panel (a chamfered, etched frame with a pink tag,
//! the cyan prompt, a status line, instrument cells and keycap hints), then
//! saved networks (or suggested ones) beside the recently used servers, each
//! a click away. It fills the window while no network is open, and comes
//! back over the networks for Ctrl+T and the new-tab button.

use crate::model::recent::{RecentServer, ago};
use crate::platform::time;
use crate::render::{Align, Cap, Color, Image, Painter, Path, Point, Rect};

use super::input::{self, LineEditor};
use super::theme::{
    self, AMBER, CANVAS, CYAN, DIM, DISABLED, EDGE, HOVER, INSET, KEY, MUTED, PINK, TEXT,
};
use super::widgets::frame;

const PANEL_WIDTH: f32 = 760.0;
const PANEL_HEIGHT: f32 = 204.0;
/// The logo's size in logical pixels, and the space below it.
pub const LOGO_SIZE: f32 = 96.0;
const LOGO_GAP: f32 = 18.0;
const RECENT_GAP: f32 = 16.0;
const RECENT_HEADER: f32 = 30.0;
const RECENT_ROW: f32 = 32.0;
const RECENT_FOOT: f32 = 8.0;

/// What the panel reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WelcomeInfo {
    pub version: &'static str,
    /// The previous frame's CPU time, painting and presenting.
    pub frame: Option<std::time::Duration>,
    /// Window size in device pixels, and the scale factor.
    pub device_size: (u32, u32),
    pub scale: f32,
    pub glyphs: usize,
    pub glyph_bytes: usize,
    pub networks: usize,
    /// Replaces the status line's usual hint, such as a mistyped address.
    pub message: Option<String>,
    /// Networks are open behind the panel, so Escape closes it.
    pub can_cancel: bool,
}

/// A saved network on the start page, or a suggested one.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedRow {
    pub name: String,
    pub address: String,
    /// Open, with its jewel's color.
    pub jewel: Option<Color>,
}

/// What the pointer is over among the saved networks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavedPart {
    Row(usize),
    Edit(usize),
    New,
}

/// What the start page shows besides the panel.
#[derive(Clone, Copy, Debug)]
pub struct StartPage<'a> {
    pub recent: &'a [RecentServer],
    /// The recent server under the pointer.
    pub hovered: Option<usize>,
    pub saved: &'a [SavedRow],
    /// The saved rows are suggestions: nothing is saved yet.
    pub suggested: bool,
    pub saved_hover: Option<SavedPart>,
    /// The logo, already sized in device pixels.
    pub logo: Option<&'a Image>,
    pub now: i64,
}

/// Where the start page's parts go in `area`.
#[derive(Clone, Debug, PartialEq)]
pub struct StartLayout {
    pub area: Rect,
    pub logo: Option<Rect>,
    pub panel: Rect,
    /// Saved networks on the left.
    pub saved: Option<Rect>,
    pub saved_rows: Vec<Rect>,
    /// Recent servers on the right.
    pub recent: Option<Rect>,
    /// One row per recent server that fits.
    pub rows: Vec<Rect>,
}

impl StartLayout {
    /// Stacks the logo, panel and lists, centered a little above the
    /// middle: saved networks beside recent servers. Short windows drop the
    /// logo first, then rows.
    pub fn new(area: Rect, saved: usize, recent: usize) -> Self {
        let width = PANEL_WIDTH.min(area.width - 32.0).max(0.0).round();
        let panel_height = PANEL_HEIGHT.min(area.height - 32.0).max(0.0);
        let room = area.height - 32.0;
        let lists_height = |rows: usize| {
            if rows == 0 {
                0.0
            } else {
                RECENT_GAP + RECENT_HEADER + rows as f32 * RECENT_ROW + RECENT_FOOT
            }
        };
        let mut rows = saved.max(recent);
        let mut logo = true;
        let total = |logo: bool, rows: usize| {
            (if logo { LOGO_SIZE + LOGO_GAP } else { 0.0 }) + panel_height + lists_height(rows)
        };
        if total(logo, rows) > room {
            logo = false;
        }
        while rows > 0 && total(logo, rows) > room {
            rows -= 1;
        }
        let x = (area.x + (area.width - width) / 2.0).round();
        let mut y = (area.y + (area.height - total(logo, rows)) * 0.42)
            .round()
            .max(area.y);
        let logo = logo.then(|| {
            let rect = Rect::new(
                (area.x + (area.width - LOGO_SIZE) / 2.0).round(),
                y,
                LOGO_SIZE,
                LOGO_SIZE,
            );
            y += LOGO_SIZE + LOGO_GAP;
            rect
        });
        let panel = Rect::new(x, y, width, panel_height);
        y += panel_height + RECENT_GAP;
        let height = lists_height(rows) - RECENT_GAP;
        let half = ((width - 16.0) / 2.0).floor();
        let column = |x: f32, width: f32, count: usize| {
            let rect = Rect::new(x, y, width, height);
            let rows = (0..count.min(rows))
                .map(|i| {
                    Rect::new(
                        rect.x + 8.0,
                        rect.y + RECENT_HEADER + i as f32 * RECENT_ROW,
                        rect.width - 16.0,
                        RECENT_ROW,
                    )
                })
                .collect::<Vec<_>>();
            (Some(rect), rows)
        };
        let ((saved, saved_rows), (recent, rows)) = match (rows > 0, saved > 0, recent > 0) {
            (false, ..) => ((None, Vec::new()), (None, Vec::new())),
            (true, true, true) => (
                column(x, half, saved),
                column(x + width - half, half, recent),
            ),
            (true, true, false) => (column(x, width, saved), (None, Vec::new())),
            (true, false, _) => ((None, Vec::new()), column(x, width, recent)),
        };
        Self {
            area,
            logo,
            panel,
            saved,
            saved_rows,
            recent,
            rows,
        }
    }

    /// The saved list's `+ NEW` button.
    pub fn new_button(&self) -> Option<Rect> {
        self.saved
            .map(|rect| Rect::new(rect.right() - 16.0 - 64.0, rect.y + 7.0, 64.0, 18.0))
    }

    /// A saved row's EDIT button.
    pub fn edit_button(row: Rect) -> Rect {
        Rect::new(
            row.right() - 8.0 - 48.0,
            (row.center().y - 9.0).round(),
            48.0,
            18.0,
        )
    }

    /// The saved network, its EDIT button, or `+ NEW` at `point`. Suggested
    /// networks have no EDIT button.
    pub fn saved_at(&self, point: Point, suggested: bool) -> Option<SavedPart> {
        if self
            .new_button()
            .is_some_and(|button| button.contains(point))
        {
            return Some(SavedPart::New);
        }
        let index = self.saved_rows.iter().position(|row| row.contains(point))?;
        if !suggested && Self::edit_button(self.saved_rows[index]).contains(point) {
            return Some(SavedPart::Edit(index));
        }
        Some(SavedPart::Row(index))
    }

    /// The quick-connect field: typing repaints only this.
    pub fn input(&self) -> Rect {
        Rect::new(
            self.panel.x + 16.0,
            self.panel.y + 40.0,
            (self.panel.width - 32.0).max(0.0),
            42.0,
        )
    }

    /// The recent server at `point`.
    pub fn recent_at(&self, point: Point) -> Option<usize> {
        self.rows.iter().position(|row| row.contains(point))
    }
}

pub fn paint_welcome(
    painter: &mut Painter<'_>,
    layout: &StartLayout,
    info: &WelcomeInfo,
    editor: &mut LineEditor,
    start: &StartPage<'_>,
) {
    let area = layout.area;
    painter.fill_rect(area, CANVAS);
    let panel = layout.panel;
    if panel.width < 120.0 || panel.height < 60.0 {
        return;
    }
    painter.save_clip(area);
    if let (Some(rect), Some(logo)) = (layout.logo, start.logo) {
        painter.draw_image(logo, Point::new(rect.x, rect.y));
    }
    frame(painter, panel, 22.0);
    let right = panel.right();
    painter.save_clip(panel);

    // Decorative traces follow the frame rather than competing with text.
    let mut traces = Path::new();
    for i in 0..4 {
        let x = right - 62.0 + i as f32 * 9.0;
        traces.move_to(x, panel.y + 5.0);
        traces.line_to(x + 10.0, panel.y + 15.0);
    }
    painter.stroke_path(&traces, 2.0, Cap::Butt, EDGE);

    let tag = Rect::new(panel.x + 16.0, panel.y + 10.0, 106.0, 22.0);
    painter.fill_rect(tag, PINK);
    painter.label("RUBYCHAT", tag, &theme::chrome(13.0), INSET, Align::Center);
    painter.label(
        &format!("IRC  /  welcome  /  v{}", info.version),
        Rect::new(
            panel.x + 138.0,
            tag.y,
            (panel.width - 212.0).max(0.0),
            tag.height,
        ),
        &theme::chrome(11.0),
        MUTED,
        Align::Start,
    );

    input::paint_input(
        painter,
        layout.input(),
        editor,
        "ruby\u{203a}",
        "irc.libera.chat:6697",
        true,
        None,
    );

    let status_y = panel.y + 89.0;
    painter.fill_rect(Rect::new(panel.x + 18.0, status_y + 7.0, 5.0, 5.0), CYAN);
    let small = theme::chrome(11.0);
    let state = "READY";
    let state_width = painter.text.width(state, &small) + 12.0;
    painter.label(
        state,
        Rect::new(panel.x + 31.0, status_y, state_width, 20.0),
        &small,
        CYAN,
        Align::Start,
    );
    let hint = "Type a server and press Enter. TLS unless the address says irc:// or port 6667.";
    painter.label(
        info.message.as_deref().unwrap_or(hint),
        Rect::new(
            panel.x + 31.0 + state_width,
            status_y,
            (panel.width - state_width - 49.0).max(0.0),
            20.0,
        ),
        &small,
        TEXT,
        Align::Start,
    );

    let frame_time = info.frame.map_or_else(
        || String::from("—"),
        |frame| format!("{:.2} ms", frame.as_secs_f64() * 1000.0),
    );
    let window = format!(
        "{} × {}  / {:.2}×",
        info.device_size.0, info.device_size.1, info.scale
    );
    let glyphs = format!(
        "{} glyphs  / {}",
        info.glyphs,
        human_bytes(info.glyph_bytes)
    );
    let networks = match info.networks {
        0 => String::from("none open"),
        1 => String::from("1 open"),
        n => format!("{n} open"),
    };
    let cells = if panel.width >= 680.0 { 4 } else { 2 };
    let cell_width = (panel.width - 32.0).max(0.0) / cells as f32;
    for (i, (title, value)) in [
        ("LAST FRAME / CPU", frame_time),
        ("WINDOW / DEVICE PX", window),
        ("GLYPH CACHE", glyphs),
        ("NETWORKS", networks),
    ]
    .into_iter()
    .take(cells)
    .enumerate()
    {
        let x = panel.x + 16.0 + i as f32 * cell_width;
        let y = panel.y + 118.0;
        painter.fill_rect(Rect::new(x, y, cell_width - 10.0, 1.0), EDGE);
        painter.label(
            title,
            Rect::new(x, y + 4.0, cell_width - 14.0, 14.0),
            &theme::chrome(9.0),
            MUTED,
            Align::Start,
        );
        painter.label(
            &value,
            Rect::new(x, y + 19.0, cell_width - 14.0, 20.0),
            &theme::chrome(12.0),
            TEXT,
            Align::Start,
        );
    }

    let mut x = panel.x + 16.0;
    let style = theme::chrome(10.0);
    let back = if info.can_cancel {
        ("ESC", "back")
    } else {
        ("CTRL+TAB", "next tab")
    };
    for (key, action) in [
        ("ENTER", "connect"),
        back,
        ("CTRL+W", "close tab"),
        ("CTRL+Q", "quit"),
    ] {
        let key_width = painter.text.width(key, &style) + 12.0;
        let action_width = painter.text.width(action, &style);
        if x + key_width + action_width + 8.0 > right - 16.0 {
            break;
        }
        let key_rect = Rect::new(x, panel.y + 174.0, key_width, 19.0);
        painter.fill_rect(key_rect, KEY);
        painter.label(key, key_rect, &style, AMBER, Align::Center);
        painter.label(
            action,
            Rect::new(
                x + key_width + 6.0,
                key_rect.y,
                action_width + 1.0,
                key_rect.height,
            ),
            &style,
            MUTED,
            Align::Start,
        );
        x += key_width + action_width + 22.0;
    }
    painter.restore();
    paint_saved(painter, layout, start);
    paint_recent(painter, layout, start);
    painter.restore();
}

fn paint_saved(painter: &mut Painter<'_>, layout: &StartLayout, start: &StartPage<'_>) {
    let Some(rect) = layout.saved else {
        return;
    };
    frame(painter, rect, 12.0);
    painter.save_clip(rect);
    let header = Rect::new(
        rect.x + 16.0,
        rect.y + 8.0,
        rect.width - 32.0,
        RECENT_HEADER - 12.0,
    );
    let title = if start.suggested {
        "SUGGESTED NETWORKS"
    } else {
        "SAVED NETWORKS"
    };
    painter.label(title, header, &theme::chrome(9.0), MUTED, Align::Start);
    if let Some(button) = layout.new_button() {
        let lit = start.saved_hover == Some(SavedPart::New);
        painter.fill_rect(button, if lit { INSET } else { KEY });
        painter.label(
            "+ NEW",
            button,
            &theme::chrome(9.0),
            if lit { CYAN } else { AMBER },
            Align::Center,
        );
    }
    for (index, (row, saved)) in layout.saved_rows.iter().zip(start.saved).enumerate() {
        let hovered =
            matches!(start.saved_hover, Some(SavedPart::Row(i) | SavedPart::Edit(i)) if i == index);
        if hovered {
            painter.fill_rect(*row, HOVER);
            painter.fill_rect(Rect::new(row.x, row.y, 2.0, row.height), CYAN);
        }
        let center = row.y + row.height / 2.0;
        let jewel = Rect::new(row.x + 10.0, (center - 2.5).round(), 5.0, 5.0);
        painter.fill_rect(
            jewel,
            saved.jewel.unwrap_or(if hovered { CYAN } else { DIM }),
        );
        let edit = (!start.suggested).then(|| StartLayout::edit_button(*row));
        let right = edit.map_or(row.right() - 8.0, |button| button.x - 8.0);
        let name_width = (row.width * 0.36).min(150.0);
        let name = Rect::new(row.x + 24.0, row.y, name_width, row.height);
        painter.label(
            &saved.name,
            name,
            &theme::chrome(12.0),
            if hovered { CYAN } else { TEXT },
            Align::Start,
        );
        let address = Rect::new(
            name.right() + 8.0,
            row.y,
            (right - name.right() - 8.0).max(0.0),
            row.height,
        );
        painter.label(
            &saved.address,
            address,
            &theme::chrome(10.5),
            MUTED,
            Align::Start,
        );
        if let Some(button) = edit {
            let lit = start.saved_hover == Some(SavedPart::Edit(index));
            painter.fill_rect(button, if lit { INSET } else { KEY });
            painter.label(
                "EDIT",
                button,
                &theme::chrome(9.0),
                if lit { CYAN } else { AMBER },
                Align::Center,
            );
        }
    }
    painter.restore();
}

fn paint_recent(painter: &mut Painter<'_>, layout: &StartLayout, start: &StartPage<'_>) {
    let Some(rect) = layout.recent else {
        return;
    };
    frame(painter, rect, 12.0);
    painter.save_clip(rect);
    let header = Rect::new(
        rect.x + 16.0,
        rect.y + 8.0,
        rect.width - 32.0,
        RECENT_HEADER - 12.0,
    );
    painter.label(
        "RECENT SERVERS",
        header,
        &theme::chrome(9.0),
        MUTED,
        Align::Start,
    );
    painter.label(
        "CLICK TO CONNECT",
        header,
        &theme::chrome(9.0),
        DISABLED,
        Align::End,
    );
    let date = |unix_ms: i64| {
        let local = time::local(unix_ms);
        format!("{:04}-{:02}-{:02}", local.year, local.month, local.day)
    };
    for (index, (row, server)) in layout.rows.iter().zip(start.recent).enumerate() {
        let hovered = start.hovered == Some(index);
        if hovered {
            painter.fill_rect(*row, HOVER);
            painter.fill_rect(Rect::new(row.x, row.y, 2.0, row.height), CYAN);
        }
        let center = row.y + row.height / 2.0;
        painter.fill_rect(
            Rect::new(row.x + 10.0, (center - 2.5).round(), 5.0, 5.0),
            if hovered { CYAN } else { DIM },
        );
        let name = Rect::new(
            row.x + 24.0,
            row.y,
            210.0f32.min(row.width - 190.0).max(60.0),
            row.height,
        );
        painter.label(
            &server.name,
            name,
            &theme::chrome(12.0),
            if hovered { CYAN } else { TEXT },
            Align::Start,
        );
        // Fixed columns, so the badges line up whatever the times say.
        let when = ago(server.last, start.now, date);
        let when_rect = Rect::new(row.right() - 12.0 - 84.0, row.y, 84.0, row.height);
        painter.label(&when, when_rect, &theme::chrome(10.0), MUTED, Align::End);
        let (badge, color) = if server.tls {
            ("TLS", CYAN)
        } else {
            ("PLAIN", AMBER)
        };
        let badge_width = painter.text.width("PLAIN", &theme::chrome(9.0)) + 12.0;
        let badge_rect = Rect::new(
            when_rect.x - 16.0 - badge_width,
            (center - 8.0).round(),
            badge_width,
            16.0,
        );
        painter.fill_rect(badge_rect, KEY);
        painter.label(badge, badge_rect, &theme::chrome(9.0), color, Align::Center);
        // A narrow column keeps the name and the time.
        let address = Rect::new(
            name.right() + 12.0,
            row.y,
            if rect.width < 560.0 {
                0.0
            } else {
                (badge_rect.x - 12.0 - name.right() - 12.0).max(0.0)
            },
            row.height,
        );
        painter.label(
            &server.address(),
            address,
            &theme::chrome(11.0),
            MUTED,
            Align::Start,
        );
    }
    painter.restore();
}

fn human_bytes(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KiB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MiB", bytes as f64 / 1_048_576.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Canvas, TextSystem};

    fn servers(count: usize) -> Vec<RecentServer> {
        (0..count)
            .map(|i| RecentServer {
                host: format!("irc{i}.example.net"),
                port: 6697,
                tls: i % 2 == 0,
                name: format!("Network {i}"),
                last: 0,
            })
            .collect()
    }

    #[test]
    fn the_page_stacks_and_gives_way_in_short_windows() {
        let tall = StartLayout::new(Rect::new(0.0, 26.0, 1100.0, 694.0), 3, 5);
        let logo = tall.logo.expect("room for the logo");
        assert!(logo.bottom() < tall.panel.y);
        assert_eq!(tall.panel.width, PANEL_WIDTH);
        assert_eq!((tall.saved_rows.len(), tall.rows.len()), (3, 5));
        let (saved, recent) = (tall.saved.unwrap(), tall.recent.unwrap());
        assert!(saved.right() < recent.x, "side by side");
        assert!(recent.y > tall.panel.bottom() && recent.bottom() <= 26.0 + 694.0);
        assert_eq!(tall.recent_at(tall.rows[2].center()), Some(2));
        assert_eq!(tall.recent_at(tall.panel.center()), None);
        let row = tall.saved_rows[1];
        assert_eq!(tall.saved_at(row.center(), false), Some(SavedPart::Row(1)));
        assert_eq!(
            tall.saved_at(StartLayout::edit_button(row).center(), false),
            Some(SavedPart::Edit(1))
        );
        assert_eq!(
            tall.saved_at(StartLayout::edit_button(row).center(), true),
            Some(SavedPart::Row(1)),
            "suggestions have no EDIT"
        );
        assert_eq!(
            tall.saved_at(tall.new_button().unwrap().center(), false),
            Some(SavedPart::New)
        );
        let short = StartLayout::new(Rect::new(0.0, 26.0, 700.0, 400.0), 5, 5);
        assert!(short.logo.is_none(), "the logo goes first");
        assert!(short.rows.len() < 5 && short.recent.is_none_or(|r| r.bottom() <= 426.0));
        let only_saved = StartLayout::new(Rect::new(0.0, 26.0, 1100.0, 694.0), 5, 0);
        assert!(only_saved.recent.is_none() && only_saved.saved.unwrap().width == PANEL_WIDTH);
    }

    fn rows() -> Vec<SavedRow> {
        ["Libera.Chat", "OFTC", "hackint"]
            .iter()
            .map(|name| SavedRow {
                name: name.to_string(),
                address: format!("irc.{}:6697", name.to_ascii_lowercase()),
                jewel: (*name == "OFTC").then_some(CYAN),
            })
            .collect()
    }

    /// `RUBYCHAT_SNAPSHOTS=DIR cargo test --lib window_snapshots -- --ignored`
    /// writes the whole window as the start page composes it, at 1× and 2×.
    #[test]
    #[ignore = "writes snapshot images; set RUBYCHAT_SNAPSHOTS"]
    fn window_snapshots() {
        use crate::ui::branding::Logo;
        use crate::ui::tab_strip::{
            STRIP_HEIGHT, StripTab, TabStatus, TabStripGeometry, TabStripPart, TabStripState,
            paint_tab_strip,
        };
        let Some(dir) = std::env::var_os("RUBYCHAT_SNAPSHOTS") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let tabs: Vec<StripTab> = [
            ("Libera.Chat", TabStatus::Connected, 0, false),
            ("OFTC", TabStatus::Connected, 2, true),
        ]
        .into_iter()
        .map(|(title, status, highlights, unread)| StripTab {
            title: title.to_string(),
            status,
            highlights,
            unread,
        })
        .collect();
        let mut recent = servers(4);
        recent[0].name = String::from("Libera.Chat");
        recent[0].host = String::from("irc.libera.chat");
        let now = crate::platform::time::now_ms();
        for (i, server) in recent.iter_mut().enumerate() {
            server.last = now - [90_000, 4 * 3_600_000, 30 * 3_600_000, 4 * 86_400_000][i];
        }
        let mut text = TextSystem::new();
        let mut logo = Logo::default();
        let (width, height) = (1100.0, 720.0);
        for scale in [1.0f32, 2.0] {
            let mut canvas = Canvas::new((width * scale) as u32, (height * scale) as u32);
            let mut painter = Painter::new(&mut canvas, &mut text, scale);
            paint_tab_strip(
                &mut painter,
                &TabStripGeometry::new(width, tabs.len(), 0.0),
                &tabs,
                &TabStripState {
                    active: usize::MAX,
                    hovered: Some(TabStripPart::NewTab),
                    window_focused: true,
                    ..Default::default()
                },
            );
            let info = WelcomeInfo {
                version: env!("CARGO_PKG_VERSION"),
                frame: Some(std::time::Duration::from_micros(412)),
                device_size: ((width * scale) as u32, (height * scale) as u32),
                scale,
                glyphs: 214,
                glyph_bytes: 61_440,
                networks: tabs.len(),
                message: None,
                can_cancel: true,
            };
            let mut editor = LineEditor::default();
            editor.insert("irc.oftc.net");
            let saved = rows();
            let layout = StartLayout::new(
                Rect::new(0.0, STRIP_HEIGHT, width, height - STRIP_HEIGHT),
                saved.len(),
                recent.len(),
            );
            let start = StartPage {
                recent: &recent,
                hovered: Some(1),
                saved: &saved,
                suggested: false,
                saved_hover: Some(SavedPart::Edit(0)),
                logo: Some(logo.at((LOGO_SIZE * scale).round() as u32)),
                now,
            };
            paint_welcome(&mut painter, &layout, &info, &mut editor, &start);
            crate::render::png::write_rgba(
                &dir.join(format!("window-{scale}x.png")),
                canvas.width(),
                canvas.height(),
                &canvas.to_rgba(),
            )
            .unwrap();
        }
    }

    #[test]
    fn painting_fills_its_area_and_nothing_else() {
        let mut canvas = Canvas::new(400, 300);
        let mut text = TextSystem::new();
        let area = Rect::new(0.0, 50.0, 400.0, 250.0);
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        let recent = servers(2);
        let saved = rows();
        let layout = StartLayout::new(area, saved.len(), recent.len());
        let start = StartPage {
            recent: &recent,
            hovered: None,
            saved: &saved,
            suggested: true,
            saved_hover: None,
            logo: None,
            now: 0,
        };
        paint_welcome(
            &mut painter,
            &layout,
            &WelcomeInfo {
                version: "0.1.0",
                ..Default::default()
            },
            &mut LineEditor::default(),
            &start,
        );
        assert_eq!(canvas.pixel(0, 49), 0);
        assert_eq!(canvas.pixel(0, 50), CANVAS.pixel());
        assert_eq!(canvas.pixel(399, 299), CANVAS.pixel());
        assert_eq!(human_bytes(1536), "1.5 KiB");
    }
}
