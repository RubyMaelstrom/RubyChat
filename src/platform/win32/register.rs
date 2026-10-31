//! `rubychat --register` and `--unregister` on Windows: irc:// and ircs://
//! links open in this RubyChat, for the current user ("Registering an
//! Application to a URI Scheme"), as the desktop entry does on Linux.
//! RubyChat never registers itself (DESIGN.md §17).
//!
//! The command ends options with `--` before the quoted link, so a link
//! with a quote in it can't pass RubyChat an option ("Security Issues").

use std::path::Path;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
};

use super::wide;

/// The schemes, under `HKEY_CURRENT_USER\Software\Classes`, and what
/// Windows calls them.
const SCHEMES: [(&str, &str); 2] = [
    ("irc", "URL:IRC Protocol"),
    ("ircs", "URL:IRC over TLS Protocol"),
];

/// Points irc:// and ircs:// links at `executable`.
pub fn register(executable: &Path) -> Result<(), String> {
    let path = executable.display().to_string();
    if path.contains('"') {
        return Err(format!("can't register a path with a quote: {path}"));
    }
    for (scheme, description) in SCHEMES {
        let key = format!(r"Software\Classes\{scheme}");
        for (subkey, name, value) in [
            (key.clone(), None, description.to_string()),
            (key.clone(), Some("URL Protocol"), String::new()),
            (format!(r"{key}\DefaultIcon"), None, format!("\"{path}\",0")),
            (
                format!(r"{key}\shell\open\command"),
                None,
                format!("\"{path}\" -- \"%1\""),
            ),
        ] {
            set(&subkey, name, &value)?;
        }
    }
    Ok(())
}

/// Removes the schemes where they open a RubyChat, leaving another
/// application's registration alone.
pub fn unregister() -> Result<(), String> {
    for (scheme, _) in SCHEMES {
        let key = format!(r"Software\Classes\{scheme}");
        let command = get(&format!(r"{key}\shell\open\command")).unwrap_or_default();
        if !opens_rubychat(&command) {
            continue;
        }
        let wide_key = wide(&key);
        // SAFETY: the key name is NUL-terminated and outlives the call.
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide_key.as_ptr()) };
        if status != ERROR_SUCCESS {
            return Err(format!("can't remove {key}: error {status}"));
        }
    }
    Ok(())
}

/// Whether a registered command runs a `rubychat.exe`.
fn opens_rubychat(command: &str) -> bool {
    let program = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => command.split(' ').next().unwrap_or(""),
    };
    program
        .rsplit(['\\', '/'])
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("rubychat.exe"))
}

fn set(subkey: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    let subkey_wide = wide(subkey);
    let name_wide = name.map(wide);
    let data = wide(value);
    // SAFETY: every string is NUL-terminated and outlives the call; the
    // data's size counts its NUL, as REG_SZ wants.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            subkey_wide.as_ptr(),
            name_wide
                .as_ref()
                .map_or(std::ptr::null(), |name| name.as_ptr()),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!(
            r"can't write HKEY_CURRENT_USER\{subkey}: error {status}"
        ))
    }
}

/// A key's default string value.
fn get(subkey: &str) -> Option<String> {
    let subkey_wide = wide(subkey);
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: the buffer and its size describe the same memory, which
    // outlives the call.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey_wide.as_ptr(),
            std::ptr::null(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let units = (size as usize / 2).min(buffer.len());
    let end = buffer[..units]
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units);
    Some(String::from_utf16_lossy(&buffer[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_rubychat_registrations_are_removed() {
        assert!(opens_rubychat(r#""C:\Apps\RubyChat\rubychat.exe" -- "%1""#));
        assert!(opens_rubychat(r#"C:\rubychat.exe %1"#));
        assert!(!opens_rubychat(
            r#""C:\Program Files\HexChat\hexchat.exe" --url="%1""#
        ));
        assert!(!opens_rubychat(""));
    }

    /// `cargo test --lib register -- --ignored` on Windows (or Wine).
    #[test]
    #[ignore = "writes and removes irc:// and ircs:// under HKEY_CURRENT_USER"]
    fn registering_writes_both_schemes_and_unregistering_removes_them() {
        let executable = Path::new(r"C:\RubyChat test\rubychat.exe");
        register(executable).unwrap();
        assert_eq!(
            get(r"Software\Classes\ircs\shell\open\command").as_deref(),
            Some(r#""C:\RubyChat test\rubychat.exe" -- "%1""#)
        );
        assert_eq!(
            get(r"Software\Classes\irc").as_deref(),
            Some("URL:IRC Protocol")
        );
        unregister().unwrap();
        assert_eq!(get(r"Software\Classes\irc\shell\open\command"), None);
    }
}
