//! Single-pass JSON → arena parser.
//!
//! `serde_json` drives a [`Visitor`] that appends straight into the arena, so a
//! 10MB document is never materialized as an intermediate value tree. Each
//! string is copied once into the shared blob; nothing else allocates.

use std::fmt;

use serde::de::{
    DeserializeSeed, Deserializer, Error as SerdeError, MapAccess, SeqAccess, Visitor,
};

use crate::arena::{Doc, Kind, Node, Span, MAX_DEPTH};
use crate::error::{Error, Result};

/// Parse `bytes` as a single JSON value into `doc`, returning the root node id.
///
/// Trailing non-whitespace is rejected, so a truncated or concatenated file
/// reports an error instead of silently parsing only its first value.
pub fn parse_json(bytes: &[u8], doc: &mut Doc) -> Result<u32> {
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Err(Error::Empty);
    }
    doc.reserve_for(bytes.len());
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let root = serde::Deserializer::deserialize_any(&mut de, JsonVisitor(doc)).map_err(map_err)?;
    de.end().map_err(map_err)?;
    Ok(root)
}

fn map_err(err: serde_json::Error) -> Error {
    Error::Json { message: err.to_string(), line: err.line(), column: err.column() }
}

/// Routes one JSON value into the arena. Reused by the embedded-JSON-string
/// expander, which parses field values with the same machinery.
pub(crate) struct NodeSeed<'a>(pub(crate) &'a mut Doc);

impl<'de> DeserializeSeed<'de> for NodeSeed<'_> {
    type Value = u32;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<u32, D::Error> {
        d.deserialize_any(JsonVisitor(self.0))
    }
}

/// Routes one object key into the blob and hands back its span.
struct KeySeed<'a>(&'a mut Doc);

impl<'de> DeserializeSeed<'de> for KeySeed<'_> {
    type Value = Span;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Span, D::Error> {
        d.deserialize_str(SpanVisitor(self.0))
    }
}

struct SpanVisitor<'a>(&'a mut Doc);

impl<'de> Visitor<'de> for SpanVisitor<'_> {
    type Value = Span;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string")
    }

    fn visit_borrowed_str<E: serde::de::Error>(self, v: &'de str) -> std::result::Result<Span, E> {
        Ok(self.0.push_blob(v.as_bytes()))
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Span, E> {
        Ok(self.0.push_blob(v.as_bytes()))
    }

    fn visit_string<E: serde::de::Error>(self, v: String) -> std::result::Result<Span, E> {
        Ok(self.0.push_blob(v.as_bytes()))
    }
}

struct JsonVisitor<'a>(&'a mut Doc);

impl JsonVisitor<'_> {
    #[inline]
    fn scalar(&mut self, kind: Kind) -> u32 {
        self.0.add_node(Node::new(kind))
    }
}

impl<'de> Visitor<'de> for JsonVisitor<'_> {
    type Value = u32;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E: serde::de::Error>(mut self, v: bool) -> std::result::Result<u32, E> {
        let id = self.scalar(Kind::Bool);
        self.0.nodes[id as usize].sub = u8::from(v);
        Ok(id)
    }

    fn visit_i64<E: serde::de::Error>(mut self, v: i64) -> std::result::Result<u32, E> {
        let id = self.scalar(Kind::Int64);
        self.0.nodes[id as usize].i = v;
        Ok(id)
    }

    fn visit_u64<E: serde::de::Error>(mut self, v: u64) -> std::result::Result<u32, E> {
        // Keep integers exact: only values past `i64::MAX` fall back to the
        // unsigned slot, and only past `u64::MAX` would precision be lost.
        let id = if v <= i64::MAX as u64 {
            let id = self.scalar(Kind::Int64);
            self.0.nodes[id as usize].i = v as i64;
            id
        } else {
            let id = self.scalar(Kind::UInt64);
            self.0.nodes[id as usize].j = v;
            id
        };
        Ok(id)
    }

    fn visit_f64<E: serde::de::Error>(mut self, v: f64) -> std::result::Result<u32, E> {
        let id = self.scalar(Kind::Double);
        self.0.nodes[id as usize].f = v;
        Ok(id)
    }

    fn visit_unit<E: serde::de::Error>(mut self) -> std::result::Result<u32, E> {
        Ok(self.scalar(Kind::Null))
    }

    fn visit_borrowed_str<E: serde::de::Error>(
        mut self,
        v: &'de str,
    ) -> std::result::Result<u32, E> {
        Ok(self.string(v.as_bytes()))
    }

    fn visit_str<E: serde::de::Error>(mut self, v: &str) -> std::result::Result<u32, E> {
        Ok(self.string(v.as_bytes()))
    }

    fn visit_string<E: serde::de::Error>(mut self, v: String) -> std::result::Result<u32, E> {
        Ok(self.string(v.as_bytes()))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<u32, A::Error> {
        if self.0.depth >= MAX_DEPTH {
            return Err(A::Error::custom(Error::TooDeep(MAX_DEPTH)));
        }
        let id = self.0.open(Kind::Array, Span::NONE);
        while let Some(child) = seq.next_element_seed(NodeSeed(&mut *self.0))? {
            self.0.push_child(child, id);
        }
        self.0.close(id);
        Ok(id)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<u32, A::Error> {
        if self.0.depth >= MAX_DEPTH {
            return Err(A::Error::custom(Error::TooDeep(MAX_DEPTH)));
        }
        let id = self.0.open(Kind::Object, Span::NONE);
        // Duplicate keys are kept as separate members: the arena is a faithful
        // record of the input, not a map.
        while let Some(key) = map.next_key_seed(KeySeed(&mut *self.0))? {
            let child = map.next_value_seed(NodeSeed(&mut *self.0))?;
            self.0.nodes[child as usize].key = key;
            self.0.push_child(child, id);
        }
        self.0.close(id);
        Ok(id)
    }
}

impl JsonVisitor<'_> {
    fn string(&mut self, bytes: &[u8]) -> u32 {
        let span = self.0.push_blob(bytes);
        let id = self.0.add_node(Node::new(Kind::Str));
        self.0.nodes[id as usize].val = span;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opts::Format;

    fn parse(text: &str) -> Doc {
        let mut doc = Doc::new(Format::Json);
        let root = parse_json(text.as_bytes(), &mut doc).expect("valid json");
        doc.root = root;
        doc
    }

    fn kind_of(doc: &Doc, id: u32) -> Kind {
        doc.node_at(id).kind
    }

    #[test]
    fn scalars_map_to_exact_types() {
        let doc = parse(
            r#"{"i":42,"neg":-7,"big":18446744073709551615,"f":1.5,"s":"x","b":true,"n":null}"#,
        );
        let kids = doc.children(doc.root());
        assert_eq!(kind_of(&doc, kids[0]), Kind::Int64);
        assert_eq!(doc.node_at(kids[0]).i, 42);
        assert_eq!(doc.node_at(kids[1]).i, -7);
        // Past i64::MAX but still exact as u64.
        assert_eq!(kind_of(&doc, kids[2]), Kind::UInt64);
        assert_eq!(doc.node_at(kids[2]).j, u64::MAX);
        assert_eq!(kind_of(&doc, kids[3]), Kind::Double);
        assert_eq!(doc.node_at(kids[3]).f, 1.5);
        assert_eq!(kind_of(&doc, kids[4]), Kind::Str);
        assert_eq!(doc.text(doc.node_at(kids[4]).val), "x");
        assert_eq!(doc.node_at(kids[5]).sub, 1);
        assert_eq!(kind_of(&doc, kids[6]), Kind::Null);
    }

    #[test]
    fn keys_are_preserved_in_order_with_duplicates() {
        let doc = parse(r#"{"a":1,"b":2,"a":3}"#);
        let kids = doc.children(doc.root());
        let keys: Vec<_> = kids.iter().map(|&k| doc.key_of(k).into_owned()).collect();
        assert_eq!(keys, ["a", "b", "a"]);
        assert_eq!(doc.node_at(kids[2]).i, 3);
    }

    #[test]
    fn nesting_builds_contiguous_child_ranges() {
        let doc = parse(r#"{"a":[1,[2,{"b":3}]],"c":{}}"#);
        let root_kids = doc.children(doc.root());
        let arr = root_kids[0];
        assert_eq!(kind_of(&doc, arr), Kind::Array);
        assert_eq!(doc.children(arr).len(), 2);
        let empty = root_kids[1];
        assert_eq!(doc.children(empty).len(), 0);
        assert_eq!(doc.path_to(arr).unwrap(), vec![doc.root(), arr]);
    }

    #[test]
    fn escaped_strings_are_unescaped_into_the_blob() {
        let doc = parse(r#"{"k\u0065y":"line\nbreak \"quoted\" \u00e9"}"#);
        let kid = doc.children(doc.root())[0];
        assert_eq!(doc.key_of(kid), "key");
        assert_eq!(doc.text(doc.node_at(kid).val), "line\nbreak \"quoted\" é");
    }

    #[test]
    fn trailing_garbage_is_rejected() {
        let mut doc = Doc::new(Format::Json);
        assert!(matches!(parse_json(b"{} {}", &mut doc), Err(Error::Json { .. })));
    }

    #[test]
    fn empty_input_is_rejected() {
        let mut doc = Doc::new(Format::Json);
        assert!(matches!(parse_json(b"   ", &mut doc), Err(Error::Empty)));
        assert!(matches!(parse_json(b"{", &mut doc), Err(Error::Json { .. })));
    }

    #[test]
    fn runaway_nesting_is_stopped() {
        let text = format!("{}1{}", "[".repeat(400), "]".repeat(400));
        let mut doc = Doc::new(Format::Json);
        assert!(parse_json(text.as_bytes(), &mut doc).is_err());
    }

    #[test]
    fn large_documents_parse_without_reallocation_stalls() {
        // 200k members: enough to prove the arena scales and that ids stay
        // dense.
        let mut text = String::with_capacity(4_000_000);
        text.push('{');
        for i in 0..200_000u32 {
            if i > 0 {
                text.push(',');
            }
            text.push_str(&format!("\"k{i}\":{i}"));
        }
        text.push('}');
        let doc = parse(&text);
        assert_eq!(doc.child_count(doc.root()), 200_000);
        assert_eq!(doc.nodes.len(), 200_001);
    }
}
