//! Core document model for the bson-editor app.
//!
//! Everything large happens here, in Rust: parsing, expansion of embedded JSON
//! strings, editing, and serialization. The webview only ever asks for the slice
//! of the tree it is about to paint, which is what keeps 10MB+ documents
//! interactive.

pub mod arena;
pub mod document;
pub mod error;
pub mod json_read;
pub mod json_write;
pub mod numfmt;
pub mod opts;

pub use arena::{Doc, Kind, Node, Span, Stats, MAX_DEPTH, NO_NODE};
pub use error::{Error, Result};
pub use opts::{Flavor, Format, Indent, JsonOptions, LoadOptions};

/// Version of the core crate, surfaced in the UI diagnostics panel.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
