//! The app beyond its window (DESIGN.md §5.3, §12): closing to the tray and
//! coming back, the tray's icon and menu, notifications for highlights and
//! private messages, and addresses handed over by later launches.
//!
//! The window is dropped when it closes to the tray (Wayland can't hide
//! one) and a new one opens at the same size when it's wanted again; every
//! cache survives. Showing the window only asks for it: the next wake opens
//! it, with the event loop at hand.

use std::collections::HashSet;
use std::time::Duration;

use winit::event_loop::ActiveEventLoop;
use winit::window::UserAttentionType;

use crate::irc::format::strip;
use crate::model::LineKind;
use crate::net::NetCommand;
use crate::platform::desktop::{
    Attention, Desktop, DesktopCommand, DesktopEvent, MenuAction, Notice, NoticeKey, TrayState,
};
use crate::platform::instance::Request;
use crate::platform::time::now_ms;

use super::App;
use super::commands::{self, Entity};

/// Lines older than this by server time (a bouncer's playback) don't
/// notify.
const STALE_MS: i64 = 2 * 60 * 1000;
/// A window that lost the keyboard this recently counts as focused when
/// the tray icon is clicked: a panel that takes the keyboard on a click
/// does so just before the click reaches RubyChat.
const FOCUS_GRACE: Duration = Duration::from_millis(300);

#[derive(Default)]
pub(super) struct Shell {
    /// The desktop thread; `None` when isolated or it couldn't start.
    pub(super) desktop: Option<Desktop>,
    /// A tray shows RubyChat's icon, so closing the window leaves RubyChat
    /// there.
    pub(super) tray: bool,
    /// No window is wanted: it closed to the tray, or RubyChat started
    /// there (`--tray`).
    pub(super) hidden: bool,
    /// The window's logical size and whether it was maximized as it
    /// closed, for the next one.
    pub(super) geometry: Option<(f64, f64, bool)>,
    /// A token for raising the window, from whatever asked to show it.
    pub(super) token: Option<String>,
    /// What the tray was last sent.
    tray_sent: Option<TrayState>,
    /// Buffers whose notification is up.
    noticed: HashSet<NoticeKey>,
    pub(super) away: bool,
    /// Closing to the tray has been explained once (`session.toml`).
    pub(super) explained: bool,
    /// The window asked for attention since it last had focus.
    pub(super) attention: bool,
    /// Activation on Wayland, for raising an open window with a token.
    #[cfg(all(unix, not(target_vendor = "apple")))]
    pub(super) activation: Option<crate::platform::activation::Activation>,
    /// What would have gone to the desktop thread, while isolated.
    #[cfg(test)]
    pub(super) sent: Vec<DesktopCommand>,
}

/// The key of RubyChat's own notice: no network has ID 0.
fn tray_notice() -> NoticeKey {
    (0, String::new())
}

impl App {
    fn desktop_send(&mut self, command: DesktopCommand) -> bool {
        match &self.shell.desktop {
            Some(desktop) => desktop.send(command),
            #[cfg(test)]
            None => {
                self.shell.sent.push(command);
                true
            }
            #[cfg(not(test))]
            None => false,
        }
    }

    /// The window's close button: to the tray while one shows RubyChat,
    /// else quitting.
    pub(super) fn close_window(&mut self) {
        if !self.shell.tray || !self.settings.close_to_tray {
            self.quit = true;
            return;
        }
        self.hide_window();
        if !self.shell.explained {
            self.shell.explained = true;
            self.session_changed();
            self.desktop_send(DesktopCommand::Notify(Notice {
                key: tray_notice(),
                title: String::from("RubyChat is still running"),
                nick: String::new(),
                text: String::from(
                    "Closing the window leaves RubyChat in the tray. Quit from the \
                     tray's menu, or with Ctrl+Q.",
                ),
            }));
        }
    }

    /// Drops the window, remembering its size.
    pub(super) fn hide_window(&mut self) {
        if let Some(view) = self.view.take() {
            let size = view
                .window
                .inner_size()
                .to_logical::<f64>(f64::from(view.scale));
            self.shell.geometry = Some((size.width, size.height, view.window.is_maximized()));
        }
        self.shell.hidden = true;
        self.forget_pointer_state();
        self.strip = Default::default();
        self.sync_seen();
    }

    /// Asks for the window: a closed one opens on the next wake, and an
    /// open one is raised with `token`, if there is one.
    pub(super) fn show_window(&mut self, token: Option<String>) {
        self.shell.hidden = false;
        if token.is_some() {
            self.shell.token = token;
        }
    }

    /// Opens the window if it's wanted and missing, and raises it with a
    /// waiting token. Every wake calls it.
    pub(super) fn ensure_window(&mut self, event_loop: &ActiveEventLoop) {
        if self.shell.hidden {
            return;
        }
        if self.view.is_none() {
            if let Err(error) = self.open_window(event_loop) {
                self.failure = Some(error);
                event_loop.exit();
            }
            self.update_title();
        } else if let Some(token) = self.shell.token.take() {
            self.raise(token);
        }
    }

    /// Hands the open window's surface to the compositor with `token`.
    fn raise(&mut self, token: String) {
        let Some(view) = &self.view else {
            return;
        };
        #[cfg(all(unix, not(target_vendor = "apple")))]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Some(activation) = &mut self.shell.activation
                && let Ok(handle) = view.window.window_handle()
                && let RawWindowHandle::Wayland(wayland) = handle.as_raw()
            {
                // SAFETY: the surface is the open window's, on the
                // connection the activation shares.
                unsafe { activation.activate(&token, wayland.surface) };
                return;
            }
        }
        let _ = token;
        view.window.focus_window();
    }

    /// The tray icon was clicked: a focused window goes to the tray; an
    /// unfocused one comes forward (a new window, without a token to raise
    /// the old one); a closed one opens.
    fn toggle_window(&mut self, token: Option<String>) {
        let focused = |view: &super::View| {
            view.focused
                || view
                    .unfocused_at
                    .is_some_and(|at| at.elapsed() < FOCUS_GRACE)
        };
        match self.view.as_ref().map(focused) {
            Some(true) => self.hide_window(),
            Some(false) if token.is_none() => {
                self.hide_window();
                self.show_window(None);
            }
            _ => self.show_window(token),
        }
    }

    pub(super) fn desktop_event(&mut self, event: DesktopEvent) {
        match event {
            DesktopEvent::Tray(shown) => {
                self.shell.tray = shown;
                // Without a tray, a closed window couldn't be reached.
                if !shown && self.shell.hidden {
                    self.show_window(None);
                }
            }
            DesktopEvent::Activate(token) => self.toggle_window(token),
            DesktopEvent::Menu(MenuAction::ShowHide) => {
                if self.view.is_some() {
                    self.hide_window();
                } else {
                    self.show_window(None);
                }
            }
            DesktopEvent::Menu(MenuAction::MarkAllRead) => self.mark_all_read(),
            DesktopEvent::Menu(MenuAction::Away) => self.toggle_away(),
            DesktopEvent::Menu(MenuAction::Quit) => self.quit = true,
            DesktopEvent::Open(key, token) => {
                self.open_noticed(&key);
                self.show_window(token);
            }
        }
    }

    /// Shows the buffer a notification was about.
    fn open_noticed(&mut self, key: &NoticeKey) {
        let Some(index) = self.networks.iter().position(|n| n.id.0 == key.0) else {
            return;
        };
        self.prompt_open = false;
        self.activate(index);
        if let Some(buffer) = self
            .networks
            .get(index)
            .and_then(|network| network.state.buffers.iter().position(|b| b.key == key.1))
        {
            self.activate_buffer(buffer);
        }
    }

    fn mark_all_read(&mut self) {
        for network in &mut self.networks {
            for buffer in &mut network.state.buffers {
                buffer.mark_read();
            }
        }
        for key in std::mem::take(&mut self.shell.noticed) {
            self.desktop_send(DesktopCommand::Withdraw(key));
        }
        self.mark_strip();
        self.mark_sidebar();
    }

    /// Away on every network, or back. Servers confirm with 305 and 306,
    /// which set the flag as `/away` does.
    fn toggle_away(&mut self) {
        let away = !self.shell.away;
        self.shell.away = away;
        let line = if away {
            String::from("AWAY :Away")
        } else {
            String::from("AWAY")
        };
        let ids: Vec<_> = self
            .networks
            .iter()
            .filter(|network| network.registered() && !network.demo)
            .map(|network| network.id)
            .collect();
        for id in ids {
            self.send(NetCommand::Send(id, line.clone()));
        }
    }

    /// RPL_UNAWAY (305) and RPL_NOWAWAY (306) say where things stand.
    pub(super) fn away_reply(&mut self, command: &str) {
        match command {
            "305" => self.shell.away = false,
            "306" => self.shell.away = true,
            _ => {}
        }
    }

    /// Lines that notify arrived in network `index`'s buffers `alerts`:
    /// notify while nobody watches the window, and ask for attention.
    pub(super) fn alert(&mut self, index: usize, alerts: &[usize]) {
        let watched = !self.shell.hidden && self.view.as_ref().is_some_and(|view| view.focused);
        let Some(network) = self.networks.get(index) else {
            return;
        };
        if watched || alerts.is_empty() || network.demo || !self.settings.notifications {
            return;
        }
        let now = now_ms();
        let notices: Vec<Notice> = alerts
            .iter()
            .filter_map(|&b| {
                let buffer = network.state.buffers.get(b)?;
                let line = buffer.lines().back()?;
                if now - line.time > STALE_MS {
                    return None;
                }
                let (nick, text) = match line.kind {
                    LineKind::Action => (
                        String::new(),
                        format!("* {} {}", line.nick, strip(&line.text)),
                    ),
                    _ => (line.nick.clone(), strip(&line.text).into_owned()),
                };
                Some(Notice {
                    key: (network.id.0, buffer.key.clone()),
                    title: format!("{} · {}", buffer.name, network.title),
                    nick,
                    text,
                })
            })
            .collect();
        for notice in notices {
            let key = notice.key.clone();
            if self.desktop_send(DesktopCommand::Notify(notice)) {
                self.shell.noticed.insert(key);
            }
        }
        if !self.shell.attention
            && let Some(view) = &self.view
        {
            view.window
                .request_user_attention(Some(UserAttentionType::Informational));
            self.shell.attention = true;
        }
    }

    /// Buffers someone now sees lose their notification.
    pub(super) fn withdraw_seen(&mut self, keys: Vec<NoticeKey>) {
        for key in keys {
            if self.shell.noticed.remove(&key) {
                self.desktop_send(DesktopCommand::Withdraw(key));
            }
        }
    }

    pub(super) fn noticed(&self, key: &NoticeKey) -> bool {
        self.shell.noticed.contains(key)
    }

    /// Tells the tray what changed: every wake checks.
    pub(super) fn sync_tray(&mut self) {
        if self.shell.desktop.is_none() && !cfg!(test) {
            return;
        }
        let (mut highlights, mut unread) = (0u32, 0u32);
        for network in &self.networks {
            for buffer in &network.state.buffers {
                highlights = highlights.saturating_add(buffer.highlights);
                unread = unread.saturating_add(buffer.unread);
            }
        }
        let attention = match (highlights, unread) {
            (0, 0) => Attention::None,
            (0, _) => Attention::Unread,
            _ => Attention::Highlight,
        };
        let tooltip = match (highlights, unread) {
            (0, 0) => String::from("Nothing unread"),
            (0, unread) => format!("{unread} unread"),
            (1, unread) => format!("1 highlight · {unread} unread"),
            (highlights, unread) => format!("{highlights} highlights · {unread} unread"),
        };
        let state = TrayState {
            attention,
            window_shown: self.view.is_some(),
            away: self.shell.away,
            tooltip,
        };
        if self.shell.tray_sent.as_ref() != Some(&state)
            && self.desktop_send(DesktopCommand::Tray(state.clone()))
        {
            self.shell.tray_sent = Some(state);
        }
    }

    /// A later launch handed over: open its addresses and show the window.
    pub(super) fn handed_over(&mut self, request: Request) {
        for address in &request.addresses {
            if !self.startup.done {
                // Once saved networks and the session are open.
                self.startup.addresses.push(address.clone());
                continue;
            }
            match commands::parse_server(address) {
                Ok(server) => self.open_address(server),
                Err(message) => {
                    self.prompt_message = Some(message);
                    self.mark_all();
                }
            }
        }
        if request.show || !request.addresses.is_empty() {
            self.show_window(request.token);
        }
    }

    /// An address from the command line or a link: a network already open
    /// to that server takes it, joining its channel or opening its query;
    /// otherwise RubyChat connects.
    pub(super) fn open_address(&mut self, server: commands::Server) {
        let Some(index) = self.networks.iter().position(|network| {
            !network.demo
                && network.config.host.eq_ignore_ascii_case(&server.host)
                && network.config.port == server.port
        }) else {
            return self.connect(server);
        };
        self.prompt_open = false;
        self.activate(index);
        match server.entity {
            Some(Entity::Channel { name, key }) => match self.networks[index].state.find(&name) {
                Some(buffer) => self.activate_buffer(buffer),
                None => self.join(&match key {
                    Some(key) => format!("{name} {key}"),
                    None => name,
                }),
            },
            Some(Entity::Nick(nick)) => self.open_query(&nick),
            None => {}
        }
    }
}
