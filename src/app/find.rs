//! Search (DESIGN.md §8.10): Ctrl+F turns the input row into a find field
//! for the buffer shown. Every occurrence in view is highlighted in place,
//! and one is the current match: typing finds the nearest, Enter (or Up)
//! moves to the next earlier one and Shift+Enter (or Down) to the next
//! later one. Past the oldest line, Enter asks for earlier history, from
//! the server or the logs on disk, and searches on as it arrives.

use std::collections::VecDeque;
use std::ops::Range;

use crate::model::buffer::{Anchor, BufferKind};
use crate::model::search::Query;
use crate::model::{Line, LineKind};
use crate::net::NetworkId;
use crate::ui::chat_view::{self, ChatContext};
use crate::ui::input::LineEditor;

use super::App;

/// A buffer, as the find field remembers it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Place {
    network: NetworkId,
    kind: BufferKind,
    key: String,
}

/// An occurrence: its line and where it is in the line's text as shown.
#[derive(Clone, Debug)]
struct Match {
    place: Place,
    line: u64,
    range: Range<usize>,
}

/// The find field and where it has got to.
#[derive(Default)]
pub(super) struct Find {
    pub editor: LineEditor,
    /// The text the matches are for, the buffer last searched, and its
    /// oldest and newest lines then.
    searched: String,
    searched_in: Option<Place>,
    span: (Option<u64>, Option<u64>),
    query: Option<Query>,
    current: Option<Match>,
    /// Earlier history asked for to search on: lines older than `before`
    /// are new when it arrives.
    waiting: Option<(Place, u64)>,
    /// What the field says beside the query.
    pub note: Option<&'static str>,
}

/// The nearest occurrence of `query` from `from`, a line's index and an
/// offset in its text: toward earlier lines when `older`, else later ones,
/// never at `from` itself. Lines the smart filter hides (`filter`) are
/// passed over, as they don't show.
fn search(
    lines: &VecDeque<Line>,
    query: &Query,
    from: (usize, usize),
    older: bool,
    filter: bool,
) -> Option<(usize, Range<usize>)> {
    let (start, offset) = from;
    let in_line = |index: usize| {
        let line = &lines[index];
        if filter && line.quiet {
            return None;
        }
        // What shows is the text less its formatting codes, or an action
        // led by its nick.
        if line.kind != LineKind::Action && !query.may_be_in(&line.text) {
            return None;
        }
        let found = query.find(&chat_view::shown_text(line));
        let admitted = |range: &Range<usize>| {
            index != start
                || if older {
                    range.start < offset
                } else {
                    range.start > offset
                }
        };
        if older {
            found.into_iter().rev().find(admitted)
        } else {
            found.into_iter().find(admitted)
        }
        .map(|range| (index, range))
    };
    if older {
        (0..=start.min(lines.len().checked_sub(1)?))
            .rev()
            .find_map(in_line)
    } else {
        (start..lines.len()).find_map(in_line)
    }
}

impl Find {
    /// What the field looks for, when it has text.
    pub fn query(&self) -> Option<&Query> {
        self.query.as_ref()
    }
}

impl App {
    /// The buffer shown, to remember.
    fn shown_place(&self) -> Option<Place> {
        if self.welcome_visible() {
            return None;
        }
        let network = self.networks.get(self.active)?;
        let buffer = network.buffer();
        Some(Place {
            network: network.id,
            kind: buffer.kind,
            key: buffer.key.clone(),
        })
    }

    /// The current match, if it is in the buffer shown.
    pub(super) fn current_match(&self) -> Option<(u64, Range<usize>)> {
        let current = self.find.as_ref()?.current.as_ref()?;
        (Some(&current.place) == self.shown_place().as_ref())
            .then(|| (current.line, current.range.clone()))
    }

    /// Ctrl+F and `/search`: opens the find field, looking for `text`, or
    /// with what it last looked for selected, to type anew.
    pub(super) fn open_find(&mut self, text: Option<&str>) {
        if self.welcome_visible() {
            return;
        }
        self.close_channels();
        self.close_switcher();
        self.close_settings();
        let find = self.find.get_or_insert_with(Find::default);
        match text {
            Some(text) => find.editor.set(text),
            None => find.editor.select_all(),
        }
        self.mark_input();
        self.find_edited();
    }

    pub(super) fn close_find(&mut self) {
        if self.find.take().is_some() {
            self.mark_input();
            self.mark_chat();
        }
    }

    /// The oldest and newest lines of the buffer shown.
    fn shown_span(&self) -> (Option<u64>, Option<u64>) {
        let Some(network) = self.networks.get(self.active) else {
            return (None, None);
        };
        let lines = network.buffer().lines();
        (
            lines.front().map(|line| line.id),
            lines.back().map(|line| line.id),
        )
    }

    /// The query changed, or another buffer shows, or earlier lines came
    /// while nothing was found: the nearest match to what was current, or
    /// else to the view's bottom line, earlier first. Only what's loaded is
    /// searched; Enter goes further. Newer lines alone only make an old
    /// "no matches" doubtful.
    pub(super) fn find_edited(&mut self) {
        if self.find.is_none() {
            return;
        }
        let current = self.current_match();
        let shown = self.shown_place();
        let span = self.shown_span();
        let Some(find) = &mut self.find else {
            return;
        };
        let idle = find.current.is_none() && find.waiting.is_none();
        let edited = find.editor.text() != find.searched || find.searched_in != shown;
        let earlier = idle && span.0 != find.span.0;
        if !edited && !earlier {
            if idle && span.1 != find.span.1 {
                find.span = span;
                if find.note.take().is_some() {
                    self.mark_input();
                }
            }
            return;
        }
        find.searched = find.editor.text().to_string();
        find.searched_in = shown;
        find.span = span;
        find.query = Query::new(&find.searched);
        find.waiting = None;
        find.note = None;
        find.current = None;
        self.mark_input();
        self.mark_chat();
        let Some(query) = self.find.as_ref().and_then(|find| find.query.clone()) else {
            return;
        };
        let network = &self.networks[self.active];
        let buffer = network.buffer();
        let lines = buffer.lines();
        let bottom = buffer
            .anchor
            .and_then(|anchor| buffer.index_of(anchor.id))
            .unwrap_or(lines.len().saturating_sub(1));
        // Just after the old match, so it stays current while it matches.
        let from = current
            .and_then(|(line, range)| Some((buffer.index_of(line)?, range.start + 1)))
            .unwrap_or((bottom, usize::MAX));
        let found = search(lines, &query, from, true, self.smart_filter)
            .or_else(|| search(lines, &query, from, false, self.smart_filter));
        match found {
            Some((index, range)) => {
                let line = lines[index].id;
                self.show_match(line, range);
            }
            None => {
                let more = self.history_left(self.active, network.active);
                if let Some(find) = &mut self.find {
                    find.note = Some(if more {
                        "NO MATCHES · ENTER SEARCHES EARLIER"
                    } else {
                        "NO MATCHES"
                    });
                }
            }
        }
    }

    /// Enter and Up: the next earlier match; Shift+Enter and Down: the
    /// next later one.
    pub(super) fn find_step(&mut self, older: bool) {
        let current = self.current_match();
        let Some(query) = self.find.as_ref().and_then(|find| find.query.clone()) else {
            return;
        };
        let Some(place) = self.shown_place() else {
            return;
        };
        let network = &self.networks[self.active];
        let buffer = network.buffer();
        let lines = buffer.lines();
        let bottom = buffer
            .anchor
            .and_then(|anchor| buffer.index_of(anchor.id))
            .unwrap_or(lines.len().saturating_sub(1));
        let from = current
            .and_then(|(line, range)| Some((buffer.index_of(line)?, range.start)))
            .unwrap_or((bottom, usize::MAX));
        if let Some((index, range)) = search(lines, &query, from, older, self.smart_filter) {
            let line = lines[index].id;
            return self.show_match(line, range);
        }
        let oldest = lines.front().map(|line| line.id);
        let note = if !older {
            "NO LATER MATCHES"
        } else if let Some(oldest) = oldest
            && self.request_history(self.active, self.networks[self.active].active)
        {
            if let Some(find) = &mut self.find {
                find.waiting = Some((place, oldest));
            }
            "SEARCHING EARLIER MESSAGES"
        } else {
            "NO EARLIER MATCHES"
        };
        if let Some(find) = &mut self.find {
            find.note = Some(note);
        }
        self.mark_input();
    }

    /// Earlier history arrived, or won't: a search waiting for it carries
    /// on in the lines before those it had.
    pub(super) fn find_resume(&mut self) {
        let Some((place, before)) = self.find.as_ref().and_then(|find| find.waiting.clone()) else {
            return;
        };
        let shown = self.shown_place();
        let network = &self.networks[self.active];
        let buffer = network.buffer();
        if shown.as_ref() != Some(&place) {
            // Another buffer shows now: the search stops there.
            if let Some(find) = &mut self.find {
                find.waiting = None;
                find.note = None;
            }
            return self.mark_input();
        }
        if buffer.history.loading || buffer.history.deferred {
            return;
        }
        let Some(query) = self.find.as_ref().and_then(|find| find.query.clone()) else {
            return;
        };
        let lines = buffer.lines();
        let found = buffer
            .index_of(before)
            .filter(|&index| index > 0)
            .and_then(|index| search(lines, &query, (index, 0), true, self.smart_filter));
        if let Some((index, range)) = found {
            let line = lines[index].id;
            if let Some(find) = &mut self.find {
                find.waiting = None;
            }
            return self.show_match(line, range);
        }
        // Nothing yet: earlier still, while there is any.
        let oldest = lines.front().map(|line| line.id);
        let grew = oldest.is_some_and(|oldest| oldest < before);
        let more = grew && self.request_history(self.active, self.networks[self.active].active);
        if let Some(find) = &mut self.find {
            match (more, oldest) {
                (true, Some(oldest)) => find.waiting = Some((place, oldest)),
                _ => {
                    find.waiting = None;
                    find.note = Some(if find.current.is_some() {
                        "NO EARLIER MATCHES"
                    } else {
                        "NO MATCHES"
                    });
                }
            }
        }
        self.mark_input();
    }

    /// Makes an occurrence current and scrolls its line into view, a third
    /// of the way up, unless it shows already.
    fn show_match(&mut self, line: u64, range: Range<usize>) {
        let Some(place) = self.shown_place() else {
            return;
        };
        if let Some(find) = &mut self.find {
            find.current = Some(Match { place, line, range });
            find.note = None;
        }
        self.mark_input();
        self.mark_chat();
        if self.chat_frame.shows(line) {
            return;
        }
        let Some(regions) = self.regions() else {
            return;
        };
        let network = &mut self.networks[self.active];
        let state = &mut network.state;
        let buffer = &mut state.buffers[network.active];
        let context = ChatContext {
            casemap: state.isupport.casemap,
            chantypes: &state.isupport.chantypes,
            marker: buffer.marker,
            label_chars: buffer.label_chars,
            filter: self.smart_filter,
            loading: buffer.history.loading,
            seconds: self.settings.seconds,
        };
        buffer.anchor = chat_view::scroll(
            &mut self.text,
            regions.chat,
            &context,
            buffer.lines(),
            Some(Anchor {
                id: line,
                below: 0.0,
            }),
            -(regions.chat.height / 3.0).round(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(texts: &[&str]) -> VecDeque<Line> {
        texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let mut line = Line::message(0, LineKind::Message, "alice", *text);
                line.id = index as u64;
                line
            })
            .collect()
    }

    #[test]
    fn searches_step_through_lines_and_within_them() {
        let lines = lines(&["rust here", "nothing", "rust and rust", "end"]);
        let query = Query::new("RUST").unwrap();
        // From below everything, earlier: the later of two in a line first.
        let first = search(&lines, &query, (3, usize::MAX), true, false);
        assert_eq!(first, Some((2, 9..13)));
        let second = search(&lines, &query, (2, 9), true, false);
        assert_eq!(second, Some((2, 0..4)));
        assert_eq!(search(&lines, &query, (2, 0), true, false), Some((0, 0..4)));
        assert_eq!(search(&lines, &query, (0, 0), true, false), None);
        // And back.
        assert_eq!(
            search(&lines, &query, (0, 0), false, false),
            Some((2, 0..4))
        );
        assert_eq!(search(&lines, &query, (2, 9), false, false), None);
        // An action is searched as it reads, nick first.
        let mut action = Line::message(0, LineKind::Action, "alice", "waves");
        action.id = 9;
        let lines: VecDeque<Line> = [action].into();
        let found = search(
            &lines,
            &Query::new("alice w").unwrap(),
            (0, usize::MAX),
            true,
            false,
        );
        assert_eq!(found, Some((0, 0..7)));
    }

    #[test]
    fn hidden_lines_are_not_found() {
        let mut lines = lines(&["rust", "rust"]);
        lines[1].quiet = true;
        let query = Query::new("rust").unwrap();
        assert_eq!(
            search(&lines, &query, (1, usize::MAX), true, true),
            Some((0, 0..4))
        );
        assert_eq!(
            search(&lines, &query, (1, usize::MAX), true, false),
            Some((1, 0..4))
        );
    }
}
