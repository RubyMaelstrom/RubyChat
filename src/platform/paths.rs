//! Where RubyChat keeps its files: the XDG Base Directory Specification's
//! directories on Linux and the BSDs, `%APPDATA%` and `%LOCALAPPDATA%` on
//! Windows (DESIGN.md §11).

use std::path::PathBuf;

#[cfg(not(windows))]
const DIRECTORY: &str = "rubychat";

/// `$XDG_STATE_HOME/rubychat`: state that should outlive a restart but isn't
/// worth backing up, such as the recently used servers.
pub fn state_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        windows_dir("LOCALAPPDATA")
    }
    #[cfg(not(windows))]
    {
        xdg_dir("XDG_STATE_HOME", ".local/state")
    }
}

/// `$XDG_DATA_HOME/rubychat`: chat logs.
pub fn data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        windows_dir("LOCALAPPDATA")
    }
    #[cfg(not(windows))]
    {
        xdg_dir("XDG_DATA_HOME", ".local/share")
    }
}

/// `$XDG_CACHE_HOME/rubychat`: what can be worked out again, such as the
/// font index.
pub fn cache_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        windows_dir("LOCALAPPDATA").map(|dir| dir.join("cache"))
    }
    #[cfg(not(windows))]
    {
        xdg_dir("XDG_CACHE_HOME", ".cache")
    }
}

/// Where fonts are installed, most important first: `fonts` under
/// `$XDG_DATA_HOME` and each of `$XDG_DATA_DIRS` (by default
/// `/usr/local/share/:/usr/share/`), then the old `~/.fonts`; the system
/// and per-user font folders on Windows.
pub fn font_dirs() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut dirs = Vec::new();
        if let Some(windows) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(windows).join("Fonts"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft/Windows/Fonts"));
        }
        dirs
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute());
        let mut dirs = Vec::new();
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
        dirs.extend(data_home.map(|dir| dir.join("fonts")));
        let data_dirs = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or_else(|| String::from("/usr/local/share/:/usr/share/"));
        dirs.extend(
            data_dirs
                .split(':')
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|dir| dir.join("fonts")),
        );
        dirs.extend(home.map(|home| home.join(".fonts")));
        let mut seen = Vec::new();
        dirs.retain(|dir| {
            let new = !seen.contains(dir);
            seen.push(dir.clone());
            new
        });
        dirs
    }
}

/// `$XDG_CONFIG_HOME/rubychat`: settings and saved networks.
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        windows_dir("APPDATA")
    }
    #[cfg(not(windows))]
    {
        xdg_dir("XDG_CONFIG_HOME", ".config")
    }
}

/// The variable's directory if set to an absolute path; a relative path is
/// invalid and ignored, as is an empty value, and the default under `$HOME`
/// applies (XDG Base Directory Specification, "Environment variables").
#[cfg(not(windows))]
fn xdg_dir(variable: &str, default_under_home: &str) -> Option<PathBuf> {
    let base = std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            let home = PathBuf::from(std::env::var_os("HOME")?);
            home.is_absolute().then(|| home.join(default_under_home))
        })?;
    Some(base.join(DIRECTORY))
}

#[cfg(windows)]
fn windows_dir(variable: &str) -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os(variable)?);
    base.is_absolute().then(|| base.join("RubyChat"))
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn relative_and_empty_variables_fall_back_to_home() {
        // The variable names are unique to this test, so no other test races
        // on them.
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        // SAFETY: tests touching these variables don't run in parallel.
        unsafe {
            std::env::set_var("RUBYCHAT_TEST_XDG", "relative/path");
        }
        assert_eq!(
            xdg_dir("RUBYCHAT_TEST_XDG", ".local/state"),
            Some(home.join(".local/state/rubychat"))
        );
        unsafe {
            std::env::set_var("RUBYCHAT_TEST_XDG", "/tmp/xdg-state");
        }
        assert_eq!(
            xdg_dir("RUBYCHAT_TEST_XDG", ".local/state"),
            Some(PathBuf::from("/tmp/xdg-state/rubychat"))
        );
        unsafe {
            std::env::set_var("RUBYCHAT_TEST_XDG", "");
        }
        assert_eq!(
            xdg_dir("RUBYCHAT_TEST_XDG", ".config"),
            Some(home.join(".config/rubychat"))
        );
        unsafe {
            std::env::remove_var("RUBYCHAT_TEST_XDG");
        }
        assert_eq!(
            xdg_dir("RUBYCHAT_TEST_XDG", ".config"),
            Some(home.join(".config/rubychat"))
        );
    }
}
