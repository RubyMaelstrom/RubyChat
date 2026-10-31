//! Platform services behind small interfaces (DESIGN.md §3).

#[cfg(all(unix, not(target_vendor = "apple")))]
pub mod activation;
#[cfg(all(unix, not(target_vendor = "apple")))]
pub mod clipboard;
#[cfg(all(unix, not(target_vendor = "apple")))]
pub mod dbus;
pub mod desktop;
pub mod fonts;
pub mod instance;
pub mod mmap;
pub mod open;
pub mod paths;
pub mod time;
