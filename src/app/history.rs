//! History (DESIGN.md §7, §11): lines are logged as they settle; a buffer's
//! earlier lines come from the server's history (IRCv3 chathistory) where
//! it keeps one, else from the logs, when the buffer first shows and as it
//! is scrolled back; and read markers stay in step with your other clients
//! (IRCv3 read-marker).
//!
//! A buffer that first shows while its network connects waits to learn
//! whether the server keeps history, so the two never interleave: the
//! registration (or a failure to connect) decides.

use std::time::{Duration, Instant};

use crate::irc::time::format_server_time;
use crate::model::LineKind;
use crate::model::buffer::BufferKind;
use crate::net::NetCommand;
use crate::store::BacklogRequest;
use crate::store::log::LogLine;

use super::App;

/// Lines asked for at once from the logs.
const LOG_PAGE: usize = 200;
/// Messages asked for at once from the server, within its limit.
const SERVER_PAGE: usize = 100;
/// Scrolling this close to the oldest line asks for earlier ones.
const NEAR_TOP: usize = 20;
/// Backlog requests remembered while their answers come, at most.
const MAX_REQUESTS: usize = 64;
/// While lines arrive in the buffer you're looking at, the server hears
/// they were read at most this often; looking away tells it at once.
const READ_INTERVAL: Duration = Duration::from_secs(30);

impl App {
    /// Logs the lines buffer `buffer` of network `network` gained, in
    /// order: one waiting for its echo holds back those after it, and
    /// history, failed sends and RubyChat's own news aren't logged.
    pub(super) fn log_lines(&mut self, network: usize, buffer: usize) {
        let Some(store) = &self.store else {
            return;
        };
        let Some(net) = self.networks.get_mut(network) else {
            return;
        };
        if net.demo {
            return;
        }
        let Some(buf) = net.state.buffers.get_mut(buffer) else {
            return;
        };
        if buf.kind == BufferKind::Server {
            return;
        }
        let lines = buf.lines();
        let start = lines.partition_point(|line| line.id <= buf.logged);
        let mut logged = buf.logged;
        for line in lines.range(start..) {
            if line.pending {
                break;
            }
            logged = line.id;
            let kept = matches!(
                line.kind,
                LineKind::Message | LineKind::Action | LineKind::Notice | LineKind::Event
            );
            if kept && !line.history && !line.failed && self.settings.logging {
                store.log(LogLine {
                    network: net.title.clone(),
                    buffer: buf.key.clone(),
                    line: line.clone(),
                });
            }
        }
        buf.logged = logged;
    }

    /// Asks for buffer `buffer`'s earlier lines, unless they're on their
    /// way or there are none: the server's history while it has more, then
    /// the logs. True while some are on their way.
    pub(super) fn request_history(&mut self, network: usize, buffer: usize) -> bool {
        self.ask_history(network, buffer);
        self.networks
            .get(network)
            .and_then(|net| net.state.buffers.get(buffer))
            .is_some_and(|buf| buf.history.loading || buf.history.deferred)
    }

    /// Whether buffer `buffer` may have earlier lines than it holds.
    pub(super) fn history_left(&self, network: usize, buffer: usize) -> bool {
        let Some(net) = self.networks.get(network) else {
            return false;
        };
        let Some(buf) = net.state.buffers.get(buffer) else {
            return false;
        };
        if net.demo || buf.kind == BufferKind::Server {
            return false;
        }
        let server = net.state.has("draft/chathistory") && net.state.has("batch");
        (server && !buf.history.server_done) || (self.store.is_some() && !buf.history.logs_done)
    }

    fn ask_history(&mut self, network: usize, buffer: usize) {
        let Some(net) = self.networks.get_mut(network) else {
            return;
        };
        let Some(buf) = net.state.buffers.get(buffer) else {
            return;
        };
        let history = buf.history;
        if net.demo || buf.kind == BufferKind::Server || history.loading {
            return;
        }
        let server = net.state.has("draft/chathistory") && net.state.has("batch");
        // Until the network registers, it isn't known whether the server
        // keeps history.
        if !history.requested && net.registering() {
            net.state.buffers[buffer].history.requested = true;
            net.state.buffers[buffer].history.deferred = true;
            return;
        }
        if server && !history.server_done && net.registered() {
            let limit = match net.state.isupport.chathistory {
                Some(0) | None => SERVER_PAGE,
                Some(limit) => limit.min(SERVER_PAGE),
            };
            let reference = match (history.requested, buf.oldest_time()) {
                (true, Some(oldest)) => format!("timestamp={}", format_server_time(oldest)),
                _ => String::from("*"),
            };
            let subcommand = if reference == "*" { "LATEST" } else { "BEFORE" };
            let line = format!("CHATHISTORY {subcommand} {} {reference} {limit}", buf.name);
            let line = match net.state.label(buffer) {
                Some(label) => format!("@label={label} {line}"),
                None => line,
            };
            let id = net.id;
            let history = &mut net.state.buffers[buffer].history;
            history.requested = true;
            history.deferred = false;
            history.loading = true;
            self.send(NetCommand::Send(id, line));
            return;
        }
        if history.logs_done && (history.server_done || !server) {
            return;
        }
        let Some(store) = &self.store else {
            return;
        };
        self.next_backlog += 1;
        let request = BacklogRequest {
            id: self.next_backlog,
            network: net.title.clone(),
            buffer: buf.key.clone(),
            before: buf.oldest_time(),
            limit: LOG_PAGE,
        };
        if self.backlogs.len() == MAX_REQUESTS {
            self.backlogs.pop_front();
        }
        self.backlogs
            .push_back((request.id, net.id, request.buffer.clone()));
        store.backlog(request);
        let history = &mut net.state.buffers[buffer].history;
        history.requested = true;
        history.deferred = false;
        history.loading = true;
    }

    /// Lines from the logs arrived: they go in front of their buffer,
    /// without any it already shows.
    pub(super) fn backlog_arrived(
        &mut self,
        id: u64,
        result: Result<Vec<crate::model::Line>, String>,
    ) {
        let Some(position) = self.backlogs.iter().position(|(sent, _, _)| *sent == id) else {
            return;
        };
        let Some((_, network_id, key)) = self.backlogs.remove(position) else {
            return;
        };
        let Some(network) = self.index_of(network_id) else {
            return;
        };
        let state = &mut self.networks[network].state;
        let Some(buffer) = state
            .buffers
            .iter()
            .position(|buffer| buffer.kind != BufferKind::Server && buffer.key == key)
        else {
            return;
        };
        let buf = &mut state.buffers[buffer];
        buf.history.loading = false;
        let mut lines = match result {
            Ok(lines) => lines,
            Err(error) => {
                eprintln!("rubychat: {error}");
                buf.history.logs_done = true;
                return;
            }
        };
        if lines.len() < LOG_PAGE {
            buf.history.logs_done = true;
        }
        let shown: std::collections::HashSet<&str> = buf
            .lines()
            .iter()
            .filter_map(|line| line.msgid.as_deref())
            .collect();
        lines.retain(|line| line.msgid.as_deref().is_none_or(|id| !shown.contains(id)));
        if buf.prepend(lines) == 0 {
            buf.history.logs_done = true;
        }
        if self.is_shown(network) && self.networks[network].active == buffer {
            self.mark_chat();
        }
    }

    /// The network registered or stopped trying: buffers that waited ask
    /// for their history now.
    pub(super) fn resolve_history(&mut self, network: usize) {
        let waiting: Vec<usize> = self.networks[network]
            .state
            .buffers
            .iter()
            .enumerate()
            .filter(|(_, buffer)| buffer.history.deferred)
            .map(|(index, _)| index)
            .collect();
        for buffer in waiting {
            // Still the first request: the latest history, not before
            // anything.
            let history = &mut self.networks[network].state.buffers[buffer].history;
            history.deferred = false;
            history.requested = false;
            self.request_history(network, buffer);
        }
    }

    /// The shown buffer asks for its history the first time it shows.
    pub(super) fn first_show(&mut self) {
        if self.welcome_visible() {
            return;
        }
        let network = self.active;
        let Some(buffer) = self.networks.get(network).map(|network| network.active) else {
            return;
        };
        if !self.networks[network].state.buffers[buffer]
            .history
            .requested
        {
            self.request_history(network, buffer);
        }
    }

    /// Scrolled near the oldest line: earlier ones are asked for.
    pub(super) fn near_top(&mut self) {
        let network = self.active;
        let Some(net) = self.networks.get(network) else {
            return;
        };
        let buffer = net.active;
        let buf = &net.state.buffers[buffer];
        let near = match buf.anchor {
            Some(anchor) => buf
                .index_of(anchor.id)
                .is_some_and(|index| index < NEAR_TOP),
            None => buf.lines().len() < NEAR_TOP,
        };
        if near {
            self.request_history(network, buffer);
        }
    }

    /// Tells the server buffer `buffer` was read up to its newest line, so
    /// your other clients know (IRCv3 read-marker).
    pub(super) fn mark_read_on_server(&mut self, network: usize, buffer: usize) {
        let Some(net) = self.networks.get_mut(network) else {
            return;
        };
        if net.demo || !net.registered() || !net.state.has("draft/read-marker") {
            return;
        }
        let Some(buf) = net.state.buffers.get_mut(buffer) else {
            return;
        };
        if buf.kind == BufferKind::Server {
            return;
        }
        let Some(newest) = buf.newest_time() else {
            return;
        };
        if buf.read_marker.is_some_and(|read| read >= newest) {
            return;
        }
        buf.read_marker = Some(newest);
        let line = format!(
            "MARKREAD {} timestamp={}",
            buf.name,
            format_server_time(newest)
        );
        let id = net.id;
        self.send(NetCommand::Send(id, line));
    }

    /// Lines arrived in buffer `buffer`, which you're looking at: they're
    /// read, and the server hears so now and then.
    pub(super) fn read_while_seen(&mut self, network: usize, buffer: usize) {
        let now = Instant::now();
        if self
            .read_sent
            .is_some_and(|sent| now.duration_since(sent) < READ_INTERVAL)
        {
            return;
        }
        self.read_sent = Some(now);
        self.mark_read_on_server(network, buffer);
    }

    /// Quitting: what's in view was read.
    pub(super) fn read_on_exit(&mut self) {
        for network in 0..self.networks.len() {
            for buffer in 0..self.networks[network].state.buffers.len() {
                if self.networks[network].state.buffers[buffer].seen {
                    self.mark_read_on_server(network, buffer);
                }
            }
        }
    }

    /// Asks what was read in each query: channels say so on joining
    /// (IRCv3 read-marker).
    pub(super) fn ask_read_markers(&mut self, network: usize) {
        let Some(net) = self.networks.get(network) else {
            return;
        };
        if net.demo || !net.state.has("draft/read-marker") {
            return;
        }
        let id = net.id;
        let lines: Vec<String> = net
            .state
            .buffers
            .iter()
            .filter(|buffer| buffer.kind == BufferKind::Query)
            .map(|buffer| format!("MARKREAD {}", buffer.name))
            .collect();
        for line in lines {
            self.send(NetCommand::Send(id, line));
        }
    }
}
