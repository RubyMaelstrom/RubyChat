//! The chat view: a buffer's lines, newest at the bottom, in columns—the
//! time, the nick (right-aligned and colored), a rule, and the message with
//! a hanging indent—with mIRC formatting, links, highlights, and labeled
//! rules for unread messages and new days (DESIGN.md §8.6).
//!
//! Only the lines in view are laid out. A scrolled view rests on an anchor
//! line, so arriving lines and width changes never move what you're reading.

use std::collections::VecDeque;
use std::ops::Range;

use crate::irc::casemap::Casemap;
use crate::irc::format::{self, Formatted};
use crate::model::buffer::Anchor;
use crate::model::links::{find_channels, find_links, target};
use crate::model::{Line, LineKind};
use crate::platform::time;
use crate::render::paragraph::{self, Paragraph};
use crate::render::{
    Align, Color, Decoration, Painter, Point, Rect, Span, TextStyle, TextSystem, Weight,
};

use super::colors::{irc_color, nick_color};
use super::theme::{
    self, AMBER, CANVAS, CYAN, DIM, DISABLED, EDGE, INSET, MUTED, PINK, RULE, TEXT,
};

/// 11 pt: trust-desktop's terminal size.
pub const CHAT_SIZE: f32 = 11.0 * 96.0 / 72.0;
const PADDING_X: f32 = 14.0;
const PADDING_Y: f32 = 10.0;
/// Extra space between rows, for easier reading.
const ROW_GAP: f32 = 3.0;
const COLUMN_GAP: f32 = 10.0;
/// The nick column is as wide as the buffer's longest label, within these
/// bounds in characters; longer nicks are ellipsized.
const LABEL_MIN: usize = 5;
const LABEL_MAX: usize = 14;
/// A labeled rule: NEW MESSAGES, or a new day.
const RULE_HEIGHT: f32 = 26.0;
/// Selected text's background.
pub const SELECTION: Color = Color::rgba(0x6e, 0x4e, 0x9e, 150);

/// Chat text: monospaced and without ligatures, so `->` stays two
/// characters and ASCII takes the shaping fast path.
pub fn chat_style() -> TextStyle {
    TextStyle::new(CHAT_SIZE, Weight::Regular).without_ligatures()
}

/// What a link opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Url(String),
    Channel(String),
}

/// A place in the text: a line and a byte offset into its message column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPosition {
    pub id: u64,
    pub offset: usize,
}

/// What the view needs to know about its buffer and network.
#[derive(Clone, Copy, Debug)]
pub struct ChatContext<'a> {
    pub casemap: Casemap,
    pub chantypes: &'a str,
    /// Where the NEW MESSAGES rule goes.
    pub marker: Option<u64>,
    pub label_chars: usize,
}

/// Where the columns go in a view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Columns {
    pub time_x: f32,
    pub label_x: f32,
    pub label_width: f32,
    pub rule_x: f32,
    pub text_x: f32,
    pub text_width: f32,
    pub row_height: f32,
}

impl Columns {
    pub fn new(text: &mut TextSystem, rect: Rect, label_chars: usize) -> Self {
        let style = chat_style();
        let metrics = text.metrics(&style);
        let digit = text.width("0", &style);
        let time_x = rect.x + PADDING_X;
        let label_x = time_x + 5.0 * digit + COLUMN_GAP;
        let label_width = digit * label_chars.clamp(LABEL_MIN, LABEL_MAX) as f32;
        let rule_x = (label_x + label_width + COLUMN_GAP - 2.0).round();
        let text_x = rule_x + 1.0 + COLUMN_GAP - 2.0;
        Self {
            time_x,
            label_x,
            label_width,
            rule_x,
            text_x,
            text_width: (rect.right() - PADDING_X - text_x).max(digit * 8.0),
            row_height: (metrics.line_height + ROW_GAP).round(),
        }
    }
}

/// One line laid out: an optional labeled rule above, then its rows.
struct Block {
    rule: Option<(String, bool)>,
    paragraph: Paragraph,
    links: Vec<(Range<usize>, Target)>,
    /// The message column's text, as laid out.
    plain: String,
    height: f32,
}

impl Block {
    fn rule_height(&self) -> f32 {
        if self.rule.is_some() {
            RULE_HEIGHT
        } else {
            0.0
        }
    }
}

/// Label and text colors for each kind of line.
fn colors(line: &Line, casemap: Casemap) -> (Color, Color) {
    match line.kind {
        LineKind::Message if line.own => (MUTED, TEXT),
        LineKind::Message if !line.nick.is_empty() => (nick_color(&casemap.fold(&line.nick)), TEXT),
        LineKind::Message | LineKind::Server => (MUTED, TEXT),
        LineKind::Action => (MUTED, TEXT),
        LineKind::Notice => (AMBER, TEXT),
        LineKind::Status => (CYAN, MUTED),
        LineKind::Sent => (DISABLED, MUTED),
        LineKind::Event => (MUTED, MUTED),
        LineKind::Error => (PINK, PINK),
    }
}

/// Control characters left after formatting is read are drawn as visible
/// symbols (Control Pictures), so nothing in a message is invisible
/// (DESIGN.md §13). Styled ranges move with the text.
fn show_controls(formatted: &mut Formatted) {
    if !formatted.text.chars().any(|c| c.is_ascii_control()) {
        return;
    }
    let mut text = String::with_capacity(formatted.text.len() + 8);
    let mut moved = Vec::with_capacity(formatted.text.len() + 1);
    for c in formatted.text.chars() {
        let at = text.len();
        moved.extend(std::iter::repeat_n(at, c.len_utf8()));
        text.push(match c {
            '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + c as u32).unwrap_or(c),
            '\u{7f}' => '\u{2421}',
            c => c,
        });
    }
    moved.push(text.len());
    for (range, _) in &mut formatted.spans {
        *range = moved[range.start]..moved[range.end];
    }
    formatted.text = text;
}

fn weekday(year: i32, month: u8, day: u8) -> &'static str {
    const DAYS: [&str; 7] = [
        "THURSDAY",
        "FRIDAY",
        "SATURDAY",
        "SUNDAY",
        "MONDAY",
        "TUESDAY",
        "WEDNESDAY",
    ];
    // 1 January 1970 was a Thursday.
    DAYS[time::days_from_civil(year, month, day).rem_euclid(7) as usize]
}

/// The day a time falls on, as a rule's label: FRIDAY 30 OCTOBER 2026.
fn day_label(unix_ms: i64) -> String {
    const MONTHS: [&str; 12] = [
        "JANUARY",
        "FEBRUARY",
        "MARCH",
        "APRIL",
        "MAY",
        "JUNE",
        "JULY",
        "AUGUST",
        "SEPTEMBER",
        "OCTOBER",
        "NOVEMBER",
        "DECEMBER",
    ];
    let local = time::local(unix_ms);
    let month = MONTHS[usize::from(local.month.clamp(1, 12) - 1)];
    format!(
        "{} {} {month} {}",
        weekday(local.year, local.month, local.day),
        local.day,
        local.year
    )
}

fn same_day(a: i64, b: i64) -> bool {
    let (a, b) = (time::local(a), time::local(b));
    (a.year, a.month, a.day) == (b.year, b.month, b.day)
}

fn layout_block(
    text: &mut TextSystem,
    columns: &Columns,
    context: &ChatContext<'_>,
    lines: &VecDeque<Line>,
    index: usize,
) -> Block {
    let line = &lines[index];
    let new_day = index > 0 && !same_day(lines[index - 1].time, line.time);
    let rule = match (context.marker == Some(line.id), new_day) {
        (true, true) => Some((format!("NEW MESSAGES · {}", day_label(line.time)), true)),
        (true, false) => Some((String::from("NEW MESSAGES"), true)),
        (false, true) => Some((day_label(line.time), false)),
        (false, false) => None,
    };
    let (_, base) = colors(line, context.casemap);
    let formatting = matches!(
        line.kind,
        LineKind::Message | LineKind::Action | LineKind::Notice
    );
    let mut formatted = if formatting {
        format::parse(&line.text)
    } else {
        Formatted {
            text: line.text.clone(),
            spans: Vec::new(),
        }
    };
    show_controls(&mut formatted);
    // An action reads "nick waves": the nick leads the text.
    let mut lead = 0..0;
    if line.kind == LineKind::Action {
        let nick = format!("{} ", line.nick);
        lead = 0..line.nick.len();
        for (range, _) in &mut formatted.spans {
            *range = range.start + nick.len()..range.end + nick.len();
        }
        formatted.text.insert_str(0, &nick);
    }
    let plain = formatted.text;
    let mut links: Vec<(Range<usize>, Target)> = find_links(&plain)
        .into_iter()
        .map(|range| (range.clone(), Target::Url(target(&plain[range]))))
        .collect();
    if formatting {
        for range in find_channels(&plain, context.chantypes) {
            if !links
                .iter()
                .any(|(link, _)| link.start < range.end && range.start < link.end)
            {
                links.push((range.clone(), Target::Channel(plain[range].to_string())));
            }
        }
        links.sort_by_key(|(range, _)| range.start);
    }
    let mut cuts = vec![0, plain.len(), lead.start, lead.end];
    for (range, _) in &formatted.spans {
        cuts.extend([range.start, range.end]);
    }
    for (range, _) in &links {
        cuts.extend([range.start, range.end]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let regular = chat_style();
    let mut spans = Vec::with_capacity(cuts.len());
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        let style = formatted
            .spans
            .iter()
            .find(|(range, _)| range.start <= start && start < range.end)
            .map(|(_, style)| *style)
            .unwrap_or_default();
        let link = links
            .iter()
            .any(|(range, _)| range.start <= start && start < range.end);
        let leading = lead.start <= start && start < lead.end;
        let mut foreground = style.foreground.map(|color| color.rgb());
        let mut background = style.background.map(|color| color.rgb());
        if style.reverse {
            (foreground, background) = (
                Some(background.unwrap_or(CANVAS.pixel())),
                Some(foreground.unwrap_or(base.pixel())),
            );
        }
        let underneath = background.map_or(CANVAS, Color::hex);
        let color = if leading {
            nick_color(&context.casemap.fold(&line.nick))
        } else if link && style.foreground.is_none() {
            CYAN
        } else {
            foreground.map_or(base, |rgb| {
                irc_color(format::IrcColor::Rgb(rgb), underneath)
            })
        };
        spans.push(Span {
            text: &plain[start..end],
            style: TextStyle {
                weight: if style.bold || leading {
                    Weight::Bold
                } else {
                    Weight::Regular
                },
                ..regular
            },
            color,
            decoration: Decoration {
                underline: style.underline,
                strikethrough: style.strikethrough,
                oblique: style.italic || line.kind == LineKind::Action,
                background: background.map(Color::hex),
            },
        });
    }
    let paragraph = paragraph::layout(text, &spans, columns.text_width);
    let height = paragraph.row_count() as f32 * columns.row_height
        + if rule.is_some() { RULE_HEIGHT } else { 0.0 };
    Block {
        rule,
        paragraph,
        links,
        plain,
        height,
    }
}

fn index_of(lines: &VecDeque<Line>, id: u64) -> Option<usize> {
    let first = lines.front()?.id;
    if id < first {
        return Some(0);
    }
    let index = usize::try_from(id - first).ok()?;
    (index < lines.len()).then_some(index)
}

/// The anchor after scrolling `pixels` toward older lines (negative:
/// toward newer). `None` follows the newest line. Scrolling past the
/// oldest line is corrected when the view is painted.
pub fn scroll(
    text: &mut TextSystem,
    rect: Rect,
    context: &ChatContext<'_>,
    lines: &VecDeque<Line>,
    anchor: Option<Anchor>,
    pixels: f32,
) -> Option<Anchor> {
    let last = lines.len().checked_sub(1)?;
    let columns = Columns::new(text, rect, context.label_chars);
    let height =
        |text: &mut TextSystem, index| layout_block(text, &columns, context, lines, index).height;
    let (mut index, mut below) = match anchor {
        Some(anchor) => (index_of(lines, anchor.id).unwrap_or(last), anchor.below),
        None => (last, 0.0),
    };
    below += pixels;
    while index > 0 {
        let height = height(text, index);
        if below < height {
            break;
        }
        below -= height;
        index -= 1;
    }
    while below < 0.0 {
        if index == last {
            return None;
        }
        index += 1;
        below += height(text, index);
    }
    Some(Anchor {
        id: lines[index].id,
        below,
    })
}

/// A line as painted: for finding what the pointer is over, and for
/// painting it again without laying it out anew.
#[derive(Clone, Debug)]
pub struct ShownLine {
    pub id: u64,
    /// The first row's top, below any rule.
    pub top: f32,
    /// Its layout, without selection or hover.
    pub paragraph: Paragraph,
    pub links: Vec<(Range<usize>, Target)>,
    pub plain: String,
    /// Who said it, when the nick column names them.
    pub nick: Option<String>,
    rule: Option<(String, bool)>,
    /// With the rule.
    height: f32,
}

impl ShownLine {
    fn into_block(self) -> Block {
        Block {
            rule: self.rule,
            paragraph: self.paragraph,
            links: self.links,
            plain: self.plain,
            height: self.height,
        }
    }
}

/// What the last paint showed.
#[derive(Clone, Debug, Default)]
pub struct ChatFrame {
    pub rect: Rect,
    /// The anchor as shown: corrected when it scrolled past the oldest line.
    pub anchor: Option<Anchor>,
    pub columns: Option<Columns>,
    /// Painted lines, newest first.
    pub shown: Vec<ShownLine>,
    /// What the layouts depended on besides the lines themselves.
    marker: Option<u64>,
    chantypes: String,
    casemap: Casemap,
}

impl ChatFrame {
    fn row_at(&self, point: Point) -> Option<(&ShownLine, usize)> {
        let columns = self.columns?;
        self.shown.iter().find_map(|line| {
            let rows = line.paragraph.rows.len().max(1);
            let row = ((point.y - line.top) / columns.row_height).floor();
            (row >= 0.0 && (row as usize) < rows).then_some((line, row as usize))
        })
    }

    /// The byte offset in `line`'s text nearest `x` on `row`.
    fn offset_in(&self, line: &ShownLine, row: usize, x: f32) -> usize {
        let Some(columns) = self.columns else {
            return 0;
        };
        let Some((range, origin)) = line.paragraph.rows.get(row) else {
            return 0;
        };
        let x = x - columns.text_x + origin;
        let glyphs = &line.paragraph.glyphs[range.clone()];
        if x < glyphs.first().map_or(0.0, |g| g.glyph.x) {
            return glyphs.first().map_or(0, |g| g.glyph.cluster as usize);
        }
        for (index, placed) in glyphs.iter().enumerate() {
            if x < placed.glyph.x + placed.glyph.advance / 2.0 {
                return placed.glyph.cluster as usize;
            }
            if x < placed.glyph.x + placed.glyph.advance {
                return glyphs
                    .get(index + 1)
                    .map_or_else(|| self.row_end(line, row), |g| g.glyph.cluster as usize);
            }
        }
        self.row_end(line, row)
    }

    fn row_end(&self, line: &ShownLine, row: usize) -> usize {
        match line.paragraph.rows.get(row + 1) {
            Some((next, _)) => line.paragraph.glyphs[next.start].glyph.cluster as usize,
            None => line.plain.len(),
        }
    }

    /// The nick whose label is under `point`.
    pub fn nick_at(&self, point: Point) -> Option<&str> {
        let columns = self.columns?;
        if point.x < columns.label_x || point.x >= columns.label_x + columns.label_width {
            return None;
        }
        match self.row_at(point)? {
            (line, 0) => line.nick.as_deref(),
            _ => None,
        }
    }

    /// The link under `point`.
    pub fn link_at(&self, point: Point) -> Option<(u64, Range<usize>, Target)> {
        let columns = self.columns?;
        let (line, row) = self.row_at(point)?;
        let (range, origin) = line.paragraph.rows.get(row)?;
        let x = point.x - columns.text_x + origin;
        let under = line.paragraph.glyphs[range.clone()]
            .iter()
            .find(|g| g.glyph.x <= x && x < g.glyph.x + g.glyph.advance)?
            .glyph
            .cluster as usize;
        line.links
            .iter()
            .find(|(link, _)| link.start <= under && under < link.end)
            .map(|(link, target)| (line.id, link.clone(), target.clone()))
    }

    /// The text position nearest `point`, for selecting: before a line's
    /// text when left of it, and at the ends of the view when above or
    /// below every line.
    pub fn position_at(&self, point: Point) -> Option<TextPosition> {
        let columns = self.columns?;
        if let Some((line, row)) = self.row_at(point) {
            let offset = if point.x < columns.text_x {
                0
            } else {
                self.offset_in(line, row, point.x)
            };
            return Some(TextPosition {
                id: line.id,
                offset,
            });
        }
        // Between rows (a rule) or beyond the lines: the nearest line below
        // the point, else the oldest line's start or the newest line's end.
        let newest = self.shown.first()?;
        let oldest = self.shown.last()?;
        if point.y >= newest.top {
            return Some(TextPosition {
                id: newest.id,
                offset: newest.plain.len(),
            });
        }
        let below = self
            .shown
            .iter()
            .rev()
            .find(|line| line.top >= point.y)
            .unwrap_or(oldest);
        Some(TextPosition {
            id: below.id,
            offset: 0,
        })
    }
}

/// A selection, from its earlier end to its later one.
pub fn ordered(a: TextPosition, b: TextPosition) -> (TextPosition, TextPosition) {
    if a <= b { (a, b) } else { (b, a) }
}

/// What to paint.
pub struct ChatView<'a> {
    pub lines: &'a VecDeque<Line>,
    pub context: ChatContext<'a>,
    pub anchor: Option<Anchor>,
    pub selection: Option<(TextPosition, TextPosition)>,
    /// The link under the pointer.
    pub hovered: Option<(u64, Range<usize>)>,
    pub focused: bool,
}

/// Paints the view in `rect` and reports what it showed.
///
/// `previous` is the frame last painted for the same buffer, or the
/// default: lines still in view keep their layouts. `scrolled` says that
/// since then only the anchor moved, so the canvas still shows `previous`:
/// its pixels shift, and only the band they expose is painted (DESIGN.md
/// §10).
pub fn paint_chat(
    painter: &mut Painter<'_>,
    rect: Rect,
    view: &ChatView<'_>,
    previous: ChatFrame,
    scrolled: bool,
) -> ChatFrame {
    let lines = view.lines;
    let columns = Columns::new(painter.text, rect, view.context.label_chars);
    let reusable = previous.columns == Some(columns)
        && previous.rect == rect
        && previous.marker == view.context.marker
        && previous.chantypes == view.context.chantypes
        && previous.casemap == view.context.casemap;
    let mut frame = ChatFrame {
        rect,
        anchor: view.anchor,
        columns: Some(columns),
        shown: Vec::new(),
        marker: view.context.marker,
        chantypes: view.context.chantypes.to_string(),
        casemap: view.context.casemap,
    };
    let old_tops: Vec<(u64, f32)> = previous
        .shown
        .iter()
        .map(|line| (line.id, line.top))
        .collect();
    let mut kept: std::collections::HashMap<u64, ShownLine> = if reusable {
        previous
            .shown
            .into_iter()
            .map(|line| (line.id, line))
            .collect()
    } else {
        Default::default()
    };
    // The oldest line's rule depends on lines gone from the buffer, so it is
    // always laid out anew.
    let mut block = |text: &mut TextSystem, index: usize| match kept.remove(&lines[index].id) {
        Some(shown) if index > 0 => shown.into_block(),
        _ => layout_block(text, &columns, &view.context, lines, index),
    };
    let Some(last) = lines.len().checked_sub(1) else {
        frame.anchor = None;
        painter.fill_rect(rect, CANVAS);
        return frame;
    };
    let (top_limit, bottom_limit) = (rect.y + PADDING_Y, rect.bottom() - PADDING_Y);
    let (anchor_index, below) = match view.anchor {
        Some(anchor) => (index_of(lines, anchor.id).unwrap_or(last), anchor.below),
        None => (last, 0.0),
    };
    // Up from the anchor until the view is full, to its very edge: lines
    // reach into the padding.
    let mut blocks: Vec<(usize, Block, f32)> = Vec::new();
    let mut bottom = bottom_limit + below;
    let mut index = anchor_index;
    loop {
        let block = block(painter.text, index);
        let top = bottom - block.height;
        blocks.push((index, block, top));
        bottom = top;
        if top <= rect.y || index == 0 {
            break;
        }
        index -= 1;
    }
    // Scrolled past the oldest line: it comes to rest at the top.
    let clamped = bottom > top_limit;
    if clamped {
        let shift = bottom - top_limit;
        for (_, _, top) in &mut blocks {
            *top -= shift;
        }
    }
    // Newer lines fill the view below the anchor, to its bottom edge.
    let mut end = blocks[0].2 + blocks[0].1.height;
    let mut index = anchor_index;
    while end < rect.bottom() && index < last {
        index += 1;
        let block = block(painter.text, index);
        let height = block.height;
        blocks.insert(0, (index, block, end));
        end += height;
    }
    if clamped {
        if end < bottom_limit {
            // Everything fits: it sits at the bottom, following the newest
            // line.
            let gap = bottom_limit - end;
            for (_, _, top) in &mut blocks {
                *top += gap;
            }
            frame.anchor = None;
        } else {
            // The line across the bottom edge anchors the view now; blocks
            // are newest first.
            let (index, block, top) = blocks
                .iter()
                .find(|(_, _, top)| *top < bottom_limit)
                .expect("a line reaches the bottom");
            let below = top + block.height - bottom_limit;
            frame.anchor = (*index < last || below > 0.0).then(|| Anchor {
                id: lines[*index].id,
                below,
            });
        }
    }
    // How far what's shown moved, in whole device pixels, if only that.
    let scale = painter.scale();
    let moved = (scrolled && reusable && view.hovered.is_none())
        .then(|| {
            blocks.iter().find_map(|(index, block, top)| {
                let id = lines[*index].id;
                let old = old_tops.iter().find(|(old, _)| *old == id)?.1;
                Some((top + block.rule_height() - old) * scale)
            })
        })
        .flatten()
        .filter(|device| {
            (device - device.round()).abs() < 0.01
                && device.round().abs() < (rect.height * scale).floor()
        })
        .map(|device| device.round() as i32);
    painter.save_clip(rect);
    let band = match moved {
        Some(0) => None,
        Some(device) => {
            painter.shift_rows(rect, device);
            let height = device.unsigned_abs() as f32 / scale;
            Some(if device > 0 {
                Rect::new(rect.x, rect.y, rect.width, height)
            } else {
                Rect::new(rect.x, rect.bottom() - height, rect.width, height)
            })
        }
        None => Some(rect),
    };
    if let Some(band) = band {
        painter.save_clip(band);
        painter.fill_rect(band, CANVAS);
    }
    let style = chat_style();
    for (index, block, top) in blocks {
        let line = &lines[index];
        let rows_top = top + block.rule_height();
        let rows_height = block.height - block.rule_height();
        let visible = top < rect.bottom() && top + block.height > rect.y;
        let exposed = band.is_some_and(|band| top < band.bottom() && top + block.height > band.y);
        if exposed {
            if let Some((label, new)) = &block.rule {
                let rule = Rect::new(
                    rect.x + PADDING_X,
                    top,
                    rect.width - 2.0 * PADDING_X,
                    RULE_HEIGHT,
                );
                paint_rule(painter, rule, label, *new, view.focused);
            }
            if line.highlight {
                painter.fill_rect(
                    Rect::new(rect.x, rows_top, rect.width, rows_height),
                    PINK.with_alpha(22),
                );
                painter.fill_rect(
                    Rect::new(rect.x, rows_top, 2.0, rows_height),
                    if view.focused { PINK } else { DIM },
                );
            }
            painter.fill_rect(Rect::new(columns.rule_x, rows_top, 1.0, rows_height), RULE);
            let decorated = decorated(&block, line.id, view);
            let paragraph = decorated.as_ref().unwrap_or(&block.paragraph);
            let baseline = |row: usize| {
                (rows_top
                    + row as f32 * columns.row_height
                    + (columns.row_height - paragraph.line_height) / 2.0
                    + paragraph.ascent)
                    .round()
            };
            let local = time::local(line.time);
            let stamp = painter
                .text
                .shape(&format!("{:02}:{:02}", local.hour, local.minute), &style);
            painter.draw_text(&stamp, columns.time_x, baseline(0), DISABLED);
            let label = line.label();
            if !label.is_empty() {
                let (label_color, _) = colors(line, view.context.casemap);
                let label_style = TextStyle {
                    weight: Weight::Bold,
                    ..style
                };
                let fitted = painter.text.fit(&label, &label_style, columns.label_width);
                let x = columns.label_x + columns.label_width - fitted.advance;
                painter.save_clip(Rect::new(
                    columns.label_x,
                    rows_top,
                    columns.label_width,
                    rows_height,
                ));
                painter.draw_text(&fitted, x, baseline(0), label_color);
                painter.restore();
            }
            for row in 0..paragraph.rows.len() {
                painter.draw_row(paragraph, row, columns.text_x, baseline(row));
            }
        }
        if visible {
            let named =
                matches!(line.kind, LineKind::Message | LineKind::Notice) && !line.nick.is_empty();
            frame.shown.push(ShownLine {
                id: line.id,
                top: rows_top,
                paragraph: block.paragraph,
                links: block.links,
                plain: block.plain,
                nick: named.then(|| line.nick.clone()),
                rule: block.rule,
                height: block.height,
            });
        }
    }
    if band.is_some() {
        painter.restore();
    }
    if let Some((id, range)) = &view.hovered
        && let Some(shown) = frame.shown.iter().find(|line| line.id == *id)
        && let Some((_, Target::Url(url))) = shown.links.iter().find(|(link, _)| link == range)
    {
        paint_url(painter, rect, url);
    }
    painter.restore();
    frame
}

/// A copy of `block`'s layout with the selection and the hovered link, if
/// either touches it.
fn decorated(block: &Block, id: u64, view: &ChatView<'_>) -> Option<Paragraph> {
    let selected = view.selection.and_then(|(start, end)| {
        if id < start.id || id > end.id {
            return None;
        }
        let from = if id == start.id { start.offset } else { 0 };
        let to = if id == end.id {
            end.offset
        } else {
            block.plain.len()
        };
        (from < to).then_some(from..to)
    });
    let hovered = view
        .hovered
        .as_ref()
        .filter(|(line, _)| *line == id)
        .map(|(_, range)| range.clone());
    if selected.is_none() && hovered.is_none() {
        return None;
    }
    let mut paragraph = block.paragraph.clone();
    for placed in &mut paragraph.glyphs {
        let cluster = placed.glyph.cluster as usize;
        if selected
            .as_ref()
            .is_some_and(|range| range.contains(&cluster))
        {
            placed.decoration.background = Some(SELECTION);
        }
        if hovered
            .as_ref()
            .is_some_and(|range| range.contains(&cluster))
        {
            placed.decoration.underline = true;
        }
    }
    Some(paragraph)
}

fn paint_rule(painter: &mut Painter<'_>, rect: Rect, label: &str, new: bool, focused: bool) {
    let accent = match (new, focused) {
        (true, true) => PINK,
        (true, false) => DIM,
        (false, _) => MUTED,
    };
    let middle = (rect.y + rect.height / 2.0).round();
    let line = if new { accent.with_alpha(150) } else { RULE };
    painter.fill_rect(Rect::new(rect.x, middle, rect.width, 1.0), line);
    let style = theme::chrome(9.0);
    let width = painter.text.width(label, &style) + 16.0;
    let pill = Rect::new(
        (rect.x + (rect.width - width) / 2.0).round(),
        rect.y + 4.0,
        width.round(),
        rect.height - 8.0,
    );
    painter.fill_rect(pill, CANVAS);
    painter.label(label, pill, &style, accent, Align::Center);
}

/// The hovered link's full address, before anything opens (DESIGN.md
/// §8.6): a strip along the view's bottom left.
fn paint_url(painter: &mut Painter<'_>, rect: Rect, url: &str) {
    let style = theme::chrome(10.0);
    let width = (painter.text.width(url, &style) + 20.0).min(rect.width - 8.0);
    let strip = Rect::new(rect.x + 4.0, rect.bottom() - 26.0, width.round(), 22.0);
    painter.fill_rect(strip, INSET);
    painter.stroke_rect(strip, 1.0, EDGE);
    painter.label(url, strip.inset(10.0, 0.0), &style, CYAN, Align::Start);
}

/// The selected text for the clipboard. Within one line, just the text;
/// across lines, each as `[HH:MM] <nick> text`.
pub fn selected_text(lines: &VecDeque<Line>, start: TextPosition, end: TextPosition) -> String {
    let plain = |line: &Line| -> String {
        let text = format::strip(&line.text).into_owned();
        if line.kind == LineKind::Action {
            format!("{} {text}", line.nick)
        } else {
            text
        }
    };
    let clamp = |text: &str, offset: usize| {
        let mut offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    if start.id == end.id {
        return lines
            .iter()
            .find(|line| line.id == start.id)
            .map(|line| {
                let text = plain(line);
                text[clamp(&text, start.offset)..clamp(&text, end.offset)].to_string()
            })
            .unwrap_or_default();
    }
    let mut out = String::new();
    for line in lines
        .iter()
        .filter(|line| (start.id..=end.id).contains(&line.id))
    {
        let text = plain(line);
        let from = if line.id == start.id {
            clamp(&text, start.offset)
        } else {
            0
        };
        let to = if line.id == end.id {
            clamp(&text, end.offset)
        } else {
            text.len()
        };
        let local = time::local(line.time);
        let who = match line.kind {
            LineKind::Message => format!("<{}> ", line.nick),
            LineKind::Notice if !line.nick.is_empty() => format!("-{}- ", line.nick),
            LineKind::Action => String::from("* "),
            _ if !line.prefix.is_empty() => format!("{} ", line.prefix),
            _ => String::new(),
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "[{:02}:{:02}] {who}{}",
            local.hour,
            local.minute,
            &text[from..to]
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Canvas;

    fn context() -> ChatContext<'static> {
        ChatContext {
            casemap: Casemap::Rfc1459,
            chantypes: "#",
            marker: None,
            label_chars: 6,
        }
    }

    fn lines(count: usize) -> VecDeque<Line> {
        (0..count)
            .map(|i| {
                let mut line =
                    Line::message(0, LineKind::Message, "alice", format!("line number {i}"));
                line.id = i as u64 + 1;
                line
            })
            .collect()
    }

    fn paint(lines: &VecDeque<Line>, anchor: Option<Anchor>) -> ChatFrame {
        let mut canvas = Canvas::new(500, 200);
        let mut text = TextSystem::new();
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        let view = ChatView {
            lines,
            context: context(),
            anchor,
            selection: None,
            hovered: None,
            focused: true,
        };
        paint_chat(
            &mut painter,
            Rect::new(0.0, 0.0, 500.0, 200.0),
            &view,
            ChatFrame::default(),
            false,
        )
    }

    #[test]
    fn the_view_rests_on_its_anchor() {
        let few = lines(3);
        let frame = paint(
            &few,
            Some(Anchor {
                id: 1,
                below: 500.0,
            }),
        );
        assert_eq!(frame.anchor, None, "a short buffer can't scroll");
        assert_eq!(frame.shown.len(), 3);
        let many = lines(100);
        let following = paint(&many, None);
        assert_eq!(following.shown.first().unwrap().id, 100);
        let anchored = paint(&many, Some(Anchor { id: 60, below: 0.0 }));
        assert_eq!(anchored.anchor, Some(Anchor { id: 60, below: 0.0 }));
        let row = anchored.columns.unwrap().row_height;
        let sixty = anchored.shown.iter().find(|line| line.id == 60).unwrap();
        assert_eq!(
            sixty.top + row,
            200.0 - PADDING_Y,
            "the anchor line ends at the padding"
        );
        assert_eq!(
            anchored.shown.first().unwrap().id,
            61,
            "the next line shows in the padding"
        );
        let mut more = many.clone();
        let mut newer = Line::message(0, LineKind::Message, "bob", "a new line");
        newer.id = 101;
        more.push_back(newer);
        let still = paint(&more, anchored.anchor);
        assert_eq!(
            still.shown.first().unwrap().top,
            anchored.shown.first().unwrap().top,
            "arrivals don't move it"
        );
        let top = paint(&many, Some(Anchor { id: 1, below: 1e6 }));
        let oldest = top.shown.last().unwrap();
        assert_eq!(
            (oldest.id, oldest.top),
            (1, PADDING_Y),
            "past the oldest line, it comes to rest at the top"
        );
        assert!(top.anchor.is_some());
    }

    #[test]
    fn scrolling_moves_the_anchor_line_by_line() {
        let many = lines(100);
        let mut text = TextSystem::new();
        let rect = Rect::new(0.0, 0.0, 500.0, 200.0);
        let row = Columns::new(&mut text, rect, 6).row_height;
        let up = scroll(&mut text, rect, &context(), &many, None, row * 2.5).unwrap();
        assert_eq!(up.id, 98);
        assert!((up.below - row * 0.5).abs() < 1e-3);
        assert_eq!(
            scroll(&mut text, rect, &context(), &many, Some(up), -row * 3.0),
            None,
            "back to the present"
        );
    }

    #[test]
    fn formatting_links_and_controls() {
        let mut text = TextSystem::new();
        let rect = Rect::new(0.0, 0.0, 600.0, 100.0);
        let columns = Columns::new(&mut text, rect, 6);
        let mut lines = VecDeque::new();
        lines.push_back(Line::message(
            0,
            LineKind::Message,
            "alice",
            "\u{2}bold\u{2} see https://example.org and #rust \u{7}",
        ));
        let block = layout_block(&mut text, &columns, &context(), &lines, 0);
        assert_eq!(
            block.plain,
            "bold see https://example.org and #rust \u{2407}"
        );
        assert_eq!(
            block.links[0],
            (9..28, Target::Url(String::from("https://example.org")))
        );
        assert_eq!(block.links[1].1, Target::Channel(String::from("#rust")));
        let link_glyph = block
            .paragraph
            .glyphs
            .iter()
            .find(|g| g.glyph.cluster == 9)
            .unwrap();
        assert_eq!(link_glyph.color, CYAN);
        assert_eq!(
            block.paragraph.glyphs[0].glyph.face,
            text.shape("b", &TextStyle::new(CHAT_SIZE, Weight::Bold))
                .glyphs[0]
                .face
        );
        lines.push_back(Line::message(0, LineKind::Action, "alice", "waves"));
        let action = layout_block(&mut text, &columns, &context(), &lines, 1);
        assert_eq!(action.plain, "alice waves");
        assert!(action.paragraph.glyphs.iter().all(|g| g.decoration.oblique));
    }

    #[test]
    fn rules_mark_new_messages_and_days() {
        let mut text = TextSystem::new();
        let rect = Rect::new(0.0, 0.0, 600.0, 100.0);
        let columns = Columns::new(&mut text, rect, 6);
        let day = 86_400_000;
        let mut lines: VecDeque<Line> = (0..3)
            .map(|i| {
                let mut line = Line::message(i * day + 12 * 3_600_000, LineKind::Message, "a", "x");
                line.id = i as u64 + 1;
                line
            })
            .collect();
        lines[2].time = lines[1].time + 60_000;
        let context = ChatContext {
            marker: Some(3),
            ..context()
        };
        let blocks: Vec<Block> = (0..3)
            .map(|i| layout_block(&mut text, &columns, &context, &lines, i))
            .collect();
        assert!(blocks[0].rule.is_none(), "no rule above the first line");
        assert!(
            blocks[1]
                .rule
                .as_ref()
                .is_some_and(|(label, new)| !new && label.ends_with("1970"))
        );
        assert_eq!(blocks[2].rule, Some((String::from("NEW MESSAGES"), true)));
        assert_eq!(blocks[2].height, columns.row_height + RULE_HEIGHT);
        assert_eq!(
            (
                weekday(1970, 1, 1),
                weekday(2026, 10, 30),
                weekday(2000, 2, 29)
            ),
            ("THURSDAY", "FRIDAY", "TUESDAY")
        );
    }

    #[test]
    fn hit_testing_finds_links_and_positions() {
        let mut lines = VecDeque::new();
        let mut line = Line::message(
            0,
            LineKind::Message,
            "alice",
            "go to https://example.org now",
        );
        line.id = 1;
        lines.push_back(line);
        let frame = paint(&lines, None);
        let columns = frame.columns.unwrap();
        let shown = &frame.shown[0];
        let digit = shown.paragraph.glyphs[1].glyph.x;
        let y = shown.top + 4.0;
        let (id, range, target) = frame
            .link_at(Point::new(columns.text_x + digit * 10.5, y))
            .unwrap();
        assert_eq!((id, range), (1, 6..25));
        assert_eq!(target, Target::Url(String::from("https://example.org")));
        assert!(
            frame
                .link_at(Point::new(columns.text_x + digit * 1.5, y))
                .is_none()
        );
        assert_eq!(
            frame.nick_at(Point::new(columns.label_x + columns.label_width - 2.0, y)),
            Some("alice")
        );
        assert_eq!(frame.nick_at(Point::new(columns.text_x + 2.0, y)), None);
        assert_eq!(
            frame.position_at(Point::new(columns.text_x + digit * 2.9, y)),
            Some(TextPosition { id: 1, offset: 3 })
        );
        assert_eq!(
            frame.position_at(Point::new(2.0, y)).unwrap().offset,
            0,
            "left of the text"
        );
        assert_eq!(
            frame
                .position_at(Point::new(columns.text_x, 199.0))
                .unwrap()
                .offset,
            shown.plain.len()
        );
    }

    #[test]
    fn selections_paint_behind_their_text() {
        let mut lines = VecDeque::new();
        let mut line = Line::message(0, LineKind::Message, "alice", "aaa   bbb");
        line.id = 1;
        lines.push_back(line);
        let mut canvas = Canvas::new(500, 200);
        let mut text = TextSystem::new();
        let rect = Rect::new(0.0, 0.0, 500.0, 200.0);
        let mut view = ChatView {
            lines: &lines,
            context: context(),
            anchor: None,
            selection: None,
            hovered: None,
            focused: true,
        };
        let frame = paint_chat(
            &mut Painter::new(&mut canvas, &mut text, 1.0),
            rect,
            &view,
            ChatFrame::default(),
            false,
        );
        let shown = &frame.shown[0];
        let columns = frame.columns.unwrap();
        // The middle of the blank between the words, halfway down the row.
        let space = &shown.paragraph.glyphs[4];
        let (x, y) = (
            (columns.text_x + space.glyph.x + space.glyph.advance / 2.0) as u32,
            (shown.top + columns.row_height / 2.0) as u32,
        );
        assert_eq!(canvas.pixel(x, y), CANVAS.pixel());
        let at = |offset| TextPosition { id: 1, offset };
        view.selection = Some((at(3), at(6)));
        paint_chat(
            &mut Painter::new(&mut canvas, &mut text, 1.0),
            rect,
            &view,
            frame,
            false,
        );
        let expected = crate::render::color::blend(CANVAS.pixel(), SELECTION, 255);
        assert_eq!(canvas.pixel(x, y), expected);
    }

    #[test]
    fn scrolling_moves_pixels_exactly_as_repainting_would() {
        let day = 86_400_000;
        let mut lines: VecDeque<Line> = (0..120)
            .map(|i| {
                let text = if i % 3 == 0 { "a longer line that wraps onto a second row in this narrow view, as long lines do" } else { "short" };
                let mut line = Line::message(i as i64 / 60 * day, LineKind::Message, "alice", format!("{i} \u{3}04,01{text}"));
                line.id = i + 1;
                line.highlight = i % 7 == 0;
                line
            })
            .collect();
        lines[40].kind = LineKind::Event;
        let rect = Rect::new(10.0, 20.0, 480.0, 300.0);
        let view = |anchor| ChatView {
            lines: &lines,
            context: ChatContext {
                marker: Some(50),
                ..context()
            },
            anchor,
            selection: Some((
                TextPosition { id: 55, offset: 2 },
                TextPosition { id: 58, offset: 4 },
            )),
            hovered: None,
            focused: true,
        };
        for scale in [1.0f32, 1.5, 2.0] {
            let size = ((520.0 * scale) as u32, (340.0 * scale) as u32);
            let (mut moved, mut repainted) =
                (Canvas::new(size.0, size.1), Canvas::new(size.0, size.1));
            let mut text = TextSystem::new();
            let mut anchor = Some(Anchor { id: 70, below: 0.0 });
            let mut frame = paint_chat(
                &mut Painter::new(&mut moved, &mut text, scale),
                rect,
                &view(anchor),
                ChatFrame::default(),
                false,
            );
            for step in [30.0, 30.0, -60.0, 46.0, -17.0, 120.0] {
                anchor = scroll(
                    &mut text,
                    rect,
                    &view(anchor).context,
                    &lines,
                    frame.anchor,
                    step,
                );
                frame = paint_chat(
                    &mut Painter::new(&mut moved, &mut text, scale),
                    rect,
                    &view(anchor),
                    frame,
                    true,
                );
                paint_chat(
                    &mut Painter::new(&mut repainted, &mut text, scale),
                    rect,
                    &view(anchor),
                    ChatFrame::default(),
                    false,
                );
                assert!(
                    moved.pixels() == repainted.pixels(),
                    "scale {scale}, step {step}"
                );
            }
        }
    }

    #[test]
    fn copying_reads_like_a_log() {
        let mut lines = VecDeque::new();
        for (id, (kind, nick, text)) in [
            (LineKind::Message, "alice", "hello \u{2}there\u{2}"),
            (LineKind::Action, "bob", "waves"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut line = Line::message(0, kind, nick, text);
            line.id = id as u64 + 1;
            lines.push_back(line);
        }
        let at = |id, offset| TextPosition { id, offset };
        assert_eq!(selected_text(&lines, at(1, 6), at(1, 11)), "there");
        let both = selected_text(&lines, at(1, 0), at(2, 9));
        let stamp = {
            let local = time::local(0);
            format!("[{:02}:{:02}]", local.hour, local.minute)
        };
        assert_eq!(
            both,
            format!("{stamp} <alice> hello there\n{stamp} * bob waves")
        );
    }

    #[test]
    fn controls_show_and_styles_follow() {
        let mut formatted = format::parse("a\u{7}\u{2}b\u{2}");
        show_controls(&mut formatted);
        assert_eq!(formatted.text, "a\u{2407}b");
        assert_eq!(formatted.spans[0].0, 4..5);
    }
}
