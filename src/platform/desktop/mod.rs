//! The desktop thread (DESIGN.md §5.1, §12): the tray icon and its menu,
//! and notifications. The app sends it commands without waiting and hears
//! back through a sink, as from the network thread. On Linux and the BSDs
//! it speaks to the session bus (`service`); elsewhere there is no tray
//! yet, and closing the window quits.

#[cfg(all(unix, not(target_vendor = "apple")))]
mod notify;
#[cfg(all(unix, not(target_vendor = "apple")))]
mod service;
#[cfg(all(unix, not(target_vendor = "apple")))]
mod tray;

use std::io;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use mio::{Poll, Token, Waker};

const WAKER: Token = Token(0);
/// Commands waiting for the thread, at most; past this they're dropped.
const COMMANDS: usize = 1024;

/// How much in RubyChat wants attention.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Attention {
    #[default]
    None,
    Unread,
    Highlight,
}

/// RubyChat's pictures for the desktop, drawn on the desktop thread as
/// straight RGBA, `pixels` square.
#[derive(Clone, Copy)]
pub struct Artwork {
    /// The tray icon, marked for the attention level.
    pub tray: fn(u32, Attention) -> Vec<u8>,
    /// The logo, for notifications.
    pub logo: fn(u32) -> Vec<u8>,
}

/// What the tray shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayState {
    pub attention: Attention,
    pub window_shown: bool,
    pub away: bool,
    pub tooltip: String,
}

/// A buffer's notification: its network's ID and its casefolded name.
pub type NoticeKey = (u64, String);

/// A line that notifies: a highlight or a private message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub key: NoticeKey,
    /// The buffer and network, such as `#rust · Libera.Chat`.
    pub title: String,
    /// Who said it; empty for RubyChat's own news.
    pub nick: String,
    /// What they said, without formatting.
    pub text: String,
}

pub enum DesktopCommand {
    Tray(TrayState),
    Notify(Notice),
    /// The buffer was seen: its notification goes.
    Withdraw(NoticeKey),
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopEvent {
    /// Whether a tray shows RubyChat's icon, so closing the window can
    /// leave RubyChat running there.
    Tray(bool),
    /// The icon was clicked, with the host's activation token if it gave
    /// one.
    Activate(Option<String>),
    Menu(MenuAction),
    /// A notification was clicked: show its buffer, with the server's
    /// activation token if it gave one.
    Open(NoticeKey, Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    ShowHide,
    MarkAllRead,
    Away,
    Quit,
}

pub struct Desktop {
    commands: mpsc::SyncSender<DesktopCommand>,
    waker: Arc<Waker>,
    thread: Option<JoinHandle<()>>,
}

impl Desktop {
    /// Starts the desktop thread. `sink` receives its events on that
    /// thread; it should hand them to the UI and wake it.
    pub fn spawn(
        sink: impl FnMut(DesktopEvent) + Send + 'static,
        artwork: Artwork,
    ) -> io::Result<Self> {
        let poll = Poll::new()?;
        let waker = Arc::new(Waker::new(poll.registry(), WAKER)?);
        let (commands, receiver) = mpsc::sync_channel(COMMANDS);
        #[cfg(all(unix, not(target_vendor = "apple")))]
        let thread = std::thread::Builder::new()
            .name(String::from("rubychat-desktop"))
            .spawn(move || service::run(poll, receiver, Box::new(sink), artwork))?;
        #[cfg(not(all(unix, not(target_vendor = "apple"))))]
        let thread = {
            let _ = (poll, receiver, sink, artwork);
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        };
        Ok(Self {
            commands,
            waker,
            thread: Some(thread),
        })
    }

    /// Queues `command` without waiting; false if the queue is full or the
    /// thread has ended (no session bus).
    pub fn send(&self, command: DesktopCommand) -> bool {
        let sent = self.commands.try_send(command).is_ok();
        if sent {
            let _ = self.waker.wake();
        }
        sent
    }
}

impl Drop for Desktop {
    /// Withdraws RubyChat's notifications and waits for the thread.
    fn drop(&mut self) {
        if self.commands.send(DesktopCommand::Shutdown).is_ok() {
            let _ = self.waker.wake();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
