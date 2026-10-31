//! `--demo`: placeholder networks with channels, members and conversation,
//! to try the interface and take snapshots without connecting anywhere.

use crate::model::buffer::BufferKind;
use crate::model::members::Member;
use crate::model::network::NetworkState;
use crate::model::{Line, LineKind};
use crate::net::{ConnectConfig, LinkStatus};
use crate::platform::time::now_ms;

use super::App;

const MINUTE: i64 = 60_000;

/// Adds lines to buffer `index`: `(minutes ago, kind, nick, text)`. Your
/// own lines and mentions of you are marked as the network would.
fn say(state: &mut NetworkState, index: usize, now: i64, lines: &[(i64, LineKind, &str, &str)]) {
    for &(ago, kind, nick, text) in lines {
        let time = now - ago * MINUTE;
        let mut line = match kind {
            LineKind::Message | LineKind::Action | LineKind::Notice => {
                Line::message(time, kind, nick, text)
            }
            _ => Line::new(time, kind, nick, text),
        };
        line.own = state.is_me(nick);
        line.highlight = !line.own
            && kind == LineKind::Message
            && state.buffers[index].kind == BufferKind::Channel
            && state.mentions(text);
        state.buffers[index].push(line);
    }
}

fn members(state: &mut NetworkState, index: usize, names: &[&str]) {
    for name in names {
        let (prefixes, nick) = state.isupport.split_prefixes(name);
        let member = Member::new(nick, prefixes);
        state.buffers[index].members.insert(member, &state.isupport);
    }
}

impl App {
    pub(super) fn open_demo(&mut self) {
        use LineKind::{Action, Event, Message, Notice, Server, Status};
        let now = now_ms();
        let day = 24 * 60;
        for (title, status) in [
            (
                "Libera.Chat",
                LinkStatus::Registered {
                    nick: String::from("ruby"),
                },
            ),
            (
                "OFTC",
                LinkStatus::Registered {
                    nick: String::from("ruby"),
                },
            ),
            ("hackint", LinkStatus::Connecting),
            (
                "Rizon",
                LinkStatus::Disconnected {
                    reason: String::from("Disconnected"),
                    retry_in: None,
                },
            ),
        ] {
            let host = format!("irc.{}", title.to_ascii_lowercase());
            let id = self.add_network(
                title.to_string(),
                ConnectConfig::new(host, 6697, true, "ruby"),
            );
            let network = self.network_mut(id).expect("just added");
            network.demo = true;
            network.status = status;
            let state = &mut network.state;
            say(
                state,
                0,
                now,
                &[
                    (
                        90,
                        Status,
                        "•",
                        "A placeholder network: --demo never connects.",
                    ),
                    (90, Server, "", "Welcome to the demo network ruby"),
                    (
                        90,
                        Server,
                        "",
                        "Your host is irc.example.net, running version solanum-1.0",
                    ),
                    (
                        89,
                        Notice,
                        "NickServ",
                        "This nickname is registered. Please identify.",
                    ),
                ],
            );
            if title != "Libera.Chat" {
                continue;
            }
            network.lag = Some(std::time::Duration::from_millis(38));
            let state = &mut network.state;
            let (rust, _) = state.open(BufferKind::Channel, "#rust");
            let buffer = &mut state.buffers[rust];
            buffer.joined = true;
            buffer.topic = Some(String::from(
                "Rust programming | https://www.rust-lang.org | Be \u{2}kind\u{2}, stay on topic",
            ));
            buffer.modes = vec![('n', None), ('t', None)];
            members(
                state,
                rust,
                &[
                    "@ferris", "@rustbot", "+alice", "+bob", "ruby", "carol", "dave", "erin",
                    "frank", "grace", "heidi", "ivan", "judy", "mallory", "niaj", "olivia",
                    "peggy", "rupert", "sybil", "trent", "victor", "walter", "xavier", "yolanda",
                    "zoe",
                ],
            );
            say(
                state,
                rust,
                now,
                &[
                    (
                        day + 40,
                        Message,
                        "ferris",
                        "Reminder: the 2026 survey closes Friday.",
                    ),
                    (day + 38, Message, "alice", "Filled it in, thanks!"),
                    (
                        day + 30,
                        Event,
                        "←",
                        "dave quit (Ping timeout: 240 seconds)",
                    ),
                    (52, Event, "→", "You joined #rust"),
                    (
                        52,
                        Event,
                        "•",
                        "Topic: Rust programming | https://www.rust-lang.org | Be kind, stay on topic",
                    ),
                    (
                        31,
                        Message,
                        "carol",
                        "does anyone know why my iterator chain allocates?",
                    ),
                    (
                        30,
                        Message,
                        "alice",
                        "\u{2}collect()\u{2} in the middle, probably; try \u{1d}by_ref\u{1d} and keep it lazy",
                    ),
                    (29, Message, "carol", "oh, that was it. thanks"),
                    (24, Event, "→", "erin (erin@user/erin) joined"),
                    (
                        21,
                        Message,
                        "ruby",
                        "I put the profiler notes at https://example.org/notes/iterators#allocations if that helps",
                    ),
                    (20, Action, "bob", "bookmarks it"),
                ],
            );
            state.buffers[rust].mark_read();
            state.buffers[rust].marker = None;
            say(
                state,
                rust,
                now,
                &[
                    (
                        12,
                        Message,
                        "ferris",
                        "\u{3}04heads up:\u{3} the nightly from today breaks \u{1f}no_std\u{1f} builds; see #rust-embedded",
                    ),
                    (
                        9,
                        Message,
                        "dave",
                        "ruby: did the TLS rewrite land yet? the old code path still shows up in my traces",
                    ),
                    (8, Message, "ruby", "dave: yes, it went in this morning"),
                    (
                        7,
                        Message,
                        "grace",
                        "Grüße aus Zürich · привет · 你好，世界 · こんにちは · 안녕하세요 · ✓ → ∞ 🎉🦀",
                    ),
                    (6, Event, "•", "rustbot sets mode +v erin"),
                    (
                        3,
                        Message,
                        "erin",
                        "Has anyone tried the new \u{3}11,01 async closures \u{3}in a real project? Curious how they compare with boxed futures for a long-running server with many small tasks.",
                    ),
                ],
            );
            let (labwc, _) = state.open(BufferKind::Channel, "#labwc");
            let buffer = &mut state.buffers[labwc];
            buffer.joined = true;
            buffer.topic = Some(String::from(
                "labwc: a wlroots window-stacking compositor | https://labwc.github.io",
            ));
            members(
                state,
                labwc,
                &["@johan", "+consolatis", "ruby", "tokyo4j", "spl"],
            );
            say(
                state,
                labwc,
                now,
                &[
                    (
                        15,
                        Message,
                        "consolatis",
                        "the theme parser now reads window.button.spacing",
                    ),
                    (14, Message, "spl", "nice, that fixes my titlebar"),
                    (
                        2,
                        Message,
                        "tokyo4j",
                        "ruby: the sfwbar tray issue is upstream, filed it",
                    ),
                ],
            );
            let (offtopic, _) = state.open(BufferKind::Channel, "#rust-offtopic");
            members(state, offtopic, &["@ferris", "ruby", "mallory"]);
            state.buffers[offtopic].joined = true;
            say(
                state,
                offtopic,
                now,
                &[
                    (44, Event, "→", "mallory joined"),
                    (43, Event, "←", "trent left"),
                ],
            );
            let (old, _) = state.open(BufferKind::Channel, "#rubychat-old");
            say(
                state,
                old,
                now,
                &[(70, Event, "←", "You left (moved to #rubychat)")],
            );
            let (query, _) = state.open(BufferKind::Query, "alice");
            say(
                state,
                query,
                now,
                &[(5, Message, "alice", "got a minute to look at my patch?")],
            );
            network.active = rust;
        }
        // OFTC has news for you in the background.
        if let Some(oftc) = self.networks.get_mut(1) {
            let state = &mut oftc.state;
            let (index, _) = state.open(BufferKind::Channel, "#oftc");
            state.buffers[index].joined = true;
            members(state, index, &["@services", "ruby", "nick"]);
            say(
                state,
                index,
                now,
                &[
                    (7, Message, "nick", "ruby: welcome back"),
                    (6, Message, "nick", "ping me later"),
                ],
            );
        }
        self.active = 0;
    }
}
