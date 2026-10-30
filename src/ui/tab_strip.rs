//! The network tab strip, which replaces the system title bar: one tab per
//! network, the new-tab button, scroll arrows when the tabs overflow, and the
//! window buttons. Ported from trust-desktop (`src/render/tab_strip.rs`):
//! one geometry serves painting and hit testing, and the look is the same,
//! but a tab's jewel shows its connection and the slot trust-desktop uses
//! for a tab's sound shows unread activity.

use std::time::Duration;

use crate::render::{Align, Cap, Color, Painter, Path, Point, Rect};

use super::theme::{
    self, AMBER, CYAN, DIM, DISABLED, EDGE, HEART, HOVER, INSET, MUTED, PINK, RULE, STRIP, SURFACE,
    TEXT,
};

/// The strip's height: the system title bar it replaces. labwc's themes size
/// theirs to the window button height (26 px by default); Windows draws a
/// 32 px caption.
#[cfg(windows)]
pub const STRIP_HEIGHT: f32 = 32.0;
#[cfg(not(windows))]
pub const STRIP_HEIGHT: f32 = 26.0;

#[cfg(windows)]
const BUTTON_WIDTH: f32 = 46.0;
#[cfg(not(windows))]
const BUTTON_WIDTH: f32 = 40.0;
const TAB_MAX_WIDTH: f32 = 224.0;
/// Narrower tabs scroll instead of shrinking further.
const TAB_MIN_WIDTH: f32 = 112.0;
const ARROW_WIDTH: f32 = 20.0;
/// Empty strip that always remains for moving the window.
const DRAG_RESERVE: f32 = 40.0;
const LEFT_PAD: f32 = 4.0;
/// A tab's close button and badge slot are squares this wide.
const TAB_BUTTON: f32 = 16.0;
const TITLE_SIZE: f32 = 11.5;
const BADGE_SIZE: f32 = 11.0;
/// From this width every tab shows its close button; narrower inactive tabs
/// show it while hovered.
const CLOSE_ALWAYS_WIDTH: f32 = 140.0;

/// A network's connection, as its tab's jewel shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabStatus {
    Connected,
    /// Resolving, connecting, negotiating TLS or registering: the jewel
    /// beats.
    Connecting,
    /// Not connected, or waiting to reconnect.
    #[default]
    Disconnected,
    /// Login refused, banned, or certificate rejected.
    Failed,
}

/// A tab as the strip presents it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripTab {
    pub title: String,
    pub status: TabStatus,
    /// Unread messages that mention you, across the network's buffers.
    pub highlights: u32,
    /// Unread messages in any of the network's buffers.
    pub unread: bool,
}

/// One interactive part of the strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TabStripPart {
    Tab(usize),
    CloseTab(usize),
    NewTab,
    ScrollBack,
    ScrollForward,
    Minimize,
    Maximize,
    Close,
    /// Strip without a control: dragging it moves the window.
    Empty,
}

/// Where the strip's parts are for a window `width` wide.
#[derive(Clone, Debug, PartialEq)]
pub struct TabStripGeometry {
    pub strip: Rect,
    /// The tabs show inside this rectangle; the rest are scrolled away.
    pub viewport: Rect,
    /// One rectangle per tab, in strip order, at the current scroll.
    pub tabs: Vec<Rect>,
    pub new_tab: Rect,
    /// Only while the tabs overflow their viewport.
    pub scroll_back: Option<Rect>,
    pub scroll_forward: Option<Rect>,
    pub minimize: Rect,
    pub maximize: Rect,
    pub close: Rect,
    pub scroll: f32,
    pub max_scroll: f32,
}

impl TabStripGeometry {
    /// Lays out `count` tabs: as wide as fits up to their maximum, then at
    /// their minimum width scrolled by `scroll`, with the scroll arrows at
    /// the far right. Tabs fill the strip's height and are whole logical
    /// pixels wide, so their edges stay crisp. The window buttons stay at
    /// the right edge and some empty strip always remains to move the window
    /// by.
    pub fn new(width: f32, count: usize, scroll: f32) -> Self {
        let height = STRIP_HEIGHT;
        let width = width.max(0.0);
        let strip = Rect::new(0.0, 0.0, width, height);
        let close = Rect::new((width - BUTTON_WIDTH).max(0.0), 0.0, BUTTON_WIDTH, height);
        let maximize = Rect::new((close.x - BUTTON_WIDTH).max(0.0), 0.0, BUTTON_WIDTH, height);
        let minimize = Rect::new(
            (maximize.x - BUTTON_WIDTH).max(0.0),
            0.0,
            BUTTON_WIDTH,
            height,
        );
        let controls_x = minimize.x;
        let new_tab_width = height;
        let room = (controls_x - LEFT_PAD - new_tab_width - DRAG_RESERVE).max(0.0);
        let count_f = count as f32;
        let overflow = count > 0 && count_f * TAB_MIN_WIDTH > room;
        let (tab_width, viewport_width, arrows) = if overflow {
            let viewport = (room - 2.0 * ARROW_WIDTH).max(0.0);
            (TAB_MIN_WIDTH, viewport, true)
        } else if count > 0 {
            let tab = (room / count_f)
                .floor()
                .clamp(TAB_MIN_WIDTH.min(room), TAB_MAX_WIDTH);
            (tab, tab * count_f, false)
        } else {
            (TAB_MAX_WIDTH, 0.0, false)
        };
        let viewport = Rect::new(LEFT_PAD, 0.0, viewport_width, height);
        let max_scroll = if overflow {
            (tab_width * count_f - viewport_width).max(0.0)
        } else {
            0.0
        };
        let scroll = if scroll.is_finite() {
            scroll.clamp(0.0, max_scroll).round()
        } else {
            0.0
        };
        let tabs = (0..count)
            .map(|index| {
                Rect::new(
                    viewport.x + index as f32 * tab_width - scroll,
                    0.0,
                    tab_width,
                    height,
                )
            })
            .collect();
        let new_tab = Rect::new(viewport.right(), 0.0, new_tab_width, height);
        let (scroll_back, scroll_forward) = if arrows {
            let forward = Rect::new(controls_x - ARROW_WIDTH, 0.0, ARROW_WIDTH, height);
            let back = Rect::new(forward.x - ARROW_WIDTH, 0.0, ARROW_WIDTH, height);
            (Some(back), Some(forward))
        } else {
            (None, None)
        };
        Self {
            strip,
            viewport,
            tabs,
            new_tab,
            scroll_back,
            scroll_forward,
            minimize,
            maximize,
            close,
            scroll,
            max_scroll,
        }
    }

    /// Whether the tabs are wider than their viewport (the arrows show).
    pub fn overflows(&self) -> bool {
        self.scroll_back.is_some()
    }

    /// The visible part of tab `index`, if any of it shows.
    pub fn visible_tab(&self, index: usize) -> Option<Rect> {
        let rect = *self.tabs.get(index)?;
        let left = rect.x.max(self.viewport.x);
        let right = rect.right().min(self.viewport.right());
        (right > left).then(|| Rect::new(left, rect.y, right - left, rect.height))
    }

    /// Tab `index`'s close button.
    pub fn close_button(&self, index: usize) -> Option<Rect> {
        let tab = *self.tabs.get(index)?;
        Some(Rect::new(
            tab.right() - TAB_BUTTON - 6.0,
            tab.y + ((tab.height - TAB_BUTTON) / 2.0).round(),
            TAB_BUTTON,
            TAB_BUTTON,
        ))
    }

    /// Where tab `index` shows its activity, beside its close button's
    /// place. It stays there while the close button hides, so hovering never
    /// moves it.
    pub fn badge_slot(&self, index: usize) -> Option<Rect> {
        let close = self.close_button(index)?;
        Some(Rect::new(
            close.x - 2.0 - TAB_BUTTON,
            close.y,
            TAB_BUTTON,
            TAB_BUTTON,
        ))
    }

    /// The part under `point`; `None` outside the strip.
    pub fn part_at(&self, point: Point) -> Option<TabStripPart> {
        if !self.strip.contains(point) {
            return None;
        }
        for (rect, part) in [
            (Some(self.close), TabStripPart::Close),
            (Some(self.maximize), TabStripPart::Maximize),
            (Some(self.minimize), TabStripPart::Minimize),
            (self.scroll_forward, TabStripPart::ScrollForward),
            (self.scroll_back, TabStripPart::ScrollBack),
            (Some(self.new_tab), TabStripPart::NewTab),
        ] {
            if rect.is_some_and(|rect| rect.contains(point)) {
                return Some(part);
            }
        }
        if self.viewport.contains(point)
            && let Some(index) = self.tabs.iter().position(|tab| tab.contains(point))
        {
            if self
                .close_button(index)
                .is_some_and(|close| close.contains(point))
            {
                return Some(TabStripPart::CloseTab(index));
            }
            return Some(TabStripPart::Tab(index));
        }
        Some(TabStripPart::Empty)
    }

    /// The scroll offset that shows all of tab `index`, changing `scroll` as
    /// little as possible.
    pub fn reveal(&self, index: usize) -> f32 {
        let Some(tab) = self.tabs.get(index) else {
            return self.scroll;
        };
        let start = tab.x + self.scroll - self.viewport.x;
        let end = start + tab.width;
        let scroll = if start < self.scroll {
            start
        } else if end > self.scroll + self.viewport.width {
            end - self.viewport.width
        } else {
            self.scroll
        };
        scroll.clamp(0.0, self.max_scroll)
    }

    /// The scroll offset one arrow press moves to: about one tab toward the
    /// start (`back`) or the end.
    pub fn step(&self, back: bool) -> f32 {
        let tab = self.tabs.first().map_or(TAB_MIN_WIDTH, |tab| tab.width);
        let scroll = if back {
            self.scroll - tab
        } else {
            self.scroll + tab
        };
        scroll.clamp(0.0, self.max_scroll)
    }

    /// The strip index a tab dragged so that its center is at `x` moves to.
    pub fn drop_index(&self, x: f32) -> usize {
        let Some(first) = self.tabs.first() else {
            return 0;
        };
        let offset = x - first.x;
        ((offset / first.width).floor().max(0.0) as usize).min(self.tabs.len() - 1)
    }
}

/// The strip's interaction state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TabStripState {
    /// The tab shown in the window.
    pub active: usize,
    pub hovered: Option<TabStripPart>,
    pub pressed: Option<TabStripPart>,
    pub maximized: bool,
    /// The window has the keyboard focus; the strip dims without it.
    pub window_focused: bool,
    /// The connecting jewels' heartbeat, 0 to 1.
    pub pulse: f32,
    /// The tab being dragged to a new position, and how far its paint is
    /// from its slot.
    pub dragging: Option<(usize, f32)>,
}

/// One heartbeat: a "lub" pulse centred at 0.10 s, a "dub" at 0.32 s, and a
/// rest until the next beat (trust-desktop's loading jewel).
const HEARTBEAT_PERIOD: f32 = 1.35;
const HEARTBEAT_ACTIVE: f32 = 0.40;
const HEARTBEAT_FRAME: Duration = Duration::from_millis(16);
const HEARTBEAT_REST_TICK: Duration = Duration::from_millis(120);

/// The heartbeat's energy, 0 to 1, `elapsed` after it started.
pub fn heartbeat(elapsed: Duration) -> f32 {
    fn pulse(phase: f32, center: f32, half_width: f32) -> f32 {
        (1.0 - (phase - center).abs() / half_width).clamp(0.0, 1.0)
    }
    let phase = elapsed.as_secs_f32() % HEARTBEAT_PERIOD;
    pulse(phase, 0.10, 0.09).max(pulse(phase, 0.32, 0.075))
}

/// When the heartbeat next needs a frame: every frame while it beats, then
/// asleep until the next beat.
pub fn heartbeat_tick(elapsed: Duration) -> Duration {
    let phase = elapsed.as_secs_f32() % HEARTBEAT_PERIOD;
    if phase < HEARTBEAT_ACTIVE {
        HEARTBEAT_FRAME
    } else {
        Duration::from_secs_f32(HEARTBEAT_PERIOD - phase)
            .clamp(HEARTBEAT_FRAME, HEARTBEAT_REST_TICK)
    }
}

/// Paints the strip: its tabs with their titles and states, the new-tab
/// button, the scroll arrows while the tabs overflow, and the window
/// buttons, in COMMAND's surfaces, etched edges and accents.
pub fn paint_tab_strip(
    painter: &mut Painter<'_>,
    geometry: &TabStripGeometry,
    tabs: &[StripTab],
    state: &TabStripState,
) {
    let strip = geometry.strip;
    if strip.width <= 0.0 {
        return;
    }
    let focused = state.window_focused;
    let accent = |color: Color| if focused { color } else { DIM };
    painter.fill_rect(strip, STRIP);
    // The strip's lower edge separates the chrome from the content; the
    // shown tab opens through it.
    painter.fill_rect(
        Rect::new(strip.x, strip.bottom() - 1.0, strip.width, 1.0),
        EDGE,
    );
    painter.fill_rect(
        Rect::new(
            (geometry.minimize.x - 132.0).max(0.0),
            strip.bottom() - 1.0,
            108.0,
            1.0,
        ),
        accent(CYAN),
    );

    painter.save_clip(geometry.viewport);
    let dragged = state.dragging.map(|(index, _)| index);
    for (index, tab) in tabs.iter().enumerate() {
        if Some(index) == dragged || index == state.active {
            continue;
        }
        let Some(rect) = geometry.tabs.get(index).copied() else {
            continue;
        };
        if geometry.visible_tab(index).is_none() {
            continue;
        }
        let hovered = tab_hovered(state, index);
        if hovered {
            painter.fill_path(&tab_outline(rect), HOVER);
            painter.fill_rect(
                Rect::new(rect.x + 4.0, rect.y, (rect.width * 0.3).min(40.0), 1.0),
                accent(CYAN),
            );
        } else if index + 1 != state.active && index + 1 < tabs.len() {
            // A rule between two inactive neighbors.
            painter.fill_rect(
                Rect::new(rect.right() - 1.0, rect.y + 6.0, 1.0, rect.height - 11.0),
                RULE,
            );
        }
        paint_tab_contents(painter, geometry, index, rect, tab, state, false, hovered);
    }
    for index in [Some(state.active), dragged].into_iter().flatten() {
        let (Some(tab), Some(slot)) = (tabs.get(index), geometry.tabs.get(index).copied()) else {
            continue;
        };
        let offset = state
            .dragging
            .filter(|(dragging, _)| *dragging == index)
            .map_or(0.0, |(_, offset)| offset);
        let rect = slot.translate(offset, 0.0);
        let active = index == state.active;
        painter.fill_path(&tab_outline(rect), if active { SURFACE } else { HOVER });
        painter.stroke_path(&tab_edge(rect), 1.0, Cap::Butt, EDGE);
        if active {
            painter.fill_rect(
                Rect::new(rect.x + 4.0, rect.y, (rect.width * 0.4).min(64.0), 2.0),
                accent(PINK),
            );
        }
        let hovered = tab_hovered(state, index);
        paint_tab_contents(painter, geometry, index, rect, tab, state, active, hovered);
    }
    painter.restore();

    paint_button(
        painter,
        geometry.new_tab,
        state,
        TabStripPart::NewTab,
        false,
        accent(CYAN),
        |painter, c, color| {
            let mut plus = Path::new();
            plus.move_to(c.x - 5.0, c.y);
            plus.line_to(c.x + 5.0, c.y);
            plus.move_to(c.x, c.y - 5.0);
            plus.line_to(c.x, c.y + 5.0);
            painter.stroke_path(&plus, 1.5, Cap::Round, color);
        },
    );
    if let (Some(back), Some(forward)) = (geometry.scroll_back, geometry.scroll_forward) {
        for (rect, part, enabled, sign) in [
            (back, TabStripPart::ScrollBack, geometry.scroll > 0.0, -1.0),
            (
                forward,
                TabStripPart::ScrollForward,
                geometry.scroll < geometry.max_scroll,
                1.0,
            ),
        ] {
            let color = if enabled { accent(CYAN) } else { DISABLED };
            paint_button(
                painter,
                rect,
                state,
                part,
                false,
                color,
                |painter, c, color| {
                    let mut chevron = Path::new();
                    chevron.move_to(c.x - 2.5 * sign, c.y - 5.0);
                    chevron.line_to(c.x + 2.5 * sign, c.y);
                    chevron.line_to(c.x - 2.5 * sign, c.y + 5.0);
                    painter.stroke_path(&chevron, 1.5, Cap::Round, color);
                },
            );
        }
    }
    // Decorative traces in the empty strip, as on COMMAND's frame.
    let traces_end = geometry
        .scroll_back
        .map_or(geometry.minimize.x, |back| back.x)
        - 10.0;
    if traces_end - geometry.new_tab.right() > 48.0 {
        let mut traces = Path::new();
        for i in 0..3 {
            let x = traces_end - 24.0 + i as f32 * 8.0;
            traces.move_to(x, strip.y + 8.0);
            traces.line_to(x + 8.0, strip.y + 16.0);
        }
        painter.stroke_path(&traces, 2.0, Cap::Butt, EDGE);
    }

    let text = if focused { TEXT } else { MUTED };
    paint_button(
        painter,
        geometry.minimize,
        state,
        TabStripPart::Minimize,
        false,
        text,
        |painter, c, color| {
            let mut line = Path::new();
            line.move_to(c.x - 5.0, c.y + 0.5);
            line.line_to(c.x + 5.0, c.y + 0.5);
            painter.stroke_path(&line, 1.25, Cap::Round, color);
        },
    );
    let maximized = state.maximized;
    paint_button(
        painter,
        geometry.maximize,
        state,
        TabStripPart::Maximize,
        false,
        text,
        |painter, c, color| {
            if maximized {
                // Restore: the window behind the window.
                let mut behind = Path::new();
                behind.move_to(c.x - 2.5, c.y - 2.5);
                behind.line_to(c.x - 2.5, c.y - 4.5);
                behind.line_to(c.x + 4.5, c.y - 4.5);
                behind.line_to(c.x + 4.5, c.y + 2.5);
                behind.line_to(c.x + 2.5, c.y + 2.5);
                painter.stroke_path(&behind, 1.25, Cap::Round, color);
                painter.stroke_path(
                    &rect_path(Rect::new(c.x - 4.5, c.y - 2.5, 7.0, 7.0)),
                    1.25,
                    Cap::Round,
                    color,
                );
            } else {
                painter.stroke_path(
                    &rect_path(Rect::new(c.x - 4.5, c.y - 4.5, 9.0, 9.0)),
                    1.25,
                    Cap::Round,
                    color,
                );
            }
        },
    );
    paint_button(
        painter,
        geometry.close,
        state,
        TabStripPart::Close,
        true,
        text,
        |painter, c, color| {
            cross(painter, c, 4.5, color, 1.25);
        },
    );
}

fn tab_hovered(state: &TabStripState, index: usize) -> bool {
    matches!(
        state.hovered,
        Some(TabStripPart::Tab(i) | TabStripPart::CloseTab(i)) if i == index
    )
}

/// The tab's shape: COMMAND's chamfered corners, small at the top left and
/// larger at the top right, open at the bottom where the content begins.
fn tab_outline(rect: Rect) -> Path {
    let (right, bottom) = (rect.right(), rect.bottom());
    Path::polygon(&[
        Point::new(rect.x, bottom),
        Point::new(rect.x, rect.y + 4.0),
        Point::new(rect.x + 4.0, rect.y),
        Point::new(right - 9.0, rect.y),
        Point::new(right, rect.y + 9.0),
        Point::new(right, bottom),
    ])
}

/// The tab outline's etched edge, without the open bottom, on pixel centers.
fn tab_edge(rect: Rect) -> Path {
    let (right, bottom) = (rect.right() - 0.5, rect.bottom());
    let (left, top) = (rect.x + 0.5, rect.y + 0.5);
    let mut path = Path::new();
    path.move_to(left, bottom);
    path.line_to(left, top + 4.0);
    path.line_to(left + 4.0, top);
    path.line_to(right - 9.0, top);
    path.line_to(right, top + 9.0);
    path.line_to(right, bottom);
    path
}

#[allow(clippy::too_many_arguments)]
fn paint_tab_contents(
    painter: &mut Painter<'_>,
    geometry: &TabStripGeometry,
    index: usize,
    rect: Rect,
    tab: &StripTab,
    state: &TabStripState,
    active: bool,
    hovered: bool,
) {
    let focused = state.window_focused;
    let center_y = rect.y + rect.height / 2.0;
    // The jewel: the connection, as COMMAND's status light shows state.
    let jewel = Rect::new(rect.x + 11.0, (center_y - 2.5).round(), 5.0, 5.0);
    match tab.status {
        TabStatus::Connected => {
            painter.fill_rect(jewel, if active && focused { CYAN } else { DIM })
        }
        TabStatus::Connecting => {
            let alpha = (110.0 + 145.0 * state.pulse.clamp(0.0, 1.0)) as u8;
            painter.fill_rect(jewel, AMBER.with_alpha(alpha));
        }
        TabStatus::Disconnected => painter.stroke_rect(jewel, 1.0, DISABLED),
        TabStatus::Failed => painter.fill_rect(jewel, if focused { PINK } else { DIM }),
    }

    let show_close = active || hovered || rect.width >= CLOSE_ALWAYS_WIDTH;
    // A dragged tab's buttons move with it.
    let shift = |button: Rect| button.translate(rect.x - geometry.tabs[index].x, 0.0);
    let mut title_right = rect.right() - 8.0;
    if show_close && let Some(close) = geometry.close_button(index).map(shift) {
        title_right = close.x - 3.0;
        let close_hovered = state.hovered == Some(TabStripPart::CloseTab(index));
        let pressed = state.pressed == Some(TabStripPart::CloseTab(index));
        if close_hovered || pressed {
            painter.fill_rect(close, if pressed { INSET } else { PINK });
        }
        let color = if close_hovered || pressed {
            HEART
        } else if active && focused {
            MUTED
        } else {
            DISABLED
        };
        cross(painter, close.center(), 3.5, color, 1.25);
    }
    if let Some(slot) = geometry.badge_slot(index).map(shift) {
        if tab.highlights > 0 {
            let count = if tab.highlights > 99 {
                String::from("@99+")
            } else {
                format!("@{}", tab.highlights)
            };
            let style = theme::chrome(BADGE_SIZE);
            let width = painter.text.width(&count, &style);
            let badge = Rect::new(slot.right() - width, rect.y, width, rect.height);
            painter.label(
                &count,
                badge,
                &style,
                if focused { PINK } else { DIM },
                Align::Start,
            );
            title_right = title_right.min(badge.x - 4.0);
        } else if tab.unread {
            let c = slot.center();
            let dot = Rect::new((c.x - 2.0).round(), (c.y - 2.0).round(), 4.0, 4.0);
            painter.fill_rect(dot, if focused { CYAN } else { DIM });
            title_right = title_right.min(dot.x - 4.0);
        }
    }
    let title_left = rect.x + 23.0;
    let color = match (tab.status == TabStatus::Disconnected, active, focused) {
        (true, ..) => DISABLED,
        (false, true, true) => TEXT,
        (false, true, false) | (false, false, true) => MUTED,
        (false, false, false) => DISABLED,
    };
    painter.label(
        &tab.title,
        Rect::new(
            title_left,
            rect.y,
            (title_right - title_left).max(0.0),
            rect.height,
        ),
        &theme::chrome(TITLE_SIZE),
        color,
        Align::Start,
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_button(
    painter: &mut Painter<'_>,
    rect: Rect,
    state: &TabStripState,
    part: TabStripPart,
    danger: bool,
    color: Color,
    icon: impl FnOnce(&mut Painter<'_>, Point, Color),
) {
    let hovered = state.hovered == Some(part);
    let pressed = state.pressed == Some(part);
    let color = if pressed && danger {
        painter.fill_rect(rect, INSET);
        PINK
    } else if hovered && danger {
        painter.fill_rect(rect, PINK);
        HEART
    } else if pressed {
        painter.fill_rect(rect, INSET);
        color
    } else if hovered {
        painter.fill_rect(rect, HOVER);
        color
    } else {
        color
    };
    let center = Point::new(
        (rect.x + rect.width / 2.0).round(),
        (rect.y + rect.height / 2.0).round(),
    );
    icon(painter, center, color);
}

fn cross(painter: &mut Painter<'_>, center: Point, half: f32, color: Color, width: f32) {
    let mut path = Path::new();
    path.move_to(center.x - half, center.y - half);
    path.line_to(center.x + half, center.y + half);
    path.move_to(center.x + half, center.y - half);
    path.line_to(center.x - half, center.y + half);
    painter.stroke_path(&path, width, Cap::Round, color);
}

fn rect_path(rect: Rect) -> Path {
    Path::polygon(&[
        Point::new(rect.x, rect.y),
        Point::new(rect.right(), rect.y),
        Point::new(rect.right(), rect.bottom()),
        Point::new(rect.x, rect.bottom()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Canvas, TextSystem};

    fn tabs(count: usize) -> Vec<StripTab> {
        (0..count)
            .map(|index| StripTab {
                title: format!("Network number {index}"),
                status: TabStatus::Connected,
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn tabs_share_the_strip_until_they_reach_their_minimum_width() {
        let geometry = TabStripGeometry::new(1280.0, 3, 0.0);
        assert!(!geometry.overflows());
        assert_eq!(geometry.tabs[0].width, TAB_MAX_WIDTH);
        assert_eq!(geometry.tabs[1].x, geometry.tabs[0].x + TAB_MAX_WIDTH);
        assert_eq!(
            geometry.new_tab.x,
            geometry.tabs[2].right(),
            "the new-tab button follows the last tab"
        );
        assert_eq!(geometry.close.right(), 1280.0);
        let many = TabStripGeometry::new(1280.0, 9, 0.0);
        assert!(!many.overflows());
        assert!(many.tabs[0].width < TAB_MAX_WIDTH && many.tabs[0].width >= TAB_MIN_WIDTH);
        assert_eq!(
            many.tabs[0].width.fract(),
            0.0,
            "whole pixels keep edges crisp"
        );
        let none = TabStripGeometry::new(1280.0, 0, 0.0);
        assert_eq!(none.new_tab.x, LEFT_PAD);
    }

    #[test]
    fn arrows_appear_only_when_the_tabs_overflow() {
        let fits = TabStripGeometry::new(1280.0, 8, 0.0);
        assert!(fits.scroll_back.is_none() && fits.scroll_forward.is_none());
        let overflow = TabStripGeometry::new(1280.0, 30, 0.0);
        assert!(overflow.overflows());
        let (back, forward) = (
            overflow.scroll_back.unwrap(),
            overflow.scroll_forward.unwrap(),
        );
        assert_eq!(
            forward.right(),
            overflow.minimize.x,
            "the arrows sit beside the window buttons"
        );
        assert!(back.x < forward.x);
        assert!(overflow.viewport.right() <= overflow.new_tab.x);
        assert!(
            overflow.new_tab.right() + DRAG_RESERVE <= back.x + 0.01,
            "some empty strip always remains to move the window by"
        );
        assert_eq!(overflow.tabs[0].width, TAB_MIN_WIDTH);
        assert!(overflow.max_scroll > 0.0);
    }

    #[test]
    fn scrolling_reveals_tabs_and_clamps() {
        let geometry = TabStripGeometry::new(900.0, 40, 0.0);
        let last = geometry.tabs.len() - 1;
        let scroll = geometry.reveal(last);
        assert_eq!(scroll, geometry.max_scroll);
        let scrolled = TabStripGeometry::new(900.0, 40, scroll);
        assert_eq!(
            scrolled.visible_tab(last).unwrap().width,
            TAB_MIN_WIDTH,
            "the whole tab shows"
        );
        assert!(scrolled.visible_tab(0).is_none());
        assert_eq!(
            scrolled.reveal(last),
            scroll,
            "a visible tab needs no scroll"
        );
        assert_eq!(scrolled.reveal(0), 0.0);
        assert_eq!(
            TabStripGeometry::new(900.0, 40, 1e9).scroll,
            geometry.max_scroll
        );
        assert_eq!(TabStripGeometry::new(900.0, 40, f32::NAN).scroll, 0.0);
        assert!(geometry.step(false) > 0.0 && geometry.step(true) == 0.0);
    }

    #[test]
    fn hit_testing_finds_each_part() {
        let geometry = TabStripGeometry::new(1280.0, 3, 0.0);
        let center = |rect: Rect| rect.center();
        assert_eq!(
            geometry.part_at(center(geometry.tabs[1])),
            Some(TabStripPart::Tab(1))
        );
        assert_eq!(
            geometry.part_at(center(geometry.close_button(2).unwrap())),
            Some(TabStripPart::CloseTab(2))
        );
        assert_eq!(
            geometry.part_at(center(geometry.badge_slot(2).unwrap())),
            Some(TabStripPart::Tab(2)),
            "the badge is not a button"
        );
        assert_eq!(
            geometry.part_at(center(geometry.new_tab)),
            Some(TabStripPart::NewTab)
        );
        assert_eq!(
            geometry.part_at(center(geometry.minimize)),
            Some(TabStripPart::Minimize)
        );
        assert_eq!(
            geometry.part_at(center(geometry.maximize)),
            Some(TabStripPart::Maximize)
        );
        assert_eq!(
            geometry.part_at(center(geometry.close)),
            Some(TabStripPart::Close)
        );
        assert_eq!(
            geometry.part_at(Point::new(geometry.new_tab.x + 80.0, 10.0)),
            Some(TabStripPart::Empty)
        );
        assert_eq!(
            geometry.part_at(Point::new(geometry.tabs[0].x + 30.0, 0.0)),
            Some(TabStripPart::Tab(0)),
            "tabs fill the strip's height"
        );
        assert_eq!(
            geometry.part_at(Point::new(geometry.tabs[2].x + 30.0, STRIP_HEIGHT - 0.5)),
            Some(TabStripPart::Tab(2))
        );
        assert_eq!(geometry.part_at(Point::new(10.0, STRIP_HEIGHT + 1.0)), None);
        let overflow = TabStripGeometry::new(900.0, 40, 0.0);
        assert_ne!(
            overflow.part_at(center(overflow.tabs[39])),
            Some(TabStripPart::Tab(39)),
            "a scrolled-away tab cannot be hit"
        );
        assert_eq!(overflow.drop_index(overflow.tabs[3].x + 5.0), 3);
        assert_eq!(overflow.drop_index(-1e6), 0);
        assert_eq!(overflow.drop_index(1e6), 39);
    }

    #[test]
    fn the_heartbeat_beats_twice_then_rests() {
        let at = |ms| heartbeat(Duration::from_millis(ms));
        assert_eq!(at(100), 1.0, "lub");
        assert_eq!(at(320), 1.0, "dub");
        assert_eq!(at(800), 0.0, "rest");
        assert_eq!(heartbeat_tick(Duration::from_millis(100)), HEARTBEAT_FRAME);
        assert_eq!(
            heartbeat_tick(Duration::from_millis(800)),
            HEARTBEAT_REST_TICK
        );
        assert!(
            heartbeat_tick(Duration::from_millis(1340)) < HEARTBEAT_REST_TICK,
            "wakes for the next beat"
        );
    }

    #[test]
    fn painting_stays_inside_the_strip() {
        let (width, height) = (1024u32, 60u32);
        let mut canvas = Canvas::new(width, height);
        let mut text = TextSystem::new();
        let geometry = TabStripGeometry::new(width as f32, 12, 30.0);
        let mut list = tabs(12);
        list[3].highlights = 4;
        list[5].unread = true;
        list[6].status = TabStatus::Connecting;
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        paint_tab_strip(
            &mut painter,
            &geometry,
            &list,
            &TabStripState {
                active: 2,
                hovered: Some(TabStripPart::CloseTab(3)),
                window_focused: true,
                pulse: 0.5,
                ..Default::default()
            },
        );
        for y in STRIP_HEIGHT as u32..height {
            for x in 0..width {
                assert_eq!(canvas.pixel(x, y), 0, "paint below the strip at ({x}, {y})");
            }
        }
        assert_eq!(canvas.pixel(width - 1, 0), STRIP.pixel());
    }

    /// `RUBYCHAT_SNAPSHOTS=DIR cargo test --lib tab_strip_snapshots -- --ignored`
    /// writes strips at twice their size, as trust-desktop's
    /// `tab_strip_preview` does, for side-by-side comparison.
    #[test]
    #[ignore = "writes snapshot images; set RUBYCHAT_SNAPSHOTS"]
    fn tab_strip_snapshots() {
        let Some(dir) = std::env::var_os("RUBYCHAT_SNAPSHOTS") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let titles = [
            ("Libera.Chat", TabStatus::Connected, 2, true),
            (
                "OFTC — a very long network name that goes on",
                TabStatus::Connecting,
                0,
                false,
            ),
            ("hackint", TabStatus::Connected, 0, true),
            ("Rizon", TabStatus::Disconnected, 0, false),
            ("EFnet", TabStatus::Failed, 0, false),
            ("Ergo test network", TabStatus::Connected, 0, false),
        ];
        let mut text = TextSystem::new();
        // trust-desktop's own preview titles, for a like-for-like comparison
        // with its `tab-strip-few.png`.
        let parity = [
            ("Ruby's House — strawberries and tea", TabStatus::Connected),
            (
                "https://rubymaelstrom.com/blog/a-very-long-address-that-goes-on",
                TabStatus::Connecting,
            ),
            ("telnet://bbs.example.org:23", TabStatus::Connected),
            ("Radio Paradise — Main Mix", TabStatus::Connected),
            ("Sleeping page", TabStatus::Disconnected),
        ];
        let list: Vec<StripTab> = parity
            .iter()
            .map(|&(title, status)| StripTab {
                title: title.to_string(),
                status,
                ..Default::default()
            })
            .collect();
        let geometry = TabStripGeometry::new(1100.0, list.len(), 0.0);
        let mut canvas = Canvas::new(2200, 120);
        let mut painter = Painter::new(&mut canvas, &mut text, 2.0);
        painter.fill_rect(Rect::new(0.0, 0.0, 1100.0, 60.0), Color::rgb(250, 250, 252));
        paint_tab_strip(
            &mut painter,
            &geometry,
            &list,
            &TabStripState {
                active: 0,
                hovered: Some(TabStripPart::Tab(1)),
                window_focused: true,
                pulse: 0.6,
                ..Default::default()
            },
        );
        crate::render::png::write_rgba(
            &dir.join("tab-strip-parity.png"),
            2200,
            120,
            &canvas.to_rgba(),
        )
        .unwrap();

        for (name, width, count, active, hovered, focused, maximized, scroll) in [
            (
                "few",
                1100.0,
                5,
                0,
                Some(TabStripPart::Tab(1)),
                true,
                false,
                0.0,
            ),
            (
                "hover-close",
                1100.0,
                5,
                3,
                Some(TabStripPart::CloseTab(3)),
                true,
                true,
                0.0,
            ),
            ("unfocused", 1100.0, 5, 1, None, false, false, 0.0),
            (
                "overflow",
                900.0,
                14,
                6,
                Some(TabStripPart::ScrollForward),
                true,
                false,
                300.0,
            ),
            (
                "window-close",
                700.0,
                2,
                0,
                Some(TabStripPart::Close),
                true,
                false,
                0.0,
            ),
            (
                "empty",
                700.0,
                0,
                0,
                Some(TabStripPart::NewTab),
                true,
                false,
                0.0,
            ),
        ] {
            let list: Vec<StripTab> = (0..count)
                .map(|index| {
                    let (title, status, highlights, unread) = titles[index % titles.len()];
                    StripTab {
                        title: title.to_string(),
                        status,
                        highlights,
                        unread,
                    }
                })
                .collect();
            let geometry = TabStripGeometry::new(width, count, scroll);
            let height = 60.0;
            let mut canvas = Canvas::new(width as u32 * 2, height as u32 * 2);
            let mut painter = Painter::new(&mut canvas, &mut text, 2.0);
            painter.fill_rect(
                Rect::new(0.0, 0.0, width, height),
                Color::rgb(250, 250, 252),
            );
            paint_tab_strip(
                &mut painter,
                &geometry,
                &list,
                &TabStripState {
                    active,
                    hovered,
                    pressed: None,
                    maximized,
                    window_focused: focused,
                    pulse: 0.6,
                    dragging: None,
                },
            );
            crate::render::png::write_rgba(
                &dir.join(format!("tab-strip-{name}.png")),
                canvas.width(),
                canvas.height(),
                &canvas.to_rgba(),
            )
            .unwrap();
        }
    }
}
