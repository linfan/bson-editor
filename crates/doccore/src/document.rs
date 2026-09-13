//! Document-level entry points: load bytes or pasted text, hand back a [`Doc`],
//! serialize it again.

use crate::arena::{Doc, Kind, Span};
use crate::bson_read;
use crate::bson_write;
use crate::error::{Error, Result};
use crate::json_read::parse_json;
use crate::json_write::{to_json_string, write_json};
use crate::magic;
use crate::numfmt;
use crate::opts::{Format, JsonOptions, LoadOptions, SaveOptions};

/// Result of serializing a document.
pub struct Saved {
    pub bytes: Vec<u8>,
    /// Fidelity notes the user should see, such as a type that had to widen to
    /// fit the target format.
    pub warnings: Vec<String>,
}

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

    /// Parse a BSON file, setting aside any magic-byte prefix and trailing bytes
    /// so both can be restored verbatim on save (requirement 1).
    pub fn from_bson(source: Vec<u8>, forced_magic: Option<usize>) -> Result<Doc> {
        let framing = magic::detect(&source, forced_magic)?;
        Doc::from_bson_framed(source, framing)
    }

    /// Parse a BSON file whose framing has already been worked out.
    pub fn from_bson_framed(source: Vec<u8>, framing: magic::Framing) -> Result<Doc> {
        let mut doc = Doc::new(Format::Bson);
        let parsed = bson_read::parse(&source[framing.body.clone()], &mut doc)?;
        doc.magic = source[..framing.magic_len].to_vec();
        doc.trailing = source[framing.trailing.clone()].to_vec();

        if parsed.roots.len() == 1 {
            doc.root = parsed.roots[0];
        } else {
            // A multi-document file gets a synthetic array root so the tree view
            // has something to hang the documents from. It is not written back
            // out: the serializer emits each child document in turn.
            let array = doc.open(Kind::Array, Span::NONE);
            for &root in &parsed.roots {
                doc.push_child(root, array);
            }
            doc.close(array);
            doc.root = array;
            doc.synthetic_root = true;
            doc.max_depth += 1;
        }
        doc.document_count = parsed.roots.len();
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

    /// Serialize a node subtree to bytes, skipping the UTF-8 conversion the
    /// clipboard path needs.
    pub fn to_json_bytes(&self, node: u32, opts: &JsonOptions, out: &mut Vec<u8>) -> Result<()> {
        self.node(node)?;
        write_json(self, node, opts, out)
    }

    /// Serialize the whole document for writing to disk.
    ///
    /// For BSON this restores the magic-byte prefix and any trailing bytes that
    /// were set aside at load time, so a round trip through the editor leaves
    /// the parts it does not understand untouched.
    pub fn save(&self, opts: &SaveOptions) -> Result<Saved> {
        let format = opts.format.unwrap_or(self.format);
        let mut bytes = Vec::with_capacity(self.stats.source_bytes + 64);
        match format {
            Format::Bson => {
                bytes.extend_from_slice(&self.magic);
                let warnings = bson_write::write_bson(self, &mut bytes)?;
                bytes.extend_from_slice(&self.trailing);
                Ok(Saved { bytes, warnings })
            }
            Format::Json => {
                write_json(self, self.root(), &opts.json, &mut bytes)?;
                if opts.newline_terminated {
                    bytes.push(b'\n');
                }
                Ok(Saved { bytes, warnings: Vec::new() })
            }
        }
    }
}

/// Cheap heuristic: does this look like JSON text?
///
/// Not used to pick the format — a BSON length prefix can start with a digit and
/// fool it. It routes pasted text and decides which error to report when both
/// parsers reject the input.
pub fn looks_like_json(bytes: &[u8]) -> bool {
    let first = bytes.iter().find(|b| !b.is_ascii_whitespace());
    matches!(first, Some(b'{' | b'[' | b'"' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n'))
}

/// Load a document from raw bytes, honoring the magic-byte and expand-depth
/// options.
pub fn load(source: Vec<u8>, opts: LoadOptions) -> Result<Doc> {
    if source.is_empty() {
        return Err(Error::Empty);
    }
    if source.len() > opts.max_input_bytes {
        return Err(Error::TooLarge { size: source.len(), limit: opts.max_input_bytes });
    }
    if let Some(forced) = opts.format {
        return match forced {
            Format::Json => Doc::from_json(source),
            Format::Bson => Doc::from_bson(source, opts.magic_len),
        };
    }
    // Sniffing the leading byte is not safe: a BSON length prefix whose low byte
    // happens to be a digit looks exactly like the start of a JSON number.
    // Framing detection is structural instead, and rejects JSON text after
    // reading four bytes, so trying it first costs nothing.
    let looked_like_json = looks_like_json(&source);
    match magic::detect(&source, opts.magic_len) {
        Ok(framing) => Doc::from_bson_framed(source, framing),
        Err(bson_error) => match Doc::from_json(source) {
            Ok(doc) => Ok(doc),
            // Both failed: report whichever format the input actually resembled.
            Err(json_error) => Err(if looked_like_json { json_error } else { bson_error }),
        },
    }
}

/// Load a document from text the user pasted (requirement 3).
///
/// JSON is tried first when the text looks like JSON. A hex-encoded BSON blob
/// can also start with a digit, so that guess is allowed to fail: the encoded
/// paths are tried next, as hex then base64, which is how BSON gets copied out
/// of logs and shell output.
pub fn load_text(text: &str, opts: LoadOptions) -> Result<Doc> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(Error::Empty);
    }
    let mut json_error = None;
    if looks_like_json(trimmed.as_bytes()) {
        match load(trimmed.as_bytes().to_vec(), opts) {
            Ok(doc) => return Ok(doc),
            Err(err) => json_error = Some(err),
        }
    }
    let attempts = [numfmt::hex_decode(trimmed), numfmt::base64_decode(trimmed)];
    for bytes in attempts.into_iter().flatten() {
        let bson_opts = LoadOptions { format: Some(Format::Bson), ..opts };
        if let Ok(doc) = load(bytes, bson_opts) {
            return Ok(doc);
        }
    }
    Err(json_error.unwrap_or(Error::Unrecognized))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{bson, i32, string};

    fn sample_bson() -> Vec<u8> {
        bson(&[("id", 0x10, &i32(7)), ("name", 0x02, &string("sample"))])
    }

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
        assert!(matches!(load(Vec::new(), LoadOptions::default()), Err(Error::Empty)));
        let opts = LoadOptions { max_input_bytes: 4, ..Default::default() };
        assert!(matches!(
            load(b"{\"a\":1}".to_vec(), opts),
            Err(Error::TooLarge { size: 7, limit: 4 })
        ));
    }

    #[test]
    fn a_loaded_document_serializes_back_to_equivalent_json() {
        let text = r#"{"a":1,"b":[true,null,"x"],"c":{"d":2.5}}"#;
        let doc = load(text.as_bytes().to_vec(), LoadOptions::default()).unwrap();
        assert_eq!(doc.to_json(doc.root(), &JsonOptions::compact()).unwrap(), text);
    }

    #[test]
    fn binary_input_is_loaded_as_bson() {
        let doc = load(sample_bson(), LoadOptions::default()).unwrap();
        assert_eq!(doc.format, Format::Bson);
        assert!(doc.magic.is_empty());
        assert_eq!(doc.document_count, 1);
        let keys: Vec<String> =
            doc.children(doc.root()).iter().map(|&c| doc.key_of(c).into_owned()).collect();
        assert_eq!(keys, ["id", "name"]);
    }

    #[test]
    fn a_digit_leading_length_prefix_is_not_mistaken_for_json() {
        // The low byte of a BSON length prefix is an ASCII digit for some
        // document sizes, which used to send the file to the JSON parser.
        let bytes = (0..300usize)
            .map(|pad| bson(&[(&format!("k{}", "x".repeat(pad)), 0x10, &i32(1))]))
            .find(|candidate| candidate[0].is_ascii_digit())
            .expect("some document size puts a digit in the length prefix");

        let doc = load(bytes, LoadOptions::default()).unwrap();
        assert_eq!(doc.format, Format::Bson);
        assert!(doc.key_of(doc.children(doc.root())[0]).starts_with('k'));
    }

    #[test]
    fn json_text_is_not_claimed_by_bson_detection() {
        // Leading bytes here read as a large little-endian integer, so framing
        // detection rejects them and JSON gets its turn.
        for text in [r#"{"a":1}"#, "[1,2,3]", "1234567890", r#""scalar""#, "true"] {
            let doc = load(text.as_bytes().to_vec(), LoadOptions::default()).unwrap();
            assert_eq!(doc.format, Format::Json, "{text}");
        }
    }

    #[test]
    fn a_magic_prefix_is_stripped_on_load_and_restored_on_save() {
        let mut bytes = b"\xDE\xAD\xBE\xEF".to_vec();
        bytes.extend_from_slice(&sample_bson());

        let doc = load(bytes.clone(), LoadOptions::default()).unwrap();
        assert_eq!(doc.magic, b"\xDE\xAD\xBE\xEF");
        assert_eq!(doc.stats.magic_bytes, 4);
        assert_eq!(doc.key_of(doc.children(doc.root())[0]), "id");

        let saved = doc.save(&SaveOptions::default()).unwrap();
        assert_eq!(saved.bytes, bytes, "saving restores the prefix byte for byte");
    }

    #[test]
    fn trailing_bytes_survive_a_round_trip() {
        let mut bytes = b"MAGIC".to_vec();
        bytes.extend_from_slice(&sample_bson());
        bytes.extend_from_slice(b"\x00\x01\x02checksum");

        let doc = load(bytes.clone(), LoadOptions::default()).unwrap();
        assert_eq!(doc.magic, b"MAGIC");
        assert_eq!(doc.trailing, b"\x00\x01\x02checksum");
        assert_eq!(doc.save(&SaveOptions::default()).unwrap().bytes, bytes);
    }

    #[test]
    fn a_forced_prefix_length_overrides_detection() {
        let mut bytes = b"HDR!".to_vec();
        bytes.extend_from_slice(&sample_bson());
        let opts =
            LoadOptions { magic_len: Some(4), format: Some(Format::Bson), ..Default::default() };
        assert_eq!(load(bytes, opts).unwrap().magic, b"HDR!");
    }

    #[test]
    fn a_document_stream_gets_a_synthetic_array_root() {
        let mut bytes = sample_bson();
        bytes.extend_from_slice(&bson(&[("other", 0x08, &[1])]));
        let doc = load(bytes.clone(), LoadOptions::default()).unwrap();
        assert!(doc.synthetic_root);
        assert_eq!(doc.document_count, 2);
        assert_eq!(doc.node(doc.root()).unwrap().kind, Kind::Array);
        assert_eq!(doc.children(doc.root()).len(), 2);
        // The synthetic root is not written out; the documents are.
        assert_eq!(doc.save(&SaveOptions::default()).unwrap().bytes, bytes);
    }

    #[test]
    fn bson_saves_as_json_and_json_saves_as_bson() {
        let doc = load(sample_bson(), LoadOptions::default()).unwrap();
        let as_json =
            doc.save(&SaveOptions { format: Some(Format::Json), ..Default::default() }).unwrap();
        assert_eq!(as_json.bytes, b"{\n  \"id\": 7,\n  \"name\": \"sample\"\n}\n");

        let json_doc = load(br#"{"a":1}"#.to_vec(), LoadOptions::default()).unwrap();
        let as_bson = json_doc
            .save(&SaveOptions { format: Some(Format::Bson), ..Default::default() })
            .unwrap();
        // Re-reading what we wrote must give the same tree back.
        let again = load(as_bson.bytes, LoadOptions::default()).unwrap();
        assert_eq!(again.format, Format::Bson);
        assert_eq!(again.key_of(again.children(again.root())[0]), "a");
    }

    #[test]
    fn pasted_json_text_loads() {
        let doc = load_text("  {\"a\": [1,2]}  ", LoadOptions::default()).unwrap();
        assert_eq!(doc.format, Format::Json);
        assert_eq!(doc.child_count(doc.root()), 1);
    }

    #[test]
    fn pasted_hex_bson_loads() {
        let hex = numfmt::hex_encode(&sample_bson());
        let doc = load_text(&hex, LoadOptions::default()).unwrap();
        assert_eq!(doc.format, Format::Bson);
        assert_eq!(doc.key_of(doc.children(doc.root())[0]), "id");
    }

    #[test]
    fn pasted_base64_bson_loads() {
        let encoded = numfmt::base64_encode(&sample_bson());
        let doc = load_text(&encoded, LoadOptions::default()).unwrap();
        assert_eq!(doc.format, Format::Bson);
        assert_eq!(doc.key_of(doc.children(doc.root())[1]), "name");
    }

    #[test]
    fn pasted_nonsense_is_rejected() {
        assert!(matches!(load_text("", LoadOptions::default()), Err(Error::Empty)));
        assert!(matches!(
            load_text("hello there, not a document", LoadOptions::default()),
            Err(Error::Unrecognized)
        ));
    }
}
