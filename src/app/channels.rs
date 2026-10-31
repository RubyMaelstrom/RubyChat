//! The channel list (DESIGN.md §8.10): `/list` asks the server for its
//! channels and shows them in the chat view's place as they stream in. The
//! input row filters them, the column titles (or Tab) sort them, and Enter
//! or a double click joins the selected one. Filtering and sorting run a
//! slice per wake (DESIGN.md §10), so typing never waits for them.

use std::time::{Duration, Instant};

use crate::model::LineKind;
use crate::model::channels::{ChannelList, Order, SLICE};
use crate::net::{NetCommand, NetworkId};
use crate::ui::channel_list::{TableLayout, TablePart};
use crate::ui::input::LineEditor;

use super::App;

/// Less time than this left in a wake, and the slice waits for the next.
const MIN_SLICE: Duration = Duration::from_micros(500);

/// The channel list shown over a network, and what's done to it.
pub(super) struct Browser {
    pub network: NetworkId,
    pub filter: LineEditor,
    /// The filter the list has.
    filtered: String,
    pub scroll: f32,
    /// The selected channel, by its index in the list: it stays selected
    /// as the rows move.
    pub selected: Option<u32>,
    pub hovered: Option<TablePart>,
}

impl App {
    /// `/list [arguments]`: asks for the channel list and shows it.
    /// Arguments pass on as LIST's: channels, or the server's ELIST
    /// conditions.
    pub(super) fn open_channels(&mut self, arguments: &str) {
        let network = &mut self.networks[self.active];
        if network.demo || !network.registered() {
            return self.say(
                LineKind::Error,
                "!",
                "Not connected; /list needs the server.",
            );
        }
        network.state.channels = Some(ChannelList::loading());
        let id = network.id;
        // Unlabeled, so no batch holds the replies back.
        let line = match arguments.trim() {
            "" => String::from("LIST"),
            arguments => format!("LIST {arguments}"),
        };
        self.send(NetCommand::Send(id, line));
        self.close_find();
        self.close_switcher();
        self.close_settings();
        self.browser = Some(Browser {
            network: id,
            filter: LineEditor::default(),
            filtered: String::new(),
            scroll: 0.0,
            selected: None,
            hovered: None,
        });
        self.mark_body();
    }

    pub(super) fn close_channels(&mut self) {
        if self.browser.take().is_some() {
            self.mark_body();
        }
    }

    /// Whether the channel list shows, over the network shown.
    pub(super) fn browsing(&self) -> bool {
        !self.welcome_visible()
            && self.browser.as_ref().is_some_and(|browser| {
                self.networks
                    .get(self.active)
                    .is_some_and(|network| network.id == browser.network)
            })
    }

    fn channel_list(&self) -> Option<&ChannelList> {
        self.networks.get(self.active)?.state.channels.as_ref()
    }

    /// How many rows the channel list shows.
    pub(super) fn channel_rows(&self) -> usize {
        self.channel_list().map_or(0, |list| list.rows().len())
    }

    /// After input: a changed filter filters anew.
    pub(super) fn channels_edited(&mut self) {
        if !self.browsing() {
            return;
        }
        let Some(browser) = &mut self.browser else {
            return;
        };
        if browser.filter.text() == browser.filtered {
            return;
        }
        browser.filtered = browser.filter.text().to_string();
        browser.scroll = 0.0;
        let filter = browser.filtered.clone();
        if let Some(list) = &mut self.networks[self.active].state.channels {
            list.set_filter(&filter);
        }
        self.mark_header();
        self.mark_chat();
    }

    /// Filtering and sorting go on, a slice per wake, while the list shows,
    /// within what's left of the wake's budget (`deadline`); arrivals join
    /// it.
    pub(super) fn step_channels(&mut self, deadline: Instant) {
        if !self.browsing() {
            return;
        }
        let typed = self.view.as_ref().is_some_and(|view| view.dirty.input);
        let Some(list) = &mut self.networks[self.active].state.channels else {
            return;
        };
        if !list.busy() {
            return;
        }
        // What was typed shows first, and a wake spent draining leaves the
        // slice for the next: it waits.
        let until = deadline.min(Instant::now() + SLICE);
        if typed || until < Instant::now() + MIN_SLICE {
            return (self.wake)();
        }
        let settling = list.settling();
        let done = list.step(until);
        let note = settling != list.settling();
        self.mark_header();
        self.mark_chat();
        if note {
            self.mark_input();
        }
        if !done {
            // The rest after the next round of input.
            (self.wake)();
        }
    }

    /// The channel list grew or ended: its counts and rows show it.
    pub(super) fn channels_arrived(&mut self, network: usize) {
        if network == self.active && self.browsing() {
            self.mark_header();
            self.mark_chat();
        }
    }

    /// Sorts by `order`, from a column title or Tab.
    pub(super) fn sort_channels(&mut self, order: Order) {
        if let Some(list) = &mut self.networks[self.active].state.channels {
            list.set_order(order);
            if let Some(browser) = &mut self.browser {
                browser.scroll = 0.0;
            }
            self.mark_header();
            self.mark_chat();
        }
    }

    /// The selected row's place among the rows.
    pub(super) fn selected_row(&self) -> Option<usize> {
        let selected = self.browser.as_ref()?.selected?;
        self.channel_list()?
            .rows()
            .iter()
            .position(|&index| index == selected)
    }

    fn table_layout(&self) -> Option<TableLayout> {
        Some(TableLayout::new(self.regions()?.chat))
    }

    /// Up and Down move the selection by a row, Page Up and Page Down by a
    /// page; it stays in view.
    pub(super) fn move_selection(&mut self, rows: isize) {
        let page = self.table_layout().map_or(10, |layout| layout.page()) as isize;
        let step = match rows {
            isize::MIN..=-2 => -page,
            2.. => page,
            step => step,
        };
        let current = self.selected_row();
        let Some(list) = self.channel_list() else {
            return;
        };
        let count = list.rows().len();
        if count == 0 {
            return;
        }
        let row = match current {
            Some(row) => (row as isize + step).clamp(0, count as isize - 1) as usize,
            None if step < 0 => count - 1,
            None => 0,
        };
        let index = list.rows()[row];
        let layout = self.table_layout();
        if let Some(browser) = &mut self.browser {
            browser.selected = Some(index);
            if let Some(layout) = layout {
                browser.scroll = layout.reveal(row, browser.scroll);
            }
        }
        self.mark_chat();
    }

    /// Enter and double clicks: joins the selected channel, or the first,
    /// and shows it.
    pub(super) fn join_selected(&mut self) {
        let row = self.selected_row().unwrap_or(0);
        let Some(name) = self.channel_list().and_then(|list| {
            let index = *list.rows().get(row)?;
            Some(list.channel(index)?.name().to_string())
        }) else {
            return;
        };
        self.close_channels();
        self.join(&name);
    }

    /// The pointer moved over the table.
    pub(super) fn table_hover(&mut self, part: Option<TablePart>) {
        if let Some(browser) = &mut self.browser
            && browser.hovered != part
        {
            browser.hovered = part;
            self.mark_chat();
        }
    }

    /// A press in the table: a title sorts, a row selects, and a second
    /// press joins.
    pub(super) fn table_press(&mut self, part: Option<TablePart>, double: bool) {
        match part {
            Some(TablePart::Heading(order)) => self.sort_channels(order),
            Some(TablePart::Row(row)) => {
                let index = self
                    .channel_list()
                    .and_then(|list| list.rows().get(row).copied());
                if let Some(browser) = &mut self.browser {
                    browser.selected = index;
                }
                self.mark_chat();
                if double {
                    self.join_selected();
                }
            }
            None => {}
        }
    }

    pub(super) fn scroll_table(&mut self, pixels: f32) {
        let rows = self.channel_rows();
        let layout = self.table_layout();
        if let Some(browser) = &mut self.browser {
            let max = layout.map_or(0.0, |layout| layout.max_scroll(rows));
            browser.scroll = (browser.scroll - pixels).clamp(0.0, max);
            self.mark_chat();
        }
    }

    /// The header's line about the list: how many channels, and what's
    /// happening to them.
    pub(super) fn channels_detail(&self) -> String {
        let Some(list) = self.channel_list() else {
            return String::new();
        };
        let mut detail = if self
            .browser
            .as_ref()
            .is_some_and(|browser| !browser.filtered.is_empty())
        {
            format!(
                "{} of {} channels",
                crate::ui::header::thousands(list.rows().len()),
                crate::ui::header::thousands(list.len())
            )
        } else {
            let count = list.len();
            format!(
                "{} channel{}",
                crate::ui::header::thousands(count),
                if count == 1 { "" } else { "s" }
            )
        };
        if list.dropped > 0 {
            detail.push_str(&format!(
                " · {} more not kept",
                crate::ui::header::thousands(list.dropped)
            ));
        }
        if list.loading {
            detail.push_str(" · loading…");
        } else if list.settling() {
            detail.push_str(" · sorting…");
        }
        detail
    }
}
