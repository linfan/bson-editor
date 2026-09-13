//! Arena → JSON serializer.
//!
//! Writes straight into a caller-supplied byte buffer with `itoa`/`ryu` for
//! numbers and a table-driven escaper for strings, so formatting a 10MB
//! document allocates nothing beyond the output itself.

use std::borrow::Cow;

use crate::arena::{Doc, Kind, Node, MAX_DEPTH, NO_NODE};
use crate::error::Result;
use crate::numfmt;
use crate::opts::{Flavor, JsonOptions};

/// Escape table: `0` means "copy verbatim", `1` means "emit `\u00xx`", anything
/// else is the short escape character.
const ESCAPE: [u8; 256] = {
    let mut table = [0u8; 256];
    table[b'"' as usize] = b'"';
    table[b'\\' as usize] = b'\\';
    table[0x08] = b'b';
    table[0x09] = b't';
    table[0x0a] = b'n';
    table[0x0c] = b'f';
    table[0x0d] = b'r';
    let mut i = 0;
    while i < 0x20 {
        if table[i] == 0 {
            table[i] = 1;
        }
        i += 1;
    }
    table
};

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Serialize `node` and everything below it as JSON. No trailing newline: the
/// file-save path adds one, the clipboard path must not.
pub fn write_json(doc: &Doc, node: u32, opts: &JsonOptions, out: &mut Vec<u8>) -> Result<()> {
    Writer::new(out, opts).node(doc, node, 0)
}

/// Serialize `node` into a fresh `String`.
pub fn to_json_string(doc: &Doc, node: u32, opts: &JsonOptions) -> Result<String> {
    let mut out = Vec::with_capacity(estimate_size(doc, node));
    write_json(doc, node, opts, &mut out)?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Rough output size, used to pre-size the buffer and avoid growth reallocs.
fn estimate_size(doc: &Doc, node: u32) -> usize {
    let n = doc.node_at(node);
    if n.kind.is_container() {
        (n.nkids as usize).saturating_mul(24).max(2)
    } else {
        n.val.len as usize + 16
    }
}

struct Writer<'a> {
    out: &'a mut Vec<u8>,
    opts: &'a JsonOptions,
    /// Pre-filled run of the indent byte, sliced once per level.
    pad: Vec<u8>,
    indent_width: usize,
    compact: bool,
}

impl<'a> Writer<'a> {
    fn new(out: &'a mut Vec<u8>, opts: &'a JsonOptions) -> Writer<'a> {
        let (byte, unit) = opts.indent.unit();
        Writer {
            out,
            opts,
            // Two spare levels: a container at the parse depth cap still calls
            // newline() for its children.
            pad: vec![byte; (MAX_DEPTH as usize + 2) * unit],
            indent_width: unit,
            compact: unit == 0,
        }
    }

    #[inline]
    fn push(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }

    #[inline]
    fn newline(&mut self, depth: u32) {
        if self.compact {
            return;
        }
        self.out.push(b'\n');
        // Disjoint field borrows: `out` is written while `pad` is read.
        let want = (depth as usize * self.indent_width).min(self.pad.len());
        self.out.extend_from_slice(&self.pad[..want]);
    }

    #[inline]
    fn colon(&mut self) {
        if self.compact {
            self.out.push(b':');
        } else {
            self.push(b": ");
        }
    }

    fn node(&mut self, doc: &Doc, id: u32, depth: u32) -> Result<()> {
        if let Some(max) = self.opts.max_depth {
            if depth > max {
                self.push("…".as_bytes());
                return Ok(());
            }
        }
        let n = *doc.node(id)?;
        // Requirement 2: an embedded JSON string can be emitted as the
        // structure it decodes to instead of as an escaped literal.
        if self.opts.expand_strings && n.overlay != NO_NODE {
            return self.node(doc, n.overlay, depth);
        }
        self.value(doc, id, n, depth)
    }

    fn value(&mut self, doc: &Doc, id: u32, n: Node, depth: u32) -> Result<()> {
        let extended = self.opts.flavor == Flavor::Extended;
        match n.kind {
            Kind::Object => self.object(doc, id, depth),
            Kind::Array => self.array(doc, id, depth),
            Kind::Str => {
                let bytes = string_payload(doc, n)?;
                self.escaped(&bytes);
                Ok(())
            }
            Kind::Double => {
                if n.f.is_finite() {
                    let mut buf = ryu::Buffer::new();
                    self.push(buf.format(n.f).as_bytes());
                } else if extended {
                    self.push(b"{\"$numberDouble\":");
                    let label = if n.f.is_nan() {
                        "NaN"
                    } else if n.f > 0.0 {
                        "Infinity"
                    } else {
                        "-Infinity"
                    };
                    self.escaped(label.as_bytes());
                    self.out.push(b'}');
                } else {
                    // JSON has no NaN or Infinity; `null` is the only legal
                    // stand-in.
                    self.push(b"null");
                }
                Ok(())
            }
            Kind::Int32 => {
                let mut buf = itoa::Buffer::new();
                self.push(buf.format(n.i as i32).as_bytes());
                Ok(())
            }
            Kind::Int64 => {
                let mut buf = itoa::Buffer::new();
                let text = buf.format(n.i);
                if extended {
                    self.push(b"{\"$numberLong\":");
                    self.escaped(text.as_bytes());
                    self.out.push(b'}');
                } else {
                    self.push(text.as_bytes());
                }
                Ok(())
            }
            Kind::UInt64 => {
                let mut buf = itoa::Buffer::new();
                self.push(buf.format(n.j).as_bytes());
                Ok(())
            }
            Kind::Decimal128 => {
                let text = numfmt::decimal128_to_string(n.i as u64, n.j);
                if extended {
                    self.push(b"{\"$numberDecimal\":");
                    self.escaped(text.as_bytes());
                    self.out.push(b'}');
                } else if text
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'.' | b'E' | b'+'))
                {
                    self.push(text.as_bytes());
                } else {
                    // NaN / Infinity have no JSON number form.
                    self.push(b"null");
                }
                Ok(())
            }
            Kind::Bool => {
                self.push(if n.sub == 1 { b"true" } else { b"false" });
                Ok(())
            }
            Kind::Null => {
                self.push(b"null");
                Ok(())
            }
            Kind::Undefined => {
                self.push(if extended { b"{\"$undefined\":true}" } else { b"null" });
                Ok(())
            }
            Kind::ObjectId => {
                let hex = numfmt::hex_encode(doc.raw(n.val));
                if extended {
                    self.push(b"{\"$oid\":");
                    self.escaped(hex.as_bytes());
                    self.out.push(b'}');
                } else {
                    self.escaped(hex.as_bytes());
                }
                Ok(())
            }
            Kind::DateTime => {
                let iso = numfmt::iso8601(n.i);
                if extended {
                    self.push(b"{\"$date\":");
                    self.escaped(iso.as_bytes());
                    self.out.push(b'}');
                } else {
                    self.escaped(iso.as_bytes());
                }
                Ok(())
            }
            Kind::Binary => {
                let b64 = numfmt::base64_encode(doc.raw(n.val));
                if extended {
                    self.push(b"{\"$binary\":{\"base64\":");
                    self.escaped(b64.as_bytes());
                    self.push(b",\"subType\":\"");
                    self.out.push(HEX_DIGITS[(n.sub >> 4) as usize]);
                    self.out.push(HEX_DIGITS[(n.sub & 0x0f) as usize]);
                    self.push(b"\"}}");
                } else {
                    self.escaped(b64.as_bytes());
                }
                Ok(())
            }
            Kind::Regex => {
                let (pattern, options) = split_nul(doc.raw(n.val));
                if extended {
                    self.push(b"{\"$regularExpression\":{\"pattern\":");
                    self.escaped(pattern);
                    self.push(b",\"options\":");
                    self.escaped(options);
                    self.push(b"}}");
                } else {
                    self.escaped(pattern);
                }
                Ok(())
            }
            Kind::Symbol | Kind::JavaScript => {
                let text = doc.raw(n.val);
                if extended {
                    self.push(if n.kind == Kind::Symbol {
                        b"{\"$symbol\":"
                    } else {
                        b"{\"$code\":"
                    });
                    self.escaped(text);
                    self.out.push(b'}');
                } else {
                    self.escaped(text);
                }
                Ok(())
            }
            Kind::JavaScriptWithScope => {
                self.push(b"{\"$code\":");
                let code = doc.raw(n.val);
                self.escaped(code);
                self.push(b",\"$scope\":");
                if n.aux == NO_NODE {
                    self.push(b"{}");
                } else {
                    self.node(doc, n.aux, depth)?;
                }
                self.out.push(b'}');
                Ok(())
            }
            Kind::DbPointer => {
                let (namespace, oid) = split_nul(doc.raw(n.val));
                let hex = numfmt::hex_encode(oid);
                self.push(b"{\"$ref\":");
                self.escaped(namespace);
                self.push(b",\"$id\":");
                if extended {
                    self.push(b"{\"$oid\":");
                    self.escaped(hex.as_bytes());
                    self.out.push(b'}');
                } else {
                    self.escaped(hex.as_bytes());
                }
                self.out.push(b'}');
                Ok(())
            }
            Kind::Timestamp => {
                self.push(b"{\"$timestamp\":{\"t\":");
                let mut buf = itoa::Buffer::new();
                self.push(buf.format(n.i).as_bytes());
                self.push(b",\"i\":");
                let mut buf = itoa::Buffer::new();
                self.push(buf.format(n.j).as_bytes());
                self.push(b"}}");
                Ok(())
            }
            Kind::MinKey | Kind::MaxKey => {
                let min = n.kind == Kind::MinKey;
                if extended {
                    self.push(if min { b"{\"$minKey\":1}" } else { b"{\"$maxKey\":1}" });
                } else {
                    self.escaped(if min { b"$minKey" } else { b"$maxKey" });
                }
                Ok(())
            }
        }
    }

    fn object(&mut self, doc: &Doc, id: u32, depth: u32) -> Result<()> {
        let kids = doc.children(id);
        if kids.is_empty() {
            self.push(b"{}");
            return Ok(());
        }
        self.out.push(b'{');
        for (i, &child) in kids.iter().enumerate() {
            if i > 0 {
                self.out.push(b',');
            }
            self.newline(depth + 1);
            let key = doc.key_of(child);
            self.escaped(key.as_bytes());
            self.colon();
            self.node(doc, child, depth + 1)?;
        }
        self.newline(depth);
        self.out.push(b'}');
        Ok(())
    }

    fn array(&mut self, doc: &Doc, id: u32, depth: u32) -> Result<()> {
        let kids = doc.children(id);
        if kids.is_empty() {
            self.push(b"[]");
            return Ok(());
        }
        self.out.push(b'[');
        for (i, &child) in kids.iter().enumerate() {
            if i > 0 {
                self.out.push(b',');
            }
            self.newline(depth + 1);
            self.node(doc, child, depth + 1)?;
        }
        self.newline(depth);
        self.out.push(b']');
        Ok(())
    }

    /// Copy `bytes` into the output as a JSON string literal. Bytes that are not
    /// valid UTF-8 pass through untouched; the caller converts lossily at the
    /// boundary.
    fn escaped(&mut self, bytes: &[u8]) {
        self.out.push(b'"');
        let mut start = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let marker = ESCAPE[b as usize];
            if marker == 0 {
                continue;
            }
            self.push(&bytes[start..i]);
            start = i + 1;
            self.out.push(b'\\');
            if marker == 1 {
                self.push(b"u00");
                self.out.push(HEX_DIGITS[(b >> 4) as usize]);
                self.out.push(HEX_DIGITS[(b & 0x0f) as usize]);
            } else {
                self.out.push(marker);
            }
        }
        self.push(&bytes[start..]);
        self.out.push(b'"');
    }
}

/// The bytes a string node contributes to the output.
///
/// Normally that is the stored text. When the node's embedded-JSON overlay has
/// been edited the overlay is authoritative instead, and is re-serialized to
/// compact JSON so the change lands back inside the escaped string — that is
/// what makes editing an expanded JSON string persist (requirement 2).
pub(crate) fn string_payload(doc: &Doc, n: Node) -> Result<Cow<'_, [u8]>> {
    if n.overlay != NO_NODE && n.flags & Node::OVERLAY_DIRTY != 0 {
        let mut out = Vec::with_capacity(n.val.len as usize + 16);
        write_json(doc, n.overlay, &JsonOptions::compact(), &mut out)?;
        return Ok(Cow::Owned(out));
    }
    Ok(Cow::Borrowed(doc.raw(n.val)))
}

/// Split a blob region that packs two NUL-separated parts (regex pattern and
/// options, or a DBPointer namespace and id).
pub(crate) fn split_nul(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&b| b == 0) {
        Some(i) => (&bytes[..i], &bytes[i + 1..]),
        None => (bytes, &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json_read::parse_json;
    use crate::opts::{Format, Indent};

    fn doc(text: &str) -> Doc {
        let mut d = Doc::new(Format::Json);
        let root = parse_json(text.as_bytes(), &mut d).expect("valid json");
        d.root = root;
        d
    }

    fn render(d: &Doc, node: u32, opts: &JsonOptions) -> String {
        to_json_string(d, node, opts).unwrap()
    }

    fn pretty(d: &Doc) -> String {
        render(d, d.root(), &JsonOptions::default())
    }

    fn compact(d: &Doc) -> String {
        render(d, d.root(), &JsonOptions::compact())
    }

    #[test]
    fn compact_output_has_no_whitespace() {
        let d = doc(r#"{ "a" : [ 1, 2, { "b" : null } ] }"#);
        assert_eq!(compact(&d), r#"{"a":[1,2,{"b":null}]}"#);
    }

    #[test]
    fn pretty_output_uses_two_space_indent() {
        let d = doc(r#"{"a":[1,{"b":2}],"c":{}}"#);
        assert_eq!(
            pretty(&d),
            "{\n  \"a\": [\n    1,\n    {\n      \"b\": 2\n    }\n  ],\n  \"c\": {}\n}"
        );
    }

    #[test]
    fn tab_indent_is_supported() {
        let d = doc(r#"{"a":[1]}"#);
        let opts = JsonOptions { indent: Indent::Tab, ..Default::default() };
        assert_eq!(render(&d, d.root(), &opts), "{\n\t\"a\": [\n\t\t1\n\t]\n}");
    }

    #[test]
    fn any_subtree_serializes_on_its_own() {
        let d = doc(r#"{"a":{"deep":[1,2,3]}}"#);
        let a = d.children(d.root())[0];
        let deep = d.children(a)[0];
        assert_eq!(render(&d, deep, &JsonOptions::compact()), "[1,2,3]");
        assert_eq!(render(&d, d.children(deep)[1], &JsonOptions::compact()), "2");
        assert_eq!(render(&d, a, &JsonOptions::compact()), r#"{"deep":[1,2,3]}"#);
    }

    #[test]
    fn strings_are_escaped_and_unicode_survives() {
        let d = doc("{\"s\":\"a\\nb\\\"c\\\\d\\u0001é中\"}");
        assert_eq!(compact(&d), "{\"s\":\"a\\nb\\\"c\\\\d\\u0001é中\"}");
        // And the output parses back to the same bytes.
        let again = doc(&compact(&d));
        let s = again.children(again.root())[0];
        assert_eq!(again.text(again.node_at(s).val), "a\nb\"c\\d\u{1}é中");
    }

    #[test]
    fn invalid_utf8_becomes_the_replacement_char_on_the_way_out() {
        let mut d = Doc::new(Format::Json);
        let span = d.push_blob(&[b'a', 0xFF, b'z']);
        let id = d.add_node(Node::new(Kind::Str));
        d.nodes[id as usize].val = span;
        assert_eq!(render(&d, id, &JsonOptions::compact()), "\"a\u{fffd}z\"");
    }

    #[test]
    fn numbers_keep_their_precision() {
        let d = doc(r#"{"big":18446744073709551615,"neg":-9223372036854775808,"f":0.1,"e":1e30}"#);
        let out = compact(&d);
        assert!(out.contains("18446744073709551615"), "{out}");
        assert!(out.contains("-9223372036854775808"), "{out}");
        assert!(out.contains("0.1"), "{out}");
        assert!(out.contains("1e30"), "{out}");
    }

    #[test]
    fn max_depth_truncates_with_an_ellipsis() {
        let d = doc(r#"{"a":{"b":{"c":1}}}"#);
        // The root is depth 0, so `Some(0)` keeps the root and cuts its children.
        let at_zero = JsonOptions { max_depth: Some(0), ..JsonOptions::compact() };
        assert_eq!(render(&d, d.root(), &at_zero), r#"{"a":…}"#);
        let at_one = JsonOptions { max_depth: Some(1), ..JsonOptions::compact() };
        assert_eq!(render(&d, d.root(), &at_one), r#"{"a":{"b":…}}"#);
    }

    #[test]
    fn empty_containers_stay_inline() {
        let d = doc(r#"{"o":{},"a":[]}"#);
        assert_eq!(compact(&d), r#"{"o":{},"a":[]}"#);
        assert_eq!(pretty(&d), "{\n  \"o\": {},\n  \"a\": []\n}");
    }

    #[test]
    fn deep_nesting_indents_every_level() {
        // 40 levels of nesting, well past one indent pad's worth of columns.
        let text = format!("{}{}", "[".repeat(40), "]".repeat(40));
        let d = doc(&text);
        let opts = JsonOptions { indent: Indent::Spaces(8), ..Default::default() };
        let out = render(&d, d.root(), &opts);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 79);
        // The innermost array is empty, so it renders inline at depth 39.
        assert_eq!(lines[39], format!("{}[]", " ".repeat(8 * 39)));
        // The next line closes the depth-38 array.
        assert_eq!(lines[40], format!("{}]", " ".repeat(8 * 38)));
    }
}
