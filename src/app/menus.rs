//! Context menus: what a right click (or a click on a nick) offers for a
//! sidebar row, a nick, a link or selected text, and doing what was chosen
//! (DESIGN.md §8.4, §8.6, §8.7, §8.10). Most choices run the command a
//! person would type, so menus and commands behave alike.

use crate::model::buffer::BufferKind;
use crate::render::{Point, Rect};
use crate::ui::chat_view::{self, Target};
use crate::ui::context_menu::{MenuGeometry, MenuIcon, MenuItem};

use super::App;

/// What choosing an item does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MenuAction {
    /// A command line, as typed in the shown buffer.
    Command(String),
    Copy(String),
    Follow(Target),
    Query(String),
    MarkRead(usize),
    Close(usize),
    /// Opens the editor on this saved network.
    EditSaved(String),
    /// Opens the editor to save the shown network.
    SaveOpen,
}

pub(super) struct Menu {
    badge: &'static str,
    title: String,
    items: Vec<MenuItem>,
    actions: Vec<MenuAction>,
    pub(super) geometry: MenuGeometry,
    pub(super) highlighted: Option<usize>,
}

impl Menu {
    pub(super) fn paint(&self, painter: &mut crate::render::Painter<'_>) {
        crate::ui::context_menu::paint_context_menu(
            painter,
            &self.geometry,
            &self.items,
            self.highlighted,
            self.badge,
            &self.title,
        );
    }

    /// The next enabled item after (or before) the highlighted one.
    fn step(&self, forward: bool) -> Option<usize> {
        let count = self.items.len();
        let start = self
            .highlighted
            .unwrap_or(if forward { count - 1 } else { 0 });
        (1..=count)
            .map(|n| {
                if forward {
                    (start + n) % count
                } else {
                    (start + count - n) % count
                }
            })
            .find(|&index| self.items[index].enabled)
    }
}

/// Items and their actions, built together.
#[derive(Default)]
struct Builder {
    items: Vec<MenuItem>,
    actions: Vec<MenuAction>,
}

impl Builder {
    fn add(&mut self, item: MenuItem, action: MenuAction) {
        self.items.push(item);
        self.actions.push(action);
    }
}

impl App {
    fn open_menu(&mut self, badge: &'static str, title: String, builder: Builder, anchor: Point) {
        if builder.items.is_empty() {
            return;
        }
        // Without a window (tests), as if in one of the default size.
        let bounds = self
            .regions()
            .map_or(Rect::new(0.0, 26.0, 1100.0, 694.0), |regions| regions.body);
        let geometry = MenuGeometry::new(anchor, bounds, &builder.items);
        self.close_menu();
        self.menu = Some(Menu {
            badge,
            title,
            items: builder.items,
            actions: builder.actions,
            geometry,
            highlighted: None,
        });
        self.mark_under_menu();
    }

    pub(super) fn close_menu(&mut self) {
        if self.menu.is_some() {
            self.mark_under_menu();
            self.menu = None;
        }
    }

    /// The menu changed: what lies under it repaints, then the menu over it.
    fn mark_under_menu(&mut self) {
        if let Some(menu) = &self.menu {
            let reach = menu.geometry.damage();
            self.mark_under(reach);
        }
    }

    /// Pointer motion while a menu is open: it highlights the item under it.
    pub(super) fn menu_pointer_moved(&mut self, point: Point) {
        let Some(menu) = &mut self.menu else {
            return;
        };
        let highlighted = menu
            .geometry
            .item_at(point)
            .filter(|&index| menu.items[index].enabled);
        if highlighted != menu.highlighted {
            menu.highlighted = highlighted;
            self.mark_under_menu();
        }
    }

    /// A press while a menu is open: an item does its action; anywhere else
    /// closes the menu. Returns whether the press was used up.
    pub(super) fn menu_pointer_pressed(&mut self, point: Point, right: bool) -> bool {
        let Some(menu) = &self.menu else {
            return false;
        };
        if menu.geometry.panel.contains(point) {
            if let Some(index) = menu.geometry.item_at(point)
                && menu.items[index].enabled
            {
                self.choose(index);
            }
            return true;
        }
        self.close_menu();
        // A right press elsewhere opens that place's menu instead.
        !right
    }

    /// Keys while a menu is open; returns whether the key was used.
    pub(super) fn menu_key(&mut self, key: &winit::keyboard::Key) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(menu) = &mut self.menu else {
            return false;
        };
        match key {
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) => {
                menu.highlighted = menu.step(matches!(key, Key::Named(NamedKey::ArrowDown)));
                self.mark_under_menu();
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(index) = menu.highlighted {
                    self.choose(index);
                } else {
                    self.close_menu();
                }
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.close_menu();
                true
            }
            _ => {
                self.close_menu();
                false
            }
        }
    }

    fn choose(&mut self, index: usize) {
        let Some(action) = self
            .menu
            .as_ref()
            .and_then(|menu| menu.actions.get(index).cloned())
        else {
            return;
        };
        self.close_menu();
        match action {
            MenuAction::Command(line) => self.run_command(&line),
            MenuAction::Copy(text) => self.put_on_clipboard(text),
            MenuAction::Follow(target) => self.follow(target),
            MenuAction::Query(nick) => self.open_query(&nick),
            MenuAction::MarkRead(index) => {
                if let Some(buffer) = self
                    .networks
                    .get_mut(self.active)
                    .and_then(|n| n.state.buffers.get_mut(index))
                {
                    buffer.mark_read();
                    self.mark_sidebar();
                }
            }
            MenuAction::Close(index) => self.close_buffer(index),
            MenuAction::EditSaved(name) => {
                if let Some(index) = self
                    .saved
                    .list
                    .iter()
                    .position(|network| network.name == name)
                {
                    self.open_manager(Some(index));
                }
            }
            MenuAction::SaveOpen => self.save_open_network(self.active),
        }
    }

    /// The menu for buffer `index`'s sidebar row.
    pub(super) fn open_buffer_menu(&mut self, index: usize, anchor: Point) {
        let Some(network) = self.networks.get(self.active) else {
            return;
        };
        let Some(buffer) = network.state.buffers.get(index) else {
            return;
        };
        let mut menu = Builder::default();
        let name = buffer.name.clone();
        let (badge, title) = match buffer.kind {
            BufferKind::Server => {
                if network.connected() {
                    let item = MenuItem::new("Disconnect", MenuIcon::Leave).enabled(!network.demo);
                    menu.add(item, MenuAction::Command(String::from("/disconnect")));
                } else {
                    let item = MenuItem::new("Reconnect", MenuIcon::Reload).enabled(!network.demo);
                    menu.add(item, MenuAction::Command(String::from("/reconnect")));
                }
                let address = format!("{}:{}", network.config.host, network.config.port);
                menu.add(
                    MenuItem::new("Copy server address", MenuIcon::Copy),
                    MenuAction::Copy(address),
                );
                match &network.saved {
                    Some(name) => menu.add(
                        MenuItem::new("Edit network", MenuIcon::Command),
                        MenuAction::EditSaved(name.clone()),
                    ),
                    None if !network.demo => menu.add(
                        MenuItem::new("Save network", MenuIcon::Check),
                        MenuAction::SaveOpen,
                    ),
                    None => {}
                }
                let close = MenuItem::new("Close network", MenuIcon::Leave)
                    .caution()
                    .shortcut("Ctrl+W");
                menu.add(close, MenuAction::Close(0));
                ("SERVER", network.title.clone())
            }
            BufferKind::Channel => {
                if buffer.unread > 0 {
                    menu.add(
                        MenuItem::new("Mark as read", MenuIcon::Check),
                        MenuAction::MarkRead(index),
                    );
                }
                menu.add(
                    MenuItem::new("Copy channel name", MenuIcon::Copy),
                    MenuAction::Copy(name.clone()),
                );
                if buffer.joined {
                    let part = MenuItem::new(format!("Leave {name}"), MenuIcon::Leave).caution();
                    menu.add(part, MenuAction::Command(format!("/part {name}")));
                } else {
                    menu.add(
                        MenuItem::new(format!("Join {name}"), MenuIcon::Command),
                        MenuAction::Command(format!("/join {name}")),
                    );
                    menu.add(
                        MenuItem::new("Close", MenuIcon::Leave).caution(),
                        MenuAction::Close(index),
                    );
                }
                ("CHANNEL", name)
            }
            BufferKind::Query => {
                if buffer.unread > 0 {
                    menu.add(
                        MenuItem::new("Mark as read", MenuIcon::Check),
                        MenuAction::MarkRead(index),
                    );
                }
                menu.add(
                    MenuItem::new("Who is this?", MenuIcon::Info),
                    MenuAction::Command(format!("/whois {name}")),
                );
                menu.add(
                    MenuItem::new("Copy nick", MenuIcon::Copy),
                    MenuAction::Copy(name.clone()),
                );
                menu.add(
                    MenuItem::new("Close", MenuIcon::Leave).caution(),
                    MenuAction::Close(index),
                );
                ("QUERY", name)
            }
        };
        self.open_menu(badge, title, menu, anchor);
    }

    /// The menu for a nick, from the nick list or the chat view.
    pub(super) fn open_nick_menu(&mut self, nick: &str, anchor: Point) {
        let Some(network) = self.networks.get_mut(self.active) else {
            return;
        };
        let mut menu = Builder::default();
        let me = network.state.is_me(nick);
        if !me {
            menu.add(
                MenuItem::new("Open a query", MenuIcon::Chat),
                MenuAction::Query(nick.to_string()),
            );
        }
        menu.add(
            MenuItem::new("Who is this?", MenuIcon::Info),
            MenuAction::Command(format!("/whois {nick}")),
        );
        menu.add(
            MenuItem::new("Copy nick", MenuIcon::Copy),
            MenuAction::Copy(nick.to_string()),
        );
        let state = &network.state;
        let buffer = &state.buffers[network.active];
        if buffer.kind == BufferKind::Channel && buffer.joined {
            let isupport = &state.isupport;
            let prefixes = |who: &str| {
                buffer
                    .members
                    .get(who, isupport)
                    .map(|m| m.prefixes.clone())
                    .unwrap_or_default()
            };
            let (theirs, mine) = (prefixes(nick), prefixes(&state.nick));
            let op = mine.contains(['~', '&', '@']);
            let halfop = op || mine.contains('%');
            let (verb, command) = if theirs.contains('@') {
                ("Take operator", "deop")
            } else {
                ("Make operator", "op")
            };
            menu.add(
                MenuItem::new(verb, MenuIcon::Prefix('@'))
                    .separated()
                    .enabled(op),
                MenuAction::Command(format!("/{command} {nick}")),
            );
            let (verb, command) = if theirs.contains('+') {
                ("Take voice", "devoice")
            } else {
                ("Give voice", "voice")
            };
            menu.add(
                MenuItem::new(verb, MenuIcon::Prefix('+')).enabled(halfop),
                MenuAction::Command(format!("/{command} {nick}")),
            );
            if !me {
                let kick = MenuItem::new("Kick from channel", MenuIcon::Leave)
                    .caution()
                    .enabled(halfop);
                menu.add(kick, MenuAction::Command(format!("/kick {nick}")));
            }
        }
        self.open_menu("NICK", nick.to_string(), menu, anchor);
    }

    /// The menu for a link in the chat view.
    pub(super) fn open_link_menu(&mut self, target: Target, anchor: Point) {
        let mut menu = Builder::default();
        let (badge, title) = match &target {
            Target::Url(url) => {
                menu.add(
                    MenuItem::new("Open link", MenuIcon::Link),
                    MenuAction::Follow(target.clone()),
                );
                menu.add(
                    MenuItem::new("Copy link address", MenuIcon::Copy),
                    MenuAction::Copy(url.clone()),
                );
                ("LINK", url.clone())
            }
            Target::Channel(name) => {
                let open = self
                    .networks
                    .get(self.active)
                    .and_then(|n| n.state.find(name))
                    .is_some();
                let verb = if open {
                    format!("Show {name}")
                } else {
                    format!("Join {name}")
                };
                menu.add(
                    MenuItem::new(verb, MenuIcon::Command),
                    MenuAction::Follow(target.clone()),
                );
                menu.add(
                    MenuItem::new("Copy channel name", MenuIcon::Copy),
                    MenuAction::Copy(name.clone()),
                );
                ("CHANNEL", name.clone())
            }
        };
        self.open_menu(badge, title, menu, anchor);
    }

    /// The menu for the chat view's selected text.
    pub(super) fn open_selection_menu(&mut self, anchor: Point) {
        let Some(selection) = self.selection else {
            return;
        };
        let (start, end) = chat_view::ordered(selection.anchor, selection.focus);
        let text = chat_view::selected_text(
            self.networks[self.active].buffer().lines(),
            start,
            end,
            self.smart_filter,
        );
        let count = text.chars().count();
        let mut menu = Builder::default();
        menu.add(
            MenuItem::new("Copy", MenuIcon::Copy).shortcut("Ctrl+C"),
            MenuAction::Copy(text),
        );
        let title = format!("{count} character{}", if count == 1 { "" } else { "s" });
        self.open_menu("TEXT", title, menu, anchor);
    }

    /// What the chosen menu shows, for tests.
    #[cfg(test)]
    pub(super) fn menu_labels(&self) -> Vec<(String, bool)> {
        self.menu.as_ref().map_or_else(Vec::new, |menu| {
            menu.items
                .iter()
                .map(|item| (item.label.clone(), item.enabled))
                .collect()
        })
    }

    #[cfg(test)]
    pub(super) fn choose_labeled(&mut self, label: &str) {
        let index = self
            .menu
            .as_ref()
            .and_then(|menu| menu.items.iter().position(|item| item.label == label));
        if let Some(index) = index {
            self.choose(index);
        }
    }
}
