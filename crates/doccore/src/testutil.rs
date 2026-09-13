//! Builders shared by the codec tests.
//!
//! Hand-assembling BSON in each test module meant three copies of the same
//! length-prefix arithmetic, which is exactly the kind of thing that hides an
//! off-by-one.

#![cfg(test)]

use crate::arena::Doc;
use crate::bson_read;
use crate::opts::Format;

/// Assemble a BSON document from `(name, type byte, payload)` triples.
///
/// The wire order is type byte, key, NUL, then payload — which is why the type
/// byte stays separate from the payload instead of being pre-spliced onto it.
pub fn bson(elements: &[(&str, u8, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, type_byte, payload) in elements {
        body.push(*type_byte);
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(payload);
    }
    let mut out = Vec::with_capacity(body.len() + 5);
    out.extend_from_slice(&((body.len() + 5) as i32).to_le_bytes());
    out.append(&mut body);
    out.push(0);
    out
}

/// A BSON string payload: int32 length counting its own NUL, the bytes, NUL.
pub fn string(s: &str) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&((s.len() + 1) as i32).to_le_bytes());
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

pub fn i32(v: i32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

pub fn i64(v: i64) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

pub fn u32(v: u32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

pub fn u64(v: u64) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

pub fn f64(v: f64) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

/// Wrap `{a: 1}` in `levels` further `{a: <document>}` layers.
pub fn nest(levels: usize) -> Vec<u8> {
    let mut current = bson(&[("a", 0x10, &i32(1))]);
    for _ in 0..levels {
        current = bson(&[("a", 0x03, &current)]);
    }
    current
}

/// Parse a single-document BSON buffer into a `Doc` with its root set.
pub fn parse_one(bytes: &[u8]) -> Doc {
    let mut doc = Doc::new(Format::Bson);
    let parsed = bson_read::parse(bytes, &mut doc).expect("valid bson");
    assert_eq!(parsed.roots.len(), 1, "expected exactly one document");
    doc.root = parsed.roots[0];
    doc.refresh_stats();
    doc
}
