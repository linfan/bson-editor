//! Document-level entry points: load bytes, hand back a [`Doc`], serialize it
//! again.

use crate::arena::Doc;
use crate::error::{Error, Result};
use crate::json_read::parse_json;
use crate::json_write::{to_json_string, write_json};
use crate::opts::{Format, JsonOptions, LoadOptions};

impl Doc {
    /// Parse JSON bytes into a standalone document. Takes ownership of the
    /// source so a large input is never copied.
    pub fn from_json(source: Vec<u8>) -> Result<Doc> {
        let mut doc = Doc::new(Format::Json);
        let root = parse_json(&source, &mut doc)?;
        doc.root = root;
        doc.document_count = 1;
        doc.source = source;
        doc.refresh_stats();
        Ok(doc)
    }

    /// Serialize a node subtree to a JSON string. This backs "copy as formatted
    /// JSON" in the UI (requirement 3).
    pub fn to_json(&self, node: u32, opts: &JsonOptions) -> Result<String> {
        self.node(node)?;
        to_json_string(self, node, opts)
    }

    /// Serialize a node subtree to bytes, without the UTF-8 conversion the
    /// clipboard path needs.
    pub fn to_json_bytes(&self, node: u32, opts: &JsonOptions, out: &mut Vec<u8>) -> Result<()> {
        self.node(node)?;
        write_json(self, node, opts, out)
    }
}

/// Detect whether `bytes` start a JSON document.
pub fn looks_like_json(bytes: &[u8]) -> bool {
    let first = bytes.iter().find(|b| !b.is_ascii_whitespace());
    matches!(
        first,
        Some(b'{' | b'[' | b'"' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n')
    )
}

/// Load a document from raw bytes, honoring the magic-byte and expand-depth
/// options.
pub fn load(source: Vec<u8>, opts: LoadOptions) -> Result<Doc> {
    if source.is_empty() {
        return Err(Error::Empty);
    }
    if source.len() > opts.max_input_bytes {
        return Err(Error::Json {
            message: format!(
                "input is {} bytes, over the {} byte limit",
                source.len(),
                opts.max_input_bytes
            ),
            line: 0,
            column: 0,
        });
    }
    let format = match opts.format {
        Some(f) => f,
        None if looks_like_json(&source) => Format::Json,
        None => return Err(Error::Unrecognized),
    };
    match format {
        Format::Json => Doc::from_json(source),
        Format::Bson => Err(Error::Unrecognized),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_detected_from_its_first_meaningful_byte() {
        assert!(looks_like_json(b"{\"a\":1}"));
        assert!(looks_like_json(b"\n\r\t  [1,2]"));
        assert!(looks_like_json(b"  null"));
        assert!(!looks_like_json(b""));
        assert!(!looks_like_json(&[0x1b, 0x00, 0x00, 0x00]));
    }

    #[test]
    fn loading_populates_stats_and_source() {
        let doc = load(br#"{"a":[1,2,3]}"#.to_vec(), LoadOptions::default()).unwrap();
        assert_eq!(doc.format, Format::Json);
        assert_eq!(doc.stats.node_count, 5, "object + array + three ints");
        assert_eq!(doc.stats.max_depth, 2);
        assert_eq!(doc.stats.source_bytes, 13);
        assert_eq!(doc.stats.blob_bytes, 1, "only the key \"a\" is interned");
        assert_eq!(doc.stats.child_slots, 4, "1 object member + 3 array items");
        assert_eq!(doc.stats.heap_bytes, 5 * 64 + 4 * 4 + 1 + 13);
    }

    #[test]
    fn empty_and_oversized_inputs_are_refused() {
        assert!(matches!(
            load(Vec::new(), LoadOptions::default()),
            Err(Error::Empty)
        ));
        let opts = LoadOptions {
            max_input_bytes: 4,
            ..Default::default()
        };
        assert!(load(b"{\"a\":1}".to_vec(), opts).is_err());
    }

    #[test]
    fn a_loaded_document_serializes_back_to_equivalent_json() {
        let text = r#"{"a":1,"b":[true,null,"x"],"c":{"d":2.5}}"#;
        let doc = load(text.as_bytes().to_vec(), LoadOptions::default()).unwrap();
        assert_eq!(
            doc.to_json(doc.root(), &JsonOptions::compact()).unwrap(),
            text
        );
    }
}
