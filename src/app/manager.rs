//! The Server Manager (DESIGN.md §8.9): editing saved networks. The form
//! edits a draft; Save checks it and writes `networks.toml` and
//! `secrets.toml`; Connect saves first when there are changes.

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};
use winit::window::CursorIcon;

use crate::config::networks::{
    AutoJoin, CertificatePolicy, FloodRate, LoginMethod, SavedNetwork, SavedServer,
};
use crate::irc::encoding::Fallback;
use crate::model::editor::LineEditor;
use crate::net::{LinkStatus, NetCommand};
use crate::render::Color;
use crate::ui::manager::{self, Action, Control, Entry, Field, Hit, ListEntry, ManagerLayout};
use crate::ui::theme::{AMBER, CYAN, DISABLED, PINK};

use super::App;
#[cfg(test)]
use super::commands;

pub(super) struct Manager {
    /// The saved network edited, by index; `None` for a new one.
    pub(super) selected: Option<usize>,
    pub(super) draft: SavedNetwork,
    original: SavedNetwork,
    pub(super) focus: Option<Field>,
    pub(super) editor: LineEditor,
    pub(super) scroll: f32,
    pub(super) reveal: bool,
    pub(super) message: Option<(String, bool)>,
    pub(super) hover: Option<Hit>,
    /// Delete, or Close with unsaved changes, waiting for a second press.
    confirm: Option<Action>,
}

impl Manager {
    fn new(selected: Option<usize>, draft: SavedNetwork, original: SavedNetwork) -> Self {
        Self {
            selected,
            draft,
            original,
            focus: Some(Field::Name),
            editor: LineEditor::default(),
            scroll: 0.0,
            reveal: false,
            message: None,
            hover: None,
            confirm: None,
        }
    }

    pub(super) fn dirty(&self) -> bool {
        self.draft != self.original
    }
}

/// A network to start from: one TLS server to fill in.
fn blank(name: String) -> SavedNetwork {
    SavedNetwork {
        name,
        servers: vec![SavedServer::new("", 6697, true)],
        ..SavedNetwork::default()
    }
}

fn field_text(draft: &SavedNetwork, field: Field) -> Option<String> {
    Some(match field {
        Field::Name => draft.name.clone(),
        Field::ServerHost(i) => draft.servers.get(i)?.host.clone(),
        Field::ServerPort(i) => draft.servers.get(i)?.port.to_string(),
        Field::Nick => draft.nick.clone(),
        Field::AltNicks => draft.alt_nicks.join(" "),
        Field::Username => draft.username.clone(),
        Field::Realname => draft.realname.clone(),
        Field::Account => draft.login.account.clone(),
        Field::Password => draft.login.password.clone(),
        Field::ClientCertificate => draft.login.client_certificate.clone(),
        Field::ChannelName(i) => draft.autojoin.get(i)?.channel.clone(),
        Field::ChannelKey(i) => draft.autojoin.get(i)?.key.clone(),
        Field::Command(i) => draft.commands.get(i)?.clone(),
        Field::QuitMessage => draft.quit_message.clone(),
        Field::PartMessage => draft.part_message.clone(),
        _ => return None,
    })
}

fn set_field_text(draft: &mut SavedNetwork, field: Field, text: &str) {
    let set = |slot: &mut String| *slot = text.to_string();
    match field {
        Field::Name => set(&mut draft.name),
        Field::ServerHost(i) => draft
            .servers
            .get_mut(i)
            .map(|s| set(&mut s.host))
            .unwrap_or_default(),
        Field::ServerPort(i) => {
            if let (Some(server), Ok(port)) = (draft.servers.get_mut(i), text.trim().parse::<u16>())
                && port != 0
            {
                server.port = port;
            }
        }
        Field::Nick => set(&mut draft.nick),
        Field::AltNicks => {
            draft.alt_nicks = text
                .split([' ', ','])
                .filter(|nick| !nick.is_empty())
                .map(str::to_string)
                .collect()
        }
        Field::Username => set(&mut draft.username),
        Field::Realname => set(&mut draft.realname),
        Field::Account => set(&mut draft.login.account),
        Field::Password => set(&mut draft.login.password),
        Field::ClientCertificate => set(&mut draft.login.client_certificate),
        Field::ChannelName(i) => draft
            .autojoin
            .get_mut(i)
            .map(|join| set(&mut join.channel))
            .unwrap_or_default(),
        Field::ChannelKey(i) => draft
            .autojoin
            .get_mut(i)
            .map(|join| set(&mut join.key))
            .unwrap_or_default(),
        Field::Command(i) => draft.commands.get_mut(i).map(set).unwrap_or_default(),
        Field::QuitMessage => set(&mut draft.quit_message),
        Field::PartMessage => set(&mut draft.part_message),
        _ => {}
    }
}

/// The next of `all` after `current`, `step` places on, wrapping.
fn cycle<T: Copy + PartialEq>(all: &[T], current: T, step: isize) -> T {
    let index = all.iter().position(|item| *item == current).unwrap_or(0) as isize;
    all[(index + step).rem_euclid(all.len() as isize) as usize]
}

/// Checks and tidies a draft before it's saved.
fn checked(mut draft: SavedNetwork, others: &[&SavedNetwork]) -> Result<SavedNetwork, String> {
    draft.name = draft.name.trim().to_string();
    if draft.name.is_empty() {
        return Err(String::from("A network needs a name."));
    }
    if others.iter().any(|other| other.name == draft.name) {
        return Err(format!("Another network is already called {}.", draft.name));
    }
    draft
        .servers
        .retain(|server| !server.host.trim().is_empty());
    for server in &mut draft.servers {
        server.host = server.host.trim().to_string();
    }
    if draft.servers.is_empty() {
        return Err(String::from(
            "Add a server to connect to, such as irc.libera.chat.",
        ));
    }
    draft
        .autojoin
        .retain(|join| !join.channel.trim().is_empty());
    for join in &mut draft.autojoin {
        let channel = join.channel.trim();
        join.channel = if channel.starts_with(['#', '&']) {
            channel.to_string()
        } else {
            format!("#{channel}")
        };
    }
    draft.commands.retain(|command| !command.trim().is_empty());
    draft.nick = draft.nick.trim().to_string();
    Ok(draft)
}

impl App {
    /// Opens the editor on saved network `selected`, or on a new one.
    pub(super) fn open_manager(&mut self, selected: Option<usize>) {
        let draft = match selected.and_then(|index| self.saved.list.get(index)) {
            Some(network) => network.clone(),
            None => blank(self.saved.unique_name("New network")),
        };
        let original = if selected.is_some() {
            draft.clone()
        } else {
            SavedNetwork::default()
        };
        let mut manager = Manager::new(
            selected.filter(|_| original != SavedNetwork::default()),
            draft,
            original,
        );
        manager.editor.set(&manager.draft.name);
        self.manager = Some(manager);
        self.forget_pointer_state();
        self.sync_seen();
        self.mark_all();
        self.update_title();
    }

    /// Opens the editor on a new network filled in from an open one, to
    /// save what quick connect opened (DESIGN.md §8.9).
    pub(super) fn save_open_network(&mut self, index: usize) {
        let Some(network) = self.networks.get(index) else {
            return;
        };
        let config = &network.config;
        let mut draft = blank(self.saved.unique_name(&network.title));
        draft.servers = vec![SavedServer::new(
            config.host.clone(),
            config.port,
            config.tls,
        )];
        draft.nick = network.state.nick.clone();
        draft.autojoin = network
            .state
            .buffers
            .iter()
            .filter(|buffer| {
                buffer.kind == crate::model::buffer::BufferKind::Channel && buffer.rejoin
            })
            .map(|buffer| AutoJoin {
                channel: buffer.name.clone(),
                key: String::new(),
            })
            .collect();
        self.open_manager(None);
        if let Some(manager) = &mut self.manager {
            manager.editor.set(&draft.name);
            manager.draft = draft;
        }
    }

    pub(super) fn close_manager(&mut self) {
        if self.manager.take().is_some() {
            self.sync_seen();
            self.mark_all();
            self.update_title();
        }
    }

    pub(super) fn manager_entries(&self) -> Vec<Entry> {
        let Some(manager) = &self.manager else {
            return Vec::new();
        };
        let pins: Vec<_> = manager
            .draft
            .servers
            .iter()
            .map(|server| self.known_hosts.get(&server.host, server.port))
            .collect();
        manager::entries(&manager.draft, &pins, manager.reveal)
    }

    pub(super) fn manager_layout(&self, entries: &[Entry]) -> Option<ManagerLayout> {
        let manager = self.manager.as_ref()?;
        let body = self.regions()?.body;
        Some(ManagerLayout::new(
            body,
            self.saved.list.len(),
            entries,
            manager.scroll,
        ))
    }

    /// The saved networks for the list, with jewels for those open.
    pub(super) fn manager_list(&self) -> Vec<(String, Option<Color>)> {
        self.saved
            .list
            .iter()
            .map(|saved| {
                let jewel = self
                    .networks
                    .iter()
                    .find(|n| n.saved.as_deref() == Some(saved.name.as_str()))
                    .map(|n| match n.status {
                        LinkStatus::Registered { .. } => CYAN,
                        LinkStatus::Failed { .. } => PINK,
                        LinkStatus::Disconnected { .. } => DISABLED,
                        _ => AMBER,
                    });
                (saved.name.clone(), jewel)
            })
            .collect()
    }

    /// The saved network's open network, if it's open.
    fn open_saved(&self, name: &str) -> Option<usize> {
        self.networks
            .iter()
            .position(|network| network.saved.as_deref() == Some(name))
    }

    /// What Connect does now: disconnect an open network, reconnect one that
    /// stopped, or connect (saving first when there are changes).
    pub(super) fn manager_connect_label(&self) -> &'static str {
        let Some(manager) = &self.manager else {
            return "Connect";
        };
        match self
            .open_saved(&manager.original.name)
            .filter(|_| manager.selected.is_some())
        {
            Some(index) => match self.networks[index].status {
                LinkStatus::Disconnected { retry_in: None, .. } | LinkStatus::Failed { .. } => {
                    "Reconnect"
                }
                _ => "Disconnect",
            },
            None if manager.dirty() => "Save and connect",
            None => "Connect",
        }
    }

    fn manager_say(&mut self, text: impl Into<String>, problem: bool) {
        if let Some(manager) = &mut self.manager {
            manager.message = Some((text.into(), problem));
        }
        self.mark_body();
    }

    /// Moves the focus, loading a text field into the editor and scrolling
    /// it into view.
    fn focus_field(&mut self, field: Option<Field>) {
        let entries = self.manager_entries();
        let layout = self.manager_layout(&entries);
        let Some(manager) = &mut self.manager else {
            return;
        };
        manager.focus = field;
        manager.confirm = None;
        manager.editor = LineEditor::default();
        if let Some(text) = field.and_then(|field| field_text(&manager.draft, field)) {
            manager.editor.set(&text);
        }
        if let (Some(field), Some(layout)) = (field, layout) {
            manager.scroll = layout.reveal(field);
        }
        self.mark_body();
    }

    /// The next or previous control, Tab's order.
    fn step_focus(&mut self, step: isize) {
        let order = manager::focus_order(&self.manager_entries());
        if order.is_empty() {
            return;
        }
        let current = self
            .manager
            .as_ref()
            .and_then(|manager| manager.focus)
            .and_then(|field| order.iter().position(|f| *f == field));
        let next = match current {
            Some(index) => (index as isize + step).rem_euclid(order.len() as isize) as usize,
            None if step > 0 => 0,
            None => order.len() - 1,
        };
        self.focus_field(Some(order[next]));
    }

    /// After the draft's shape changed: keep the focus on a control that
    /// still exists.
    fn keep_focus(&mut self, fallback: Field) {
        let order = manager::focus_order(&self.manager_entries());
        let focus = self.manager.as_ref().and_then(|manager| manager.focus);
        if focus.is_some_and(|field| !order.contains(&field)) {
            self.focus_field(Some(fallback));
        }
        self.mark_body();
    }

    /// The editor changed: the draft follows.
    pub(super) fn manager_edited(&mut self) {
        if let Some(manager) = &mut self.manager
            && let Some(field) = manager.focus
        {
            let text = manager.editor.text().to_string();
            set_field_text(&mut manager.draft, field, &text);
            manager.confirm = None;
        }
        self.mark_body();
    }

    /// Changes a choice by `step`, or flips a toggle.
    fn change_field(&mut self, field: Field, step: isize) {
        let Some(manager) = &mut self.manager else {
            return;
        };
        let draft = &mut manager.draft;
        match field {
            Field::ConnectAtStartup => draft.connect_at_startup = !draft.connect_at_startup,
            Field::Reconnect => draft.reconnect = !draft.reconnect,
            Field::ServerTls(i) => {
                if let Some(server) = draft.servers.get_mut(i) {
                    server.tls = !server.tls;
                    // The usual ports follow (RFC 7194).
                    server.port = match (server.tls, server.port) {
                        (true, 6667) => 6697,
                        (false, 6697) => 6667,
                        (_, port) => port,
                    };
                }
            }
            Field::ServerCertificate(i) => {
                if let Some(server) = draft.servers.get_mut(i) {
                    server.certificate = cycle(&CertificatePolicy::ALL, server.certificate, step);
                }
            }
            Field::Method => {
                draft.login.method = cycle(&LoginMethod::ALL, draft.login.method, step)
            }
            Field::Reveal => manager.reveal = !manager.reveal,
            Field::Require => draft.login.require = !draft.login.require,
            Field::Encoding => {
                draft.fallback_encoding = cycle(
                    &[Fallback::Windows1252, Fallback::Latin1],
                    draft.fallback_encoding,
                    step,
                )
            }
            Field::Flood => draft.flood = cycle(&FloodRate::ALL, draft.flood, step),
            Field::Ctcp => draft.ctcp_replies = !draft.ctcp_replies,
            _ => return,
        }
        manager.confirm = None;
        self.keep_focus(Field::Method);
    }

    /// Presses a button in the form.
    fn press_field(&mut self, field: Field) {
        let Some(manager) = &mut self.manager else {
            return;
        };
        let draft = &mut manager.draft;
        let focus = match field {
            Field::AddServer => {
                draft.servers.push(SavedServer::new("", 6697, true));
                Some(Field::ServerHost(draft.servers.len() - 1))
            }
            Field::ServerRemove(i) if i < draft.servers.len() => {
                draft.servers.remove(i);
                Some(Field::AddServer)
            }
            Field::AddChannel => {
                draft.autojoin.push(AutoJoin::default());
                Some(Field::ChannelName(draft.autojoin.len() - 1))
            }
            Field::ChannelRemove(i) if i < draft.autojoin.len() => {
                draft.autojoin.remove(i);
                Some(Field::AddChannel)
            }
            Field::AddCommand => {
                draft.commands.push(String::new());
                Some(Field::Command(draft.commands.len() - 1))
            }
            Field::CommandRemove(i) if i < draft.commands.len() => {
                draft.commands.remove(i);
                Some(Field::AddCommand)
            }
            Field::ServerForgetPin(i) => {
                let Some(server) = draft.servers.get(i).cloned() else {
                    return;
                };
                if self.known_hosts.forget(&server.host, server.port) {
                    self.save_known_hosts();
                    self.manager_say(
                        format!("Forgot the pinned certificate of {}.", server.address()),
                        false,
                    );
                }
                Some(Field::ServerHost(i))
            }
            _ => return self.change_field(field, 1),
        };
        self.focus_field(focus);
    }

    /// Checks the draft and saves it; true when saved.
    fn save_draft(&mut self) -> bool {
        let Some(manager) = &self.manager else {
            return false;
        };
        let others: Vec<&SavedNetwork> = self
            .saved
            .list
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != manager.selected)
            .map(|(_, n)| n)
            .collect();
        let draft = match checked(manager.draft.clone(), &others) {
            Ok(draft) => draft,
            Err(problem) => {
                self.manager_say(problem, true);
                return false;
            }
        };
        let old_name = manager.original.name.clone();
        let selected = match manager.selected {
            Some(index) => {
                self.saved.list[index] = draft.clone();
                index
            }
            None => {
                self.saved.list.push(draft.clone());
                self.saved.list.len() - 1
            }
        };
        // An open network follows its new name.
        if !old_name.is_empty() && old_name != draft.name {
            for network in self
                .networks
                .iter_mut()
                .filter(|n| n.saved.as_deref() == Some(old_name.as_str()))
            {
                network.saved = Some(draft.name.clone());
                network.title = draft.name.clone();
            }
        }
        let result = self.save_networks();
        if let Some(manager) = &mut self.manager {
            manager.selected = Some(selected);
            manager.original = draft.clone();
            manager.draft = draft;
            manager.confirm = None;
        }
        match result {
            Ok(()) => self.manager_say("Saved.", false),
            Err(problem) => self.manager_say(problem, true),
        }
        self.mark_strip();
        self.session_changed();
        true
    }

    fn manager_action(&mut self, action: Action) {
        let Some(manager) = &mut self.manager else {
            return;
        };
        match action {
            Action::Save => {
                self.save_draft();
            }
            Action::Close => {
                if manager.dirty() && manager.confirm != Some(Action::Close) {
                    manager.confirm = Some(Action::Close);
                    self.manager_say(
                        "Unsaved changes: Esc again leaves them; Ctrl+S saves them.",
                        true,
                    );
                } else {
                    self.close_manager();
                }
            }
            Action::Duplicate if manager.selected.is_some() => {
                let mut copy = manager.draft.clone();
                copy.name = self.saved.unique_name(&copy.name);
                copy.connect_at_startup = false;
                let mut duplicate = Manager::new(None, copy, SavedNetwork::default());
                duplicate.editor.set(&duplicate.draft.name);
                duplicate.message = Some((String::from("A copy, not saved yet."), false));
                self.manager = Some(duplicate);
                self.mark_body();
            }
            Action::Delete if manager.selected.is_some() => {
                if manager.confirm != Some(Action::Delete) {
                    manager.confirm = Some(Action::Delete);
                    let name = manager.original.name.clone();
                    self.manager_say(
                        format!("Delete {name}? Press Delete again to delete it."),
                        true,
                    );
                    return;
                }
                let index = manager.selected.expect("checked above");
                let removed = self.saved.list.remove(index);
                for network in self
                    .networks
                    .iter_mut()
                    .filter(|n| n.saved.as_deref() == Some(removed.name.as_str()))
                {
                    network.saved = None;
                }
                let _ = self.save_networks();
                let next =
                    (!self.saved.list.is_empty()).then(|| index.min(self.saved.list.len() - 1));
                self.open_manager(next);
                self.manager_say(format!("Deleted {}.", removed.name), false);
            }
            Action::Duplicate | Action::Delete => {}
            Action::Connect => self.manager_connect(),
        }
    }

    fn manager_connect(&mut self) {
        let Some(manager) = &self.manager else {
            return;
        };
        if manager.selected.is_some()
            && let Some(index) = self.open_saved(&manager.original.name)
        {
            let network = &self.networks[index];
            let id = network.id;
            match network.status {
                LinkStatus::Disconnected { retry_in: None, .. } | LinkStatus::Failed { .. } => {
                    if manager.dirty() && !self.save_draft() {
                        return;
                    }
                    let saved = self
                        .manager
                        .as_ref()
                        .map(|manager| manager.draft.clone())
                        .expect("open");
                    let config = self.config_for(&saved, &saved.servers[0]);
                    self.networks[index].config = config.clone();
                    self.networks[index].server = 0;
                    self.send(NetCommand::Connect(id, config));
                    self.manager_say("Reconnecting.", false);
                }
                _ => {
                    self.send(NetCommand::Disconnect(id, None));
                    self.manager_say("Disconnected.", false);
                }
            }
            return;
        }
        if manager.dirty() && !self.save_draft() {
            return;
        }
        let saved = self
            .manager
            .as_ref()
            .map(|manager| manager.draft.clone())
            .expect("open");
        self.manager = None;
        self.connect_saved(&saved, None);
    }

    /// Keys while the editor is open; it takes them all.
    pub(super) fn manager_key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let control = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let Some(manager) = &mut self.manager else {
            return;
        };
        let focus = manager.focus;
        let entries = self.manager_entries();
        let control_of = |field: Field| {
            entries.iter().find_map(|entry| match entry {
                Entry::Line(line) => line
                    .controls
                    .iter()
                    .find(|(f, _, _)| *f == field)
                    .map(|(_, control, _)| control.clone()),
                Entry::Section(_) => None,
            })
        };
        let kind = focus.and_then(control_of);
        let is_text = kind.as_ref().is_some_and(Control::is_text);
        match &event.logical_key {
            Key::Character(text) if control => match text.to_lowercase().as_str() {
                "s" => {
                    self.save_draft();
                }
                "q" => self.quit = true,
                "a" if is_text => {
                    if let Some(manager) = &mut self.manager {
                        manager.editor.select_all();
                    }
                    self.mark_body();
                }
                "c" => self.copy(),
                "x" if is_text => self.cut(),
                "v" if is_text => self.paste(),
                _ => {}
            },
            Key::Named(NamedKey::Escape) => self.manager_action(Action::Close),
            Key::Named(NamedKey::Tab) => self.step_focus(if shift { -1 } else { 1 }),
            Key::Named(NamedKey::ArrowUp) => self.step_focus(-1),
            Key::Named(NamedKey::ArrowDown) => self.step_focus(1),
            Key::Named(NamedKey::Enter) if control => self.manager_action(Action::Connect),
            Key::Named(NamedKey::Enter) if is_text => self.step_focus(1),
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) if !is_text => {
                if let Some(field) = focus {
                    self.press_field(field);
                }
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight)
                if matches!(kind, Some(Control::Choice(_))) =>
            {
                let step = if matches!(event.logical_key, Key::Named(NamedKey::ArrowLeft)) {
                    -1
                } else {
                    1
                };
                if let Some(field) = focus {
                    self.change_field(field, step);
                }
            }
            _ if is_text => {
                let Some(manager) = &mut self.manager else {
                    return;
                };
                let editor = &mut manager.editor;
                match &event.logical_key {
                    Key::Named(NamedKey::Backspace) => editor.backspace(control),
                    Key::Named(NamedKey::Delete) => editor.delete(control),
                    Key::Named(NamedKey::ArrowLeft) => editor.left(control, shift),
                    Key::Named(NamedKey::ArrowRight) => editor.right(control, shift),
                    Key::Named(NamedKey::Home) => editor.home(shift),
                    Key::Named(NamedKey::End) => editor.end(shift),
                    _ => match &event.text {
                        Some(text) if !control => editor.insert(text),
                        _ => return,
                    },
                }
                self.manager_edited();
            }
            _ => {}
        }
    }

    pub(super) fn manager_pointer_moved(&mut self) {
        let point = self.view.as_ref().map(|view| view.pointer);
        let entries = self.manager_entries();
        let hit = match (point, self.manager_layout(&entries)) {
            (Some(point), Some(layout)) => layout.hit(point),
            _ => None,
        };
        let text = hit.is_some_and(|hit| match hit {
            Hit::Field(field) => entries.iter().any(|entry| matches!(entry, Entry::Line(line) if line.controls.iter().any(|(f, c, _)| *f == field && c.is_text()))),
            _ => false,
        });
        if let Some(manager) = &mut self.manager
            && manager.hover != hit
        {
            manager.hover = hit;
            self.mark_body();
        }
        self.set_cursor(match (hit, text) {
            (_, true) => CursorIcon::Text,
            (Some(_), false) => CursorIcon::Pointer,
            (None, _) => CursorIcon::Default,
        });
    }

    pub(super) fn manager_pointer_pressed(&mut self) {
        let point = self.view.as_ref().map(|view| view.pointer);
        let entries = self.manager_entries();
        let layout = self.manager_layout(&entries);
        let (Some(point), Some(layout)) = (point, layout) else {
            return;
        };
        match layout.hit(point) {
            Some(Hit::Action(action)) => self.manager_action(action),
            Some(Hit::Network(which)) => {
                let Some(manager) = &self.manager else {
                    return;
                };
                if which == manager.selected && which.is_some() {
                    return;
                }
                if manager.dirty() {
                    self.manager_say("Save or leave your changes first: Ctrl+S saves them; Esc twice leaves them.", true);
                    return;
                }
                self.open_manager(which);
            }
            Some(Hit::Field(field)) => {
                let control = entries.iter().find_map(|entry| match entry {
                    Entry::Line(line) => line
                        .controls
                        .iter()
                        .find(|(f, _, _)| *f == field)
                        .map(|(_, control, _)| control.clone()),
                    Entry::Section(_) => None,
                });
                match control {
                    Some(Control::Text { .. }) => self.focus_field(Some(field)),
                    Some(Control::Choice(_)) => {
                        // The left half goes back, the right half forward.
                        let rect = layout
                            .controls
                            .iter()
                            .find(|(f, _)| *f == field)
                            .map(|(_, rect)| *rect);
                        let step = if rect.is_some_and(|rect| point.x < rect.center().x) {
                            -1
                        } else {
                            1
                        };
                        self.focus_field(Some(field));
                        self.change_field(field, step);
                    }
                    Some(_) => {
                        self.focus_field(Some(field));
                        self.press_field(field);
                    }
                    None => {}
                }
            }
            None => {}
        }
    }

    pub(super) fn manager_scroll(&mut self, pixels: f32) {
        let entries = self.manager_entries();
        let max = self
            .manager_layout(&entries)
            .map_or(0.0, |layout| layout.max_scroll);
        if let Some(manager) = &mut self.manager {
            manager.scroll = (manager.scroll - pixels).clamp(0.0, max);
        }
        self.mark_body();
    }

    /// The list for painting.
    pub(super) fn manager_list_entries<'a>(
        list: &'a [(String, Option<Color>)],
    ) -> Vec<ListEntry<'a>> {
        list.iter()
            .map(|(name, jewel)| ListEntry {
                name,
                jewel: *jewel,
            })
            .collect()
    }
}

/// Quick connect's address as a saved network's server, for tests.
#[cfg(test)]
pub(super) fn server_of(address: &str) -> SavedServer {
    let server = commands::parse_server(address).unwrap();
    SavedServer::new(server.host, server.port, server.tls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_are_checked_and_tidied() {
        let mut draft = blank(String::from(" Libera.Chat "));
        draft.servers.push(server_of("irc.libera.chat"));
        draft.autojoin = vec![
            AutoJoin {
                channel: String::from("rust"),
                key: String::new(),
            },
            AutoJoin::default(),
        ];
        draft.commands = vec![String::new(), String::from("/mode ruby +R")];
        let tidy = checked(draft.clone(), &[]).unwrap();
        assert_eq!(tidy.name, "Libera.Chat");
        assert_eq!(tidy.servers.len(), 1, "the empty server row goes");
        assert_eq!(
            tidy.autojoin,
            [AutoJoin {
                channel: String::from("#rust"),
                key: String::new()
            }]
        );
        assert_eq!(tidy.commands, ["/mode ruby +R"]);
        let other = tidy.clone();
        assert!(
            checked(draft.clone(), &[&other])
                .unwrap_err()
                .contains("already called")
        );
        assert!(
            checked(blank(String::from("X")), &[])
                .unwrap_err()
                .contains("server")
        );
        assert!(checked(blank(String::new()), &[]).is_err());
    }

    fn app() -> App {
        let options = super::super::Options {
            isolated: true,
            ..Default::default()
        };
        App::new(options, || -> super::super::Wake { Box::new(|| {}) }).unwrap()
    }

    /// Types into `field` as a person would.
    fn type_into(app: &mut App, field: Field, text: &str) {
        app.focus_field(Some(field));
        let manager = app.manager.as_mut().unwrap();
        manager.editor.select_all();
        manager.editor.insert(text);
        app.manager_edited();
    }

    #[test]
    fn a_new_network_is_filled_in_and_saved() {
        let mut app = app();
        app.open_manager(None);
        assert_eq!(app.manager.as_ref().unwrap().draft.name, "New network");
        type_into(&mut app, Field::Name, "Libera.Chat");
        type_into(&mut app, Field::ServerHost(0), "irc.libera.chat");
        app.change_field(Field::Method, 2);
        assert_eq!(
            app.manager.as_ref().unwrap().draft.login.method,
            LoginMethod::SaslPlain
        );
        type_into(&mut app, Field::Password, "hunter2");
        app.press_field(Field::AddChannel);
        assert_eq!(
            app.manager.as_ref().unwrap().focus,
            Some(Field::ChannelName(0)),
            "the new row has the focus"
        );
        type_into(&mut app, Field::ChannelName(0), "rust");
        assert!(app.save_draft());
        let saved = &app.saved.list[0];
        assert_eq!(
            (saved.name.as_str(), saved.login.password.as_str()),
            ("Libera.Chat", "hunter2")
        );
        assert_eq!(saved.autojoin[0].channel, "#rust", "tidied on saving");
        let manager = app.manager.as_ref().unwrap();
        assert!(!manager.dirty() && manager.selected == Some(0));
    }

    #[test]
    fn deleting_and_leaving_unsaved_changes_ask_twice() {
        let mut app = app();
        app.open_manager(None);
        type_into(&mut app, Field::ServerHost(0), "irc.oftc.net");
        assert!(app.save_draft());
        type_into(&mut app, Field::Nick, "ruby");
        app.manager_action(Action::Close);
        assert!(app.manager.is_some(), "the first Esc warns");
        app.manager_action(Action::Close);
        assert!(app.manager.is_none());
        app.open_manager(Some(0));
        app.manager_action(Action::Delete);
        assert_eq!(app.saved.list.len(), 1, "the first press asks");
        app.manager_action(Action::Delete);
        assert!(app.saved.list.is_empty());
    }

    #[test]
    fn renaming_carries_over_to_the_open_network_and_connect_opens_it() {
        let mut app = app();
        app.open_manager(None);
        type_into(&mut app, Field::Name, "Libera");
        type_into(&mut app, Field::ServerHost(0), "irc.libera.chat");
        app.manager_action(Action::Connect);
        assert!(app.manager.is_none(), "connecting shows the network");
        assert_eq!(
            (app.networks.len(), app.networks[0].saved.as_deref()),
            (1, Some("Libera"))
        );
        app.open_manager(Some(0));
        assert_eq!(app.manager_connect_label(), "Disconnect");
        type_into(&mut app, Field::Name, "Libera.Chat");
        assert!(app.save_draft());
        assert_eq!(
            (
                app.networks[0].title.as_str(),
                app.networks[0].saved.as_deref()
            ),
            ("Libera.Chat", Some("Libera.Chat"))
        );
    }

    #[test]
    fn quick_connect_networks_can_be_saved() {
        let mut app = app();
        app.connect(commands::parse_server("ircs://irc.example.net:7000/#chat").unwrap());
        app.save_open_network(0);
        let draft = &app.manager.as_ref().unwrap().draft;
        assert_eq!(draft.name, "example.net");
        assert_eq!(
            draft.servers,
            [SavedServer::new("irc.example.net", 7000, true)]
        );
        assert_eq!(draft.autojoin[0].channel, "#chat");
    }

    #[test]
    fn fields_read_and_write_the_draft() {
        let mut draft = blank(String::from("N"));
        set_field_text(&mut draft, Field::ServerPort(0), "7000");
        set_field_text(&mut draft, Field::ServerPort(0), "not a port");
        assert_eq!(
            draft.servers[0].port, 7000,
            "nonsense keeps the last good port"
        );
        set_field_text(&mut draft, Field::AltNicks, "ruby_, ruby__  ruby3");
        assert_eq!(draft.alt_nicks, ["ruby_", "ruby__", "ruby3"]);
        assert_eq!(
            field_text(&draft, Field::AltNicks).unwrap(),
            "ruby_ ruby__ ruby3"
        );
        assert_eq!(
            cycle(&LoginMethod::ALL, LoginMethod::None, -1),
            LoginMethod::NickServ
        );
    }
}
