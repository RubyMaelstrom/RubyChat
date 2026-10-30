//! The tray icon (DESIGN.md §12.1): a StatusNotifierItem (StatusNotifierItem
//! Specification §3, and KDE's `org.kde.StatusNotifierItem` interface, the
//! name most hosts use) at `/StatusNotifierItem`, and its menu over
//! libdbusmenu's `com.canonical.dbusmenu` interface at `/MenuBar`.
//!
//! The icon is RubyChat's logo with a dot: none, cyan for unread messages,
//! pink for highlights, when the status is also `NeedsAttention`. The
//! normal status is `Active`, not `Passive`, which hosts may hide.

use super::{Attention, DesktopEvent, MenuAction, TrayState};
use crate::platform::dbus::{Message, Value};

pub const ITEM_PATH: &str = "/StatusNotifierItem";
pub const MENU_PATH: &str = "/MenuBar";
/// The item interface by its two names: hosts ask by the name of the
/// watcher the item registered with.
pub const KDE: &str = "org.kde.StatusNotifierItem";
pub const FREEDESKTOP: &str = "org.freedesktop.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const INTROSPECTABLE: &str = "org.freedesktop.DBus.Introspectable";
const PEER: &str = "org.freedesktop.DBus.Peer";

/// Pixmap sizes offered; hosts take the nearest to what they draw
/// (sfwbar draws 16 px).
const SIZES: [u32; 8] = [16, 18, 20, 22, 24, 32, 48, 64];

/// Menu items by ID; 0 is the root.
const SHOW: i32 = 1;
const MARK_READ: i32 = 3;
const AWAY: i32 = 4;
const QUIT: i32 = 6;
const ITEMS: [i32; 6] = [1, 2, 3, 4, 5, 6];

pub struct Tray {
    state: TrayState,
    /// `IconPixmap` values, `a(iiay)`, by attention level.
    icons: [Value; 3],
    /// The menu layout's revision, raised when a label changes.
    revision: u32,
    /// The interface name hosts use, after the watcher's family.
    pub interface: &'static str,
    /// The token a host passed with `ProvideXdgActivationToken`, for the
    /// activation that follows.
    token: Option<String>,
}

/// What handling a call produced.
#[derive(Default)]
pub struct Handled {
    pub reply: Option<Message>,
    pub event: Option<DesktopEvent>,
}

impl Tray {
    pub fn new(icon: fn(u32, Attention) -> Vec<u8>) -> Self {
        let pixmaps = |attention| {
            Value::Array(
                String::from("(iiay)"),
                SIZES
                    .iter()
                    .map(|&size| {
                        // ARGB32 in network byte order (§6 "Icon pixmap").
                        let rgba = icon(size, attention);
                        let argb = rgba
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .flat_map(|&[r, g, b, a]| [a, r, g, b])
                            .collect();
                        Value::Struct(vec![
                            Value::I32(size as i32),
                            Value::I32(size as i32),
                            Value::Bytes(argb),
                        ])
                    })
                    .collect(),
            )
        };
        Self {
            state: TrayState::default(),
            icons: [
                pixmaps(Attention::None),
                pixmaps(Attention::Unread),
                pixmaps(Attention::Highlight),
            ],
            revision: 1,
            interface: KDE,
            token: None,
        }
    }

    fn status(&self) -> &'static str {
        match self.state.attention {
            Attention::Highlight => "NeedsAttention",
            _ => "Active",
        }
    }

    fn icon(&self) -> Value {
        self.icons[self.state.attention as usize].clone()
    }

    /// Takes the new state; the signals that tell hosts what changed.
    pub fn update(&mut self, state: TrayState) -> Vec<Message> {
        let old = std::mem::replace(&mut self.state, state);
        let new = &self.state;
        let mut signals = Vec::new();
        let mut signal = |member: &str, body| {
            signals.push(Message::signal(ITEM_PATH, self.interface, member, body));
        };
        if old.attention != new.attention {
            signal("NewIcon", Vec::new());
            if (old.attention == Attention::Highlight) != (new.attention == Attention::Highlight) {
                signal("NewStatus", vec![Value::str(self.status())]);
            }
        }
        if old.tooltip != new.tooltip {
            signal("NewToolTip", Vec::new());
        }
        let menu_changed = old.window_shown != new.window_shown
            || old.away != new.away
            || (old.attention == Attention::None) != (new.attention == Attention::None);
        if menu_changed {
            self.revision += 1;
            signals.push(Message::signal(
                MENU_PATH,
                MENU,
                "LayoutUpdated",
                vec![Value::U32(self.revision), Value::I32(0)],
            ));
        }
        signals
    }

    /// Answers a call to the item or its menu.
    pub fn handle(&mut self, call: &Message) -> Handled {
        let interface = call.interface.as_deref().unwrap_or("");
        let member = call.member.as_deref().unwrap_or("");
        let path = call.path.as_deref().unwrap_or("");
        let mut handled = Handled::default();
        let reply = |body| Some(Message::reply(call, body));
        handled.reply = match (path, interface, member) {
            (_, PEER | "", "Ping") => reply(Vec::new()),
            (ITEM_PATH | MENU_PATH, INTROSPECTABLE | "", "Introspect") => {
                let xml = if path == ITEM_PATH {
                    item_xml(self.interface)
                } else {
                    menu_xml()
                };
                reply(vec![Value::Str(xml)])
            }
            (ITEM_PATH | MENU_PATH, PROPERTIES, "Get") => {
                let (Some(target), Some(name)) = (call.arg_str(0), call.arg_str(1)) else {
                    return invalid(call);
                };
                match self
                    .properties(path, target)
                    .and_then(|properties| properties.into_iter().find(|(key, _)| *key == name))
                {
                    Some((_, value)) => reply(vec![Value::variant(value)]),
                    None => Some(Message::error(
                        call,
                        "org.freedesktop.DBus.Error.UnknownProperty",
                        &format!("no property {name}"),
                    )),
                }
            }
            (ITEM_PATH | MENU_PATH, PROPERTIES, "GetAll") => {
                let Some(target) = call.arg_str(0) else {
                    return invalid(call);
                };
                let properties = self.properties(path, target).unwrap_or_default();
                reply(vec![Value::dict(properties)])
            }
            (ITEM_PATH | MENU_PATH, PROPERTIES, "Set") => Some(Message::error(
                call,
                "org.freedesktop.DBus.Error.PropertyReadOnly",
                "properties are read-only",
            )),
            (ITEM_PATH, KDE | FREEDESKTOP | "", "Activate" | "SecondaryActivate") => {
                handled.event = Some(DesktopEvent::Activate(self.token.take()));
                reply(Vec::new())
            }
            (ITEM_PATH, KDE | FREEDESKTOP | "", "ProvideXdgActivationToken") => {
                self.token = call.arg_str(0).map(str::to_string);
                reply(Vec::new())
            }
            // The host shows the menu itself; scrolling means nothing here.
            (ITEM_PATH, KDE | FREEDESKTOP | "", "ContextMenu" | "Scroll") => reply(Vec::new()),
            (MENU_PATH, MENU | "", member) => {
                let (answer, event) = self.menu_call(call, member);
                handled.event = event;
                Some(answer)
            }
            _ => Some(Message::error(
                call,
                "org.freedesktop.DBus.Error.UnknownMethod",
                &format!("no method {interface}.{member} at {path}"),
            )),
        };
        if !call.wants_reply() {
            handled.reply = None;
        }
        handled
    }

    /// The properties of the object at `path` on `interface`; `None` for
    /// an interface it doesn't have.
    fn properties(&self, path: &str, interface: &str) -> Option<Vec<(&'static str, Value)>> {
        let empty_pixmaps = || Value::Array(String::from("(iiay)"), Vec::new());
        match (path, interface) {
            (ITEM_PATH, KDE | FREEDESKTOP) => Some(vec![
                ("Category", Value::str("Communications")),
                ("Id", Value::str("rubychat")),
                ("Title", Value::str("RubyChat")),
                ("Status", Value::str(self.status())),
                // KDE's interface declares an INT32, the specification a
                // UINT32; no window is named either way.
                (
                    "WindowId",
                    if interface == KDE {
                        Value::I32(0)
                    } else {
                        Value::U32(0)
                    },
                ),
                ("IconThemePath", Value::str("")),
                ("Menu", Value::Path(String::from(MENU_PATH))),
                ("ItemIsMenu", Value::Bool(false)),
                ("IconName", Value::str("")),
                ("IconPixmap", self.icon()),
                ("OverlayIconName", Value::str("")),
                ("OverlayIconPixmap", empty_pixmaps()),
                ("AttentionIconName", Value::str("")),
                (
                    "AttentionIconPixmap",
                    self.icons[Attention::Highlight as usize].clone(),
                ),
                ("AttentionMovieName", Value::str("")),
                (
                    "ToolTip",
                    Value::Struct(vec![
                        Value::str(""),
                        empty_pixmaps(),
                        Value::str("RubyChat"),
                        Value::str(escape_markup(&self.state.tooltip)),
                    ]),
                ),
            ]),
            (MENU_PATH, MENU) => Some(vec![
                ("Version", Value::U32(3)),
                ("TextDirection", Value::str("ltr")),
                ("Status", Value::str("normal")),
                ("IconThemePath", Value::strings(Vec::<String>::new())),
            ]),
            _ => None,
        }
    }

    /// A menu item's properties, leaving out those at their defaults.
    fn item(&self, id: i32) -> Option<Vec<(&'static str, Value)>> {
        let label = |text: &str| ("label", Value::str(text));
        Some(match id {
            0 => vec![("children-display", Value::str("submenu"))],
            SHOW if self.state.window_shown => vec![label("Hide RubyChat")],
            SHOW => vec![label("Show RubyChat")],
            MARK_READ if self.state.attention == Attention::None => {
                vec![label("Mark all read"), ("enabled", Value::Bool(false))]
            }
            MARK_READ => vec![label("Mark all read")],
            AWAY => vec![
                label("Away"),
                ("toggle-type", Value::str("checkmark")),
                ("toggle-state", Value::I32(i32::from(self.state.away))),
            ],
            QUIT => vec![label("Quit RubyChat")],
            2 | 5 => vec![("type", Value::str("separator"))],
            _ => return None,
        })
    }

    fn filtered(&self, id: i32, names: &[&str]) -> Option<Value> {
        let properties = self.item(id)?;
        Some(Value::dict(properties.into_iter().filter(|(name, _)| {
            names.is_empty() || names.contains(name)
        })))
    }

    /// `(ia{sv}av)` for item `id`, its children down `depth` levels.
    fn layout(&self, id: i32, depth: i32, names: &[&str]) -> Option<Value> {
        let children = if id == 0 && depth != 0 {
            ITEMS
                .iter()
                .filter_map(|&child| self.layout(child, depth.saturating_sub(1), names))
                .map(Value::variant)
                .collect()
        } else {
            Vec::new()
        };
        Some(Value::Struct(vec![
            Value::I32(id),
            self.filtered(id, names)?,
            Value::Array(String::from("v"), children),
        ]))
    }

    fn menu_call(&mut self, call: &Message, member: &str) -> (Message, Option<DesktopEvent>) {
        let names = |value: Option<&Value>| -> Vec<String> {
            value
                .and_then(Value::as_array)
                .unwrap_or_default()
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        };
        let ids = |value: Option<&Value>| -> Vec<i32> {
            value
                .and_then(Value::as_array)
                .unwrap_or_default()
                .iter()
                .filter_map(Value::as_i32)
                .collect()
        };
        let unknown = |id: i32| {
            Message::error(
                call,
                "org.freedesktop.DBus.Error.InvalidArgs",
                &format!("no menu item {id}"),
            )
        };
        match member {
            "GetLayout" => {
                let (Some(parent), Some(depth)) = (
                    call.body.first().and_then(Value::as_i32),
                    call.body.get(1).and_then(Value::as_i32),
                ) else {
                    return (invalid_message(call), None);
                };
                let names = names(call.body.get(2));
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                match self.layout(parent, depth, &names) {
                    Some(layout) => (
                        Message::reply(call, vec![Value::U32(self.revision), layout]),
                        None,
                    ),
                    None => (unknown(parent), None),
                }
            }
            "GetGroupProperties" => {
                let mut ids = ids(call.body.first());
                if ids.is_empty() {
                    ids = std::iter::once(0).chain(ITEMS).collect();
                }
                let names = names(call.body.get(1));
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                let found = ids
                    .into_iter()
                    .filter_map(|id| {
                        Some(Value::Struct(vec![
                            Value::I32(id),
                            self.filtered(id, &names)?,
                        ]))
                    })
                    .collect();
                (
                    Message::reply(call, vec![Value::Array(String::from("(ia{sv})"), found)]),
                    None,
                )
            }
            "GetProperty" => {
                let (Some(id), Some(name)) =
                    (call.body.first().and_then(Value::as_i32), call.arg_str(1))
                else {
                    return (invalid_message(call), None);
                };
                match self
                    .item(id)
                    .and_then(|properties| properties.into_iter().find(|(key, _)| *key == name))
                {
                    Some((_, value)) => (Message::reply(call, vec![Value::variant(value)]), None),
                    None => (unknown(id), None),
                }
            }
            "Event" => {
                let (Some(id), Some(kind)) =
                    (call.body.first().and_then(Value::as_i32), call.arg_str(1))
                else {
                    return (invalid_message(call), None);
                };
                if self.item(id).is_none() {
                    return (unknown(id), None);
                }
                (Message::reply(call, Vec::new()), self.event(id, kind))
            }
            "EventGroup" => {
                let events = call
                    .body
                    .first()
                    .and_then(Value::as_array)
                    .unwrap_or_default();
                let mut errors = Vec::new();
                let mut chosen = None;
                for event in events {
                    let Value::Struct(fields) = event else {
                        continue;
                    };
                    let (Some(id), Some(kind)) = (
                        fields.first().and_then(Value::as_i32),
                        fields.get(1).and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    if self.item(id).is_none() {
                        errors.push(Value::I32(id));
                    } else if let Some(event) = self.event(id, kind) {
                        chosen = Some(event);
                    }
                }
                if !events.is_empty() && errors.len() == events.len() {
                    return (unknown(errors[0].as_i32().unwrap_or(0)), None);
                }
                (
                    Message::reply(call, vec![Value::Array(String::from("i"), errors)]),
                    chosen,
                )
            }
            "AboutToShow" => (Message::reply(call, vec![Value::Bool(false)]), None),
            "AboutToShowGroup" => {
                let errors = ids(call.body.first())
                    .into_iter()
                    .filter(|&id| self.item(id).is_none())
                    .map(Value::I32)
                    .collect();
                (
                    Message::reply(
                        call,
                        vec![
                            Value::Array(String::from("i"), Vec::new()),
                            Value::Array(String::from("i"), errors),
                        ],
                    ),
                    None,
                )
            }
            _ => (
                Message::error(
                    call,
                    "org.freedesktop.DBus.Error.UnknownMethod",
                    &format!("no menu method {member}"),
                ),
                None,
            ),
        }
    }

    /// What clicking item `id` asks of the app.
    fn event(&self, id: i32, kind: &str) -> Option<DesktopEvent> {
        if kind != "clicked" {
            return None;
        }
        let action = match id {
            SHOW => MenuAction::ShowHide,
            MARK_READ => MenuAction::MarkAllRead,
            AWAY => MenuAction::Away,
            QUIT => MenuAction::Quit,
            _ => return None,
        };
        Some(DesktopEvent::Menu(action))
    }
}

fn invalid(call: &Message) -> Handled {
    Handled {
        reply: call.wants_reply().then(|| invalid_message(call)),
        event: None,
    }
}

fn invalid_message(call: &Message) -> Message {
    Message::error(
        call,
        "org.freedesktop.DBus.Error.InvalidArgs",
        "unexpected arguments",
    )
}

/// The tooltip's text may hold markup (§7 "Markup"); RubyChat's has none.
pub fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn item_xml(interface: &str) -> String {
    format!(
        r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="{interface}">
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="WindowId" type="{window_id}" access="read"/>
    <property name="IconThemePath" type="s" access="read"/>
    <property name="Menu" type="o" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconPixmap" type="a(iiay)" access="read"/>
    <property name="OverlayIconName" type="s" access="read"/>
    <property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
    <property name="AttentionIconName" type="s" access="read"/>
    <property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
    <property name="AttentionMovieName" type="s" access="read"/>
    <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
    <method name="ProvideXdgActivationToken"><arg name="token" type="s" direction="in"/></method>
    <method name="ContextMenu"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="Activate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="SecondaryActivate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="Scroll"><arg name="delta" type="i" direction="in"/><arg name="orientation" type="s" direction="in"/></method>
    <signal name="NewTitle"/>
    <signal name="NewIcon"/>
    <signal name="NewAttentionIcon"/>
    <signal name="NewOverlayIcon"/>
    <signal name="NewToolTip"/>
    <signal name="NewStatus"><arg name="status" type="s"/></signal>
  </interface>
{STANDARD_XML}</node>
"#,
        window_id = if interface == KDE { "i" } else { "u" },
    )
}

const STANDARD_XML: &str = r#"  <interface name="org.freedesktop.DBus.Properties">
    <method name="Get"><arg name="interface_name" type="s" direction="in"/><arg name="property_name" type="s" direction="in"/><arg name="value" type="v" direction="out"/></method>
    <method name="GetAll"><arg name="interface_name" type="s" direction="in"/><arg name="props" type="a{sv}" direction="out"/></method>
    <method name="Set"><arg name="interface_name" type="s" direction="in"/><arg name="property_name" type="s" direction="in"/><arg name="value" type="v" direction="in"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect"><arg name="xml_data" type="s" direction="out"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Peer">
    <method name="Ping"/>
  </interface>
"#;

fn menu_xml() -> String {
    format!(
        r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="com.canonical.dbusmenu">
    <property name="Version" type="u" access="read"/>
    <property name="TextDirection" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconThemePath" type="as" access="read"/>
    <method name="GetLayout"><arg type="i" name="parentId" direction="in"/><arg type="i" name="recursionDepth" direction="in"/><arg type="as" name="propertyNames" direction="in"/><arg type="u" name="revision" direction="out"/><arg type="(ia{{sv}}av)" name="layout" direction="out"/></method>
    <method name="GetGroupProperties"><arg type="ai" name="ids" direction="in"/><arg type="as" name="propertyNames" direction="in"/><arg type="a(ia{{sv}})" name="properties" direction="out"/></method>
    <method name="GetProperty"><arg type="i" name="id" direction="in"/><arg type="s" name="name" direction="in"/><arg type="v" name="value" direction="out"/></method>
    <method name="Event"><arg type="i" name="id" direction="in"/><arg type="s" name="eventId" direction="in"/><arg type="v" name="data" direction="in"/><arg type="u" name="timestamp" direction="in"/></method>
    <method name="EventGroup"><arg type="a(isvu)" name="events" direction="in"/><arg type="ai" name="idErrors" direction="out"/></method>
    <method name="AboutToShow"><arg type="i" name="id" direction="in"/><arg type="b" name="needUpdate" direction="out"/></method>
    <method name="AboutToShowGroup"><arg type="ai" name="ids" direction="in"/><arg type="ai" name="updatesNeeded" direction="out"/><arg type="ai" name="idErrors" direction="out"/></method>
    <signal name="ItemsPropertiesUpdated"><arg type="a(ia{{sv}})" name="updatedProps" direction="out"/><arg type="a(ias)" name="removedProps" direction="out"/></signal>
    <signal name="LayoutUpdated"><arg type="u" name="revision" direction="out"/><arg type="i" name="parent" direction="out"/></signal>
    <signal name="ItemActivationRequested"><arg type="i" name="id" direction="out"/><arg type="u" name="timestamp" direction="out"/></signal>
  </interface>
{STANDARD_XML}</node>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::dbus::Kind;

    fn icon(size: u32, attention: Attention) -> Vec<u8> {
        let shade = attention as u8;
        (0..size * size).flat_map(|_| [shade, 2, 3, 255]).collect()
    }

    fn call(path: &str, interface: &str, member: &str, body: Vec<Value>) -> Message {
        let mut message = Message::call(":1.5", path, interface, member, body);
        message.serial = 7;
        message.sender = Some(String::from(":1.9"));
        message
    }

    fn reply(tray: &mut Tray, message: Message) -> Message {
        let handled = tray.handle(&message);
        let reply = handled.reply.expect("a reply");
        // Every reply must survive the wire.
        Message::decode(&reply.encode(1).unwrap()).unwrap()
    }

    #[test]
    fn hosts_read_the_item_by_either_interface_name() {
        let mut tray = Tray::new(icon);
        for interface in [KDE, FREEDESKTOP] {
            let all = reply(
                &mut tray,
                call(ITEM_PATH, PROPERTIES, "GetAll", vec![Value::str(interface)]),
            );
            let properties = all.body[0].as_array().unwrap();
            let get = |name: &str| {
                properties.iter().find_map(|entry| match entry {
                    Value::Entry(key, value) if key.as_str() == Some(name) => {
                        Some(value.unwrap_variant().clone())
                    }
                    _ => None,
                })
            };
            assert_eq!(get("Status"), Some(Value::str("Active")));
            assert_eq!(get("Menu"), Some(Value::Path(String::from(MENU_PATH))));
            let pixmaps = get("IconPixmap").unwrap();
            let pixmaps = pixmaps.as_array().unwrap();
            assert_eq!(pixmaps.len(), SIZES.len());
            let Value::Struct(first) = &pixmaps[0] else {
                panic!("a pixmap is a struct");
            };
            assert_eq!(first[0], Value::I32(16));
            // ARGB in network byte order.
            assert_eq!(
                first[2],
                Value::Bytes((0..256).flat_map(|_| [255, 0, 2, 3]).collect())
            );
        }
        let status = reply(
            &mut tray,
            call(
                ITEM_PATH,
                PROPERTIES,
                "Get",
                vec![Value::str(KDE), Value::str("Id")],
            ),
        );
        assert_eq!(status.body, [Value::variant(Value::str("rubychat"))]);
    }

    #[test]
    fn attention_changes_the_icon_status_and_menu() {
        let mut tray = Tray::new(icon);
        let signals = tray.update(TrayState {
            attention: Attention::Highlight,
            window_shown: true,
            away: false,
            tooltip: String::from("1 highlight"),
        });
        let members: Vec<_> = signals.iter().filter_map(|s| s.member.as_deref()).collect();
        assert_eq!(
            members,
            ["NewIcon", "NewStatus", "NewToolTip", "LayoutUpdated"]
        );
        assert_eq!(signals[1].body, [Value::str("NeedsAttention")]);
        assert!(signals.iter().all(|s| s.encode(1).is_ok()));
        // Unread after a highlight: a new icon and status, the menu as it
        // was.
        let signals = tray.update(TrayState {
            attention: Attention::Unread,
            window_shown: true,
            away: false,
            tooltip: String::from("1 highlight"),
        });
        let members: Vec<_> = signals.iter().filter_map(|s| s.member.as_deref()).collect();
        assert_eq!(members, ["NewIcon", "NewStatus"]);
        assert!(
            tray.update(tray.state.clone()).is_empty(),
            "nothing changed"
        );
    }

    #[test]
    fn the_menu_lays_out_and_clicks() {
        let mut tray = Tray::new(icon);
        let layout = reply(
            &mut tray,
            call(
                MENU_PATH,
                MENU,
                "GetLayout",
                vec![
                    Value::I32(0),
                    Value::I32(-1),
                    Value::strings(Vec::<String>::new()),
                ],
            ),
        );
        let Value::Struct(root) = &layout.body[1] else {
            panic!("a layout is a struct");
        };
        assert_eq!(root[0], Value::I32(0));
        let children = root[2].as_array().unwrap();
        assert_eq!(children.len(), ITEMS.len());
        let label = |child: &Value| -> Option<String> {
            let Value::Struct(fields) = child.unwrap_variant() else {
                return None;
            };
            fields[1].as_array()?.iter().find_map(|entry| match entry {
                Value::Entry(key, value) if key.as_str() == Some("label") => {
                    value.unwrap_variant().as_str().map(str::to_string)
                }
                _ => None,
            })
        };
        assert_eq!(label(&children[0]).as_deref(), Some("Show RubyChat"));
        // Any depth a host sends is taken in stride.
        for depth in [i32::MIN, i32::MAX, 0, 1] {
            reply(
                &mut tray,
                call(
                    MENU_PATH,
                    MENU,
                    "GetLayout",
                    vec![
                        Value::I32(0),
                        Value::I32(depth),
                        Value::strings(Vec::<String>::new()),
                    ],
                ),
            );
        }
        assert_eq!(label(&children[5]).as_deref(), Some("Quit RubyChat"));
        let handled = tray.handle(&call(
            MENU_PATH,
            MENU,
            "Event",
            vec![
                Value::I32(QUIT),
                Value::str("clicked"),
                Value::variant(Value::str("")),
                Value::U32(0),
            ],
        ));
        assert!(matches!(
            handled.event,
            Some(DesktopEvent::Menu(MenuAction::Quit))
        ));
        let group = reply(
            &mut tray,
            call(
                MENU_PATH,
                MENU,
                "EventGroup",
                vec![Value::Array(
                    String::from("(isvu)"),
                    vec![
                        Value::Struct(vec![
                            Value::I32(AWAY),
                            Value::str("clicked"),
                            Value::variant(Value::I32(0)),
                            Value::U32(0),
                        ]),
                        Value::Struct(vec![
                            Value::I32(99),
                            Value::str("clicked"),
                            Value::variant(Value::I32(0)),
                            Value::U32(0),
                        ]),
                    ],
                )],
            ),
        );
        assert_eq!(
            group.body,
            [Value::Array(String::from("i"), vec![Value::I32(99)])]
        );
        let properties = reply(
            &mut tray,
            call(
                MENU_PATH,
                MENU,
                "GetGroupProperties",
                vec![
                    Value::Array(String::from("i"), vec![Value::I32(AWAY)]),
                    Value::strings(["toggle-state"]),
                ],
            ),
        );
        assert_eq!(
            properties.body,
            [Value::Array(
                String::from("(ia{sv})"),
                vec![Value::Struct(vec![
                    Value::I32(AWAY),
                    Value::dict([("toggle-state", Value::I32(0))]),
                ])]
            )]
        );
    }

    #[test]
    fn activation_passes_the_hosts_token() {
        let mut tray = Tray::new(icon);
        tray.handle(&call(
            ITEM_PATH,
            KDE,
            "ProvideXdgActivationToken",
            vec![Value::str("token-1")],
        ));
        let handled = tray.handle(&call(
            ITEM_PATH,
            KDE,
            "Activate",
            vec![Value::I32(10), Value::I32(20)],
        ));
        assert!(matches!(
            handled.event,
            Some(DesktopEvent::Activate(Some(ref token))) if token == "token-1"
        ));
        let handled = tray.handle(&call(
            ITEM_PATH,
            KDE,
            "Activate",
            vec![Value::I32(0), Value::I32(0)],
        ));
        assert!(matches!(handled.event, Some(DesktopEvent::Activate(None))));
        let unknown = reply(&mut tray, call(ITEM_PATH, KDE, "Explode", Vec::new()));
        assert_eq!(unknown.kind, Kind::Error);
        let xml = reply(
            &mut tray,
            call(ITEM_PATH, INTROSPECTABLE, "Introspect", Vec::new()),
        );
        assert!(
            xml.arg_str(0)
                .unwrap()
                .contains("org.kde.StatusNotifierItem")
        );
    }
}
