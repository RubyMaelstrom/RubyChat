//! Finding the session bus (D-Bus Specification, "Server Addresses",
//! "Unix Domain Sockets" and "Well-known Message Bus Instances"):
//! `DBUS_SESSION_BUS_ADDRESS` lists addresses to try in order, separated by
//! `;`; RubyChat connects to `unix:path=` and, on Linux, `unix:abstract=`.
//! Without the variable, the bus at `$XDG_RUNTIME_DIR/bus` is tried, where
//! systemd and dbus-broker sessions keep it.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    Path(PathBuf),
    /// A Linux abstract socket's name.
    Abstract(Vec<u8>),
}

/// The session bus's addresses, most preferred first.
pub fn session() -> Vec<Address> {
    match std::env::var("DBUS_SESSION_BUS_ADDRESS") {
        Ok(text) if !text.is_empty() && text != "autolaunch:" => parse(&text),
        _ => std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .map(|dir| vec![Address::Path(dir.join("bus"))])
            .unwrap_or_default(),
    }
}

/// The connectable Unix addresses in `text`; other transports and
/// malformed entries are skipped.
pub fn parse(text: &str) -> Vec<Address> {
    text.split(';')
        .filter_map(|entry| {
            let keys = entry.strip_prefix("unix:")?;
            let mut found = None;
            for pair in keys.split(',') {
                let (key, value) = pair.split_once('=')?;
                let value = unescape(value)?;
                match key {
                    "path" => found = Some(Address::Path(PathBuf::from(bytes_to_os(value)))),
                    "abstract" => found = Some(Address::Abstract(value)),
                    // Listenable-only keys make the address unusable here.
                    "dir" | "tmpdir" | "runtime" => return None,
                    _ => {}
                }
            }
            found
        })
        .collect()
}

/// "Value escaping": `%` and two hex digits stand for a byte; bytes
/// outside `[-0-9A-Za-z_/.\*]` must be escaped.
fn unescape(value: &str) -> Option<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                at += 3;
            }
            byte if byte.is_ascii_alphanumeric() || b"-_/.\\*".contains(&byte) => {
                out.push(byte);
                at += 1;
            }
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(unix)]
fn bytes_to_os(bytes: Vec<u8>) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(bytes)
}

#[cfg(not(unix))]
fn bytes_to_os(bytes: Vec<u8>) -> std::ffi::OsString {
    String::from_utf8_lossy(&bytes).into_owned().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_parse_in_order() {
        assert_eq!(
            parse("unix:path=/run/user/1000/bus"),
            [Address::Path(PathBuf::from("/run/user/1000/bus"))]
        );
        assert_eq!(
            parse(
                "tcp:host=localhost,port=1;unix:abstract=/tmp/dbus-U8OSdmf7,guid=0123;unix:path=/tmp/a%20b"
            ),
            [
                Address::Abstract(b"/tmp/dbus-U8OSdmf7".to_vec()),
                Address::Path(PathBuf::from("/tmp/a b")),
            ]
        );
    }

    #[test]
    fn listen_only_and_malformed_addresses_are_skipped() {
        assert!(parse("unix:tmpdir=/tmp").is_empty());
        assert!(parse("unix:runtime=yes").is_empty());
        assert!(parse("unix:path=/tmp/a b").is_empty(), "unescaped space");
        assert!(parse("unix:path=/tmp/%2").is_empty(), "short escape");
        assert!(parse("unix:path").is_empty());
        assert!(parse("").is_empty());
    }
}
