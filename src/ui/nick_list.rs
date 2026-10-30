//! The nick list (DESIGN.md §8.7): a channel's members grouped by rank
//! under labels with counts, away members dimmed, and only the rows in view
//! drawn, so large channels cost what small ones do.

use crate::model::members::Member;
use crate::render::{Align, Painter, Point, Rect};

use super::header::thousands;
use super::theme::{self, AMBER, CYAN, DISABLED, EDGE, HOVER, MUTED, PINK, SURFACE, TEXT};

pub const NICK_LIST_WIDTH: f32 = 176.0;
const ROW_HEIGHT: f32 = 22.0;
const GROUP_HEIGHT: f32 = 28.0;
const PADDING_TOP: f32 = 4.0;

/// A rank's label: what its highest prefix makes members.
fn group_name(prefix: Option<char>) -> &'static str {
    match prefix {
        Some('~') => "OWNERS",
        Some('&') => "ADMINS",
        Some('@') => "OPERATORS",
        Some('%') => "HALF-OPS",
        Some('+') => "VOICED",
        Some(_) => "RANKED",
        None => "USERS",
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Entry {
    Group(Option<char>, usize),
    /// A member, by its place in the sorted list.
    Member(usize),
}

/// Group labels and member rows, in order. `members` is in display order:
/// by rank, then nick.
fn entries(members: &[&Member]) -> Vec<Entry> {
    let mut entries = Vec::with_capacity(members.len() + 6);
    let mut index = 0;
    while index < members.len() {
        let prefix = members[index].prefix();
        let end = members[index..]
            .iter()
            .position(|m| m.prefix() != prefix)
            .map_or(members.len(), |n| index + n);
        entries.push(Entry::Group(prefix, end - index));
        entries.extend((index..end).map(Entry::Member));
        index = end;
    }
    entries
}

fn height(entry: &Entry) -> f32 {
    match entry {
        Entry::Group(..) => GROUP_HEIGHT,
        Entry::Member(_) => ROW_HEIGHT,
    }
}

/// The largest scroll for `members` in a list `rect` tall.
pub fn max_scroll(rect: Rect, members: &[&Member]) -> f32 {
    let content: f32 = entries(members).iter().map(height).sum::<f32>() + PADDING_TOP * 2.0;
    (content - rect.height).max(0.0)
}

/// The member under `point`, by its place in `members`.
pub fn member_at(rect: Rect, members: &[&Member], scroll: f32, point: Point) -> Option<usize> {
    if !rect.contains(point) {
        return None;
    }
    let mut y = rect.y + PADDING_TOP - scroll.clamp(0.0, max_scroll(rect, members));
    for entry in entries(members) {
        let h = height(&entry);
        if point.y >= y && point.y < y + h {
            return match entry {
                Entry::Member(index) => Some(index),
                Entry::Group(..) => None,
            };
        }
        y += h;
    }
    None
}

/// Paints the list and returns the scroll it was painted at.
pub fn paint_nick_list(
    painter: &mut Painter<'_>,
    rect: Rect,
    members: &[&Member],
    scroll: f32,
    hovered: Option<usize>,
) -> f32 {
    painter.fill_rect(rect, SURFACE);
    painter.fill_rect(Rect::new(rect.x, rect.y, 1.0, rect.height), EDGE);
    let scroll = scroll.clamp(0.0, max_scroll(rect, members));
    painter.save_clip(rect);
    let mut y = rect.y + PADDING_TOP - scroll;
    let label_style = theme::chrome(9.0);
    let nick_style = theme::body(12.0);
    for entry in entries(members) {
        let h = height(&entry);
        if y + h > rect.y && y < rect.bottom() {
            match entry {
                Entry::Group(prefix, count) => {
                    let label = format!("{} · {}", group_name(prefix), thousands(count));
                    painter.label(
                        &label,
                        Rect::new(rect.x + 14.0, y + 10.0, rect.width - 24.0, 14.0),
                        &label_style,
                        MUTED,
                        Align::Start,
                    );
                }
                Entry::Member(index) => {
                    let member = members[index];
                    let row = Rect::new(rect.x + 1.0, y, rect.width - 1.0, h);
                    if hovered == Some(index) {
                        painter.fill_rect(row, HOVER);
                        painter.fill_rect(Rect::new(row.x, row.y, 1.0, row.height), CYAN);
                    }
                    if let Some(prefix) = member.prefix() {
                        let color = match prefix {
                            '~' | '&' => PINK,
                            '@' => CYAN,
                            _ => AMBER,
                        };
                        painter.label(
                            &prefix.to_string(),
                            Rect::new(row.x + 12.0, y, 10.0, h),
                            &theme::chrome(11.0),
                            color,
                            Align::Center,
                        );
                    }
                    let color = if member.away { DISABLED } else { TEXT };
                    painter.label(
                        &member.nick,
                        Rect::new(row.x + 26.0, y, row.width - 36.0, h),
                        &nick_style,
                        color,
                        Align::Start,
                    );
                }
            }
        }
        y += h;
        if y >= rect.bottom() {
            break;
        }
    }
    painter.restore();
    scroll
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> Vec<Member> {
        let mut list = vec![Member::new("op", "@"), Member::new("voice", "+")];
        list.extend((0..50).map(|n| Member::new(format!("user{n:02}"), "")));
        list
    }

    #[test]
    fn groups_rows_and_hit_tests() {
        let owned = members();
        let members: Vec<&Member> = owned.iter().collect();
        let groups: Vec<Entry> = entries(&members)
            .into_iter()
            .filter(|e| matches!(e, Entry::Group(..)))
            .collect();
        assert_eq!(
            groups,
            [
                Entry::Group(Some('@'), 1),
                Entry::Group(Some('+'), 1),
                Entry::Group(None, 50)
            ]
        );
        let rect = Rect::new(0.0, 100.0, NICK_LIST_WIDTH, 300.0);
        let first = rect.y + PADDING_TOP + GROUP_HEIGHT + ROW_HEIGHT / 2.0;
        assert_eq!(
            member_at(rect, &members, 0.0, Point::new(50.0, first)),
            Some(0)
        );
        assert_eq!(
            member_at(
                rect,
                &members,
                0.0,
                Point::new(50.0, rect.y + PADDING_TOP + 2.0)
            ),
            None,
            "a group label"
        );
        let bottom = max_scroll(rect, &members);
        assert!(bottom > 0.0);
        assert_eq!(
            member_at(
                rect,
                &members,
                1e6,
                Point::new(50.0, rect.bottom() - PADDING_TOP - 1.0)
            ),
            Some(51)
        );
        assert_eq!(group_name(Some('@')), "OPERATORS");
    }
}
