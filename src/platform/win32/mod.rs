//! Win32 pieces of the platform layer (DESIGN.md §3, §12): what the
//! Windows modules share, the clipboard, and `--register` for irc://
//! links. The tray lives with the other desktop services, and the
//! single-instance pipe with its Unix socket.

pub mod clipboard;
pub mod register;

use std::io;

use windows_sys::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, HWND_MESSAGE, RegisterClassExW, WNDCLASSEXW, WNDPROC, WS_EX_TOOLWINDOW,
    WS_POPUP,
};

/// `text` as a NUL-terminated UTF-16 string.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Copies `text` into a fixed UTF-16 field, cut to fit with its NUL and
/// never between the halves of a surrogate pair.
pub fn fill(field: &mut [u16], text: &str) {
    let Some(limit) = field.len().checked_sub(1) else {
        return;
    };
    let mut length = 0;
    for unit in text.encode_utf16().take(limit) {
        field[length] = unit;
        length += 1;
    }
    if length == limit && length > 0 && (0xd800..0xdc00).contains(&field[length - 1]) {
        length -= 1;
    }
    field[length..].fill(0);
}

/// A window of RubyChat's own on the calling thread, never shown, its
/// class registered on first use. A message-only window can't take the
/// foreground or hear broadcasts; the tray needs both, the clipboard
/// neither.
pub fn hidden_window(class: &str, procedure: WNDPROC, message_only: bool) -> io::Result<HWND> {
    let class = wide(class);
    // SAFETY: the class name outlives both calls, and a null module name
    // asks for this executable's handle.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let description = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: procedure,
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..Default::default()
        };
        if RegisterClassExW(&description) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(io::Error::last_os_error());
        }
        let (extended, parent) = if message_only {
            (0, HWND_MESSAGE)
        } else {
            (WS_EX_TOOLWINDOW, std::ptr::null_mut())
        };
        let window = CreateWindowExW(
            extended,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            parent,
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if window.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(window)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_cut_without_splitting_a_pair() {
        let mut field = [0xffffu16; 4];
        fill(&mut field, "ab");
        assert_eq!(field, [b'a' as u16, b'b' as u16, 0, 0]);
        // "a😀" is a, then a surrogate pair: a cut through the pair drops it.
        let mut field = [0xffffu16; 3];
        fill(&mut field, "a\u{1f600}");
        assert_eq!(field, [b'a' as u16, 0, 0]);
        let mut field = [0xffffu16; 4];
        fill(&mut field, "a\u{1f600}");
        assert_eq!(field[..2], [b'a' as u16, 0xd83d]);
        assert_eq!(field[3], 0);
    }
}
