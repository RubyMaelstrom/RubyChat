//! Platform services behind small interfaces (DESIGN.md §3).

#[cfg(all(unix, not(target_vendor = "apple")))]
pub mod clipboard;
pub mod open;
pub mod paths;
pub mod time;
