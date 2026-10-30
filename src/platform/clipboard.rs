//! The Wayland clipboard: copy and paste through `wl_data_device` (Wayland
//! core protocol, "wl_data_device", "wl_data_source", "wl_data_offer").
//!
//! It shares winit's connection but has an event queue of its own, which
//! the app dispatches without blocking each time the event loop wakes; the
//! loop wakes whenever the socket has news, ours included. Data moves
//! through pipes on short-lived threads with time limits, so a slow or
//! stuck client never holds up the interface (DESIGN.md §10).

use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wayland_client::backend::{Backend, ObjectId};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child,
};

/// Text types, most preferred first: what a copy offers and a paste asks
/// for.
const TEXT_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "TEXT",
    "STRING",
];
/// A paste reads at most this much.
const PASTE_LIMIT: usize = 1 << 20;
/// How long a transfer may stall before it is given up.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(3);
/// Types remembered per offer.
const TYPES_PER_OFFER: usize = 64;

pub struct Clipboard {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
}

#[derive(Default)]
struct State {
    seat: Option<WlSeat>,
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    /// A keyboard of our own, for serials: setting the selection needs the
    /// serial of the input that asked for it.
    keyboard: Option<WlKeyboard>,
    serial: Option<u32>,
    /// Offers announced, with their types.
    offers: HashMap<ObjectId, Vec<String>>,
    /// What the clipboard holds now, when another client offers it.
    selection: Option<WlDataOffer>,
    /// A drag passing over the window, which RubyChat doesn't take.
    drag: Option<WlDataOffer>,
    /// What RubyChat offers, while it holds the clipboard.
    source: Option<(WlDataSource, Arc<str>)>,
}

impl State {
    fn forget(&mut self, offer: WlDataOffer) {
        self.offers.remove(&offer.id());
        offer.destroy();
    }

    fn get_device(&mut self, handle: &QueueHandle<State>) {
        if let (None, Some(seat), Some(manager)) = (&self.device, &self.seat, &self.manager) {
            self.device = Some(manager.get_data_device(seat, handle, ()));
        }
    }
}

impl Clipboard {
    /// The clipboard on the Wayland connection `display`.
    ///
    /// # Safety
    ///
    /// `display` must be a live `wl_display` that outlives the clipboard:
    /// the event loop's own connection. Drop the clipboard before that
    /// connection closes.
    pub unsafe fn new(display: NonNull<c_void>) -> Self {
        // SAFETY: the caller keeps the display alive.
        let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let _ = connection.flush();
        Self {
            connection,
            queue,
            state: State::default(),
        }
    }

    /// Handles what has arrived; call it whenever the event loop wakes.
    pub fn dispatch(&mut self) {
        if let Err(error) = self.queue.dispatch_pending(&mut self.state) {
            eprintln!("rubychat: clipboard: {error}");
        }
        let _ = self.connection.flush();
    }

    /// Whether copying can work: a data device, and input to name.
    pub fn ready(&self) -> bool {
        self.state.device.is_some() && self.state.serial.is_some()
    }

    /// Whether RubyChat holds the clipboard, so pasting needs no transfer.
    pub fn owns(&self) -> bool {
        self.state.source.is_some()
    }

    /// Puts `text` on the clipboard; false if there's no data device or no
    /// input to name yet.
    pub fn copy(&mut self, text: String) -> bool {
        self.dispatch();
        let state = &mut self.state;
        let (Some(manager), Some(device), Some(serial)) =
            (&state.manager, &state.device, state.serial)
        else {
            return false;
        };
        let source = manager.create_data_source(&self.queue.handle(), ());
        for kind in TEXT_TYPES {
            source.offer(kind.to_string());
        }
        // A source serves one selection; the one it replaces will be
        // cancelled.
        device.set_selection(Some(&source), serial);
        state.source = Some((source, Arc::from(text)));
        let _ = self.connection.flush();
        true
    }

    /// Starts reading the clipboard's text, which `deliver` receives on
    /// another thread; false if the clipboard holds no text.
    pub fn paste(&mut self, deliver: impl FnOnce(String) + Send + 'static) -> bool {
        self.dispatch();
        let Some(offer) = &self.state.selection else {
            return false;
        };
        let offered = self
            .state
            .offers
            .get(&offer.id())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let Some(kind) = TEXT_TYPES
            .iter()
            .find(|kind| offered.iter().any(|o| o == *kind))
        else {
            return false;
        };
        let Some((read, write)) = pipe() else {
            return false;
        };
        offer.receive(kind.to_string(), write.as_fd());
        let _ = self.connection.flush();
        // The request holds its own copy until it is sent; ours goes, so the
        // reader sees the end once the other client closes its copy.
        drop(write);
        std::thread::Builder::new()
            .name(String::from("rubychat-paste"))
            .spawn(move || {
                let bytes = read_all(read, PASTE_LIMIT, TRANSFER_TIMEOUT);
                deliver(String::from_utf8_lossy(&bytes).into_owned());
            })
            .is_ok()
    }
}

impl Drop for Clipboard {
    /// Hands back what the clipboard took. It must drop while the
    /// connection is open, before the window that holds it closes.
    fn drop(&mut self) {
        let state = &mut self.state;
        if let Some((source, _)) = state.source.take() {
            source.destroy();
        }
        for offer in [state.selection.take(), state.drag.take()]
            .into_iter()
            .flatten()
        {
            offer.destroy();
        }
        if let Some(device) = state.device.take()
            && device.version() >= 2
        {
            device.release();
        }
        if let Some(keyboard) = state.keyboard.take()
            && keyboard.version() >= 3
        {
            keyboard.release();
        }
        if let Some(seat) = state.seat.take()
            && seat.version() >= 5
        {
            seat.release();
        }
        if let Some(manager) = state.manager.take()
            && manager.version() >= 4
        {
            manager.release();
        }
        let _ = self.connection.flush();
    }
}

/// A pipe, both ends closed on exec.
fn pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `pipe2` fills the two descriptors on success, and each is
    // owned once.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return None;
    }
    Some(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Waits until `fd` is ready for `events`, or `deadline` passes.
fn ready(fd: BorrowedFd<'_>, events: libc::c_short, deadline: Instant) -> bool {
    let left = deadline.saturating_duration_since(Instant::now());
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    let n = unsafe {
        libc::poll(
            &mut poll,
            1,
            left.as_millis().min(i32::MAX as u128) as libc::c_int,
        )
    };
    n > 0
}

/// Reads until the end, `limit` bytes, or a stall longer than `timeout`.
fn read_all(fd: OwnedFd, limit: usize, timeout: Duration) -> Vec<u8> {
    let mut file = File::from(fd);
    let mut out = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let mut deadline = Instant::now() + timeout;
    while out.len() < limit && ready(file.as_fd(), libc::POLLIN, deadline) {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&chunk[..n.min(limit - out.len())]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
        deadline = Instant::now() + timeout;
    }
    out
}

/// Writes `bytes` to a client that asked for them, then closes the pipe.
fn write_all(fd: OwnedFd, bytes: &[u8], timeout: Duration) {
    // Nonblocking, so a reader that stops reading can't hold the thread
    // past the timeout.
    // SAFETY: fcntl on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut file = File::from(fd);
    let mut rest = bytes;
    let mut deadline = Instant::now() + timeout;
    while !rest.is_empty() {
        match file.write(rest) {
            Ok(0) => break,
            Ok(n) => {
                rest = &rest[n..];
                deadline = Instant::now() + timeout;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if !ready(file.as_fd(), libc::POLLOUT, deadline) {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(5), handle, ()))
                }
                "wl_data_device_manager" if state.manager.is_none() => {
                    state.manager = Some(registry.bind(name, version.min(4), handle, ()));
                }
                _ => {}
            }
            state.get_device(handle);
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Keyboard)
            && state.keyboard.is_none()
        {
            state.keyboard = Some(seat.get_keyboard(handle, ()));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { serial, .. } | wl_keyboard::Event::Key { serial, .. } => {
                state.serial = Some(serial)
            }
            // The selection's offer is valid only while the window has the
            // keyboard; another comes with the focus.
            wl_keyboard::Event::Leave { .. } => {
                if let Some(offer) = state.selection.take() {
                    state.forget(offer);
                }
            }
            // The keymap's descriptor closes as the event drops.
            _ => {}
        }
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                state.offers.insert(id.id(), Vec::new());
            }
            wl_data_device::Event::Selection { id } => {
                if let Some(previous) = state.selection.take()
                    && id.as_ref() != Some(&previous)
                {
                    state.forget(previous);
                }
                state.selection = id;
            }
            wl_data_device::Event::Enter { id, .. } => {
                if let Some(previous) = std::mem::replace(&mut state.drag, id) {
                    state.forget(previous);
                }
            }
            wl_data_device::Event::Leave | wl_data_device::Event::Drop => {
                if let Some(offer) = state.drag.take() {
                    state.forget(offer);
                }
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        state: &mut Self,
        offer: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Some(types) = state.offers.get_mut(&offer.id())
            && types.len() < TYPES_PER_OFFER
        {
            types.push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let text = state
                    .source
                    .as_ref()
                    .filter(|(current, _)| current == source)
                    .map(|(_, text)| text.clone());
                if let Some(text) = text {
                    let _ = std::thread::Builder::new()
                        .name(String::from("rubychat-copy"))
                        .spawn(move || write_all(fd, text.as_bytes(), TRANSFER_TIMEOUT));
                }
            }
            wl_data_source::Event::Cancelled => {
                if state
                    .source
                    .as_ref()
                    .is_some_and(|(current, _)| current == source)
                {
                    state.source = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfers_end_at_their_limits() {
        let (read, write) = pipe().unwrap();
        let writer =
            std::thread::spawn(move || write_all(write, &[b'x'; 100_000], TRANSFER_TIMEOUT));
        assert_eq!(
            read_all(read, 1 << 20, TRANSFER_TIMEOUT).len(),
            100_000,
            "until the writer closes"
        );
        writer.join().unwrap();
        let (read, write) = pipe().unwrap();
        let started = Instant::now();
        assert!(
            read_all(read, 1 << 20, Duration::from_millis(50)).is_empty(),
            "a writer that never writes"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(write);
        let (read, write) = pipe().unwrap();
        let writer = std::thread::spawn(move || {
            write_all(write, &[b'y'; 300_000], Duration::from_millis(50))
        });
        let started = Instant::now();
        writer.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a reader that never reads"
        );
        drop(read);
    }
}
