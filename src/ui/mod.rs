//! RubyChat's interface components. Each paints with the renderer's
//! `Painter` from plain data and has its own geometry for hit testing, so it
//! can be tested and snapshotted without a window.

pub mod branding;
#[cfg(test)]
mod budgets;
pub mod chat_view;
pub mod colors;
pub mod context_menu;
pub mod header;
pub mod input;
pub mod nick_list;
pub mod sidebar;
pub mod tab_strip;
pub mod theme;
pub mod welcome;
