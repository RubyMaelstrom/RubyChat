//! Opening links with the system's handler (`xdg-open`, or `ShellExecuteW`
//! on Windows), on a short-lived thread so the UI never waits for a process
//! to start (DESIGN.md §12.4).

use crate::model::links::SCHEMES;

/// Opens `url` if it has a scheme RubyChat allows; false otherwise. A
/// `command` (words, the address appended) opens it instead of the
/// system's handler.
pub fn open_url(url: &str, command: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !SCHEMES.iter().any(|scheme| lower.starts_with(scheme)) || url.chars().any(char::is_control)
    {
        return false;
    }
    let url = url.to_string();
    let words: Vec<String> = command.split_whitespace().map(String::from).collect();
    std::thread::Builder::new()
        .name(String::from("rubychat-open"))
        .spawn(move || {
            #[cfg(windows)]
            if words.is_empty() {
                return shell_open(&url);
            }
            let (program, arguments) = match words.split_first() {
                Some((program, arguments)) => (program.as_str(), arguments),
                None => ("xdg-open", &[][..]),
            };
            let result = std::process::Command::new(program)
                .args(arguments)
                .arg(&url)
                .status();
            if let Err(error) = result {
                eprintln!("rubychat: opening {url}: {error}");
            }
        })
        .is_ok()
}

/// The shell's handler for `url`'s scheme. Shell extensions may need COM,
/// in a single-threaded apartment (ShellExecuteW, Remarks).
#[cfg(windows)]
fn shell_open(url: &str) {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |text: &str| -> Vec<u16> { text.encode_utf16().chain([0]).collect() };
    let (verb, file) = (wide("open"), wide(url));
    // SAFETY: COM is set up and torn down on this thread alone; the strings
    // are NUL-terminated and outlive the call.
    let result = unsafe {
        let initialized = CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        ) >= 0;
        let result = ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
        if initialized {
            CoUninitialize();
        }
        result as isize
    };
    // "If the function succeeds, it returns a value greater than 32."
    if result <= 32 {
        eprintln!("rubychat: opening {url}: the shell answered {result}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_schemes_it_does_not_open() {
        assert!(!open_url("file:///etc/passwd", ""));
        assert!(!open_url("javascript:alert(1)", ""));
        assert!(!open_url("https://example.org/\nrm", "firefox"));
    }
}
