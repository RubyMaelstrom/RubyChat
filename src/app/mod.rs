//! The application: the winit event loop, the window, and the order work
//! happens in (DESIGN.md §5). The app outlives its window, which is a
//! disposable view of the app's state.
//!
//! Each wake handles input first; network events are drained afterwards
//! under a time budget, so a flood of server traffic never delays typing.
//! Each region repaints only when it changed: a keystroke repaints the input
//! field and nothing else.

mod channels;
mod chrome;
mod commands;
mod demo;
mod find;
mod history;
mod manager;
mod menus;
mod present;
mod shell;
mod startup;
#[cfg(test)]
mod tests;
mod trust;

use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{
    ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::irc::Message;
use crate::model::buffer::{Buffer, BufferKind};
use crate::model::channels::Order;
use crate::model::members::Member;
use crate::model::network::{Applied, NetworkState};
use crate::model::recent::{Recent, RecentServer};
use crate::model::{Line, LineKind};
use crate::net::{ConnectConfig, LinkStatus, NetCommand, NetEvent, NetHandle, NetworkId};
#[cfg(all(unix, not(target_vendor = "apple")))]
use crate::platform::clipboard::Clipboard;
use crate::platform::desktop::{Desktop, DesktopEvent};
use crate::platform::instance::Request;
use crate::platform::time::now_ms;
use crate::render::{Canvas, Damage, Painter, Point, Rect, TextSystem};
use crate::store::{Document, Store, StoreEvent};
use crate::ui::branding::Logo;
use crate::ui::channel_list::{self, TableLayout, TableView};
use crate::ui::chat_view::{self, ChatContext, ChatFrame, ChatView, Found, Target, TextPosition};
use crate::ui::header::{self, HEADER_HEIGHT, HeaderView, thousands};
use crate::ui::input::{LineEditor, paint_input};
use crate::ui::nick_list::{self, NICK_LIST_WIDTH};
use crate::ui::sidebar::{self, SIDEBAR_WIDTH, SidebarLayout, SidebarPart, SidebarView, Status};
use crate::ui::tab_strip::{
    self, STRIP_HEIGHT, StripTab, TabStatus, TabStripGeometry, TabStripState,
};
use crate::ui::theme::{AMBER, CYAN, DISABLED, EDGE, MUTED, PINK, SURFACE};
use crate::ui::welcome::{self, LOGO_SIZE, StartLayout, StartPage, WelcomeInfo};

use chrome::StripUi;
use present::Presenter;

/// The application identifier: the Wayland app ID, the X11 class, and the
/// name of the desktop file.
pub const APP_ID: &str = "rubychat";

const DEFAULT_SIZE: (f64, f64) = (1100.0, 720.0);
const MINIMUM_SIZE: (f64, f64) = (640.0, 400.0);
/// The input bar below the chat view.
const INPUT_AREA: f32 = 52.0;
/// The nick list gives way rather than leave the chat view narrower.
const MIN_CHAT_WIDTH: f32 = 420.0;
/// How long one wake may spend on network events before input gets a turn
/// (DESIGN.md §10).
const NET_BUDGET: Duration = Duration::from_millis(4);
/// A second press within this time and distance is a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(450);
const DOUBLE_CLICK_SLOP: f32 = 4.0;

#[derive(Debug, Default)]
pub struct Options {
    /// Open placeholder networks, to try the interface without connecting.
    pub demo: bool,
    /// Servers to connect to at startup, as quick connect reads them.
    pub connect: Vec<String>,
    /// Run without the network, store and desktop threads, touching
    /// neither the network nor the disk, as tests do.
    pub isolated: bool,
    /// Start in the tray, without a window (`--tray`).
    pub tray: bool,
    /// The launch's activation token, for the first window.
    pub token: Option<String>,
    /// The socket later launches hand over to.
    pub instance: Option<crate::platform::instance::Server>,
}

/// Wakes the event loop from other threads.
#[derive(Debug)]
pub enum AppEvent {
    Wake,
}

/// Wakes the event loop; each thread that needs to has its own.
pub type Wake = Box<dyn Fn() + Send>;

/// Events from another thread. The sender posts a wake only when it flips
/// `pending` from false to true, and the UI clears it before draining, so a
/// burst of events costs one wake.
struct Inbox<T> {
    events: mpsc::Receiver<T>,
    pending: Arc<AtomicBool>,
}

impl<T> Inbox<T> {
    /// A sink for the other thread, and the inbox it fills.
    fn new(wake: Wake) -> (impl FnMut(T) + Send + 'static, Self)
    where
        T: Send + 'static,
    {
        let (sender, events) = mpsc::channel();
        let pending = Arc::new(AtomicBool::new(false));
        let flag = pending.clone();
        let sink = move |event| {
            let _ = sender.send(event);
            if !flag.swap(true, Ordering::AcqRel) {
                wake();
            }
        };
        (sink, Self { events, pending })
    }

    /// The next event; clear the flag with `arm` before the first call.
    fn next(&self) -> Option<T> {
        self.events.try_recv().ok()
    }

    fn arm(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

pub fn run(options: Options) -> Result<(), String> {
    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .map_err(|e| format!("event loop: {e}"))?;
    let proxy = event_loop.create_proxy();
    let waker = move || -> Wake {
        let proxy = proxy.clone();
        Box::new(move || {
            let _ = proxy.send_event(AppEvent::Wake);
        })
    };
    let mut app = App::new(options, waker)?;
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    // What was open, for next time, while it still is, and what was read.
    app.session_changed();
    app.save_session();
    app.read_on_exit();
    // Dropping the handles says QUIT to every server, finishes pending
    // writes and withdraws notifications.
    drop(app.net.take());
    drop(app.store.take());
    drop(app.shell.desktop.take());
    if let Some(path) = std::env::var_os("RUBYCHAT_SNAPSHOT")
        && let Some(view) = &app.view
    {
        let canvas = &view.canvas;
        crate::render::png::write_rgba(
            std::path::Path::new(&path),
            canvas.width(),
            canvas.height(),
            &canvas.to_rgba(),
        )
        .map_err(|e| format!("snapshot: {e}"))?;
    }
    match app.failure.take() {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

/// A network: its tab, its connection and its buffers.
struct Network {
    id: NetworkId,
    title: String,
    config: ConnectConfig,
    status: LinkStatus,
    state: NetworkState,
    /// The buffer shown, by index.
    active: usize,
    lag: Option<Duration>,
    /// A `--demo` placeholder that never connects.
    demo: bool,
    /// The saved network it is, by name.
    saved: Option<String>,
    /// Which of the saved network's servers.
    server: usize,
    /// Lines to run after each registration, as typed in the server buffer.
    commands: Vec<String>,
}

impl Network {
    fn buffer(&self) -> &Buffer {
        &self.state.buffers[self.active]
    }

    fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.state.buffers[self.active]
    }

    /// Adds a line of RubyChat's own to buffer `index`.
    fn push(&mut self, index: usize, kind: LineKind, prefix: &str, text: impl Into<String>) {
        if let Some(buffer) = self.state.buffers.get_mut(index) {
            buffer.push(Line::new(now_ms(), kind, prefix, text));
        }
    }

    /// Highlights and whether anything is unread, across its buffers.
    fn badge(&self) -> (u32, bool) {
        let buffers = &self.state.buffers;
        (
            buffers.iter().map(|b| b.highlights).sum(),
            buffers.iter().any(|b| b.unread > 0),
        )
    }

    /// Its tab; only networks in the background show activity there.
    fn tab(&self, shown: bool) -> StripTab {
        let (highlights, unread) = if shown { (0, false) } else { self.badge() };
        StripTab {
            title: self.title.clone(),
            status: match self.status {
                LinkStatus::Resolving | LinkStatus::Connecting | LinkStatus::Registering => {
                    TabStatus::Connecting
                }
                LinkStatus::Registered { .. } => TabStatus::Connected,
                LinkStatus::Disconnected { .. } => TabStatus::Disconnected,
                LinkStatus::Failed { .. } => TabStatus::Failed,
            },
            highlights,
            unread,
        }
    }

    fn connected(&self) -> bool {
        matches!(
            self.status,
            LinkStatus::Registering | LinkStatus::Registered { .. }
        )
    }

    fn registered(&self) -> bool {
        matches!(self.status, LinkStatus::Registered { .. })
    }

    /// On its way to registering: what the server offers isn't known yet.
    fn registering(&self) -> bool {
        matches!(
            self.status,
            LinkStatus::Resolving | LinkStatus::Connecting | LinkStatus::Registering
        )
    }

    /// The sidebar's status line.
    fn status_line(&self) -> Status {
        // Encryption without a verified identity is shown as such
        // (DESIGN.md §6.2).
        let unverified = self.config.tls
            && self.config.certificate == crate::config::networks::CertificatePolicy::None;
        let tls = match (self.config.tls, unverified) {
            (false, _) => "PLAIN",
            (true, true) => "TLS UNVERIFIED",
            (true, false) => "TLS",
        };
        let (text, color) = match &self.status {
            _ if self.demo => (String::from("DEMO · NEVER CONNECTS"), MUTED),
            LinkStatus::Registered { .. } => {
                let text = match self.lag {
                    Some(lag) => format!("{tls} · {} MS", lag.as_millis()),
                    None => tls.to_string(),
                };
                (
                    text,
                    if self.config.tls && !unverified {
                        CYAN
                    } else {
                        AMBER
                    },
                )
            }
            LinkStatus::Resolving | LinkStatus::Connecting => (String::from("CONNECTING"), AMBER),
            LinkStatus::Registering => (String::from("REGISTERING"), AMBER),
            LinkStatus::Disconnected {
                retry_in: Some(_), ..
            } => (String::from("RECONNECTING"), AMBER),
            LinkStatus::Disconnected { .. } => (String::from("DISCONNECTED"), DISABLED),
            LinkStatus::Failed { .. } => (String::from("FAILED"), PINK),
        };
        Status { text, color }
    }
}

/// What the pointer is over below the strip.
#[derive(Clone, Debug, Default, PartialEq)]
struct Hover {
    sidebar: Option<SidebarPart>,
    /// A member, by its place in the nick list.
    nick: Option<usize>,
    link: Option<(u64, Range<usize>, Target)>,
}

/// Text selected in the chat view: where the press began and where the
/// pointer went.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Selection {
    anchor: TextPosition,
    focus: TextPosition,
    dragging: bool,
}

/// Where the parts of a network's view go.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Regions {
    body: Rect,
    sidebar: Option<Rect>,
    header: Rect,
    chat: Rect,
    input: Rect,
    nicks: Option<Rect>,
}

pub struct App {
    text: TextSystem,
    networks: Vec<Network>,
    next_id: u64,
    active: usize,
    strip: StripUi,
    view: Option<View>,
    modifiers: ModifiersState,
    quit: bool,
    failure: Option<String>,
    trace: bool,
    /// The previous frame's CPU time, shown on the welcome panel.
    last_frame: Option<Duration>,
    frames: u64,
    /// The heartbeat's clock.
    started: Instant,
    net: Option<NetHandle>,
    net_inbox: Inbox<NetEvent>,
    store: Option<Store>,
    store_inbox: Inbox<StoreEvent>,
    desktop_inbox: Inbox<DesktopEvent>,
    /// Requests from later launches.
    instance_inbox: Inbox<Request>,
    /// The tray, notifications and the window's comings and goings.
    shell: shell::Shell,
    /// Backlog requests to the store awaiting lines: their ID, network and
    /// buffer.
    backlogs: std::collections::VecDeque<(u64, NetworkId, String)>,
    next_backlog: u64,
    /// When the server last heard what was read in the buffer in view.
    read_sent: Option<Instant>,
    /// Inbox events wait to be drained after this wake's input.
    inbox_ready: bool,
    /// Wakes the event loop to drain what one wake's budget left.
    wake: Wake,
    /// What `send` would have sent, while isolated.
    #[cfg(test)]
    sent: std::cell::RefCell<Vec<NetCommand>>,
    /// The servers on the start page.
    recent: Recent,
    /// The recent server under the pointer.
    start_hover: Option<usize>,
    logo: Logo,
    /// Quick connect, in the welcome panel.
    prompt: LineEditor,
    prompt_open: bool,
    prompt_message: Option<String>,
    show_sidebar: bool,
    show_nicks: bool,
    /// Hide joins, parts, quits and nick changes of people who haven't
    /// spoken lately (DESIGN.md §8.6); `/filter` toggles it.
    smart_filter: bool,
    /// Ctrl+F's find field, while open.
    find: Option<find::Find>,
    /// The channel list (`/list`), while it shows.
    browser: Option<channels::Browser>,
    sidebar_scroll: f32,
    hover: Hover,
    /// What the chat view last showed, for finding what's under the pointer.
    chat_frame: ChatFrame,
    selection: Option<Selection>,
    completion: Option<commands::Completion>,
    menu: Option<menus::Menu>,
    /// The last left press, for double clicks.
    last_press: Option<(Instant, Point)>,
    /// Text copied in RubyChat: the clipboard where the system has none.
    copied: String,
    /// Scrolling not yet shown: the view moves by whole pixels.
    scroll_remainder: f32,
    /// The system clipboard, on Wayland.
    #[cfg(all(unix, not(target_vendor = "apple")))]
    clipboard: Option<Clipboard>,
    /// Makes a waker for a thread that wakes the event loop.
    waker: Box<dyn Fn() -> Wake>,
    /// Pasted text, read on another thread.
    pasted: (mpsc::Sender<String>, mpsc::Receiver<String>),
    /// `RUBYCHAT_CHECK_CLIPBOARD`: once the window has the keyboard, paste
    /// and then copy, reporting both (diagnostics). True once done.
    check_clipboard: Option<bool>,
    /// Saved networks with their secrets (DESIGN.md §11).
    saved: crate::config::networks::Networks,
    /// Why saved networks didn't load; they are then never saved over.
    saved_error: Option<String>,
    known_hosts: crate::config::known_hosts::KnownHosts,
    /// Hosts to reach over TLS only (IRCv3 STS).
    sts: crate::config::sts::StsPolicies,
    startup: startup::Startup,
    /// Restore and save the session: not for `--demo` or tests.
    keep_session: bool,
    session_dirty: bool,
    /// A certificate waiting for the trust prompt's answer.
    trust: Option<(NetworkId, Box<crate::net::CertificateProblem>)>,
    trust_focus: crate::ui::trust::TrustAction,
    trust_hover: Option<crate::ui::trust::TrustAction>,
    /// The Server Manager's editor, while open.
    manager: Option<manager::Manager>,
    /// The saved network (or button) under the pointer on the start page.
    start_saved_hover: Option<crate::ui::welcome::SavedPart>,
    /// `RUBYCHAT_EXIT_AFTER`: quit cleanly at this time (diagnostics).
    exit_at: Option<Instant>,
}

/// The window and everything that dies with it.
struct View {
    window: Rc<Window>,
    presenter: Presenter,
    canvas: Canvas,
    scale: f32,
    /// Pointer position in logical pixels.
    pointer: Point,
    focused: bool,
    /// When the window last lost the keyboard.
    unfocused_at: Option<Instant>,
    cursor: CursorIcon,
    dirty: Dirty,
}

impl View {
    fn logical_width(&self) -> f32 {
        self.window.inner_size().width as f32 / self.scale
    }

    fn logical_height(&self) -> f32 {
        self.window.inner_size().height as f32 / self.scale
    }
}

/// Which regions need painting before the next presentation.
#[derive(Clone, Copy, Debug, Default)]
struct Dirty {
    strip: bool,
    /// Everything below the strip.
    body: bool,
    sidebar: bool,
    header: bool,
    chat: bool,
    /// The chat view only scrolled: its pixels can move.
    scrolled: bool,
    input: bool,
    nicks: bool,
}

impl Dirty {
    const ALL: Self = Self {
        strip: true,
        body: true,
        sidebar: true,
        header: true,
        chat: true,
        scrolled: false,
        input: true,
        nicks: true,
    };

    fn any(self) -> bool {
        self.strip
            || self.body
            || self.sidebar
            || self.header
            || self.chat
            || self.scrolled
            || self.input
            || self.nicks
    }
}

impl App {
    /// `waker` makes a waker for each thread that wakes the event loop.
    pub fn new(options: Options, waker: impl Fn() -> Wake + 'static) -> Result<Self, String> {
        let (net_sink, net_inbox) = Inbox::new(waker());
        let (store_sink, store_inbox) = Inbox::new(waker());
        let (desktop_sink, desktop_inbox) = Inbox::new(waker());
        let (instance_sink, instance_inbox) = Inbox::new(waker());
        let mut shell = shell::Shell::default();
        let (net, store) = if options.isolated {
            (None, None)
        } else {
            let net = NetHandle::spawn(net_sink).map_err(|e| format!("network thread: {e}"))?;
            let store = Store::spawn(store_sink).map_err(|e| format!("store thread: {e}"))?;
            store.load(Document::Recent);
            let artwork = crate::platform::desktop::Artwork {
                tray: crate::ui::branding::tray_icon,
                logo: crate::ui::branding::logo_rgba,
            };
            match Desktop::spawn(desktop_sink, artwork) {
                Ok(desktop) => shell.desktop = Some(desktop),
                Err(error) => eprintln!("rubychat: no tray or notifications: {error}"),
            }
            (Some(net), Some(store))
        };
        if let Some(server) = options.instance
            && let Err(error) = server.serve(instance_sink)
        {
            eprintln!("rubychat: later launches can't hand over: {error}");
        }
        // Starting in the tray waits to hear whether there is one; with no
        // desktop thread there isn't.
        shell.hidden = options.tray && shell.desktop.is_some();
        shell.token = options.token;
        let mut app = Self {
            text: TextSystem::new(),
            networks: Vec::new(),
            next_id: 1,
            active: 0,
            strip: StripUi::default(),
            view: None,
            modifiers: ModifiersState::empty(),
            quit: false,
            failure: None,
            trace: std::env::var_os("RUBYCHAT_TRACE").is_some(),
            last_frame: None,
            frames: 0,
            started: Instant::now(),
            net,
            net_inbox,
            store,
            store_inbox,
            desktop_inbox,
            instance_inbox,
            shell,
            backlogs: Default::default(),
            next_backlog: 0,
            read_sent: None,
            inbox_ready: false,
            wake: waker(),
            #[cfg(test)]
            sent: Default::default(),
            recent: Recent::default(),
            start_hover: None,
            logo: Logo::default(),
            prompt: LineEditor::default(),
            prompt_open: false,
            prompt_message: None,
            show_sidebar: true,
            show_nicks: true,
            smart_filter: true,
            find: None,
            browser: None,
            sidebar_scroll: 0.0,
            hover: Hover::default(),
            chat_frame: ChatFrame::default(),
            selection: None,
            completion: None,
            menu: None,
            last_press: None,
            copied: String::new(),
            scroll_remainder: 0.0,
            #[cfg(all(unix, not(target_vendor = "apple")))]
            clipboard: None,
            waker: Box::new(waker),
            pasted: mpsc::channel(),
            check_clipboard: std::env::var_os("RUBYCHAT_CHECK_CLIPBOARD").map(|_| false),
            saved: Default::default(),
            saved_error: None,
            known_hosts: Default::default(),
            sts: Default::default(),
            startup: Default::default(),
            keep_session: !options.demo && !options.isolated,
            session_dirty: false,
            trust: None,
            trust_focus: crate::ui::trust::TrustAction::Cancel,
            trust_hover: None,
            manager: None,
            start_saved_hover: None,
            exit_at: std::env::var("RUBYCHAT_EXIT_AFTER")
                .ok()
                .and_then(|seconds| seconds.parse::<f64>().ok())
                .map(|seconds| Instant::now() + Duration::from_secs_f64(seconds.max(0.0))),
        };
        if options.demo {
            app.open_demo();
        }
        app.startup.addresses = options.connect;
        app.begin_startup();
        Ok(app)
    }

    fn add_network(&mut self, title: String, config: ConnectConfig) -> NetworkId {
        let id = NetworkId(self.next_id);
        self.next_id += 1;
        self.networks.push(Network {
            id,
            state: NetworkState::new(config.nick.clone(), title.clone()),
            title,
            config,
            status: LinkStatus::Resolving,
            active: 0,
            lag: None,
            demo: false,
            saved: None,
            server: 0,
            commands: Vec::new(),
        });
        id
    }

    fn network_mut(&mut self, id: NetworkId) -> Option<&mut Network> {
        self.networks.iter_mut().find(|network| network.id == id)
    }

    fn index_of(&self, id: NetworkId) -> Option<usize> {
        self.networks.iter().position(|network| network.id == id)
    }

    fn send(&self, command: NetCommand) {
        match &self.net {
            Some(net) => net.send(command),
            #[cfg(test)]
            None => self.sent.borrow_mut().push(command),
            #[cfg(not(test))]
            None => {}
        }
    }

    fn open_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        // The size it had when it last closed to the tray.
        let (width, height, maximized) =
            self.shell
                .geometry
                .unwrap_or((DEFAULT_SIZE.0, DEFAULT_SIZE.1, false));
        let mut attributes = Window::default_attributes()
            .with_title("RubyChat")
            // The tab strip is the title bar (`chrome`).
            .with_decorations(false)
            .with_inner_size(LogicalSize::new(width, height))
            .with_maximized(maximized)
            .with_min_inner_size(LogicalSize::new(MINIMUM_SIZE.0, MINIMUM_SIZE.1));
        #[cfg(all(unix, not(target_vendor = "apple")))]
        {
            use winit::platform::startup_notify::WindowAttributesExtStartupNotify;
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            attributes = WindowAttributesExtWayland::with_name(attributes, APP_ID, APP_ID);
            attributes = WindowAttributesExtX11::with_name(attributes, APP_ID, APP_ID);
            // A token from a notification, the tray or a launch lets the
            // new window take focus (xdg-activation-v1).
            if let Some(token) = self.shell.token.take() {
                attributes = attributes
                    .with_activation_token(winit::window::ActivationToken::from_raw(token));
            }
        }
        let window = Rc::new(
            event_loop
                .create_window(attributes)
                .map_err(|e| format!("window: {e}"))?,
        );
        window.set_ime_allowed(true);
        // X11 and Windows show this; Wayland takes the icon from the desktop
        // file.
        let (rgba, size) = self.logo.icon(64);
        if let Ok(icon) = winit::window::Icon::from_rgba(rgba, size, size) {
            window.set_window_icon(Some(icon));
        }
        let presenter = Presenter::new(window.clone())?;
        let size = window.inner_size();
        // Interaction state belongs to the window it happened in.
        self.strip = StripUi::default();
        self.strip.reveal_active = true;
        self.hover = Hover::default();
        self.chat_frame = ChatFrame::default();
        self.view = Some(View {
            scale: window.scale_factor() as f32,
            canvas: Canvas::new(size.width, size.height),
            window,
            presenter,
            pointer: Point::new(-1.0, -1.0),
            focused: true,
            unfocused_at: None,
            cursor: CursorIcon::Default,
            dirty: Dirty::ALL,
        });
        self.sync_seen();
        Ok(())
    }

    fn mark(&mut self, change: impl FnOnce(&mut Dirty)) {
        if let Some(view) = &mut self.view {
            change(&mut view.dirty);
        }
    }

    fn mark_strip(&mut self) {
        self.mark(|dirty| dirty.strip = true);
    }

    fn mark_body(&mut self) {
        self.mark(|dirty| dirty.body = true);
    }

    fn mark_sidebar(&mut self) {
        self.mark(|dirty| dirty.sidebar = true);
    }

    fn mark_header(&mut self) {
        self.mark(|dirty| dirty.header = true);
    }

    fn mark_chat(&mut self) {
        self.mark(|dirty| dirty.chat = true);
    }

    fn mark_input(&mut self) {
        self.mark(|dirty| dirty.input = true);
    }

    fn mark_nicks(&mut self) {
        self.mark(|dirty| dirty.nicks = true);
    }

    fn mark_all(&mut self) {
        self.mark(|dirty| *dirty = Dirty::ALL);
    }

    fn update_title(&self) {
        if let Some(view) = &self.view {
            let title = match self.networks.get(self.active) {
                _ if self.manager.is_some() => String::from("Servers — RubyChat"),
                Some(network) if !self.welcome_visible() => match network.buffer().kind {
                    BufferKind::Server => format!("{} — RubyChat", network.title),
                    _ => format!("{} — {} — RubyChat", network.buffer().name, network.title),
                },
                _ => String::from("RubyChat"),
            };
            view.window.set_title(&title);
        }
    }

    /// The welcome panel shows while nothing is open, or over the networks
    /// for quick connect.
    fn welcome_visible(&self) -> bool {
        self.networks.is_empty() || self.prompt_open || self.manager.is_some()
    }

    /// What lies under an overlay (a menu, the trust prompt) repaints, then
    /// the overlay over it.
    pub(super) fn mark_under(&mut self, reach: Rect) {
        if self.welcome_visible() {
            return self.mark_body();
        }
        let Some(regions) = self.regions() else {
            return;
        };
        let over = |rect: Rect| {
            rect.x < reach.right()
                && reach.x < rect.right()
                && rect.y < reach.bottom()
                && reach.y < rect.bottom()
        };
        let (sidebar, header, chat, input, nicks) = (
            regions.sidebar.is_some_and(over),
            over(regions.header),
            over(regions.chat),
            over(regions.input),
            regions.nicks.is_some_and(over),
        );
        self.mark(|dirty| {
            dirty.sidebar |= sidebar;
            dirty.header |= header;
            dirty.chat |= chat;
            dirty.input |= input;
            dirty.nicks |= nicks;
        });
    }

    /// The start page's saved networks, or suggestions when none are saved.
    fn start_saved_rows(&self) -> (Vec<crate::ui::welcome::SavedRow>, bool) {
        use crate::ui::welcome::SavedRow;
        if self.saved.list.is_empty() {
            let rows = crate::config::networks::PRESETS
                .iter()
                .map(|(name, host, port)| SavedRow {
                    name: name.to_string(),
                    address: format!("{host}:{port}"),
                    jewel: None,
                })
                .collect();
            return (rows, true);
        }
        let rows = self
            .saved
            .list
            .iter()
            .zip(self.manager_list())
            .map(|(saved, (_, jewel))| SavedRow {
                name: saved.name.clone(),
                address: saved
                    .servers
                    .first()
                    .map(|server| server.address())
                    .unwrap_or_default(),
                jewel,
            })
            .collect();
        (rows, false)
    }

    /// Network `index` is on screen.
    fn is_shown(&self, index: usize) -> bool {
        index == self.active && !self.welcome_visible()
    }

    /// Marks the buffer on screen as seen while the window has focus, and
    /// every other buffer as not: seen buffers take lines as read, and a
    /// buffer that stops being seen loses its NEW MESSAGES rule, so the
    /// next unseen line places it anew.
    fn sync_seen(&mut self) {
        let focused = self.view.as_ref().is_some_and(|view| view.focused);
        let welcome = self.welcome_visible();
        let mut changed = false;
        let mut seen_now = Vec::new();
        // Buffers seen or left: the server hears what was read.
        let mut read = Vec::new();
        for (n, network) in self.networks.iter_mut().enumerate() {
            let shown = n == self.active && !welcome && focused;
            for (b, buffer) in network.state.buffers.iter_mut().enumerate() {
                let seen = shown && b == network.active;
                if seen && !buffer.seen {
                    changed |= buffer.unread > 0 || buffer.activity != Default::default();
                    buffer.set_seen(true);
                    seen_now.push((network.id.0, buffer.key.clone()));
                    read.push((n, b));
                } else if !seen && buffer.seen {
                    buffer.leave();
                    read.push((n, b));
                }
            }
        }
        seen_now.retain(|key| self.noticed(key));
        self.withdraw_seen(seen_now);
        for (network, buffer) in read {
            self.mark_read_on_server(network, buffer);
        }
        self.first_show();
        if changed {
            self.mark_strip();
            self.mark_sidebar();
        }
    }

    fn activate(&mut self, index: usize) {
        if index < self.networks.len() {
            let changed = index != self.active || self.prompt_open;
            if changed && let Some(network) = self.networks.get_mut(self.active) {
                network.buffer_mut().leave();
            }
            self.active = index;
            self.prompt_open = false;
            self.strip.reveal_active = true;
            if changed {
                self.forget_pointer_state();
                self.sidebar_scroll = 0.0;
                self.mark_all();
            } else {
                self.mark_strip();
            }
            self.sync_seen();
            self.update_title();
            self.session_changed();
        }
    }

    fn cycle(&mut self, step: isize) {
        let count = self.networks.len() as isize;
        if count > 0 {
            self.activate((self.active as isize + step).rem_euclid(count) as usize);
        }
    }

    /// Shows buffer `index` of the active network.
    fn activate_buffer(&mut self, index: usize) {
        if self.welcome_visible() {
            return;
        }
        let Some(network) = self.networks.get_mut(self.active) else {
            return;
        };
        if index >= network.state.buffers.len() {
            return;
        }
        if index != network.active {
            network.buffer_mut().leave();
            network.active = index;
            self.browser = None;
            self.forget_pointer_state();
            // Keep the row in view.
            if let Some(rect) = self.regions().and_then(|regions| regions.sidebar) {
                let network = &self.networks[self.active];
                self.sidebar_scroll =
                    SidebarLayout::new(rect, &network.state.buffers, self.sidebar_scroll)
                        .reveal(index);
            }
            self.mark_body();
            self.session_changed();
        }
        self.sync_seen();
        self.update_title();
    }

    /// The `n`th buffer in sidebar order.
    fn activate_nth_buffer(&mut self, n: usize) {
        if let Some(network) = self.networks.get(self.active)
            && let Some(&index) = sidebar::order(&network.state.buffers).get(n)
        {
            self.activate_buffer(index);
        }
    }

    /// The next or previous buffer in sidebar order.
    fn cycle_buffer(&mut self, step: isize) {
        let Some(network) = self.networks.get(self.active) else {
            return;
        };
        let order = sidebar::order(&network.state.buffers);
        let position = order
            .iter()
            .position(|&index| index == network.active)
            .unwrap_or(0) as isize;
        let next = order[(position + step).rem_euclid(order.len() as isize) as usize];
        self.activate_buffer(next);
    }

    /// Pointer state that belongs to the buffer being left.
    fn forget_pointer_state(&mut self) {
        self.selection = None;
        self.completion = None;
        self.menu = None;
        self.hover = Hover::default();
        self.chat_frame = ChatFrame::default();
    }

    /// The new-tab button and Ctrl+T: quick connect.
    fn open_prompt(&mut self) {
        self.prompt_open = true;
        self.prompt_message = None;
        self.forget_pointer_state();
        self.sync_seen();
        self.mark_all();
        self.update_title();
    }

    fn close_prompt(&mut self) {
        if self.prompt_open && !self.networks.is_empty() {
            self.prompt_open = false;
            self.sync_seen();
            self.mark_all();
            self.update_title();
        }
    }

    fn close_network(&mut self, index: usize) {
        if index >= self.networks.len() {
            return;
        }
        let network = self.networks.remove(index);
        if !network.demo {
            self.send(NetCommand::Close(network.id));
        }
        if self.active > index || self.active >= self.networks.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.strip.hovered = None;
        self.forget_pointer_state();
        self.sync_seen();
        self.mark_all();
        self.update_title();
        self.session_changed();
    }

    /// Closes buffer `index` of the active network, parting a channel you
    /// are in; the server buffer closes the network.
    fn close_buffer(&mut self, index: usize) {
        if index == 0 {
            return self.close_network(self.active);
        }
        let Some(network) = self.networks.get_mut(self.active) else {
            return;
        };
        let Some(buffer) = network.state.buffers.get(index) else {
            return;
        };
        if buffer.kind == BufferKind::Channel
            && buffer.joined
            && network.connected()
            && !network.demo
        {
            let reason = network.state.part_message.as_str();
            let mut params = vec![buffer.name.as_str()];
            if !reason.is_empty() {
                params.push(reason);
            }
            if let Ok(line) = crate::irc::serialize(&[], None, "PART", &params) {
                let id = network.id;
                self.send(NetCommand::Send(id, line));
            }
        }
        let network = &mut self.networks[self.active];
        // Show the row above the closed one.
        let order = sidebar::order(&network.state.buffers);
        let position = order.iter().position(|&b| b == index).unwrap_or(1);
        let fallback = order.get(position.saturating_sub(1)).copied().unwrap_or(0);
        network.state.close(index);
        let shift = |b: usize| if b > index { b - 1 } else { b };
        network.active = if network.active == index {
            shift(fallback)
        } else {
            shift(network.active)
        };
        self.forget_pointer_state();
        self.sync_seen();
        self.mark_body();
        self.update_title();
        self.session_changed();
        self.sync_monitor(self.active);
    }

    /// Whether anything is animating: only a connecting network's jewel,
    /// and only while there's a window to show it.
    fn animating(&self) -> bool {
        self.view.is_some()
            && self
                .networks
                .iter()
                .any(|network| network.tab(false).status == TabStatus::Connecting)
    }

    /// Applies the store's events, then network events until they run out
    /// or the wake's budget does (`deadline`).
    fn drain_inbox(&mut self, deadline: Instant) {
        self.inbox_ready = false;
        self.store_inbox.arm();
        while let Some(event) = self.store_inbox.next() {
            self.apply_store(event);
        }
        self.net_inbox.arm();
        let mut handled = 0u32;
        while let Some(event) = self.net_inbox.next() {
            self.apply(event);
            handled += 1;
            if handled.is_multiple_of(32) && Instant::now() >= deadline {
                // The rest after the next round of input.
                self.inbox_ready = true;
                (self.wake)();
                break;
            }
        }
    }

    fn apply_store(&mut self, event: StoreEvent) {
        match event {
            StoreEvent::Loaded(Document::Recent, Ok(Some(text))) => match Recent::parse(&text) {
                Ok(recent) => {
                    self.recent = recent;
                    if self.welcome_visible() {
                        self.mark_all();
                    }
                }
                Err(error) => eprintln!("rubychat: recent.toml: {error}"),
            },
            StoreEvent::Loaded(Document::Recent, _) | StoreEvent::Saved(_, Ok(())) => {}
            StoreEvent::Backlog(id, result) => self.backlog_arrived(id, result),
            StoreEvent::Loaded(document, result) => self.startup_loaded(document, result),
            StoreEvent::Saved(_, Err(error)) => eprintln!("rubychat: {error}"),
        }
    }

    fn save_recent(&self) {
        if let Some(store) = &self.store {
            store.save(Document::Recent, self.recent.to_toml());
        }
    }

    /// Remembers a network RubyChat registered on, for the start page.
    fn remember(&mut self, id: NetworkId) {
        let Some(network) = self.networks.iter().find(|n| n.id == id && !n.demo) else {
            return;
        };
        self.recent.record(RecentServer {
            host: network.config.host.clone(),
            port: network.config.port,
            tls: network.config.tls,
            name: network.title.clone(),
            last: now_ms(),
        });
        self.save_recent();
    }

    /// Where the start page's parts are.
    fn start_layout(&self) -> Option<StartLayout> {
        let regions = self.regions()?;
        let saved = if self.saved.list.is_empty() {
            crate::config::networks::PRESETS.len()
        } else {
            self.saved.list.len()
        };
        Some(StartLayout::new(
            regions.body,
            saved,
            self.recent.servers().len(),
        ))
    }

    /// Hover over the start page's recent servers.
    fn start_pointer_moved(&mut self) {
        let point = self.view.as_ref().map(|view| view.pointer);
        let suggested = self.saved.list.is_empty();
        let (hovered, saved) = match (point, self.start_layout()) {
            (Some(point), Some(layout)) => {
                (layout.recent_at(point), layout.saved_at(point, suggested))
            }
            _ => (None, None),
        };
        if hovered != self.start_hover || saved != self.start_saved_hover {
            self.start_hover = hovered;
            self.start_saved_hover = saved;
            self.mark_all();
        }
        self.set_cursor(if hovered.is_some() || saved.is_some() {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        });
    }

    /// A click on a saved or recent server connects to it; EDIT and NEW
    /// open the editor.
    fn start_pointer_pressed(&mut self) {
        use crate::ui::welcome::SavedPart;
        if let Some(part) = self.start_saved_hover.take() {
            match part {
                SavedPart::New => self.open_manager(None),
                SavedPart::Edit(index) => self.open_manager(Some(index)),
                SavedPart::Row(index) if self.saved.list.is_empty() => {
                    if let Some((_, host, port)) = crate::config::networks::PRESETS.get(index) {
                        self.connect(commands::Server {
                            host: host.to_string(),
                            port: *port,
                            tls: true,
                            password: None,
                            entity: None,
                        });
                    }
                }
                SavedPart::Row(index) => {
                    let Some(saved) = self.saved.list.get(index).cloned() else {
                        return;
                    };
                    match self
                        .networks
                        .iter()
                        .position(|n| n.saved.as_deref() == Some(saved.name.as_str()))
                    {
                        Some(open) => self.activate(open),
                        None => {
                            self.connect_saved(&saved, None);
                        }
                    }
                }
            }
            return;
        }
        if let Some(index) = self.start_hover
            && let Some(server) = self.recent.servers().get(index).cloned()
        {
            self.start_hover = None;
            self.connect(commands::Server {
                host: server.host,
                port: server.port,
                tls: server.tls,
                password: None,
                entity: None,
            });
        }
    }

    fn apply(&mut self, event: NetEvent) {
        match event {
            NetEvent::Status(id, status) => self.apply_status(id, status),
            NetEvent::Received(id, message) => self.apply_message(id, &message),
            NetEvent::Notice(id, text) => {
                if let Some(index) = self.index_of(id) {
                    self.networks[index].push(0, LineKind::Status, "•", text);
                    let applied = Applied {
                        buffers: vec![0],
                        ..Applied::default()
                    };
                    self.applied(index, &applied, None);
                }
            }
            NetEvent::Lag(id, lag) => {
                if let Some(index) = self.index_of(id) {
                    self.networks[index].lag = Some(lag);
                    if self.is_shown(index) {
                        self.mark_sidebar();
                    }
                }
            }
            // The UI shows what the user sends as they send it.
            NetEvent::Sent(..) => {}
            NetEvent::Certificate(id, problem) => self.certificate_refused(id, problem),
            NetEvent::Caps(id, caps) => {
                if let Some(network) = self.network_mut(id) {
                    network.state.caps = caps;
                }
            }
            NetEvent::Sts(id, sts) => self.sts_advertised(id, sts),
        }
    }

    fn apply_status(&mut self, id: NetworkId, status: LinkStatus) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        let network = &mut self.networks[index];
        let registered = matches!(status, LinkStatus::Registered { .. }) && !network.registered();
        let down = matches!(
            status,
            LinkStatus::Disconnected { .. } | LinkStatus::Failed { .. }
        );
        if let LinkStatus::Registered { nick } = &status {
            network.state.nick = nick.clone();
        }
        network.status = status;
        if down {
            network.lag = None;
            network.state.disconnected();
            if let Some(list) = &mut network.state.channels {
                list.finish(Some(String::from("The connection was lost")));
            }
        }
        let retrying = matches!(
            network.status,
            LinkStatus::Disconnected {
                retry_in: Some(_),
                ..
            }
        );
        if registered {
            // Back in the channels you were in, and those waiting to be
            // joined.
            for line in network.state.rejoin_lines() {
                self.send(NetCommand::Send(id, line));
            }
            self.remember(id);
            self.run_connect_commands(index);
            self.resolve_history(index);
            self.ask_read_markers(index);
        } else if retrying {
            self.next_server(index);
        }
        if down {
            self.resolve_history(index);
            self.channels_arrived(index);
        }
        self.session_changed();
        self.mark_strip();
        if self.manager.is_some() || self.welcome_visible() {
            self.mark_body();
        }
        if self.is_shown(index) {
            self.mark_sidebar();
            self.mark_header();
            self.mark_input();
            self.mark_nicks();
            // Lines still waiting for their echo now show as undelivered.
            if down {
                self.mark_chat();
            }
        }
    }

    fn apply_message(&mut self, id: NetworkId, message: &Message) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        let network = &mut self.networks[index];
        // A saved network keeps the name it was saved with.
        if message.command() == "005"
            && network.saved.is_none()
            && let Some(name) = message.params().find_map(|p| p.strip_prefix("NETWORK="))
            && !name.is_empty()
            && network.title != name
        {
            network.title = name.to_string();
            network.state.buffers[0].name = name.to_string();
            let (host, port, tls) = (
                network.config.host.clone(),
                network.config.port,
                network.config.tls,
            );
            if self.recent.rename(&host, port, tls, name) {
                self.save_recent();
            }
            self.mark_strip();
            if self.is_shown(index) {
                self.mark_sidebar();
                self.update_title();
            }
        }
        let network = &mut self.networks[index];
        let badge = network.badge();
        let applied = network.state.apply(message, now_ms());
        for reply in &applied.replies {
            self.send(NetCommand::Send(id, reply.clone()));
        }
        let renamed = matches!(message.command(), "NICK" | "001");
        self.applied(index, &applied, Some(badge));
        if renamed && self.is_shown(index) {
            self.mark_input();
        }
        self.alert(index, &applied.alerts);
        self.away_reply(message.command());
        // Queries came or went, or the server just said it can MONITOR.
        if applied.list || matches!(message.command(), "005" | "376" | "422") {
            self.sync_monitor(index);
        }
    }

    /// Marks what a change to network `index` touched. `badge` is the
    /// network's badge before the change, when it may have moved.
    fn applied(&mut self, index: usize, applied: &Applied, badge: Option<(u32, bool)>) {
        if self.is_shown(index) {
            let active = self.networks[index].active;
            if applied.buffers.contains(&active) {
                self.mark_chat();
                if self.networks[index].buffer().seen {
                    self.read_while_seen(index, active);
                }
            }
            if applied.details.contains(&active) || applied.list {
                self.mark_header();
                self.mark_nicks();
            }
            if applied.list || applied.buffers.iter().any(|&b| b != active) {
                self.mark_sidebar();
            }
            if applied.list {
                self.update_title();
            }
        } else if badge.is_none_or(|badge| badge != self.networks[index].badge()) {
            self.mark_strip();
        }
        if applied.list {
            self.session_changed();
        }
        if applied.channels {
            self.channels_arrived(index);
        }
        for &buffer in &applied.buffers {
            self.log_lines(index, buffer);
        }
        // Read on another client: its notification goes, and the badges.
        if !applied.read.is_empty() {
            let id = self.networks[index].id;
            let keys: Vec<_> = applied
                .read
                .iter()
                .map(|&buffer| (id.0, self.networks[index].state.buffers[buffer].key.clone()))
                .collect();
            self.withdraw_seen(keys);
            self.mark_strip();
            self.mark_sidebar();
        }
    }

    /// Where the sidebar, header, chat view, input bar and nick list go.
    fn regions(&self) -> Option<Regions> {
        let view = self.view.as_ref()?;
        let (width, height) = (view.logical_width(), view.logical_height());
        let body = Rect::new(0.0, STRIP_HEIGHT, width, (height - STRIP_HEIGHT).max(0.0));
        let sidebar = self
            .show_sidebar
            .then(|| Rect::new(0.0, body.y, SIDEBAR_WIDTH.min(width), body.height));
        let left = sidebar.map_or(0.0, |rect| rect.right());
        let channel = self
            .networks
            .get(self.active)
            .is_some_and(|network| network.buffer().kind == BufferKind::Channel)
            && !self.browsing();
        let nicks = (self.show_nicks
            && channel
            && width - left - NICK_LIST_WIDTH >= MIN_CHAT_WIDTH)
            .then(|| {
                Rect::new(
                    width - NICK_LIST_WIDTH,
                    body.y,
                    NICK_LIST_WIDTH,
                    body.height,
                )
            });
        let right = nicks.map_or(width, |rect| rect.x);
        let center = Rect::new(left, body.y, (right - left).max(0.0), body.height);
        let header = Rect::new(
            center.x,
            center.y,
            center.width,
            HEADER_HEIGHT.min(center.height),
        );
        let input_height = INPUT_AREA.min((center.bottom() - header.bottom()).max(0.0));
        let input = Rect::new(
            center.x,
            center.bottom() - input_height,
            center.width,
            input_height,
        );
        let chat = Rect::new(
            center.x,
            header.bottom(),
            center.width,
            (input.y - header.bottom()).max(0.0),
        );
        Some(Regions {
            body,
            sidebar,
            header,
            chat,
            input,
            nicks,
        })
    }

    /// Scrolls the chat view `pixels` toward older lines.
    fn scroll_chat(&mut self, pixels: f32) {
        if self.welcome_visible() {
            return;
        }
        let Some(regions) = self.regions() else {
            return;
        };
        let Some(network) = self.networks.get_mut(self.active) else {
            return;
        };
        let state = &mut network.state;
        let buffer = &mut state.buffers[network.active];
        let context = ChatContext {
            casemap: state.isupport.casemap,
            chantypes: &state.isupport.chantypes,
            marker: buffer.marker,
            label_chars: buffer.label_chars,
            filter: self.smart_filter,
            loading: buffer.history.loading,
        };
        // Whole pixels, so what's shown can move rather than repaint; the
        // remainder carries over.
        let total = pixels + self.scroll_remainder;
        self.scroll_remainder = total - total.round();
        let anchor = chat_view::scroll(
            &mut self.text,
            regions.chat,
            &context,
            buffer.lines(),
            buffer.anchor,
            total.round(),
        );
        if anchor != buffer.anchor {
            buffer.anchor = anchor;
            self.mark(|dirty| dirty.scrolled = true);
        }
        if pixels > 0.0 {
            self.near_top();
        }
    }

    /// Back to the newest line.
    fn return_to_present(&mut self) {
        if let Some(network) = self.networks.get_mut(self.active)
            && network.buffer().anchor.is_some()
        {
            network.buffer_mut().anchor = None;
            self.mark_chat();
        }
    }

    fn page(&self) -> f32 {
        self.regions()
            .map_or(300.0, |regions| (regions.chat.height - 40.0).max(40.0))
    }

    fn editor(&mut self) -> &mut LineEditor {
        let browsing = self.browsing();
        // Disjoint fields, so each arm borrows only its own.
        match (
            &mut self.manager,
            self.networks.is_empty() || self.prompt_open,
            &mut self.find,
            &mut self.browser,
        ) {
            (Some(manager), ..) => &mut manager.editor,
            (None, true, ..) => &mut self.prompt,
            (None, false, Some(find), _) => &mut find.editor,
            (None, false, None, Some(browser)) if browsing => &mut browser.filter,
            (None, false, ..) => &mut self.networks[self.active].buffer_mut().input,
        }
    }

    /// The active channel's members in nick list order.
    fn sorted_members(network: &mut Network) -> Vec<&Member> {
        let state = &mut network.state;
        let buffer = &mut state.buffers[network.active];
        buffer.members.sorted(&state.isupport).collect()
    }

    fn key(&mut self, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        if self.trust.is_some() {
            return self.trust_key(event);
        }
        if self.menu.is_some() && self.menu_key(&event.logical_key) {
            return;
        }
        if self.manager.is_some() {
            return self.manager_key(event);
        }
        let control = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        let alt = self.modifiers.alt_key();
        let welcome = self.welcome_visible();
        if !matches!(event.logical_key, Key::Named(NamedKey::Tab)) || control {
            self.completion = None;
        }
        match &event.logical_key {
            Key::Character(text) if control => {
                match text.to_lowercase().as_str() {
                    "t" => self.open_prompt(),
                    "f" if !welcome => self.open_find(None),
                    "w" if !welcome => {
                        let index = self.networks[self.active].active;
                        self.close_buffer(index);
                    }
                    "q" => self.quit = true,
                    "a" => {
                        self.editor().select_all();
                        self.mark_input();
                    }
                    "j" if !welcome => {
                        self.close_find();
                        self.close_channels();
                        self.start_join();
                    }
                    "c" => self.copy(),
                    "x" => self.cut(),
                    "v" => self.paste(),
                    digit @ ("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8") => {
                        self.activate(usize::from(digit.as_bytes()[0] - b'1'));
                    }
                    "9" => self.activate(self.networks.len().saturating_sub(1)),
                    _ => {}
                }
                return;
            }
            Key::Character(text) if alt && !welcome => {
                if let Some(digit) = text.chars().next().and_then(|c| c.to_digit(10)) {
                    self.activate_nth_buffer(if digit == 0 { 9 } else { digit as usize - 1 });
                }
                return;
            }
            Key::Named(NamedKey::ArrowUp) if alt && !welcome => return self.cycle_buffer(-1),
            Key::Named(NamedKey::ArrowDown) if alt && !welcome => return self.cycle_buffer(1),
            Key::Named(NamedKey::Tab) if control => return self.cycle(if shift { -1 } else { 1 }),
            Key::Named(NamedKey::Tab) if !welcome && self.find.is_none() && !self.browsing() => {
                return self.complete(shift);
            }
            Key::Named(NamedKey::PageDown) if control => return self.cycle(1),
            Key::Named(NamedKey::PageUp) if control => return self.cycle(-1),
            Key::Named(NamedKey::PageUp) if self.browsing() => return self.move_selection(-2),
            Key::Named(NamedKey::PageDown) if self.browsing() => return self.move_selection(2),
            Key::Named(NamedKey::PageUp) => return self.scroll_chat(self.page()),
            Key::Named(NamedKey::PageDown) => return self.scroll_chat(-self.page()),
            Key::Named(NamedKey::End) if control => return self.return_to_present(),
            Key::Named(NamedKey::F7) => {
                self.show_nicks = !self.show_nicks;
                return self.mark_body();
            }
            Key::Named(NamedKey::F8) => {
                self.show_sidebar = !self.show_sidebar;
                return self.mark_body();
            }
            Key::Named(NamedKey::Escape) if self.prompt_open => return self.close_prompt(),
            // The find field: Enter and Up go to earlier matches,
            // Shift+Enter and Down to later ones.
            Key::Named(NamedKey::Escape) if !welcome && self.find.is_some() => {
                return self.close_find();
            }
            Key::Named(NamedKey::Enter) if !welcome && self.find.is_some() => {
                self.find_edited();
                return self.find_step(!shift);
            }
            Key::Named(NamedKey::ArrowUp) if !welcome && self.find.is_some() => {
                self.find_edited();
                return self.find_step(true);
            }
            Key::Named(NamedKey::ArrowDown) if !welcome && self.find.is_some() => {
                self.find_edited();
                return self.find_step(false);
            }
            Key::Named(NamedKey::Tab) if !welcome && self.find.is_some() => return,
            // The channel list: rows are chosen with the arrows and
            // joined with Enter, Tab sorts the other way.
            Key::Named(NamedKey::Escape) if self.browsing() => return self.close_channels(),
            Key::Named(NamedKey::Enter) if self.browsing() => return self.join_selected(),
            Key::Named(NamedKey::ArrowUp) if self.browsing() => return self.move_selection(-1),
            Key::Named(NamedKey::ArrowDown) if self.browsing() => return self.move_selection(1),
            Key::Named(NamedKey::Tab) if self.browsing() => {
                let order = match self.networks[self.active].state.channels.as_ref() {
                    Some(list) if list.order() == Order::Users => Order::Name,
                    _ => Order::Users,
                };
                return self.sort_channels(order);
            }
            Key::Named(NamedKey::Escape) if !welcome => {
                // Clear the selection and return to the present.
                if self.selection.take().is_some() {
                    self.mark_chat();
                }
                return self.return_to_present();
            }
            _ => {}
        }
        // A mistyped address's message clears once typing resumes.
        let had_message = welcome && self.prompt_message.take().is_some();
        let editor = self.editor();
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => {
                return self.submit();
            }
            Key::Named(NamedKey::Backspace) => editor.backspace(control),
            Key::Named(NamedKey::Delete) => editor.delete(control),
            Key::Named(NamedKey::ArrowLeft) => editor.left(control, shift),
            Key::Named(NamedKey::ArrowRight) => editor.right(control, shift),
            Key::Named(NamedKey::Home) => editor.home(shift),
            Key::Named(NamedKey::End) => editor.end(shift),
            Key::Named(NamedKey::ArrowUp) => editor.recall_older(),
            Key::Named(NamedKey::ArrowDown) => editor.recall_newer(),
            Key::Named(NamedKey::Escape) => editor.clear(),
            _ => match &event.text {
                Some(text) if !control => editor.insert(text),
                _ => return,
            },
        }
        if had_message {
            self.mark_all();
        } else {
            self.mark_input();
        }
    }

    /// Ctrl+J and `+ JOIN`: starts a /join in an empty input.
    fn start_join(&mut self) {
        let editor = self.editor();
        if editor.is_empty() {
            editor.set("/join #");
            self.mark_input();
        }
    }

    /// The chat selection, else the input's.
    fn copy(&mut self) {
        let text = if let Some(selection) = self.selection {
            let (start, end) = chat_view::ordered(selection.anchor, selection.focus);
            chat_view::selected_text(
                self.networks[self.active].buffer().lines(),
                start,
                end,
                self.smart_filter,
            )
        } else {
            let editor = self.editor();
            match editor.selection() {
                Some(range) => editor.text()[range].to_string(),
                None => return,
            }
        };
        self.put_on_clipboard(text);
    }

    fn cut(&mut self) {
        let editor = self.editor();
        if let Some(range) = editor.selection() {
            let text = editor.text()[range.clone()].to_string();
            editor.replace(range, "");
            self.mark_input();
            if self.manager.is_some() {
                self.manager_edited();
            }
            self.put_on_clipboard(text);
        }
    }

    fn put_on_clipboard(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        #[cfg(all(unix, not(target_vendor = "apple")))]
        if let Some(clipboard) = &mut self.clipboard {
            clipboard.copy(text.clone());
        }
        self.copied = text;
    }

    /// Pastes the system clipboard's text, which arrives on a later wake,
    /// or what was last copied in RubyChat.
    fn paste(&mut self) {
        #[cfg(all(unix, not(target_vendor = "apple")))]
        if let Some(clipboard) = &mut self.clipboard
            && !clipboard.owns()
        {
            let sender = self.pasted.0.clone();
            let wake = (self.waker)();
            let started = clipboard.paste(move |text| {
                let _ = sender.send(text);
                wake();
            });
            if started {
                return;
            }
        }
        let text = self.copied.clone();
        self.insert_pasted(&text);
    }

    /// Pasted text goes into the input, its line breaks as spaces.
    fn insert_pasted(&mut self, text: &str) {
        let line: String = text
            .trim_end_matches(['\r', '\n'])
            .replace("\r\n", " ")
            .replace(['\r', '\n'], " ");
        if !line.is_empty() {
            self.editor().insert(&line);
            self.mark_input();
            if self.manager.is_some() {
                self.manager_edited();
            }
        }
    }

    /// Pointer motion below the strip.
    fn body_pointer_moved(&mut self) {
        let Some(point) = self.view.as_ref().map(|view| view.pointer) else {
            return;
        };
        let Some(regions) = self.regions() else {
            return;
        };
        if let Some(selection) = &mut self.selection
            && selection.dragging
        {
            if let Some(position) = self.chat_frame.position_at(point)
                && position != selection.focus
            {
                selection.focus = position;
                self.mark_chat();
            }
            return;
        }
        let mut hover = Hover::default();
        let mut cursor = CursorIcon::Default;
        if let Some(rect) = regions.sidebar
            && rect.contains(point)
        {
            let network = &self.networks[self.active];
            hover.sidebar = SidebarLayout::new(rect, &network.state.buffers, self.sidebar_scroll)
                .part_at(point);
            if hover.sidebar.is_some() {
                cursor = CursorIcon::Pointer;
            }
        } else if let Some(rect) = regions.nicks
            && rect.contains(point)
        {
            let network = &mut self.networks[self.active];
            let scroll = network.buffer().nick_scroll;
            hover.nick = nick_list::member_at(rect, &Self::sorted_members(network), scroll, point);
            if hover.nick.is_some() {
                cursor = CursorIcon::Pointer;
            }
        } else if regions.chat.contains(point) && self.browsing() {
            let rows = self.channel_rows();
            let scroll = self.browser.as_ref().map_or(0.0, |browser| browser.scroll);
            let part = TableLayout::new(regions.chat).part_at(point, scroll, rows);
            if part.is_some() {
                cursor = CursorIcon::Pointer;
            }
            self.table_hover(part);
        } else if regions.chat.contains(point) {
            hover.link = self.chat_frame.link_at(point);
            cursor = if hover.link.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Text
            };
        } else if regions.input.contains(point) {
            cursor = CursorIcon::Text;
        }
        if hover.sidebar != self.hover.sidebar {
            self.mark_sidebar();
        }
        if hover.nick != self.hover.nick {
            self.mark_nicks();
        }
        if hover.link != self.hover.link {
            self.mark_chat();
        }
        self.hover = hover;
        self.set_cursor(cursor);
    }

    fn body_pointer_pressed(&mut self, button: MouseButton) {
        let Some(point) = self.view.as_ref().map(|view| view.pointer) else {
            return;
        };
        let Some(regions) = self.regions() else {
            return;
        };
        let double = button == MouseButton::Left
            && self.last_press.is_some_and(|(at, from)| {
                at.elapsed() <= DOUBLE_CLICK
                    && (from.x - point.x).abs() <= DOUBLE_CLICK_SLOP
                    && (from.y - point.y).abs() <= DOUBLE_CLICK_SLOP
            });
        if button == MouseButton::Left {
            self.last_press = (!double).then(|| (Instant::now(), point));
        }
        if let Some(rect) = regions.sidebar
            && rect.contains(point)
        {
            let network = &self.networks[self.active];
            let part = SidebarLayout::new(rect, &network.state.buffers, self.sidebar_scroll)
                .part_at(point);
            match (part, button) {
                (Some(SidebarPart::Buffer(index)), MouseButton::Left) => {
                    self.activate_buffer(index)
                }
                (Some(SidebarPart::Buffer(index)), MouseButton::Middle) if index > 0 => {
                    self.close_buffer(index)
                }
                (Some(SidebarPart::Buffer(index)), MouseButton::Right) => {
                    self.open_buffer_menu(index, point)
                }
                (Some(SidebarPart::Join), MouseButton::Left) => self.start_join(),
                _ => {}
            }
        } else if let Some(rect) = regions.nicks
            && rect.contains(point)
        {
            let network = &mut self.networks[self.active];
            let scroll = network.buffer().nick_scroll;
            let members = Self::sorted_members(network);
            let nick = nick_list::member_at(rect, &members, scroll, point)
                .map(|index| members[index].nick.clone());
            match (nick, button) {
                (Some(nick), MouseButton::Left) if double => self.open_query(&nick),
                (Some(nick), MouseButton::Right) => self.open_nick_menu(&nick, point),
                _ => {}
            }
        } else if regions.chat.contains(point) && self.browsing() {
            if button == MouseButton::Left {
                let rows = self.channel_rows();
                let scroll = self.browser.as_ref().map_or(0.0, |browser| browser.scroll);
                let part = TableLayout::new(regions.chat).part_at(point, scroll, rows);
                self.table_press(part, double);
            }
        } else if regions.chat.contains(point) {
            let link = self.chat_frame.link_at(point).map(|(_, _, target)| target);
            let nick = self.chat_frame.nick_at(point).map(str::to_string);
            match (button, link, nick) {
                (MouseButton::Left, Some(target), _) => self.follow(target),
                // A nick opens its menu (DESIGN.md §8.6).
                (MouseButton::Left | MouseButton::Right, None, Some(nick)) => {
                    self.open_nick_menu(&nick, point)
                }
                (MouseButton::Right, Some(target), _) => self.open_link_menu(target, point),
                (MouseButton::Right, None, None) => self.open_selection_menu(point),
                (MouseButton::Left, None, None) => {
                    if let Some(position) = self.chat_frame.position_at(point) {
                        self.selection = Some(Selection {
                            anchor: position,
                            focus: position,
                            dragging: true,
                        });
                        self.mark_chat();
                    }
                }
                _ => {}
            }
        }
    }

    fn body_pointer_released(&mut self) {
        if let Some(selection) = &mut self.selection {
            selection.dragging = false;
            if selection.anchor == selection.focus {
                self.selection = None;
                self.mark_chat();
            }
        }
    }

    /// The wheel over the sidebar, the nick list or the chat view.
    fn body_scroll(&mut self, pixels: f32) {
        let (Some(point), Some(regions)) =
            (self.view.as_ref().map(|view| view.pointer), self.regions())
        else {
            return;
        };
        if regions.sidebar.is_some_and(|rect| rect.contains(point)) {
            self.sidebar_scroll = (self.sidebar_scroll - pixels).max(0.0);
            self.mark_sidebar();
        } else if regions.nicks.is_some_and(|rect| rect.contains(point)) {
            let buffer = self.networks[self.active].buffer_mut();
            buffer.nick_scroll = (buffer.nick_scroll - pixels).max(0.0);
            self.mark_nicks();
        } else if self.browsing() {
            self.scroll_table(pixels);
        } else {
            self.scroll_chat(pixels);
        }
    }

    /// A clicked link: a web address opens in the browser; a channel opens
    /// its buffer, joining it if need be.
    fn follow(&mut self, target: Target) {
        match target {
            Target::Url(url) => {
                if !crate::platform::open::open_url(&url) {
                    self.say(
                        LineKind::Error,
                        "!",
                        format!("RubyChat doesn't open addresses like {url}"),
                    );
                }
            }
            Target::Channel(name) => match self.networks[self.active].state.find(&name) {
                Some(index) => self.activate_buffer(index),
                None => self.join(&name),
            },
        }
    }

    /// Paints the dirty regions and presents them.
    fn redraw(&mut self) {
        let started = Instant::now();
        let welcome = self.welcome_visible();
        let regions = self.regions();
        let welcome_info = WelcomeInfo {
            version: env!("CARGO_PKG_VERSION"),
            frame: self.last_frame,
            device_size: self
                .view
                .as_ref()
                .map_or((0, 0), |view| (view.canvas.width(), view.canvas.height())),
            scale: self.view.as_ref().map_or(1.0, |view| view.scale),
            glyphs: self.text.glyph_cache().len(),
            glyph_bytes: self.text.glyph_cache().bytes(),
            networks: self.networks.len(),
            message: self.prompt_message.clone(),
            can_cancel: !self.networks.is_empty(),
        };
        let pulse = tab_strip::heartbeat(self.started.elapsed());
        let start_layout = self.start_layout();
        // Until saved networks and the session load (a few milliseconds),
        // nothing: the start page would flash before they open.
        let starting = !self.startup.done;
        let logo_size = self.view.as_ref().map_or(1.0, |view| view.scale) * LOGO_SIZE;
        let now = now_ms();
        let manager_entries = self.manager_entries();
        let manager_layout = self.manager_layout(&manager_entries);
        let manager_list = self.manager_list();
        let manager_connect = self.manager_connect_label();
        let (saved_rows, suggested) = self.start_saved_rows();
        let trust_layout = self.trust_layout();
        let browsing = self.browsing();
        let channels_detail = self.channels_detail();
        let selected_row = self.selected_row();
        let current_match = self.current_match();
        let shown_name = self
            .networks
            .get(self.active)
            .map(|network| network.buffer().name.clone())
            .unwrap_or_default();
        let App {
            view,
            text,
            networks,
            strip,
            active,
            prompt,
            recent,
            start_hover,
            logo,
            sidebar_scroll,
            hover,
            chat_frame,
            selection,
            menu,
            manager,
            trust,
            trust_focus,
            trust_hover,
            start_saved_hover,
            smart_filter,
            find,
            browser,
            ..
        } = self;
        // What repainted below the strip: overlays paint over it, and only
        // there.
        let mut repainted: Vec<Rect> = Vec::new();
        let (Some(view), Some(regions)) = (view.as_mut(), regions) else {
            return;
        };
        let size = view.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if (size.width, size.height) != (view.canvas.width(), view.canvas.height()) {
            view.canvas.resize(size.width, size.height);
            view.dirty = Dirty::ALL;
        }
        let dirty = std::mem::take(&mut view.dirty);
        if !dirty.any() {
            return;
        }
        let scale = view.scale;
        let width = size.width as f32 / scale;
        let focused = view.focused;
        let mut damage = Damage::default();
        {
            let mut painter = Painter::new(&mut view.canvas, text, scale);
            if dirty.strip {
                let mut geometry = TabStripGeometry::new(width, networks.len(), strip.scroll);
                if std::mem::take(&mut strip.reveal_active) {
                    strip.scroll = geometry.reveal(*active);
                    geometry = TabStripGeometry::new(width, networks.len(), strip.scroll);
                }
                strip.scroll = geometry.scroll;
                let tabs: Vec<StripTab> = networks
                    .iter()
                    .enumerate()
                    .map(|(index, network)| network.tab(index == *active && !welcome))
                    .collect();
                let dragging = strip.drag.as_ref().and_then(|drag| {
                    drag.moved
                        .then(|| {
                            networks
                                .iter()
                                .position(|n| n.id == drag.id)
                                .map(|index| (index, drag.offset))
                        })
                        .flatten()
                });
                let state = TabStripState {
                    active: if welcome { usize::MAX } else { *active },
                    hovered: strip.hovered,
                    pressed: strip.pressed.map(|(part, _)| part),
                    maximized: view.window.is_maximized(),
                    window_focused: focused,
                    pulse,
                    dragging,
                };
                tab_strip::paint_tab_strip(&mut painter, &geometry, &tabs, &state);
                damage.add(Rect::new(0.0, 0.0, width, STRIP_HEIGHT).snap(scale));
            }
            let below = dirty.body
                || dirty.chat
                || dirty.sidebar
                || dirty.header
                || dirty.nicks
                || dirty.input;
            if welcome && starting {
                painter.fill_rect(regions.body, crate::ui::theme::CANVAS);
                damage.add(regions.body.snap(scale));
            } else if let (Some(manager), Some(layout)) = (manager.as_mut(), &manager_layout) {
                if below {
                    let list = Self::manager_list_entries(&manager_list);
                    let title = if manager.draft.name.trim().is_empty() {
                        String::from("New network")
                    } else {
                        manager.draft.name.clone()
                    };
                    let dirty_draft = manager.dirty();
                    let mut view = crate::ui::manager::ManagerView {
                        networks: &list,
                        selected: manager.selected,
                        title: &title,
                        entries: &manager_entries,
                        focus: manager.focus,
                        editor: &mut manager.editor,
                        hover: manager.hover,
                        message: manager
                            .message
                            .as_ref()
                            .map(|(text, problem)| (text.as_str(), *problem)),
                        dirty: dirty_draft,
                        connect: manager_connect,
                        focused,
                    };
                    crate::ui::manager::paint_manager(
                        &mut painter,
                        regions.body,
                        layout,
                        &mut view,
                    );
                    damage.add(regions.body.snap(scale));
                    repainted.push(regions.body);
                }
            } else if welcome && let Some(start_layout) = &start_layout {
                let start = StartPage {
                    recent: recent.servers(),
                    hovered: *start_hover,
                    saved: &saved_rows,
                    suggested,
                    saved_hover: *start_saved_hover,
                    logo: Some(logo.at(logo_size.round() as u32)),
                    now,
                };
                if dirty.body || dirty.chat || dirty.sidebar || dirty.header || dirty.nicks {
                    welcome::paint_welcome(
                        &mut painter,
                        start_layout,
                        &welcome_info,
                        prompt,
                        &start,
                    );
                    damage.add(regions.body.snap(scale));
                    repainted.push(regions.body);
                } else if dirty.input {
                    // Only the field changed: repaint the page clipped to it.
                    let field = start_layout.input();
                    painter.save_clip(field);
                    welcome::paint_welcome(
                        &mut painter,
                        start_layout,
                        &welcome_info,
                        prompt,
                        &start,
                    );
                    painter.restore();
                    damage.add(field.snap(scale));
                    repainted.push(field);
                }
            } else if let Some(network) = networks.get_mut(*active) {
                if (dirty.body || dirty.sidebar)
                    && let Some(rect) = regions.sidebar
                {
                    let layout = SidebarLayout::new(rect, &network.state.buffers, *sidebar_scroll);
                    *sidebar_scroll = layout.scroll;
                    let status = network.status_line();
                    let sidebar_view = SidebarView {
                        title: &network.title,
                        status: &status,
                        buffers: &network.state.buffers,
                        server: &network.config.host,
                        active: network.active,
                        hovered: hover.sidebar,
                        focused,
                    };
                    sidebar::paint_sidebar(&mut painter, &layout, &sidebar_view);
                    damage.add(rect.snap(scale));
                }
                if dirty.body || dirty.header {
                    if browsing {
                        let view = HeaderView {
                            name: "Channels",
                            detail: &channels_detail,
                            topic: None,
                            placeholder: "Type to filter by name or topic · Enter joins · Tab sorts · Esc closes",
                        };
                        header::paint_header(&mut painter, regions.header, &view);
                    } else {
                        paint_header(&mut painter, regions.header, network);
                    }
                    damage.add(regions.header.snap(scale));
                }
                if browsing && (dirty.body || dirty.chat || dirty.scrolled) {
                    if let (Some(browser), Some(list)) =
                        (browser.as_mut(), network.state.channels.as_ref())
                    {
                        let table = TableView {
                            list,
                            scroll: browser.scroll,
                            selected: selected_row,
                            hovered: browser.hovered,
                            filtered: !browser.filter.text().is_empty(),
                            focused,
                        };
                        browser.scroll = channel_list::paint_table(
                            &mut painter,
                            &TableLayout::new(regions.chat),
                            &table,
                        );
                    }
                    // Nothing of the chat view is under the pointer.
                    *chat_frame = ChatFrame::default();
                    damage.add(regions.chat.snap(scale));
                } else if dirty.body || dirty.chat || dirty.scrolled {
                    let scrolled = !dirty.body && !dirty.chat && menu.is_none();
                    let state = &mut network.state;
                    let buffer = &mut state.buffers[network.active];
                    let chat = ChatView {
                        lines: buffer.lines(),
                        context: ChatContext {
                            casemap: state.isupport.casemap,
                            chantypes: &state.isupport.chantypes,
                            marker: buffer.marker,
                            label_chars: buffer.label_chars,
                            filter: *smart_filter,
                            loading: buffer.history.loading,
                        },
                        anchor: buffer.anchor,
                        selection: selection.map(|s| chat_view::ordered(s.anchor, s.focus)),
                        hovered: hover
                            .link
                            .as_ref()
                            .map(|(id, range, _)| (*id, range.clone())),
                        found: find
                            .as_ref()
                            .and_then(|find| find.query())
                            .map(|query| Found {
                                query,
                                current: current_match.clone(),
                            }),
                        focused,
                    };
                    let previous = std::mem::take(chat_frame);
                    *chat_frame = chat_view::paint_chat(
                        &mut painter,
                        regions.chat,
                        &chat,
                        previous,
                        scrolled,
                    );
                    buffer.anchor = chat_frame.anchor;
                    damage.add(regions.chat.snap(scale));
                }
                if dirty.body || dirty.input {
                    let input = regions.input;
                    painter.fill_rect(input, SURFACE);
                    painter.fill_rect(Rect::new(input.x, input.y, input.width, 1.0), EDGE);
                    let field = Rect::new(
                        input.x + 12.0,
                        input.y + 8.0,
                        (input.width - 24.0).max(0.0),
                        (input.height - 16.0).max(0.0),
                    );
                    if let Some(find) = find.as_mut() {
                        let placeholder = format!(
                            "Find in {shown_name} · Enter: earlier · Shift+Enter: later · Esc closes"
                        );
                        let note = find.note;
                        paint_input(
                            &mut painter,
                            field,
                            &mut find.editor,
                            "FIND\u{203a}",
                            &placeholder,
                            focused,
                            note,
                        );
                    } else if let Some(browser) = browser.as_mut().filter(|_| browsing) {
                        let note = network
                            .state
                            .channels
                            .as_ref()
                            .is_some_and(|list| list.settling())
                            .then_some("FILTERING");
                        paint_input(
                            &mut painter,
                            field,
                            &mut browser.filter,
                            "FILTER\u{203a}",
                            "Channel name or topic",
                            focused,
                            note,
                        );
                    } else {
                        let prompt = format!("{}\u{203a}", network.state.nick);
                        let buffer = network.buffer();
                        let placeholder = match buffer.kind {
                            BufferKind::Server => String::from("Type /help for commands"),
                            _ => format!("Message {}", buffer.name),
                        };
                        let editor = &mut network.buffer_mut().input;
                        paint_input(
                            &mut painter,
                            field,
                            editor,
                            &prompt,
                            &placeholder,
                            focused,
                            None,
                        );
                    }
                    damage.add(input.snap(scale));
                }
                if (dirty.body || dirty.nicks)
                    && let Some(rect) = regions.nicks
                {
                    let scroll = network.buffer().nick_scroll;
                    let shown = nick_list::paint_nick_list(
                        &mut painter,
                        rect,
                        &Self::sorted_members(network),
                        scroll,
                        hover.nick,
                    );
                    network.buffer_mut().nick_scroll = shown;
                    damage.add(rect.snap(scale));
                }
                repainted.extend(
                    [
                        regions.sidebar.filter(|_| dirty.body || dirty.sidebar),
                        (dirty.body || dirty.header).then_some(regions.header),
                        (dirty.body || dirty.chat || dirty.scrolled).then_some(regions.chat),
                        (dirty.body || dirty.input).then_some(regions.input),
                        regions.nicks.filter(|_| dirty.body || dirty.nicks),
                    ]
                    .into_iter()
                    .flatten(),
                );
            }
            // Overlays go over whatever repainted beneath them, and only
            // there, so their translucent edges never paint twice.
            let meets = |rect: &Rect, reach: Rect| {
                rect.x < reach.right()
                    && reach.x < rect.right()
                    && rect.y < reach.bottom()
                    && reach.y < rect.bottom()
            };
            if let Some(menu) = menu.as_ref().filter(|_| !welcome) {
                for rect in repainted
                    .iter()
                    .filter(|rect| meets(rect, menu.geometry.damage()))
                {
                    painter.save_clip(*rect);
                    menu.paint(&mut painter);
                    painter.restore();
                }
            }
            if let (Some((_, problem)), Some(layout)) = (trust.as_ref(), &trust_layout) {
                let view = crate::ui::trust::TrustView {
                    problem,
                    focus: *trust_focus,
                    hover: *trust_hover,
                };
                for rect in repainted.iter().filter(|rect| meets(rect, layout.damage())) {
                    painter.save_clip(*rect);
                    crate::ui::trust::paint_trust(&mut painter, layout, &view);
                    painter.restore();
                }
            }
        }
        let painted = started.elapsed();
        match view.presenter.present(&view.window, &view.canvas, &damage) {
            Ok(presented) => {
                let total = started.elapsed();
                if self.trace {
                    eprintln!(
                        "frame {}: paint {:.3} ms, present {:.3} ms, damage {} px in {} rect(s), age {}, copied {} px",
                        self.frames,
                        painted.as_secs_f64() * 1e3,
                        (total - painted).as_secs_f64() * 1e3,
                        damage.area(),
                        damage.rects().len(),
                        presented.age,
                        presented.copied,
                    );
                }
                self.last_frame = Some(total);
                self.frames += 1;
            }
            Err(error) => eprintln!("rubychat: {error}"),
        }
    }
}

/// The header for the network's shown buffer.
fn paint_header(painter: &mut Painter<'_>, rect: Rect, network: &Network) {
    let buffer = network.buffer();
    let topic_text: Option<String>;
    let (name, detail, topic, placeholder) = match buffer.kind {
        BufferKind::Server => {
            let tls = if network.config.tls { "TLS" } else { "PLAIN" };
            let detail = format!("{}:{} · {tls}", network.config.host, network.config.port);
            (
                network.title.as_str(),
                detail,
                None,
                "Server messages, notices and replies",
            )
        }
        BufferKind::Channel => {
            let state = if buffer.joined {
                let count = buffer.members.len();
                let members = format!(
                    "{} member{}",
                    thousands(count),
                    if count == 1 { "" } else { "s" }
                );
                match buffer.mode_string() {
                    modes if modes.is_empty() => members,
                    modes => format!("{modes} · {members}"),
                }
            } else if buffer.rejoin && network.connected() {
                String::from("joining…")
            } else {
                String::from("not joined")
            };
            (
                buffer.name.as_str(),
                state,
                buffer.topic.as_deref(),
                "No topic",
            )
        }
        BufferKind::Query => {
            // What's known of them (DESIGN.md §8.5): online or not
            // (MONITOR), their account, and an away message or realname.
            let peer = &buffer.peer;
            let mut details = Vec::new();
            match peer.online {
                Some(true) => details.push(String::from("online")),
                Some(false) => details.push(String::from("offline")),
                None => {}
            }
            if let Some(account) = &peer.account {
                details.push(format!("account {account}"));
            }
            if peer.away.is_some() {
                details.push(String::from("away"));
            }
            if details.is_empty() {
                details.push(String::from("private messages"));
            }
            topic_text = peer
                .away
                .as_ref()
                .map(|away| format!("Away: {away}"))
                .or_else(|| peer.realname.clone());
            (
                buffer.name.as_str(),
                details.join(" · "),
                topic_text.as_deref(),
                "Messages between you two",
            )
        }
    };
    let view = HeaderView {
        name,
        detail: &detail,
        topic,
        placeholder,
    };
    header::paint_header(painter, rect, &view);
}

impl ApplicationHandler<AppEvent> for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if self.trace && !matches!(cause, StartCause::Init) {
            eprintln!("wake: {cause:?}");
        }
        if matches!(cause, StartCause::ResumeTimeReached { .. }) && self.animating() {
            self.mark_strip();
        }
        if self.exit_at.is_some_and(|at| Instant::now() >= at) {
            self.quit = true;
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(all(unix, not(target_vendor = "apple")))]
        if self.clipboard.is_none() {
            use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
            if let Ok(handle) = event_loop.display_handle()
                && let RawDisplayHandle::Wayland(wayland) = handle.as_raw()
            {
                // SAFETY: the display is the event loop's connection, and the
                // clipboard and activation are dropped in `exiting`, while
                // it is still open.
                self.clipboard = Some(unsafe { Clipboard::new(wayland.display) });
                self.shell.activation =
                    Some(unsafe { crate::platform::activation::Activation::new(wayland.display) });
            }
        }
        self.ensure_window(event_loop);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // While the connection they share is still open.
        #[cfg(all(unix, not(target_vendor = "apple")))]
        {
            drop(self.clipboard.take());
            drop(self.shell.activation.take());
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::Wake => self.inbox_ready = true,
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.close_window(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(_) => self.mark_all(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(view) = &mut self.view {
                    view.scale = scale_factor as f32;
                }
                self.mark_all();
            }
            WindowEvent::Focused(focused) => {
                if self.trace {
                    eprintln!("focused: {focused}");
                }
                if let Some(view) = &mut self.view {
                    view.focused = focused;
                    if !focused {
                        view.unfocused_at = Some(Instant::now());
                    }
                }
                if focused {
                    self.shell.attention = false;
                }
                if !focused {
                    self.close_menu();
                }
                self.sync_seen();
                self.mark_strip();
                self.mark_sidebar();
                self.mark_chat();
                self.mark_input();
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(view) = &mut self.view {
                    view.pointer = Point::new(
                        position.x as f32 / view.scale,
                        position.y as f32 / view.scale,
                    );
                }
                if self.trust.is_some() {
                    self.trust_pointer_moved();
                } else if let Some(point) = self.view.as_ref().map(|view| view.pointer)
                    && self.menu.is_some()
                {
                    self.menu_pointer_moved(point);
                    self.set_cursor(CursorIcon::Default);
                } else if !self.chrome_pointer_moved() {
                    if self.manager.is_some() {
                        self.manager_pointer_moved();
                    } else if self.welcome_visible() {
                        self.start_pointer_moved();
                    } else {
                        self.body_pointer_moved();
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(view) = &mut self.view {
                    view.pointer = Point::new(-1.0, -1.0);
                }
                self.clear_strip_hover();
                if self.hover != Hover::default() && self.selection.is_none_or(|s| !s.dragging) {
                    self.hover = Hover::default();
                    self.mark_sidebar();
                    self.mark_nicks();
                    self.mark_chat();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if self.trust.is_some() {
                    if state == ElementState::Pressed && button == MouseButton::Left {
                        self.trust_pointer_pressed();
                    }
                    return;
                }
                if state == ElementState::Pressed
                    && let Some(point) = self.view.as_ref().map(|view| view.pointer)
                    && self.menu_pointer_pressed(point, button == MouseButton::Right)
                {
                    return;
                }
                if self.chrome_pointer_button(state, button) {
                    return;
                }
                match state {
                    ElementState::Pressed if self.manager.is_some() => {
                        if button == MouseButton::Left {
                            self.manager_pointer_pressed();
                        }
                    }
                    ElementState::Pressed if self.welcome_visible() => {
                        if button == MouseButton::Left {
                            self.start_pointer_pressed();
                        }
                    }
                    ElementState::Pressed => self.body_pointer_pressed(button),
                    ElementState::Released if button == MouseButton::Left => {
                        self.body_pointer_released()
                    }
                    ElementState::Released => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.close_menu();
                if self.trust.is_some() {
                    return;
                }
                if !self.chrome_scroll(delta) {
                    let scale = self.view.as_ref().map_or(1.0, |view| view.scale);
                    let pixels = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y * 3.0 * 20.0,
                        MouseScrollDelta::PixelDelta(position) => position.y as f32 / scale,
                    };
                    if self.manager.is_some() {
                        self.manager_scroll(pixels);
                    } else if !self.welcome_visible() {
                        self.body_scroll(pixels);
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => self.key(&event),
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.editor().insert(&text);
                self.mark_input();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.quit {
            event_loop.exit();
            return;
        }
        // One budget for the wake's work: draining, then the channel
        // list's slice, so input waits at most this long (DESIGN.md §10).
        let deadline = Instant::now() + NET_BUDGET;
        if self.inbox_ready {
            self.drain_inbox(deadline);
        }
        self.desktop_inbox.arm();
        while let Some(event) = self.desktop_inbox.next() {
            self.desktop_event(event);
        }
        self.instance_inbox.arm();
        while let Some(request) = self.instance_inbox.next() {
            self.handed_over(request);
        }
        // What was typed into the find field or the channel filter applies,
        // a search waiting for history carries on, and the channel list
        // takes its next slice of work.
        self.find_edited();
        self.find_resume();
        self.channels_edited();
        self.step_channels(deadline);
        if self.quit {
            event_loop.exit();
            return;
        }
        self.ensure_window(event_loop);
        #[cfg(all(unix, not(target_vendor = "apple")))]
        if let Some(activation) = &mut self.shell.activation {
            activation.dispatch();
        }
        self.sync_tray();
        self.save_session();
        #[cfg(all(unix, not(target_vendor = "apple")))]
        if let Some(clipboard) = &mut self.clipboard {
            clipboard.dispatch();
            if self.check_clipboard == Some(false) && clipboard.ready() {
                self.check_clipboard = Some(true);
                eprintln!("clipboard: ready");
                self.paste();
                self.put_on_clipboard(String::from("RubyChat clipboard check \u{2713}"));
                eprintln!("clipboard: copied");
            }
        }
        while let Ok(text) = self.pasted.1.try_recv() {
            if self.check_clipboard.is_some() {
                eprintln!("clipboard: pasted {text:?}");
            }
            self.insert_pasted(&text);
        }
        if let Some(view) = &self.view
            && view.dirty.any()
        {
            view.window.request_redraw();
        }
        // Sleep until something happens; a connecting network's heartbeat is
        // the only clock.
        let heartbeat = self
            .animating()
            .then(|| Instant::now() + tab_strip::heartbeat_tick(self.started.elapsed()));
        event_loop.set_control_flow(match heartbeat.into_iter().chain(self.exit_at).min() {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }
}
