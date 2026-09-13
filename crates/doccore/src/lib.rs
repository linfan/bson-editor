//! Core document model for the bson-editor app.
//!
//! Everything large happens here, in Rust: parsing, expansion of embedded JSON
//! strings, editing, and serialization. The webview only ever asks for the slice
//! of the tree it is about to paint, which is what keeps 10MB+ documents
//! interactive.

pub mod arena;
pub mod bson_read;
pub mod bson_write;
pub mod document;
pub mod error;
pub mod expand;
pub mod json_read;
pub mod json_write;
pub mod magic;
pub mod numfmt;
pub mod opts;

#[cfg(test)]
mod testutil;

pub use arena::{Doc, Kind, Node, Span, Stats, MAX_DEPTH, NO_NODE};
pub use document::{load, load_text, looks_like_json, Saved};
pub use error::{Error, Result};
pub use expand::Expansion;
pub use magic::Framing;
pub use opts::{Flavor, Format, Indent, JsonOptions, LoadOptions, SaveOptions};

/// Version of the core crate, surfaced in the UI diagnostics panel.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
