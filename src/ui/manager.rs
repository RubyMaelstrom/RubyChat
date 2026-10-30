//! The Server Manager's editor (DESIGN.md §8.9): saved networks down the
//! left, the selected one's settings in a form on the right, and the
//! actions along the bottom, in a COMMAND-style panel that works entirely
//! from the keyboard.

use crate::config::known_hosts::{Fingerprint, display};
use crate::config::networks::{SavedNetwork, fallback_label};
use crate::model::editor::LineEditor;
use crate::render::{Align, Color, Painter, Point, Rect};

use super::theme::{self, CANVAS, CYAN, DISABLED, HOVER, MUTED, PINK, RULE, TEXT};
use super::widgets;

const MAX_WIDTH: f32 = 1000.0;
const MAX_HEIGHT: f32 = 760.0;
const HEADER: f32 = 44.0;
const LIST_WIDTH: f32 = 210.0;
const LIST_ROW: f32 = 30.0;
const ACTIONS: f32 = 52.0;
const SECTION: f32 = 32.0;
const LINE: f32 = 34.0;
const CONTROL: f32 = 26.0;
const LABEL_WIDTH: f32 = 150.0;
const GAP: f32 = 8.0;

/// Every control in the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Name,
    ConnectAtStartup,
    Reconnect,
    ServerHost(usize),
    ServerPort(usize),
    ServerTls(usize),
    ServerCertificate(usize),
    ServerRemove(usize),
    ServerForgetPin(usize),
    AddServer,
    Nick,
    AltNicks,
    Username,
    Realname,
    Method,
    Account,
    Password,
    Reveal,
    ClientCertificate,
    Require,
    ChannelName(usize),
    ChannelKey(usize),
    ChannelRemove(usize),
    AddChannel,
    Command(usize),
    CommandRemove(usize),
    AddCommand,
    Encoding,
    Flood,
    Ctcp,
    QuitMessage,
    PartMessage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    Text {
        value: String,
        masked: bool,
        placeholder: &'static str,
    },
    Toggle {
        on: bool,
        text: &'static str,
    },
    Choice(String),
    Button {
        text: String,
        caution: bool,
    },
}

impl Control {
    pub fn is_text(&self) -> bool {
        matches!(self, Control::Text { .. })
    }
}

/// A form line: a label, then controls with their widths (0 fills).
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub label: &'static str,
    pub controls: Vec<(Field, Control, f32)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Section(&'static str),
    Line(Line),
}

fn text(value: &str, placeholder: &'static str) -> Control {
    Control::Text {
        value: value.to_string(),
        masked: false,
        placeholder,
    }
}

fn line(label: &'static str, controls: Vec<(Field, Control, f32)>) -> Entry {
    Entry::Line(Line { label, controls })
}

/// The form for `network`. `pins` holds each server's pinned fingerprint.
pub fn entries(network: &SavedNetwork, pins: &[Option<Fingerprint>], reveal: bool) -> Vec<Entry> {
    let login = &network.login;
    let mut entries = vec![
        Entry::Section("NETWORK"),
        line(
            "Name",
            vec![(Field::Name, text(&network.name, "Libera.Chat"), 0.0)],
        ),
        line(
            "",
            vec![
                (
                    Field::ConnectAtStartup,
                    Control::Toggle {
                        on: network.connect_at_startup,
                        text: "Connect at startup",
                    },
                    200.0,
                ),
                (
                    Field::Reconnect,
                    Control::Toggle {
                        on: network.reconnect,
                        text: "Reconnect automatically",
                    },
                    0.0,
                ),
            ],
        ),
        Entry::Section("SERVERS, TRIED IN ORDER"),
    ];
    for (index, server) in network.servers.iter().enumerate() {
        entries.push(line(
            if index == 0 { "Server" } else { "" },
            vec![
                (
                    Field::ServerHost(index),
                    text(&server.host, "irc.libera.chat"),
                    0.0,
                ),
                (
                    Field::ServerPort(index),
                    text(&server.port.to_string(), "6697"),
                    64.0,
                ),
                (
                    Field::ServerTls(index),
                    Control::Toggle {
                        on: server.tls,
                        text: "TLS",
                    },
                    60.0,
                ),
                (
                    Field::ServerCertificate(index),
                    Control::Choice(server.certificate.label().to_string()),
                    170.0,
                ),
                (
                    Field::ServerRemove(index),
                    Control::Button {
                        text: String::from("\u{2715}"),
                        caution: true,
                    },
                    30.0,
                ),
            ],
        ));
        if let Some(Some(pin)) = pins.get(index) {
            let mut short = display(pin);
            short.truncate(23);
            entries.push(line(
                "",
                vec![(
                    Field::ServerForgetPin(index),
                    Control::Button {
                        text: format!("Forget pinned certificate {short}…"),
                        caution: true,
                    },
                    0.0,
                )],
            ));
        }
    }
    entries.push(line(
        "",
        vec![(
            Field::AddServer,
            Control::Button {
                text: String::from("+ Add server"),
                caution: false,
            },
            140.0,
        )],
    ));
    entries.push(Entry::Section("IDENTITY"));
    entries.push(line(
        "Nick",
        vec![(Field::Nick, text(&network.nick, "your login name"), 0.0)],
    ));
    entries.push(line(
        "Alternates",
        vec![(
            Field::AltNicks,
            text(&network.alt_nicks.join(" "), "ruby_ ruby__"),
            0.0,
        )],
    ));
    entries.push(line(
        "Username",
        vec![(Field::Username, text(&network.username, "the nick"), 0.0)],
    ));
    entries.push(line(
        "Real name",
        vec![(Field::Realname, text(&network.realname, "RubyChat"), 0.0)],
    ));
    entries.push(Entry::Section("LOGIN"));
    entries.push(line(
        "Method",
        vec![(
            Field::Method,
            Control::Choice(login.method.label().to_string()),
            0.0,
        )],
    ));
    if login.method.uses_account() {
        entries.push(line(
            "Account",
            vec![(Field::Account, text(&login.account, "the nick"), 0.0)],
        ));
    }
    if login.method.uses_password() {
        entries.push(line(
            "Password",
            vec![
                (
                    Field::Password,
                    Control::Text {
                        value: login.password.clone(),
                        masked: !reveal,
                        placeholder: "kept in secrets.toml",
                    },
                    0.0,
                ),
                (
                    Field::Reveal,
                    Control::Toggle {
                        on: reveal,
                        text: "Show",
                    },
                    80.0,
                ),
            ],
        ));
    }
    entries.push(line(
        "Client certificate",
        vec![(
            Field::ClientCertificate,
            text(
                &login.client_certificate,
                "a PEM file in certs/, for EXTERNAL or CertFP",
            ),
            0.0,
        )],
    ));
    if login.method.is_sasl() {
        entries.push(line(
            "",
            vec![(
                Field::Require,
                Control::Toggle {
                    on: login.require,
                    text: "Require login: disconnect if it fails",
                },
                0.0,
            )],
        ));
    }
    entries.push(Entry::Section("CHANNELS"));
    for (index, join) in network.autojoin.iter().enumerate() {
        entries.push(line(
            if index == 0 { "Join" } else { "" },
            vec![
                (
                    Field::ChannelName(index),
                    text(&join.channel, "#channel"),
                    0.0,
                ),
                (
                    Field::ChannelKey(index),
                    Control::Text {
                        value: join.key.clone(),
                        masked: !reveal,
                        placeholder: "key",
                    },
                    140.0,
                ),
                (
                    Field::ChannelRemove(index),
                    Control::Button {
                        text: String::from("\u{2715}"),
                        caution: true,
                    },
                    30.0,
                ),
            ],
        ));
    }
    entries.push(line(
        "",
        vec![(
            Field::AddChannel,
            Control::Button {
                text: String::from("+ Add channel"),
                caution: false,
            },
            140.0,
        )],
    ));
    entries.push(Entry::Section("AFTER CONNECTING"));
    for (index, command) in network.commands.iter().enumerate() {
        entries.push(line(
            if index == 0 { "Run" } else { "" },
            vec![
                (Field::Command(index), text(command, "/mode ruby +R"), 0.0),
                (
                    Field::CommandRemove(index),
                    Control::Button {
                        text: String::from("\u{2715}"),
                        caution: true,
                    },
                    30.0,
                ),
            ],
        ));
    }
    entries.push(line(
        "",
        vec![(
            Field::AddCommand,
            Control::Button {
                text: String::from("+ Add command"),
                caution: false,
            },
            140.0,
        )],
    ));
    entries.push(Entry::Section("ADVANCED"));
    entries.push(line(
        "Encoding",
        vec![(
            Field::Encoding,
            Control::Choice(fallback_label(network.fallback_encoding).to_string()),
            0.0,
        )],
    ));
    entries.push(line(
        "Flood control",
        vec![(
            Field::Flood,
            Control::Choice(network.flood.label().to_string()),
            0.0,
        )],
    ));
    entries.push(line(
        "",
        vec![(
            Field::Ctcp,
            Control::Toggle {
                on: network.ctcp_replies,
                text: "Answer CTCP queries",
            },
            0.0,
        )],
    ));
    entries.push(line(
        "Quit message",
        vec![(
            Field::QuitMessage,
            text(&network.quit_message, "RubyChat"),
            0.0,
        )],
    ));
    entries.push(line(
        "Part message",
        vec![(Field::PartMessage, text(&network.part_message, "none"), 0.0)],
    ));
    entries
}

/// The controls in order, for Tab.
pub fn focus_order(entries: &[Entry]) -> Vec<Field> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Line(line) => Some(line.controls.iter().map(|(field, _, _)| *field)),
            Entry::Section(_) => None,
        })
        .flatten()
        .collect()
}

/// The buttons along the bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Connect,
    Save,
    Duplicate,
    Delete,
    Close,
}

/// What the pointer can be over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A saved network, or the new one (`None`).
    Network(Option<usize>),
    Field(Field),
    Action(Action),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ManagerLayout {
    pub panel: Rect,
    pub list: Rect,
    /// Saved networks' rows, then "+ New network".
    pub rows: Vec<Rect>,
    /// The form's viewport.
    pub form: Rect,
    /// Lines and sections, placed after scrolling.
    pub placed: Vec<(usize, Rect)>,
    pub controls: Vec<(Field, Rect)>,
    pub actions: Vec<(Action, Rect)>,
    pub scroll: f32,
    pub max_scroll: f32,
}

impl ManagerLayout {
    pub fn new(area: Rect, networks: usize, entries: &[Entry], scroll: f32) -> Self {
        let width = MAX_WIDTH.min(area.width - 32.0).max(0.0).round();
        let height = MAX_HEIGHT.min(area.height - 32.0).max(0.0).round();
        let panel = Rect::new(
            (area.x + (area.width - width) / 2.0).round(),
            (area.y + (area.height - height) / 2.0).round(),
            width,
            height,
        );
        let body_top = panel.y + HEADER;
        let body_height = (panel.height - HEADER - ACTIONS).max(0.0);
        let list = Rect::new(panel.x + 12.0, body_top, LIST_WIDTH, body_height);
        let rows = (0..=networks)
            .map(|i| {
                Rect::new(
                    list.x,
                    list.y + 8.0 + i as f32 * LIST_ROW,
                    list.width,
                    LIST_ROW,
                )
            })
            .collect();
        let form = Rect::new(
            list.right() + 16.0,
            body_top,
            (panel.right() - 16.0 - list.right() - 16.0).max(0.0),
            body_height,
        );
        let content: f32 = entries
            .iter()
            .map(|entry| {
                if matches!(entry, Entry::Section(_)) {
                    SECTION
                } else {
                    LINE
                }
            })
            .sum::<f32>()
            + 12.0;
        let max_scroll = (content - form.height).max(0.0);
        let scroll = scroll.clamp(0.0, max_scroll);
        let mut y = form.y + 4.0 - scroll;
        let mut placed = Vec::with_capacity(entries.len());
        let mut controls = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let height = if matches!(entry, Entry::Section(_)) {
                SECTION
            } else {
                LINE
            };
            let rect = Rect::new(form.x, y, form.width, height);
            placed.push((index, rect));
            if let Entry::Line(line) = entry {
                let start = form.x + LABEL_WIDTH;
                let room = (form.right() - start - 8.0).max(0.0);
                let fixed: f32 = line
                    .controls
                    .iter()
                    .map(|(_, _, width)| *width)
                    .sum::<f32>()
                    + GAP * line.controls.len().saturating_sub(1) as f32;
                let fills = line
                    .controls
                    .iter()
                    .filter(|(_, _, width)| *width == 0.0)
                    .count()
                    .max(1) as f32;
                let fill = ((room - fixed) / fills).max(40.0);
                let mut x = start;
                for (field, _, width) in &line.controls {
                    let w = if *width == 0.0 { fill } else { *width };
                    controls.push((
                        *field,
                        Rect::new(x, (y + (LINE - CONTROL) / 2.0).round(), w, CONTROL),
                    ));
                    x += w + GAP;
                }
            }
            y += height;
        }
        let bar = Rect::new(
            panel.x + 12.0,
            panel.bottom() - ACTIONS,
            panel.width - 24.0,
            ACTIONS,
        );
        let mut actions = Vec::new();
        let mut x = bar.right() - 4.0;
        for (action, width) in [
            (Action::Close, 92.0),
            (Action::Delete, 92.0),
            (Action::Duplicate, 108.0),
            (Action::Save, 112.0),
            (Action::Connect, 150.0),
        ] {
            x -= width;
            actions.push((action, Rect::new(x, bar.y + 12.0, width, 28.0)));
            x -= GAP;
        }
        Self {
            panel,
            list,
            rows,
            form,
            placed,
            controls,
            actions,
            scroll,
            max_scroll,
        }
    }

    /// What `point` is over: controls only within the form's viewport.
    pub fn hit(&self, point: Point) -> Option<Hit> {
        if let Some((action, _)) = self.actions.iter().find(|(_, rect)| rect.contains(point)) {
            return Some(Hit::Action(*action));
        }
        if self.list.contains(point)
            && let Some(index) = self.rows.iter().position(|row| row.contains(point))
        {
            return Some(Hit::Network((index + 1 < self.rows.len()).then_some(index)));
        }
        if !self.form.contains(point) {
            return None;
        }
        self.controls
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(field, _)| Hit::Field(*field))
    }

    /// The scroll that brings `field` into view.
    pub fn reveal(&self, field: Field) -> f32 {
        let Some((_, rect)) = self.controls.iter().find(|(f, _)| *f == field) else {
            return self.scroll;
        };
        let (top, bottom) = (rect.y - 8.0, rect.bottom() + 8.0);
        if top < self.form.y {
            (self.scroll - (self.form.y - top)).max(0.0)
        } else if bottom > self.form.bottom() {
            (self.scroll + bottom - self.form.bottom()).min(self.max_scroll)
        } else {
            self.scroll
        }
    }
}

/// One saved network in the list.
pub struct ListEntry<'a> {
    pub name: &'a str,
    /// The jewel: its connection's color, if it's open.
    pub jewel: Option<Color>,
}

pub struct ManagerView<'a> {
    pub networks: &'a [ListEntry<'a>],
    /// The network being edited; `None` for a new one.
    pub selected: Option<usize>,
    pub title: &'a str,
    pub entries: &'a [Entry],
    pub focus: Option<Field>,
    /// The focused text field's editor.
    pub editor: &'a mut LineEditor,
    pub hover: Option<Hit>,
    /// The status line: text, and whether it reports a problem.
    pub message: Option<(&'a str, bool)>,
    pub dirty: bool,
    pub connect: &'a str,
    pub focused: bool,
}

pub fn paint_manager(
    painter: &mut Painter<'_>,
    area: Rect,
    layout: &ManagerLayout,
    view: &mut ManagerView<'_>,
) {
    painter.fill_rect(area, CANVAS);
    let panel = layout.panel;
    if panel.width < 200.0 || panel.height < 160.0 {
        return;
    }
    painter.save_clip(area);
    widgets::frame(painter, panel, 22.0);
    widgets::traces(painter, panel);
    let tag = Rect::new(panel.x + 16.0, panel.y + 12.0, 96.0, 22.0);
    widgets::tag(painter, tag, "SERVERS");
    let title = if view.dirty {
        format!("{}  \u{2022} unsaved", view.title)
    } else {
        view.title.to_string()
    };
    painter.label(
        &title,
        Rect::new(tag.right() + 14.0, tag.y, panel.width - 260.0, tag.height),
        &theme::chrome(12.0),
        TEXT,
        Align::Start,
    );
    // The saved networks.
    let list = layout.list;
    painter.fill_rect(
        Rect::new(list.right() + 7.0, list.y + 8.0, 1.0, list.height - 16.0),
        RULE,
    );
    painter.save_clip(list);
    for (index, row) in layout.rows.iter().enumerate() {
        let entry = view.networks.get(index);
        let which = entry.map(|_| index);
        let selected = view.selected == which && (entry.is_some() || view.selected.is_none());
        let hovered = view.hover == Some(Hit::Network(which));
        if selected {
            painter.fill_rect(*row, HOVER);
            painter.fill_rect(
                Rect::new(row.x, row.y, 2.0, row.height),
                if view.focused { PINK } else { theme::DIM },
            );
        } else if hovered {
            painter.fill_rect(*row, HOVER.with_alpha(150));
            painter.fill_rect(Rect::new(row.x, row.y, 1.0, row.height), CYAN);
        }
        let label = Rect::new(row.x + 26.0, row.y, row.width - 34.0, row.height);
        match entry {
            Some(entry) => {
                let jewel = Rect::new(row.x + 11.0, (row.center().y - 3.0).round(), 6.0, 6.0);
                match entry.jewel {
                    Some(color) => painter.fill_rect(jewel, color),
                    None => painter.stroke_rect(jewel, 1.0, theme::DIM),
                }
                painter.label(
                    entry.name,
                    label,
                    &theme::chrome(11.5),
                    if selected { TEXT } else { MUTED },
                    Align::Start,
                );
            }
            None => {
                let color = if selected || hovered { CYAN } else { MUTED };
                painter.label(
                    "+ New network",
                    label,
                    &theme::chrome(11.5),
                    color,
                    Align::Start,
                );
            }
        }
    }
    painter.restore();
    // The form.
    let form = layout.form;
    painter.save_clip(form);
    for (index, rect) in &layout.placed {
        if rect.bottom() < form.y || rect.y > form.bottom() {
            continue;
        }
        match &view.entries[*index] {
            Entry::Section(name) => {
                painter.label(
                    name,
                    Rect::new(rect.x, rect.y + 12.0, rect.width, 16.0),
                    &theme::chrome(9.5),
                    MUTED,
                    Align::Start,
                );
                let width = painter.text.width(name, &theme::chrome(9.5));
                painter.fill_rect(
                    Rect::new(rect.x + width + 10.0, rect.y + 20.0, rect.width, 1.0),
                    RULE,
                );
            }
            Entry::Line(line) => {
                painter.label(
                    line.label,
                    Rect::new(rect.x, rect.y, LABEL_WIDTH - 12.0, rect.height),
                    &theme::body(12.5),
                    MUTED,
                    Align::Start,
                );
                for (field, control) in line
                    .controls
                    .iter()
                    .map(|(field, control, _)| (*field, control))
                {
                    let Some((_, area)) = layout.controls.iter().find(|(f, _)| *f == field) else {
                        continue;
                    };
                    let focused = view.focus == Some(field);
                    let lit = focused || view.hover == Some(Hit::Field(field));
                    match control {
                        Control::Text {
                            value,
                            masked,
                            placeholder,
                        } => {
                            let editor = if focused {
                                Some(&mut *view.editor)
                            } else {
                                None
                            };
                            widgets::text_field(
                                painter,
                                *area,
                                value,
                                editor,
                                *masked,
                                placeholder,
                            );
                        }
                        Control::Toggle { on, text } => {
                            widgets::toggle(painter, *area, *on, text, focused)
                        }
                        Control::Choice(value) => widgets::choice(painter, *area, value, focused),
                        Control::Button { text, caution } => {
                            widgets::button(painter, *area, text, lit, *caution, true)
                        }
                    }
                }
            }
        }
    }
    painter.restore();
    if layout.max_scroll > 0.0 {
        // Where the form is scrolled to: a thin thumb at its right.
        let track = form.height - 16.0;
        let thumb = (track * form.height / (form.height + layout.max_scroll)).max(24.0);
        let y = form.y + 8.0 + (track - thumb) * layout.scroll / layout.max_scroll;
        painter.fill_rect(Rect::new(form.right() + 6.0, y, 2.0, thumb), theme::DIM);
    }
    // The actions and the status line.
    let bar_rule = panel.bottom() - ACTIONS;
    painter.fill_rect(
        Rect::new(panel.x + 12.0, bar_rule, panel.width - 24.0, 1.0),
        RULE,
    );
    for (action, rect) in &layout.actions {
        let lit = view.hover == Some(Hit::Action(*action));
        let (text, caution) = match action {
            Action::Connect => (view.connect, false),
            Action::Save => ("Save  Ctrl+S", false),
            Action::Duplicate => ("Duplicate", false),
            Action::Delete => ("Delete", true),
            Action::Close => ("Close  Esc", false),
        };
        let enabled =
            !matches!(action, Action::Delete | Action::Duplicate) || view.selected.is_some();
        widgets::button(painter, *rect, text, lit, caution, enabled);
    }
    let left = layout
        .actions
        .last()
        .map_or(panel.right(), |(_, rect)| rect.x);
    let status = Rect::new(
        panel.x + 24.0,
        bar_rule + 12.0,
        (left - panel.x - 36.0).max(0.0),
        28.0,
    );
    let (text, problem) = view.message.unwrap_or((
        "Tab moves between fields; Enter and Space press; arrows change choices.",
        false,
    ));
    painter.label(
        text,
        status,
        &theme::body(12.0),
        if problem { PINK } else { DISABLED },
        Align::Start,
    );
    painter.restore();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::networks::{AutoJoin, LoginMethod, SavedServer};
    use crate::render::{Canvas, TextSystem};

    fn network() -> SavedNetwork {
        let mut network = SavedNetwork {
            name: String::from("Libera.Chat"),
            servers: vec![SavedServer::new("irc.libera.chat", 6697, true)],
            autojoin: vec![AutoJoin {
                channel: String::from("#rust"),
                key: String::from("sesame"),
            }],
            ..SavedNetwork::default()
        };
        network.login.method = LoginMethod::SaslScramSha256;
        network.login.password = String::from("hunter2");
        network
    }

    #[test]
    fn the_form_follows_the_login_method() {
        let mut network = network();
        let fields = focus_order(&entries(&network, &[None], false));
        assert!(
            fields.contains(&Field::Account)
                && fields.contains(&Field::Password)
                && fields.contains(&Field::Require)
        );
        network.login.method = LoginMethod::SaslExternal;
        let fields = focus_order(&entries(&network, &[None], false));
        assert!(!fields.contains(&Field::Password) && fields.contains(&Field::Require));
        network.login.method = LoginMethod::NickServ;
        let fields = focus_order(&entries(&network, &[Some([1; 32])], false));
        assert!(fields.contains(&Field::Password) && !fields.contains(&Field::Require));
        assert!(
            fields.contains(&Field::ServerForgetPin(0)),
            "a pinned server offers to forget it"
        );
    }

    #[test]
    fn passwords_and_keys_stay_masked_until_shown() {
        let network = network();
        let masked = |entries: &[Entry], field: Field| {
            entries.iter().any(|entry| match entry {
                Entry::Line(line) => line.controls.iter().any(|(f, control, _)| {
                    *f == field && matches!(control, Control::Text { masked: true, .. })
                }),
                Entry::Section(_) => false,
            })
        };
        let hidden = entries(&network, &[None], false);
        assert!(masked(&hidden, Field::Password) && masked(&hidden, Field::ChannelKey(0)));
        let shown = entries(&network, &[None], true);
        assert!(!masked(&shown, Field::Password));
    }

    #[test]
    fn layout_hits_controls_lists_and_actions_and_scrolls_to_reveal() {
        let network = network();
        let entries = entries(&network, &[None], false);
        let area = Rect::new(0.0, 26.0, 1100.0, 600.0);
        let layout = ManagerLayout::new(area, 2, &entries, 0.0);
        let name = layout
            .controls
            .iter()
            .find(|(f, _)| *f == Field::Name)
            .unwrap()
            .1;
        assert_eq!(layout.hit(name.center()), Some(Hit::Field(Field::Name)));
        assert_eq!(
            layout.hit(layout.rows[1].center()),
            Some(Hit::Network(Some(1)))
        );
        assert_eq!(
            layout.hit(layout.rows[2].center()),
            Some(Hit::Network(None)),
            "the last row is the new network"
        );
        let save = layout
            .actions
            .iter()
            .find(|(a, _)| *a == Action::Save)
            .unwrap()
            .1;
        assert_eq!(layout.hit(save.center()), Some(Hit::Action(Action::Save)));
        assert!(
            layout.max_scroll > 0.0,
            "a 600-pixel window can't show the whole form"
        );
        let scroll = layout.reveal(Field::PartMessage);
        let revealed = ManagerLayout::new(area, 2, &entries, scroll);
        let part = revealed
            .controls
            .iter()
            .find(|(f, _)| *f == Field::PartMessage)
            .unwrap()
            .1;
        assert!(part.y >= revealed.form.y && part.bottom() <= revealed.form.bottom());
    }

    #[test]
    fn paints_inside_its_area() {
        let network = network();
        let entries = entries(&network, &[None], false);
        let area = Rect::new(0.0, 20.0, 900.0, 600.0);
        let layout = ManagerLayout::new(area, 1, &entries, 0.0);
        let mut canvas = Canvas::new(900, 640);
        let mut text = TextSystem::new();
        let mut painter = Painter::new(&mut canvas, &mut text, 1.0);
        let mut editor = LineEditor::default();
        let list = [ListEntry {
            name: "Libera.Chat",
            jewel: Some(CYAN),
        }];
        let mut view = ManagerView {
            networks: &list,
            selected: Some(0),
            title: "Libera.Chat",
            entries: &entries,
            focus: Some(Field::Name),
            editor: &mut editor,
            hover: None,
            message: None,
            dirty: true,
            connect: "Connect",
            focused: true,
        };
        paint_manager(&mut painter, area, &layout, &mut view);
        assert_eq!(canvas.pixel(0, 19), 0, "nothing above the area");
        assert_eq!(canvas.pixel(0, 25), CANVAS.pixel());
    }
}

#[cfg(test)]
mod snapshots {
    use super::*;
    use crate::config::networks::{AutoJoin, LoginMethod, SavedServer};
    use crate::render::{Canvas, TextSystem};

    /// `RUBYCHAT_SNAPSHOTS=DIR cargo test --lib manager_snapshots -- --ignored`
    /// writes the editor at 1× and 2×.
    #[test]
    #[ignore = "writes snapshot images; set RUBYCHAT_SNAPSHOTS"]
    fn manager_snapshots() {
        let Some(dir) = std::env::var_os("RUBYCHAT_SNAPSHOTS") else {
            return;
        };
        let mut network = SavedNetwork {
            name: String::from("Libera.Chat"),
            connect_at_startup: true,
            servers: vec![
                SavedServer::new("irc.libera.chat", 6697, true),
                SavedServer::new("irc.eu.libera.chat", 6697, true),
            ],
            nick: String::from("ruby"),
            alt_nicks: vec![String::from("ruby_")],
            autojoin: vec![
                AutoJoin {
                    channel: String::from("#rust"),
                    key: String::new(),
                },
                AutoJoin {
                    channel: String::from("#secret"),
                    key: String::from("sesame"),
                },
            ],
            ..SavedNetwork::default()
        };
        network.login.method = LoginMethod::SaslScramSha256;
        network.login.account = String::from("ruby");
        network.login.password = String::from("hunter2");
        network.login.require = true;
        let entries = entries(&network, &[None, Some([0x73; 32])], false);
        let list = [
            ListEntry {
                name: "Libera.Chat",
                jewel: Some(CYAN),
            },
            ListEntry {
                name: "OFTC",
                jewel: None,
            },
            ListEntry {
                name: "hackint",
                jewel: Some(theme::AMBER),
            },
        ];
        let mut text = TextSystem::new();
        for scale in [1.0f32, 2.0] {
            let (width, height) = (1100.0, 720.0);
            let mut canvas = Canvas::new((width * scale) as u32, (height * scale) as u32);
            let mut painter = Painter::new(&mut canvas, &mut text, scale);
            let area = Rect::new(0.0, 26.0, width, height - 26.0);
            let layout = ManagerLayout::new(area, list.len(), &entries, 0.0);
            let mut editor = LineEditor::default();
            editor.insert("irc.libera.chat");
            let mut view = ManagerView {
                networks: &list,
                selected: Some(0),
                title: "Libera.Chat",
                entries: &entries,
                focus: Some(Field::ServerHost(0)),
                editor: &mut editor,
                hover: Some(Hit::Action(Action::Save)),
                message: Some(("Saved.", false)),
                dirty: true,
                connect: "Disconnect",
                focused: true,
            };
            paint_manager(&mut painter, area, &layout, &mut view);
            let path = std::path::Path::new(&dir).join(format!("manager-{scale}x.png"));
            crate::render::png::write_rgba(
                &path,
                canvas.width(),
                canvas.height(),
                &canvas.to_rgba(),
            )
            .unwrap();
        }
    }
}
