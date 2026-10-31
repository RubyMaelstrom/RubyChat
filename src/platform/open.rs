//! Opening links with the system's handler (`xdg-open`, or the shell on
//! Windows), on a short-lived thread so the UI never waits for a process
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
    let mut words: Vec<String> = command.split_whitespace().map(String::from).collect();
    std::thread::Builder::new()
        .name(String::from("rubychat-open"))
        .spawn(move || {
            #[cfg(windows)]
            let system = "explorer";
            #[cfg(not(windows))]
            let system = "xdg-open";
            if words.is_empty() {
                words.push(String::from(system));
            }
            let result = std::process::Command::new(&words[0])
                .args(&words[1..])
                .arg(&url)
                .status();
            if let Err(error) = result {
                eprintln!("rubychat: opening {url}: {error}");
            }
        })
        .is_ok()
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
