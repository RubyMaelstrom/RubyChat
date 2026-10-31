//! The desktop thread on Windows (DESIGN.md §12.1, §12.3): RubyChat's
//! icon in the notification area (`Shell_NotifyIconW` with
//! `NOTIFYICON_VERSION_4`), its menu (`TrackPopupMenuEx`) and the icon's
//! balloon notifications, which Windows 10 and later show as toasts.
//!
//! The thread owns a hidden top-level window. A message-only one can't be
//! made the foreground window, which the menu needs to close when you
//! click elsewhere (TrackPopupMenuEx, Remarks), and doesn't hear the
//! `TaskbarCreated` broadcast that says Explorer restarted and the icon
//! must be added again.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::mpsc;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_BITFIELDS, BITMAPINFO, BITMAPV5HEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, HBITMAP,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_LARGE_ICON, NIIF_USER, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_BALLOONUSERCLICK, NIN_SELECT,
    NOTIFY_ICON_DATA_FLAGS, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetMessageW, GetSystemMetrics, HICON, ICONINFO, MF_CHECKED,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterWindowMessageW,
    SM_CXICON, SM_CXSMICON, SM_MENUDROPALIGNMENT, SetForegroundWindow, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, TrackPopupMenuEx,
    TranslateMessage, WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_NULL,
};

use super::{
    Artwork, Attention, DesktopCommand, DesktopEvent, MenuAction, Notice, NoticeKey, TrayState,
};
use crate::platform::win32::{fill, hidden_window, wide};

/// Posted to the window: commands are waiting.
const WM_WAKE: u32 = WM_APP;
/// The icon's callback message.
const WM_ICON: u32 = WM_APP + 1;
/// NIN_SELECT | NINF_KEY: chosen with the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
/// RubyChat's one icon.
const ICON_ID: u32 = 1;
/// Lines a balloon shows, newest last, and buffers remembered at once.
const LINES: usize = 3;
const TRACKED: usize = 32;

/// The menu's items, as `TrackPopupMenuEx` returns them.
const SHOW_HIDE: usize = 1;
const MARK_ALL_READ: usize = 2;
const AWAY: usize = 3;
const QUIT: usize = 4;

/// The thread's window, for waking it from others.
#[derive(Clone, Default)]
pub(super) struct Waker(Arc<AtomicPtr<core::ffi::c_void>>);

impl Waker {
    /// Commands queued before the window exists are taken when it does.
    pub(super) fn wake(&self) {
        let window = self.0.load(Ordering::SeqCst);
        if !window.is_null() {
            // SAFETY: a window handle the thread made; posting to one that
            // ended fails harmlessly.
            unsafe { PostMessageW(window, WM_WAKE, 0, 0) };
        }
    }
}

thread_local! {
    /// The thread's tray, which the window procedure reaches.
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

pub(super) fn run(
    commands: mpsc::Receiver<DesktopCommand>,
    mut sink: Box<dyn FnMut(DesktopEvent) + Send>,
    artwork: Artwork,
    waker: Waker,
) {
    let window = match hidden_window("RubyChat.Tray", Some(procedure), false) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("rubychat: the tray has no window: {error}");
            sink(DesktopEvent::Tray(false));
            return;
        }
    };
    let created = wide("TaskbarCreated");
    // SAFETY: the name is NUL-terminated and outlives the call.
    let taskbar_created = unsafe { RegisterWindowMessageW(created.as_ptr()) };
    let mut tray = Tray {
        window,
        commands,
        sink,
        artwork,
        state: TrayState::default(),
        added: None,
        icon: std::ptr::null_mut(),
        logo: std::ptr::null_mut(),
        taskbar_created,
        notices: HashMap::new(),
        clock: 0,
        balloon: None,
    };
    tray.icon = tray.tray_icon(Attention::None);
    tray.logo = icon(&(tray.artwork.logo)(large_size()), large_size());
    tray.add();
    TRAY.with(|cell| *cell.borrow_mut() = Some(tray));
    waker.0.store(window, Ordering::SeqCst);
    // Commands sent before the window was known.
    dispatch(window, WM_WAKE, 0, 0);
    // SAFETY: a zeroed MSG is a valid value of the plain C struct.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: `message` outlives each call; WM_QUIT ends the loop.
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    waker.0.store(std::ptr::null_mut(), Ordering::SeqCst);
    TRAY.with(|cell| drop(cell.borrow_mut().take()));
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    dispatch(window, message, wparam, lparam)
}

/// What the window procedure does after letting go of the tray.
enum Then {
    Done(LRESULT),
    Default,
    Menu {
        x: i32,
        y: i32,
        items: Vec<(usize, u32, &'static str)>,
    },
}

fn dispatch(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let then = TRAY.with(|cell| match cell.try_borrow_mut() {
        Ok(mut tray) => match tray.as_mut() {
            Some(tray) => tray.message(message, wparam, lparam),
            None => Then::Default,
        },
        // Reentered, from a call that waits on Explorer or from the
        // shutdown's DestroyWindow: our own messages come again afterwards,
        // the window's end ends the loop, and the rest get the default.
        Err(_) => match message {
            WM_WAKE | WM_ICON => {
                // SAFETY: the window is this thread's.
                unsafe { PostMessageW(window, message, wparam, lparam) };
                Then::Done(0)
            }
            WM_DESTROY => {
                // SAFETY: a plain call on this thread.
                unsafe { PostQuitMessage(0) };
                Then::Done(0)
            }
            _ => Then::Default,
        },
    });
    match then {
        Then::Done(result) => result,
        // SAFETY: the arguments are the window procedure's own.
        Then::Default => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        Then::Menu { x, y, items } => {
            if let Some(chosen) = show_menu(window, x, y, &items) {
                TRAY.with(|cell| {
                    if let Ok(mut tray) = cell.try_borrow_mut()
                        && let Some(tray) = tray.as_mut()
                    {
                        tray.chose(chosen);
                    }
                });
            }
            0
        }
    }
}

struct Entry {
    title: String,
    lines: VecDeque<(String, String)>,
    count: u32,
    touched: u64,
}

struct Tray {
    window: HWND,
    commands: mpsc::Receiver<DesktopCommand>,
    sink: Box<dyn FnMut(DesktopEvent) + Send>,
    artwork: Artwork,
    state: TrayState,
    /// Whether the notification area took the icon, once tried.
    added: Option<bool>,
    icon: HICON,
    logo: HICON,
    taskbar_created: u32,
    /// Each buffer's latest lines, for its balloon.
    notices: HashMap<NoticeKey, Entry>,
    clock: u64,
    /// The buffer of the balloon shown last, which a click opens.
    balloon: Option<NoticeKey>,
}

impl Tray {
    fn message(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Then {
        match message {
            WM_WAKE => {
                while let Ok(command) = self.commands.try_recv() {
                    self.command(command);
                }
                Then::Done(0)
            }
            WM_ICON => {
                // NOTIFYICON_VERSION_4: the event in LOWORD(lParam), the
                // anchor point in wParam.
                match lparam as u32 & 0xffff {
                    NIN_SELECT | NIN_KEYSELECT => (self.sink)(DesktopEvent::Activate(None)),
                    WM_CONTEXTMENU => {
                        let x = (wparam & 0xffff) as u16 as i16 as i32;
                        let y = ((wparam >> 16) & 0xffff) as u16 as i16 as i32;
                        return Then::Menu {
                            x,
                            y,
                            items: self.menu_items(),
                        };
                    }
                    NIN_BALLOONUSERCLICK => {
                        if let Some(key) = self.balloon.clone() {
                            (self.sink)(DesktopEvent::Open(key, None));
                        }
                    }
                    _ => {}
                }
                Then::Done(0)
            }
            WM_DESTROY => {
                // SAFETY: a plain call on this thread.
                unsafe { PostQuitMessage(0) };
                Then::Done(0)
            }
            message if message == self.taskbar_created && message != 0 => {
                self.added = Some(false);
                self.add();
                Then::Done(0)
            }
            _ => Then::Default,
        }
    }

    fn command(&mut self, command: DesktopCommand) {
        match command {
            DesktopCommand::Tray(state) => {
                if state.attention != self.state.attention {
                    let icon = self.tray_icon(state.attention);
                    if !icon.is_null() {
                        destroy(self.icon);
                        self.icon = icon;
                    }
                }
                self.state = state;
                if self.added == Some(true) {
                    let mut data = self.data(NIF_ICON | NIF_TIP | NIF_SHOWTIP);
                    fill(&mut data.szTip, &self.state.tooltip);
                    // SAFETY: the struct outlives the call.
                    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
                }
            }
            DesktopCommand::Notify(notice) => self.notify(notice),
            DesktopCommand::Withdraw(key) => {
                self.notices.remove(&key);
                if self.balloon.as_ref() == Some(&key) {
                    self.withdraw_balloon();
                }
            }
            DesktopCommand::Shutdown => {
                self.withdraw_balloon();
                let data = self.data(0);
                // SAFETY: the struct outlives the call, and the window is
                // this thread's.
                unsafe {
                    Shell_NotifyIconW(NIM_DELETE, &data);
                    DestroyWindow(self.window);
                }
                destroy(self.icon);
                destroy(self.logo);
                self.icon = std::ptr::null_mut();
                self.logo = std::ptr::null_mut();
            }
        }
    }

    /// Adds the icon, or adds it again for a new taskbar, and says whether
    /// there's a tray when that changes.
    fn add(&mut self) {
        let mut data = self.data(NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP);
        data.uCallbackMessage = WM_ICON;
        fill(&mut data.szTip, &self.state.tooltip);
        // SAFETY: the struct outlives each call. NIM_SETVERSION follows
        // every NIM_ADD (Shell_NotifyIconW).
        let added = unsafe {
            let added = Shell_NotifyIconW(NIM_ADD, &data) != 0;
            if added {
                data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                Shell_NotifyIconW(NIM_SETVERSION, &data);
            }
            added
        };
        if self.added != Some(added) {
            self.added = Some(added);
            (self.sink)(DesktopEvent::Tray(added));
        }
    }

    fn data(&self, flags: NOTIFY_ICON_DATA_FLAGS) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.window,
            uID: ICON_ID,
            uFlags: flags,
            hIcon: self.icon,
            ..Default::default()
        }
    }

    fn tray_icon(&self, attention: Attention) -> HICON {
        let size = small_size();
        icon(&(self.artwork.tray)(size, attention), size)
    }

    /// A buffer's balloon, with its newest lines and a count of earlier
    /// ones, as on Linux.
    fn notify(&mut self, notice: Notice) {
        self.clock += 1;
        if !self.notices.contains_key(&notice.key) && self.notices.len() >= TRACKED {
            let oldest = self
                .notices
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.notices.remove(&oldest);
            }
        }
        let entry = self.notices.entry(notice.key.clone()).or_insert(Entry {
            title: String::new(),
            lines: VecDeque::new(),
            count: 0,
            touched: 0,
        });
        entry.title = clean(&notice.title);
        if entry.lines.len() == LINES {
            entry.lines.pop_front();
        }
        entry
            .lines
            .push_back((clean(&notice.nick), clean(&notice.text)));
        entry.count = entry.count.saturating_add(1);
        entry.touched = self.clock;
        let earlier = entry.count as usize - entry.lines.len().min(entry.count as usize);
        let mut body = Vec::new();
        if earlier > 0 {
            body.push(format!("… {earlier} earlier"));
        }
        for (nick, text) in &entry.lines {
            body.push(if nick.is_empty() {
                text.clone()
            } else {
                format!("{nick}: {text}")
            });
        }
        let title = entry.title.clone();
        if self.added != Some(true) {
            return;
        }
        let mut data = self.data(NIF_INFO);
        fill(&mut data.szInfoTitle, &title);
        fill(&mut data.szInfo, &body.join("\n"));
        data.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
        data.hBalloonIcon = self.logo;
        // SAFETY: the struct outlives the call.
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } != 0 {
            self.balloon = Some(notice.key);
        }
    }

    /// "To remove a balloon notification from the UI ... set the NIF_INFO
    /// flag in uFlags and set szInfo to an empty string."
    fn withdraw_balloon(&mut self) {
        if self.balloon.take().is_some() && self.added == Some(true) {
            let data = self.data(NIF_INFO);
            // SAFETY: the struct outlives the call.
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
        }
    }

    fn menu_items(&self) -> Vec<(usize, u32, &'static str)> {
        let show = if self.state.window_shown {
            "Hide RubyChat"
        } else {
            "Show RubyChat"
        };
        let unread = if self.state.attention == Attention::None {
            MF_GRAYED
        } else {
            0
        };
        let away = if self.state.away { MF_CHECKED } else { 0 };
        vec![
            (SHOW_HIDE, MF_STRING, show),
            (MARK_ALL_READ, MF_STRING | unread, "Mark all read"),
            (AWAY, MF_STRING | away, "Away"),
            (0, MF_SEPARATOR, ""),
            (QUIT, MF_STRING, "Quit RubyChat"),
        ]
    }

    fn chose(&mut self, item: usize) {
        let action = match item {
            SHOW_HIDE => MenuAction::ShowHide,
            MARK_ALL_READ => MenuAction::MarkAllRead,
            AWAY => MenuAction::Away,
            QUIT => MenuAction::Quit,
            _ => return,
        };
        (self.sink)(DesktopEvent::Menu(action));
    }
}

/// Shows the menu at the icon and returns the item chosen. The window
/// takes the foreground first, and a null message after lets the menu
/// close properly (TrackPopupMenuEx, Remarks).
fn show_menu(window: HWND, x: i32, y: i32, items: &[(usize, u32, &'static str)]) -> Option<usize> {
    // SAFETY: the menu is made, shown and destroyed here; every label is
    // NUL-terminated and outlives the calls that read it.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return None;
        }
        let labels: Vec<Vec<u16>> = items.iter().map(|(_, _, label)| wide(label)).collect();
        for ((id, flags, _), label) in items.iter().zip(&labels) {
            let text = if *flags & MF_SEPARATOR != 0 {
                std::ptr::null()
            } else {
                label.as_ptr()
            };
            AppendMenuW(menu, *flags, *id, text);
        }
        let side = if GetSystemMetrics(SM_MENUDROPALIGNMENT) != 0 {
            TPM_RIGHTALIGN
        } else {
            TPM_LEFTALIGN
        };
        SetForegroundWindow(window);
        let chosen = TrackPopupMenuEx(
            menu,
            side | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
            x,
            y,
            window,
            std::ptr::null(),
        );
        PostMessageW(window, WM_NULL, 0, 0);
        DestroyMenu(menu);
        (chosen > 0).then_some(chosen as usize)
    }
}

/// The notification area's icon size, and a balloon's.
fn small_size() -> u32 {
    // SAFETY: a plain call.
    (unsafe { GetSystemMetrics(SM_CXSMICON) }).clamp(16, 64) as u32
}

fn large_size() -> u32 {
    // SAFETY: a plain call.
    (unsafe { GetSystemMetrics(SM_CXICON) }).clamp(32, 128) as u32
}

/// An icon from straight RGBA rows: a 32-bit DIB whose alpha the system
/// blends, with an empty mask beside it. Null if it can't be made.
fn icon(rgba: &[u8], size: u32) -> HICON {
    let pixels = (size * size) as usize;
    if rgba.len() < pixels * 4 {
        return std::ptr::null_mut();
    }
    let header = BITMAPV5HEADER {
        bV5Size: size_of::<BITMAPV5HEADER>() as u32,
        bV5Width: size as i32,
        // Negative: rows top to bottom.
        bV5Height: -(size as i32),
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_BITFIELDS,
        bV5RedMask: 0x00ff_0000,
        bV5GreenMask: 0x0000_ff00,
        bV5BlueMask: 0x0000_00ff,
        bV5AlphaMask: 0xff00_0000,
        ..Default::default()
    };
    // SAFETY: the header describes `size`² 32-bit pixels, which the DIB
    // section holds and which are written within that; the bitmaps are
    // deleted once the icon has copied them (CreateIconIndirect, Remarks).
    unsafe {
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color: HBITMAP = CreateDIBSection(
            std::ptr::null_mut(),
            (&header as *const BITMAPV5HEADER).cast::<BITMAPINFO>(),
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if color.is_null() || bits.is_null() {
            return std::ptr::null_mut();
        }
        let target = std::slice::from_raw_parts_mut(bits.cast::<u32>(), pixels);
        for (pixel, &[red, green, blue, alpha]) in target.iter_mut().zip(rgba.as_chunks::<4>().0) {
            *pixel = u32::from(alpha) << 24
                | u32::from(red) << 16
                | u32::from(green) << 8
                | u32::from(blue);
        }
        // Monochrome rows are WORD-aligned; zeros leave every pixel to the
        // color bitmap's alpha.
        let mask_bits = vec![0u8; size.div_ceil(16) as usize * 2 * size as usize];
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, mask_bits.as_ptr().cast());
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        if !mask.is_null() {
            DeleteObject(mask);
        }
        icon
    }
}

fn destroy(icon: HICON) {
    if !icon.is_null() {
        // SAFETY: an icon this thread made and no longer shows.
        unsafe { DestroyIcon(icon) };
    }
}

/// Control characters as spaces; Windows cuts what doesn't fit.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn tray(size: u32, _: Attention) -> Vec<u8> {
        vec![200; (size * size * 4) as usize]
    }

    fn logo(size: u32) -> Vec<u8> {
        vec![100; (size * size * 4) as usize]
    }

    /// `cargo test --lib desktop -- --ignored` on Windows (or Wine).
    #[test]
    #[ignore = "shows a tray icon and a notification for a moment"]
    fn the_tray_answers_clicks_and_ends_on_shutdown() {
        let (commands, receiver) = mpsc::sync_channel(16);
        let (sender, events) = mpsc::channel();
        let waker = Waker::default();
        let window = waker.clone();
        let artwork = Artwork { tray, logo };
        let thread = std::thread::spawn(move || {
            run(
                receiver,
                Box::new(move |event| {
                    let _ = sender.send(event);
                }),
                artwork,
                window,
            )
        });
        let next = || events.recv_timeout(Duration::from_secs(5)).unwrap();
        let shown = next();
        assert!(matches!(shown, DesktopEvent::Tray(_)), "{shown:?}");
        let key: NoticeKey = (1, String::from("#rust"));
        commands
            .send(DesktopCommand::Tray(TrayState {
                attention: Attention::Highlight,
                window_shown: true,
                away: false,
                tooltip: String::from("RubyChat · 1 highlight"),
            }))
            .unwrap();
        commands
            .send(DesktopCommand::Notify(Notice {
                key: key.clone(),
                title: String::from("#rust · Libera.Chat"),
                nick: String::from("alice"),
                text: String::from("ruby: hello"),
            }))
            .unwrap();
        waker.wake();
        // A click on the icon, then on the balloon, as the shell posts them.
        let hwnd = waker.0.load(Ordering::SeqCst);
        assert!(!hwnd.is_null());
        // SAFETY: the tray's window, alive until Shutdown.
        unsafe {
            PostMessageW(hwnd, WM_ICON, 0, NIN_SELECT as LPARAM);
        }
        assert_eq!(next(), DesktopEvent::Activate(None));
        if shown == DesktopEvent::Tray(true) {
            // SAFETY: as above.
            unsafe { PostMessageW(hwnd, WM_ICON, 0, NIN_BALLOONUSERCLICK as LPARAM) };
            assert_eq!(next(), DesktopEvent::Open(key.clone(), None));
        }
        commands.send(DesktopCommand::Withdraw(key)).unwrap();
        commands.send(DesktopCommand::Shutdown).unwrap();
        waker.wake();
        thread.join().unwrap();
    }
}
