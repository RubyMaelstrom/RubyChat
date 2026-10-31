//! The quick switcher (DESIGN.md §8.10): Ctrl+K turns the input row into a
//! field for finding any buffer of any network, with the matches in a panel
//! above it. Up and Down choose, Enter or a click shows the buffer, and Esc
//! closes it.

use crate::model::buffer::{Activity, BufferKind};
use crate::model::search::fuzzy;
use crate::net::NetworkId;
use crate::render::Point;
use crate::ui::input::LineEditor;
use crate::ui::switcher::{MAX_ROWS, SwitchRow, SwitcherGeometry};

use super::App;

/// The switcher's field and what it found.
#[derive(Default)]
pub(super) struct Switcher {
    pub editor: LineEditor,
    /// The text the matches are for, once worked out.
    searched: Option<String>,
    /// The matches, best first: a network, and a buffer by kind and key.
    matches: Vec<(NetworkId, BufferKind, String)>,
    pub selected: usize,
}

/// How much is waiting in a buffer, to rank by: highlights, then messages,
/// then other activity.
type Liveliness = (u8, u32, u32);

/// A candidate: its score, liveliness, network and buffer.
type Candidate = (i32, Liveliness, usize, usize);

fn liveliness(highlights: u32, unread: u32, activity: Activity) -> Liveliness {
    let rank = match (highlights, activity) {
        (1.., _) => 3,
        (0, Activity::Highlight | Activity::Messages) => 2,
        (0, Activity::Events) => 1,
        (0, Activity::None) => 0,
    };
    (rank, highlights, unread)
}

impl App {
    /// Ctrl+K: opens the switcher, or closes it.
    pub(super) fn open_switcher(&mut self) {
        if self.welcome_visible() {
            return;
        }
        if self.switcher.is_some() {
            return self.close_switcher();
        }
        self.close_menu();
        self.close_find();
        self.close_channels();
        self.close_settings();
        self.switcher = Some(Switcher::default());
        self.switcher_edited();
        self.mark_input();
    }

    pub(super) fn close_switcher(&mut self) {
        if self.switcher.is_some() {
            self.mark_under_switcher();
            self.switcher = None;
            self.mark_input();
        }
    }

    /// After input: a changed query matches anew, the best first.
    pub(super) fn switcher_edited(&mut self) {
        let Some(switcher) = &self.switcher else {
            return;
        };
        let text = switcher.editor.text().to_lowercase();
        if switcher.searched.as_deref() == Some(text.as_str()) {
            return;
        }
        let matches = self.switcher_matches(&text);
        self.mark_under_switcher();
        if let Some(switcher) = &mut self.switcher {
            switcher.searched = Some(text);
            switcher.matches = matches;
            switcher.selected = 0;
        }
        self.mark_under_switcher();
    }

    /// Every buffer that matches `query`, best first; with no query, the
    /// most active first, and the one shown last.
    fn switcher_matches(&self, query: &str) -> Vec<(NetworkId, BufferKind, String)> {
        let mut found: Vec<Candidate> = Vec::new();
        for (n, network) in self.networks.iter().enumerate() {
            for (b, buffer) in network.state.buffers.iter().enumerate() {
                let name = match buffer.kind {
                    BufferKind::Server => network.title.as_str(),
                    _ => buffer.name.as_str(),
                };
                let score = if query.trim().is_empty() {
                    let shown = n == self.active && b == network.active;
                    i32::from(!shown)
                } else {
                    // The network's name counts for less than the buffer's.
                    let whole = format!("{name} {}", network.title);
                    match (fuzzy(query, name), fuzzy(query, &whole)) {
                        (Some(score), _) => score,
                        (None, Some(score)) => score - 64,
                        (None, None) => continue,
                    }
                };
                let lively = liveliness(buffer.highlights, buffer.unread, buffer.activity);
                found.push((score, lively, n, b));
            }
        }
        found.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.cmp(&a.1))
                .then(a.2.cmp(&b.2))
                .then(a.3.cmp(&b.3))
        });
        found
            .into_iter()
            .map(|(_, _, n, b)| {
                let network = &self.networks[n];
                let buffer = &network.state.buffers[b];
                (network.id, buffer.kind, buffer.key.clone())
            })
            .collect()
    }

    /// The rows the panel shows, and how many buffers there are.
    pub(super) fn switcher_rows(&self) -> (Vec<SwitchRow>, usize) {
        let total = self
            .networks
            .iter()
            .map(|network| network.state.buffers.len())
            .sum();
        let Some(switcher) = &self.switcher else {
            return (Vec::new(), total);
        };
        let rows = switcher
            .matches
            .iter()
            .take(MAX_ROWS)
            .filter_map(|(id, kind, key)| {
                let network = self.networks.iter().find(|network| network.id == *id)?;
                let buffer = network
                    .state
                    .buffers
                    .iter()
                    .find(|buffer| buffer.kind == *kind && buffer.key == *key)?;
                Some(SwitchRow {
                    name: match buffer.kind {
                        BufferKind::Server => network.title.clone(),
                        _ => buffer.name.clone(),
                    },
                    network: network.title.clone(),
                    kind: buffer.kind,
                    highlights: buffer.highlights,
                    unread: buffer.unread,
                    activity: buffer.activity,
                })
            })
            .collect();
        (rows, total)
    }

    pub(super) fn switcher_geometry(&self) -> Option<SwitcherGeometry> {
        let switcher = self.switcher.as_ref()?;
        let regions = self.regions()?;
        let field = regions.input.inset(12.0, 8.0);
        let count = switcher.matches.len().min(MAX_ROWS);
        Some(SwitcherGeometry::new(field, regions.body, count))
    }

    /// The switcher changed: what lies under it repaints, then it.
    fn mark_under_switcher(&mut self) {
        if let Some(geometry) = self.switcher_geometry() {
            self.mark_under(geometry.damage());
        }
    }

    /// Up and Down choose among the rows shown.
    pub(super) fn switcher_move(&mut self, step: isize) {
        let Some(switcher) = &mut self.switcher else {
            return;
        };
        let count = switcher.matches.len().min(MAX_ROWS);
        if count == 0 {
            return;
        }
        switcher.selected = (switcher.selected as isize + step).rem_euclid(count as isize) as usize;
        self.mark_under_switcher();
    }

    /// Enter or a click: shows the chosen buffer.
    pub(super) fn switcher_choose(&mut self, row: Option<usize>) {
        let Some(switcher) = &self.switcher else {
            return;
        };
        let Some((id, kind, key)) = switcher
            .matches
            .get(row.unwrap_or(switcher.selected))
            .cloned()
        else {
            return;
        };
        self.close_switcher();
        let Some(network) = self.index_of(id) else {
            return;
        };
        let Some(buffer) = self.networks[network]
            .state
            .buffers
            .iter()
            .position(|buffer| buffer.kind == kind && buffer.key == key)
        else {
            return;
        };
        self.activate(network);
        self.activate_buffer(buffer);
    }

    /// The pointer moved over the switcher: the row under it is chosen.
    pub(super) fn switcher_pointer_moved(&mut self, point: Point) -> bool {
        let Some(geometry) = self.switcher_geometry() else {
            return false;
        };
        if !geometry.panel.contains(point) {
            return false;
        }
        if let Some(row) = geometry.row_at(point)
            && let Some(switcher) = &mut self.switcher
            && switcher.selected != row
        {
            switcher.selected = row;
            self.mark_under_switcher();
        }
        true
    }

    /// A press while the switcher is open: a row shows its buffer; a press
    /// elsewhere closes it. Returns whether the press was used.
    pub(super) fn switcher_pointer_pressed(&mut self, point: Point) -> bool {
        let Some(geometry) = self.switcher_geometry() else {
            return false;
        };
        if geometry.panel.contains(point) {
            if let Some(row) = geometry.row_at(point) {
                self.switcher_choose(Some(row));
            }
            return true;
        }
        self.close_switcher();
        false
    }
}
