//! The Windows clipboard (DESIGN.md §12.5): text as `CF_UNICODETEXT`,
//! read and written by a thread of RubyChat's own. Its message-only window
//! owns what RubyChat copies, since `SetClipboardData` fails without an
//! owner, and its message loop answers what the system sends an owner. An
//! application slow to hand over what it copied stalls that thread, never
//! the UI.

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{OnceLock, mpsc};
use std::time::Duration;

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostMessageW, TranslateMessage, WM_APP,
};

/// Jobs waiting for the thread, at most; past this they're dropped.
const JOBS: usize = 16;
/// The most of a paste read, in UTF-16 units: 1 MiB, as on Wayland.
const MAX_PASTE: usize = 512 * 1024;
/// Another application may hold the clipboard open for a moment.
const OPEN_TRIES: u32 = 10;
const OPEN_RETRY: Duration = Duration::from_millis(10);
/// Posted to the window: jobs are waiting.
const WM_JOBS: u32 = WM_APP;

enum Job {
    Copy(String),
    Paste(Box<dyn FnOnce(String) + Send>),
}

struct Worker {
    jobs: mpsc::SyncSender<Job>,
    /// The thread's window, once it has one.
    window: &'static AtomicPtr<core::ffi::c_void>,
}

/// The thread, started on first use; `None` if it couldn't start.
fn worker() -> Option<&'static Worker> {
    static WORKER: OnceLock<Option<Worker>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            static WINDOW: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
            let (jobs, receiver) = mpsc::sync_channel(JOBS);
            std::thread::Builder::new()
                .name(String::from("rubychat-clipboard"))
                .spawn(move || run(&receiver, &WINDOW))
                .ok()?;
            Some(Worker {
                jobs,
                window: &WINDOW,
            })
        })
        .as_ref()
}

/// Queues `job`; false if the thread is missing or behind.
fn submit(job: Job) -> bool {
    let Some(worker) = worker() else {
        return false;
    };
    if worker.jobs.try_send(job).is_err() {
        return false;
    }
    // Before the window exists, the thread takes the job on starting.
    let window = worker.window.load(Ordering::SeqCst);
    if !window.is_null() {
        // SAFETY: the window lives as long as the process.
        unsafe { PostMessageW(window, WM_JOBS, 0, 0) };
    }
    true
}

/// Puts `text` on the clipboard, on the clipboard thread.
pub fn copy(text: String) -> bool {
    submit(Job::Copy(text))
}

/// Reads the clipboard's text on the clipboard thread and hands it to
/// `deliver` there: empty if there is none. False if it can't be asked.
pub fn paste(deliver: impl FnOnce(String) + Send + 'static) -> bool {
    submit(Job::Paste(Box::new(deliver)))
}

fn run(jobs: &mpsc::Receiver<Job>, slot: &AtomicPtr<core::ffi::c_void>) {
    let window = match super::hidden_window("RubyChat.Clipboard", Some(DefWindowProcW), true) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("rubychat: the clipboard has no window: {error}");
            return;
        }
    };
    slot.store(window, Ordering::SeqCst);
    take(jobs, window);
    // SAFETY: a zeroed MSG is a valid value of the plain C struct.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: `message` outlives each call; the loop ends with the
    // process, or on WM_QUIT.
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
        if message.message == WM_JOBS {
            take(jobs, window);
        } else {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

/// Does the jobs waiting; of several copies only the last matters.
fn take(jobs: &mpsc::Receiver<Job>, window: HWND) {
    let mut copied = None;
    while let Ok(job) = jobs.try_recv() {
        match job {
            Job::Copy(text) => copied = Some(text),
            Job::Paste(deliver) => {
                if let Some(text) = copied.take() {
                    write(window, &text);
                }
                deliver(read(window).unwrap_or_default());
            }
        }
    }
    if let Some(text) = copied {
        write(window, &text);
    }
}

/// Opens the clipboard for `window`, waiting briefly for an application
/// that has it open.
fn open(window: HWND) -> bool {
    for attempt in 0..OPEN_TRIES {
        // SAFETY: the window is this thread's.
        if unsafe { OpenClipboard(window) } != 0 {
            return true;
        }
        if attempt + 1 < OPEN_TRIES {
            std::thread::sleep(OPEN_RETRY);
        }
    }
    false
}

/// `text` as the clipboard's: lines end in CR LF, then a NUL ("Standard
/// Clipboard Formats", CF_UNICODETEXT).
fn write(window: HWND, text: &str) {
    let mut units: Vec<u16> = Vec::with_capacity(text.len() + 1);
    let mut previous = 0u16;
    for unit in text.encode_utf16() {
        if unit == u16::from(b'\n') && previous != u16::from(b'\r') {
            units.push(u16::from(b'\r'));
        }
        units.push(unit);
        previous = unit;
    }
    units.push(0);
    if !open(window) {
        return;
    }
    // SAFETY: the clipboard is open for this thread. The memory is
    // movable, as SetClipboardData requires; it is filled while locked and
    // becomes the system's once SetClipboardData succeeds, or is freed.
    unsafe {
        EmptyClipboard();
        let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
        if !memory.is_null() {
            let target = GlobalLock(memory).cast::<u16>();
            if target.is_null() {
                GlobalFree(memory);
            } else {
                std::ptr::copy_nonoverlapping(units.as_ptr(), target, units.len());
                GlobalUnlock(memory);
                if SetClipboardData(u32::from(CF_UNICODETEXT), memory as HANDLE).is_null() {
                    GlobalFree(memory);
                }
            }
        }
        CloseClipboard();
    }
}

/// The clipboard's text, up to its NUL and at most `MAX_PASTE` units.
fn read(window: HWND) -> Option<String> {
    if !open(window) {
        return None;
    }
    // SAFETY: the clipboard is open for this thread; its handle is read
    // only while locked, within the size the system reports.
    unsafe {
        let mut text = None;
        let memory = GetClipboardData(u32::from(CF_UNICODETEXT));
        if !memory.is_null() {
            let source = GlobalLock(memory).cast::<u16>().cast_const();
            if !source.is_null() {
                let units = (GlobalSize(memory) / 2).min(MAX_PASTE);
                let all = std::slice::from_raw_parts(source, units);
                let end = all.iter().position(|&unit| unit == 0).unwrap_or(units);
                text = Some(String::from_utf16_lossy(&all[..end]));
                GlobalUnlock(memory);
            }
        }
        CloseClipboard();
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test --lib clipboard -- --ignored` on Windows (or Wine).
    #[test]
    #[ignore = "replaces the clipboard's text"]
    fn text_goes_on_the_clipboard_and_comes_back() {
        assert!(copy(String::from("ruby\nchat ✓")));
        let (sender, pasted) = mpsc::channel();
        assert!(paste(move |text| {
            let _ = sender.send(text);
        }));
        let text = pasted.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(text, "ruby\r\nchat ✓");
    }
}
