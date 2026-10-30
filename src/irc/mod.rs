//! The IRC protocol: messages, encodings, and (with the network thread) the
//! per-connection session. Governing texts: Modern IRC, IRCv3, RFC 1459 and
//! RFC 2812 (DESIGN.md §18).

pub mod casemap;
pub mod encoding;
pub mod format;
pub mod isupport;
pub mod message;
pub mod time;

#[cfg(test)]
mod vectors;

pub use encoding::{Fallback, decode_line};
pub use message::{EncodeError, Message, ParseError, Source, serialize, split_source};
