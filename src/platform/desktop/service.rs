//! The desktop thread on the session bus (DESIGN.md §12): the tray icon
//! with its menu, and notifications. It owns the bus connection and sleeps
//! in a poll over the bus socket and the waker for the UI's commands.
//!
//! The item registers with a StatusNotifierWatcher (StatusNotifierItem
//! Specification §4), KDE's name first since most hosts run it, and counts
//! as shown once the watcher reports a host. When the watcher goes, the app
//! is told there's no tray; when one starts, the item registers again
//! (`NameOwnerChanged`, D-Bus Specification "Message Bus Specification").

use std::collections::HashMap;
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mio::{Events, Interest, Poll, Token};

use super::notify::{self, Call, Notices};
use super::tray::{self, Tray};
use super::{Artwork, DesktopCommand, DesktopEvent, NoticeKey};
use crate::platform::dbus::message::NO_REPLY_EXPECTED;
use crate::platform::dbus::{Connection, Kind, Message, Value};

const BUS: Token = Token(1);
/// Calls awaiting replies, at most. The bus answers every call, with an
/// error if need be, so the count only grows while the bus is stuck.
const PENDING: usize = 256;
/// How long the last writes may take as RubyChat quits.
const CLOSING: Duration = Duration::from_secs(1);

/// Watchers by their two names, with the item interface each implies.
const WATCHERS: [(&str, &str); 2] = [
    ("org.kde.StatusNotifierWatcher", tray::KDE),
    ("org.freedesktop.StatusNotifierWatcher", tray::FREEDESKTOP),
];
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// RequestName's "DBUS_NAME_FLAG_DO_NOT_QUEUE".
const DO_NOT_QUEUE: u32 = 0x4;

pub(super) fn run(
    mut poll: Poll,
    commands: mpsc::Receiver<DesktopCommand>,
    mut sink: Box<dyn FnMut(DesktopEvent) + Send>,
    artwork: Artwork,
) {
    let mut connection = match Connection::session() {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("rubychat: {error}; no tray or notifications");
            sink(DesktopEvent::Tray(false));
            return;
        }
    };
    if poll
        .registry()
        .register(connection.source(), BUS, Interest::READABLE)
        .is_err()
    {
        sink(DesktopEvent::Tray(false));
        return;
    }
    let mut service = Service::new(connection, sink, artwork);
    service.start();
    service.ready();
    let mut events = Events::with_capacity(16);
    let mut writable = false;
    let mut closing: Option<Instant> = None;
    while !service.failed {
        let wants = service.bus.wants_write();
        if closing.is_some() && !wants {
            return;
        }
        if wants != writable {
            let interest = if wants {
                Interest::READABLE | Interest::WRITABLE
            } else {
                Interest::READABLE
            };
            let _ = poll
                .registry()
                .reregister(service.bus.source(), BUS, interest);
            writable = wants;
        }
        // Closing, the last writes get a second to go out.
        let timeout = closing.map(|deadline| deadline.saturating_duration_since(Instant::now()));
        if timeout.is_some_and(|left| left.is_zero()) {
            return;
        }
        if let Err(error) = poll.poll(&mut events, timeout) {
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if events.iter().any(|event| event.token() == BUS) {
            service.ready();
        }
        while closing.is_none() {
            match commands.try_recv() {
                Ok(DesktopCommand::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                    service.shutdown();
                    closing = Some(Instant::now() + CLOSING);
                }
                Ok(command) => service.command(command),
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
    }
    // The bus is gone, and with it the tray and notifications; the thread
    // ends, and commands are refused from here on.
    if closing.is_none() {
        service.report(false);
    }
}

/// Calls whose replies matter.
enum Pending {
    /// Whether watcher `n` runs.
    HasWatcher(usize),
    Register(usize),
    /// Whether watcher `n` has a host to show items.
    HostRegistered(usize),
    Capabilities,
    Notify(NoticeKey),
}

struct Service {
    bus: Connection,
    sink: Box<dyn FnMut(DesktopEvent) + Send>,
    tray: Tray,
    notices: Notices,
    pending: HashMap<u32, Pending>,
    item_name: String,
    /// The watcher the item is registered with.
    watcher: Option<usize>,
    /// What the app was last told about the tray.
    reported: Option<bool>,
    failed: bool,
}

impl Service {
    fn new(bus: Connection, sink: Box<dyn FnMut(DesktopEvent) + Send>, artwork: Artwork) -> Self {
        Self {
            bus,
            sink,
            tray: Tray::new(artwork.tray),
            notices: Notices::new(artwork.logo),
            pending: HashMap::new(),
            // "org.freedesktop.StatusNotifierItem-PID-ID" in the
            // specification; KDE's prefix is the one hosts expect.
            item_name: format!("org.kde.StatusNotifierItem-{}-1", std::process::id()),
            watcher: None,
            reported: None,
            failed: false,
        }
    }

    /// Asks for the signals RubyChat follows, takes the item's name, and
    /// looks for a watcher and the notification server.
    fn start(&mut self) {
        let owner_changed = |name: &str| {
            format!(
                "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',\
                 member='NameOwnerChanged',arg0='{name}'"
            )
        };
        let mut rules = vec![
            owner_changed(WATCHERS[0].0),
            owner_changed(WATCHERS[1].0),
            owner_changed(notify::NAME),
            format!(
                "type='signal',sender='{name}',interface='{name}',path='{path}'",
                name = notify::NAME,
                path = notify::PATH
            ),
        ];
        for (watcher, _) in WATCHERS {
            rules.push(format!(
                "type='signal',sender='{watcher}',interface='{watcher}',\
                 member='StatusNotifierHostRegistered'"
            ));
        }
        for rule in rules {
            self.call_quietly(Message::bus_call("AddMatch", vec![Value::Str(rule)]));
        }
        self.call_quietly(Message::bus_call(
            "RequestName",
            vec![Value::Str(self.item_name.clone()), Value::U32(DO_NOT_QUEUE)],
        ));
        self.look_for_watcher(0);
        self.call(capabilities(), Pending::Capabilities);
    }

    fn look_for_watcher(&mut self, index: usize) {
        self.call(
            Message::bus_call("NameHasOwner", vec![Value::str(WATCHERS[index].0)]),
            Pending::HasWatcher(index),
        );
    }

    fn register(&mut self, index: usize) {
        let (watcher, _) = WATCHERS[index];
        self.call(
            Message::call(
                watcher,
                WATCHER_PATH,
                watcher,
                "RegisterStatusNotifierItem",
                vec![Value::Str(self.item_name.clone())],
            ),
            Pending::Register(index),
        );
    }

    /// Writes what waits and handles what arrived; how many messages.
    fn ready(&mut self) -> usize {
        if let Err(error) = self.bus.flush() {
            return self.fail(error);
        }
        match self.bus.receive() {
            Ok(messages) => {
                for message in &messages {
                    self.handle(message);
                }
                messages.len()
            }
            Err(error) => self.fail(error),
        }
    }

    fn fail(&mut self, error: String) -> usize {
        if !self.failed {
            eprintln!("rubychat: {error}; no tray or notifications from here on");
        }
        self.failed = true;
        0
    }

    fn command(&mut self, command: DesktopCommand) {
        match command {
            DesktopCommand::Tray(state) => {
                for signal in self.tray.update(state) {
                    self.send(signal);
                }
            }
            DesktopCommand::Notify(notice) => {
                for call in self.notices.add(notice) {
                    self.place(call);
                }
            }
            DesktopCommand::Withdraw(key) => {
                if let Some(call) = self.notices.withdraw(&key) {
                    self.place(call);
                }
            }
            DesktopCommand::Shutdown => {}
        }
    }

    /// Withdraws the notifications, whose clicks would find no one.
    fn shutdown(&mut self) {
        for call in self.notices.withdraw_all() {
            self.place(call);
        }
    }

    fn handle(&mut self, message: &Message) {
        match message.kind {
            Kind::Return | Kind::Error => {
                if let Some(pending) = message
                    .reply_serial
                    .and_then(|serial| self.pending.remove(&serial))
                {
                    self.answered(pending, message);
                }
            }
            Kind::Call => {
                let handled = self.tray.handle(message);
                if let Some(reply) = handled.reply {
                    self.send(reply);
                }
                if let Some(event) = handled.event {
                    (self.sink)(event);
                }
            }
            Kind::Signal => self.signalled(message),
            Kind::Unknown(_) => {}
        }
    }

    fn answered(&mut self, pending: Pending, reply: &Message) {
        let ok = reply.kind == Kind::Return;
        let first = reply.body.first().map(Value::unwrap_variant);
        match pending {
            Pending::HasWatcher(index) => {
                if ok && first.and_then(Value::as_bool) == Some(true) {
                    self.register(index);
                } else if index + 1 < WATCHERS.len() {
                    self.look_for_watcher(index + 1);
                } else {
                    self.report(false);
                }
            }
            Pending::Register(index) if ok => {
                let (watcher, interface) = WATCHERS[index];
                self.watcher = Some(index);
                self.tray.interface = interface;
                self.call(
                    Message::call(
                        watcher,
                        WATCHER_PATH,
                        "org.freedesktop.DBus.Properties",
                        "Get",
                        vec![
                            Value::str(watcher),
                            Value::str("IsStatusNotifierHostRegistered"),
                        ],
                    ),
                    Pending::HostRegistered(index),
                );
            }
            Pending::Register(_) => self.report(false),
            Pending::HostRegistered(index) => {
                // A watcher without the property is taken to come with
                // its host.
                let host = !ok || first.and_then(Value::as_bool) != Some(false);
                if self.watcher == Some(index) {
                    self.report(host);
                }
            }
            Pending::Capabilities => {
                if ok && let Some(capabilities) = first.and_then(Value::as_array) {
                    self.notices.markup = capabilities
                        .iter()
                        .any(|capability| capability.as_str() == Some("body-markup"));
                }
            }
            Pending::Notify(key) => match (ok, first.and_then(Value::as_u32)) {
                (true, Some(id)) => {
                    if let Some(call) = self.notices.shown(&key, id) {
                        self.place(call);
                    }
                }
                _ => self.notices.failed(&key),
            },
        }
    }

    fn signalled(&mut self, message: &Message) {
        let interface = message.interface.as_deref().unwrap_or("");
        let member = message.member.as_deref().unwrap_or("");
        let id = message.body.first().and_then(Value::as_u32);
        match (interface, member) {
            ("org.freedesktop.DBus", "NameOwnerChanged") => {
                let (Some(name), Some(owner)) = (message.arg_str(0), message.arg_str(2)) else {
                    return;
                };
                if let Some(index) = WATCHERS.iter().position(|(watcher, _)| *watcher == name) {
                    if !owner.is_empty() {
                        // A watcher started, perhaps as its panel restarted.
                        if self.watcher.is_none_or(|ours| ours == index) {
                            self.register(index);
                        }
                    } else if self.watcher == Some(index) {
                        self.watcher = None;
                        self.report(false);
                        self.look_for_watcher(0);
                    }
                } else if name == notify::NAME {
                    self.notices.reset();
                    if !owner.is_empty() {
                        self.call(capabilities(), Pending::Capabilities);
                    }
                }
            }
            (_, "StatusNotifierHostRegistered") => {
                if self
                    .watcher
                    .is_some_and(|index| WATCHERS[index].0 == interface)
                {
                    self.report(true);
                }
            }
            (notify::NAME, "ActionInvoked") => {
                if let Some((key, token)) = id.and_then(|id| self.notices.invoked(id)) {
                    (self.sink)(DesktopEvent::Open(key, token));
                }
            }
            (notify::NAME, "ActivationToken") => {
                if let (Some(id), Some(token)) = (id, message.arg_str(1)) {
                    self.notices.token(id, token.to_string());
                }
            }
            (notify::NAME, "NotificationClosed") => {
                if let Some(id) = id {
                    self.notices.closed(id);
                }
            }
            _ => {}
        }
    }

    fn place(&mut self, call: Call) {
        match call {
            Call::Notify(key, message) => {
                if !self.call(message, Pending::Notify(key.clone())) {
                    self.notices.failed(&key);
                }
            }
            Call::Close(message) => self.call_quietly(message),
        }
    }

    /// Sends a call whose reply `pending` handles; false if it wasn't sent.
    fn call(&mut self, message: Message, pending: Pending) -> bool {
        if self.pending.len() >= PENDING {
            return false;
        }
        match self.bus.send(message) {
            Ok(serial) => {
                self.pending.insert(serial, pending);
                true
            }
            Err(error) => {
                self.fail(error);
                false
            }
        }
    }

    /// Sends a call without asking for its reply.
    fn call_quietly(&mut self, mut message: Message) {
        message.flags |= NO_REPLY_EXPECTED;
        self.send(message);
    }

    fn send(&mut self, message: Message) {
        if let Err(error) = self.bus.send(message) {
            self.fail(error);
        }
    }

    fn report(&mut self, shown: bool) {
        if self.reported != Some(shown) {
            self.reported = Some(shown);
            (self.sink)(DesktopEvent::Tray(shown));
        }
    }
}

fn capabilities() -> Message {
    Message::call(
        notify::NAME,
        notify::PATH,
        notify::NAME,
        "GetCapabilities",
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::super::{Attention, MenuAction, Notice, TrayState};
    use super::*;
    use crate::platform::dbus::testing::FakeBus;

    fn icon(size: u32, _: Attention) -> Vec<u8> {
        vec![200; (size * size * 4) as usize]
    }

    fn logo(size: u32) -> Vec<u8> {
        vec![100; (size * size * 4) as usize]
    }

    struct Harness {
        service: Service,
        bus: FakeBus,
        events: mpsc::Receiver<DesktopEvent>,
    }

    impl Harness {
        fn new() -> Self {
            let (connection, bus) = FakeBus::pair();
            let (sender, events) = mpsc::channel();
            let sink = Box::new(move |event| {
                let _ = sender.send(event);
            });
            Self {
                service: Service::new(connection, sink, Artwork { tray: icon, logo }),
                bus,
                events,
            }
        }

        /// Handles at least one message from the bus.
        fn pump(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.service.ready() == 0 {
                assert!(!self.service.failed, "the connection failed");
                assert!(Instant::now() < deadline, "nothing arrived");
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        /// The next call the service made, skipping the signals it sent.
        fn call(&mut self) -> Message {
            loop {
                let message = self.bus.read();
                if message.kind == Kind::Call {
                    return message;
                }
            }
        }

        fn answer(&mut self, call: &Message, body: Vec<Value>) {
            self.bus.write(Message::reply(call, body));
            self.pump();
        }

        fn event(&self) -> DesktopEvent {
            self.events
                .recv_timeout(Duration::from_secs(5))
                .expect("an event")
        }

        /// Starts the service and registers with KDE's watcher, whose host
        /// is there.
        fn registered(&mut self) {
            self.service.start();
            for _ in 0..7 {
                let call = self.call();
                assert!(
                    matches!(call.member.as_deref(), Some("AddMatch" | "RequestName")),
                    "{call:?}"
                );
                assert!(!call.wants_reply());
            }
            let has = self.call();
            assert_eq!(has.arg_str(0), Some("org.kde.StatusNotifierWatcher"));
            let capabilities = self.call();
            assert_eq!(capabilities.member.as_deref(), Some("GetCapabilities"));
            self.answer(&has, vec![Value::Bool(true)]);
            let register = self.call();
            assert_eq!(
                (register.member.as_deref(), register.arg_str(0)),
                (
                    Some("RegisterStatusNotifierItem"),
                    Some(self.service.item_name.as_str())
                )
            );
            self.answer(&register, Vec::new());
            let host = self.call();
            assert_eq!(host.arg_str(1), Some("IsStatusNotifierHostRegistered"));
            self.answer(&host, vec![Value::variant(Value::Bool(true))]);
            assert_eq!(self.event(), DesktopEvent::Tray(true));
            self.answer(
                &capabilities,
                vec![Value::strings(["actions", "body", "body-markup"])],
            );
            assert!(self.service.notices.markup);
        }
    }

    fn signal(member: &str, body: Vec<Value>) -> Message {
        let mut message = Message::signal(notify::PATH, notify::NAME, member, body);
        message.sender = Some(String::from(":1.10"));
        message
    }

    #[test]
    fn a_clicked_notification_opens_its_buffer_with_the_token() {
        let mut harness = Harness::new();
        harness.registered();
        let key = (3, String::from("#rust"));
        harness.service.command(DesktopCommand::Notify(Notice {
            key: key.clone(),
            title: String::from("#rust · Libera.Chat"),
            nick: String::from("ferris"),
            text: String::from("ruby: look"),
        }));
        let notify = harness.call();
        assert_eq!(notify.member.as_deref(), Some("Notify"));
        assert_eq!(notify.arg_str(4), Some("<b>ferris</b>: ruby: look"));
        harness.answer(&notify, vec![Value::U32(12)]);
        harness.bus.write(signal(
            "ActivationToken",
            vec![Value::U32(12), Value::str("xdg-token")],
        ));
        harness.bus.write(signal(
            "ActionInvoked",
            vec![Value::U32(12), Value::str("default")],
        ));
        harness.pump();
        let opened = harness.event();
        assert_eq!(
            opened,
            DesktopEvent::Open(key.clone(), Some(String::from("xdg-token")))
        );
        // Seen: the notification is withdrawn.
        harness.service.command(DesktopCommand::Withdraw(key));
        let close = harness.call();
        assert_eq!(
            (close.member.as_deref(), close.body.as_slice()),
            (Some("CloseNotification"), [Value::U32(12)].as_slice())
        );
    }

    #[test]
    fn hosts_read_the_item_and_its_menu_clicks_reach_the_app() {
        let mut harness = Harness::new();
        harness.registered();
        let mut get_all = Message::call(
            ":1.42",
            tray::ITEM_PATH,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            vec![Value::str(tray::KDE)],
        );
        get_all.sender = Some(String::from(":1.9"));
        harness.bus.write(get_all);
        harness.pump();
        let reply = harness.bus.read();
        assert_eq!(reply.kind, Kind::Return);
        assert!(reply.body[0].as_array().unwrap().len() > 10);
        let mut click = Message::call(
            ":1.42",
            tray::MENU_PATH,
            "com.canonical.dbusmenu",
            "Event",
            vec![
                Value::I32(1),
                Value::str("clicked"),
                Value::variant(Value::I32(0)),
                Value::U32(0),
            ],
        );
        click.sender = Some(String::from(":1.9"));
        harness.bus.write(click);
        harness.pump();
        assert_eq!(harness.event(), DesktopEvent::Menu(MenuAction::ShowHide));
    }

    #[test]
    fn a_restarted_panel_gets_the_item_again() {
        let mut harness = Harness::new();
        harness.registered();
        let owner_changed = |old: &str, new: &str| {
            Message::signal(
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameOwnerChanged",
                vec![
                    Value::str("org.kde.StatusNotifierWatcher"),
                    Value::str(old),
                    Value::str(new),
                ],
            )
        };
        harness.bus.write(owner_changed(":1.9", ""));
        harness.pump();
        assert_eq!(harness.event(), DesktopEvent::Tray(false));
        // It looks for another watcher at once; there is none yet.
        let has = harness.call();
        harness.answer(&has, vec![Value::Bool(false)]);
        let has = harness.call();
        assert_eq!(
            has.arg_str(0),
            Some("org.freedesktop.StatusNotifierWatcher")
        );
        harness.answer(&has, vec![Value::Bool(false)]);
        harness.bus.write(owner_changed("", ":1.77"));
        harness.pump();
        let register = harness.call();
        assert_eq!(
            register.member.as_deref(),
            Some("RegisterStatusNotifierItem")
        );
        harness.answer(&register, Vec::new());
        let host = harness.call();
        harness.answer(&host, vec![Value::variant(Value::Bool(false))]);
        // No host yet: the tray stays away until one registers.
        assert!(harness.events.try_recv().is_err());
        let mut registered = Message::signal(
            WATCHER_PATH,
            "org.kde.StatusNotifierWatcher",
            "StatusNotifierHostRegistered",
            Vec::new(),
        );
        registered.sender = Some(String::from(":1.77"));
        harness.bus.write(registered);
        harness.pump();
        assert_eq!(harness.event(), DesktopEvent::Tray(true));
    }

    #[test]
    fn tray_state_reaches_hosts_as_signals() {
        let mut harness = Harness::new();
        harness.registered();
        harness.service.command(DesktopCommand::Tray(TrayState {
            attention: Attention::Unread,
            window_shown: false,
            away: false,
            tooltip: String::from("3 unread"),
        }));
        let mut members = Vec::new();
        while members.len() < 2 {
            let message = harness.bus.read();
            assert_eq!(message.kind, Kind::Signal);
            members.push(message.member.unwrap());
        }
        assert_eq!(members, ["NewIcon", "NewToolTip"]);
    }
}

/// The real session bus and tray: registers RubyChat's item with the
/// running panel and shows a notification, for a person to look at
/// (`cargo test --lib live_tray -- --ignored --nocapture`).
#[cfg(test)]
mod live {
    use super::super::{Attention, Desktop, Notice, TrayState};
    use super::*;
    use crate::ui::branding;

    #[test]
    #[ignore]
    fn live_tray() {
        let (sender, events) = mpsc::channel();
        let desktop = Desktop::spawn(
            move |event| {
                let _ = sender.send(event);
            },
            Artwork {
                tray: branding::tray_icon,
                logo: branding::logo_rgba,
            },
        )
        .unwrap();
        let shown = events.recv_timeout(Duration::from_secs(5)).unwrap();
        eprintln!("tray: {shown:?}");
        // `RUBYCHAT_LIVE_ATTENTION=none|unread` shows the other states.
        let attention = match std::env::var("RUBYCHAT_LIVE_ATTENTION").as_deref() {
            Ok("none") => Attention::None,
            Ok("unread") => Attention::Unread,
            _ => Attention::Highlight,
        };
        assert!(desktop.send(DesktopCommand::Tray(TrayState {
            attention,
            window_shown: false,
            away: false,
            tooltip: String::from("1 highlight in #rubychat-test"),
        })));
        assert!(desktop.send(DesktopCommand::Notify(Notice {
            key: (1, String::from("#rubychat-test")),
            title: String::from("#rubychat-test · Libera.Chat"),
            nick: String::from("ferris"),
            text: String::from("ruby: the tray works <b>& escapes</b>"),
        })));
        let seconds = std::env::var("RUBYCHAT_LIVE_SECONDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4);
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match events.recv_timeout(left) {
                Ok(event) => eprintln!("event: {event:?}"),
                Err(_) => break,
            }
        }
        drop(desktop);
    }
}
