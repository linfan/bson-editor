//! Tunables for parsing, expansion, and output formatting.

use serde::{Deserialize, Serialize};

/// Wire format a document was read from, and the format it is written back to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Json,
    Bson,
}

/// How whitespace is emitted when serializing to JSON.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "width")]
pub enum Indent {
    /// No whitespace at all: the smallest possible output.
    Compact,
    /// `width` spaces per level, capped at 8.
    Spaces(u8),
    Tab,
}

impl Default for Indent {
    fn default() -> Self {
        Indent::Spaces(2)
    }
}

impl Indent {
    /// Byte written once per nesting level, plus how many of them.
    pub(crate) fn unit(self) -> (u8, usize) {
        match self {
            Indent::Compact => (b' ', 0),
            Indent::Spaces(n) => (b' ', n.min(8) as usize),
            Indent::Tab => (b'\t', 1),
        }
    }
}

/// How BSON-only types are projected onto JSON.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Flavor {
    /// Human-readable JSON. BSON-only types degrade to the closest plain JSON
    /// value, which loses some type information but pastes cleanly anywhere.
    #[default]
    Plain,
    /// MongoDB extended JSON (`$oid`, `$date`, `$binary`, …). Lossless: the
    /// output can be read back into the exact same BSON types.
    Extended,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JsonOptions {
    pub indent: Indent,
    pub flavor: Flavor,
    /// Emit the parsed structure of an embedded JSON string in place of the
    /// escaped string literal.
    pub expand_strings: bool,
    /// Stop descending after this many levels; deeper values become `…`.
    /// `None` means unlimited.
    pub max_depth: Option<u32>,
}

impl JsonOptions {
    pub fn compact() -> Self {
        JsonOptions {
            indent: Indent::Compact,
            ..Default::default()
        }
    }
}

/// How a document is parsed on load.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LoadOptions {
    /// Requirement 2: how many levels of nested JSON-in-a-string to unfold.
    /// `0` disables the feature entirely.
    pub expand_json_depth: u8,
    /// Requirement 1: force the magic-byte prefix length instead of detecting
    /// it. `None` means auto-detect.
    pub magic_len: Option<usize>,
    /// Force the input format instead of sniffing it.
    pub format: Option<Format>,
    /// Refuse inputs larger than this many bytes. Guards against accidentally
    /// opening a multi-gigabyte file.
    pub max_input_bytes: usize,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions {
            expand_json_depth: 3,
            magic_len: None,
            format: None,
            max_input_bytes: 512 * 1024 * 1024,
        }
    }
}
