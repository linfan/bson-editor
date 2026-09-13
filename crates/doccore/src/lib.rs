//! Core document model for the bson-editor app.
//!
//! Everything large happens here, in Rust: parsing, expansion of embedded JSON
//! strings, editing, and serialization. The webview only ever asks for the
//! slice of the tree it is about to paint, which is what keeps 10MB+ documents
//! interactive.

/// Version of the core crate, surfaced in the UI diagnostics panel.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert!(!super::version().is_empty());
    }
}
