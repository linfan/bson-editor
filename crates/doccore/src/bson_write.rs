//! Arena → BSON serializer.
//!
//! Lengths are written as placeholders and patched once the element is
//! complete, so serializing never needs a second buffer per document.

use crate::arena::{Doc, Kind, Node, NO_NODE};
use crate::error::{Error, Result};
use crate::json_write::{split_nul, string_payload};

/// Serialize the whole document as a BSON stream.
///
/// Returns warnings for values that had to change type to fit BSON, so the UI
/// can tell the user what a save-as-BSON actually did.
pub fn write_bson(doc: &Doc, out: &mut Vec<u8>) -> Result<Vec<String>> {
    let mut warnings: Vec<String> = Vec::new();
    if doc.synthetic_root {
        for &child in doc.children(doc.root()) {
            write_document(doc, child, out, &mut warnings)?;
        }
    } else {
        let root = doc.root();
        let kind = doc.node(root)?.kind;
        if kind != Kind::Object {
            return Err(Error::Serialize(format!(
                "a BSON file must start with a document, but this one starts with a {}",
                kind.type_name()
            )));
        }
        write_document(doc, root, out, &mut warnings)?;
    }
    Ok(warnings)
}

fn write_document(doc: &Doc, id: u32, out: &mut Vec<u8>, warnings: &mut Vec<String>) -> Result<()> {
    let is_array = doc.node(id)?.kind == Kind::Array;
    let length_pos = out.len();
    out.extend_from_slice(&[0; 4]);
    for (index, &child) in doc.children(id).iter().enumerate() {
        let n = *doc.node(child)?;
        let kind = bson_kind(n, warnings);
        out.push(kind as u8);
        if is_array {
            // BSON arrays are documents whose keys are the decimal positions.
            let mut counter = itoa::Buffer::new();
            write_cstring(out, counter.format(index as u32).as_bytes())?;
        } else {
            let key = doc.key_of(child);
            write_cstring(out, key.as_bytes())?;
        }
        write_value(doc, child, n, kind, out, warnings)?;
    }
    out.push(0);

    let total = out.len() - length_pos;
    if total > i32::MAX as usize {
        return Err(Error::Serialize(format!(
            "document is {total} bytes, over the 2 GiB BSON length limit"
        )));
    }
    out[length_pos..length_pos + 4].copy_from_slice(&(total as i32).to_le_bytes());
    Ok(())
}

/// The BSON type a node is written as, widening values that no longer fit the
/// type they were parsed with.
fn bson_kind(n: Node, warnings: &mut Vec<String>) -> Kind {
    match n.kind {
        Kind::Int32 if n.i < i32::MIN as i64 || n.i > i32::MAX as i64 => {
            warn_once(warnings, "an int32 was out of range and was widened to int64");
            Kind::Int64
        }
        // JSON integers past i64::MAX have no BSON type at all.
        Kind::UInt64 => {
            if n.j <= i64::MAX as u64 {
                Kind::Int64
            } else {
                warn_once(
                    warnings,
                    "a uint64 has no BSON type and became a double, losing precision",
                );
                Kind::Double
            }
        }
        kind => kind,
    }
}

fn warn_once(warnings: &mut Vec<String>, message: &str) {
    let message = message.to_string();
    if !warnings.contains(&message) {
        warnings.push(message);
    }
}

fn write_value(
    doc: &Doc,
    id: u32,
    n: Node,
    kind: Kind,
    out: &mut Vec<u8>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    match kind {
        Kind::Double => {
            // A uint64 widened to a double carries its value in `j`.
            let value = if n.kind == Kind::UInt64 { n.j as f64 } else { n.f };
            out.extend_from_slice(&value.to_le_bytes());
        }
        Kind::Str | Kind::Symbol | Kind::JavaScript => {
            let bytes = string_payload(doc, n)?;
            write_bson_string(out, &bytes)?;
        }
        Kind::Object | Kind::Array => write_document(doc, id, out, warnings)?,
        Kind::Binary => {
            let data = doc.raw(n.val);
            if data.len() > i32::MAX as usize {
                return Err(Error::Serialize("binary payload is over 2 GiB".to_string()));
            }
            out.extend_from_slice(&(data.len() as i32).to_le_bytes());
            out.push(n.sub);
            out.extend_from_slice(data);
        }
        Kind::Undefined | Kind::Null | Kind::MinKey | Kind::MaxKey => {}
        Kind::ObjectId => {
            let oid = doc.raw(n.val);
            if oid.len() != 12 {
                return Err(Error::Serialize(format!(
                    "an objectId must be 12 bytes, this one is {}",
                    oid.len()
                )));
            }
            out.extend_from_slice(oid);
        }
        Kind::Bool => out.push(u8::from(n.sub == 1)),
        Kind::DateTime => out.extend_from_slice(&n.i.to_le_bytes()),
        Kind::Regex => {
            let (pattern, options) = split_nul(doc.raw(n.val));
            write_cstring(out, pattern)?;
            write_cstring(out, options)?;
        }
        Kind::DbPointer => {
            let (namespace, oid) = split_nul(doc.raw(n.val));
            if oid.len() != 12 {
                return Err(Error::Serialize(format!(
                    "a dbPointer id must be 12 bytes, this one is {}",
                    oid.len()
                )));
            }
            write_bson_string(out, namespace)?;
            out.extend_from_slice(oid);
        }
        Kind::JavaScriptWithScope => {
            let start = out.len();
            out.extend_from_slice(&[0; 4]);
            let code = doc.raw(n.val);
            write_bson_string(out, code)?;
            if n.aux == NO_NODE {
                write_empty_document(out);
            } else {
                write_document(doc, n.aux, out, warnings)?;
            }
            patch_length(out, start)?;
        }
        Kind::Int32 => out.extend_from_slice(&(n.i as i32).to_le_bytes()),
        Kind::Int64 => out.extend_from_slice(&n.i.to_le_bytes()),
        // BSON stores a timestamp as an increment followed by the seconds.
        Kind::Timestamp => {
            out.extend_from_slice(&(n.j as u32).to_le_bytes());
            out.extend_from_slice(&(n.i as u32).to_le_bytes());
        }
        Kind::Decimal128 => {
            out.extend_from_slice(&(n.i as u64).to_le_bytes());
            out.extend_from_slice(&n.j.to_le_bytes());
        }
        Kind::UInt64 => {
            // `bson_kind` always rewrites UInt64 to Int64 or Double.
            return Err(Error::Serialize("uint64 is not a BSON element type".to_string()));
        }
    }
    Ok(())
}

fn write_empty_document(out: &mut Vec<u8>) {
    out.extend_from_slice(&5i32.to_le_bytes());
    out.push(0);
}

/// Patch the placeholder length at `start`. `out` must be the whole output
/// buffer, since the length is measured from `start` to its end.
fn patch_length(out: &mut [u8], start: usize) -> Result<()> {
    let total = out.len() - start;
    if total > i32::MAX as usize {
        return Err(Error::Serialize("code-with-scope is over the 2 GiB limit".to_string()));
    }
    out[start..start + 4].copy_from_slice(&(total as i32).to_le_bytes());
    Ok(())
}

fn write_bson_string(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let len = bytes.len() + 1;
    if len > i32::MAX as usize {
        return Err(Error::Serialize("string is over the 2 GiB BSON limit".to_string()));
    }
    out.extend_from_slice(&(len as i32).to_le_bytes());
    out.extend_from_slice(bytes);
    out.push(0);
    Ok(())
}

fn write_cstring(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    if bytes.contains(&0) {
        return Err(Error::Serialize("a BSON key cannot contain a NUL byte".to_string()));
    }
    out.extend_from_slice(bytes);
    out.push(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bson_read;
    use crate::opts::Format;
    use crate::testutil::{bson, f64, i32, i64, parse_one, string, u32, u64};

    fn bson_doc(json: &str) -> Vec<u8> {
        let doc = Doc::from_json(json.as_bytes().to_vec()).expect("valid json");
        let mut out = Vec::new();
        write_bson(&doc, &mut out).expect("serializes");
        out
    }

    fn round_trip(bytes: &[u8]) -> Vec<u8> {
        let mut doc = Doc::new(Format::Bson);
        let parsed = bson_read::parse(bytes, &mut doc).expect("parses");
        assert_eq!(parsed.roots.len(), 1);
        doc.root = parsed.roots[0];
        let mut out = Vec::new();
        write_bson(&doc, &mut out).expect("serializes");
        out
    }

    #[test]
    fn a_json_object_becomes_a_well_formed_document() {
        let bytes = bson_doc(r#"{"a":1,"b":"two","c":true,"d":null}"#);
        let declared = i32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        assert_eq!(declared, bytes.len(), "the length prefix covers the whole document");
        assert_eq!(*bytes.last().unwrap(), 0, "documents end with a terminator");

        // Read the wire form back rather than poking at fixed offsets.
        let mut doc = Doc::new(Format::Bson);
        let parsed = bson_read::parse(&bytes, &mut doc).unwrap();
        doc.root = parsed.roots[0];
        let kinds: Vec<Kind> =
            doc.children(doc.root()).iter().map(|&c| doc.node_at(c).kind).collect();
        assert_eq!(kinds, [Kind::Int64, Kind::Str, Kind::Bool, Kind::Null]);
        let keys: Vec<String> =
            doc.children(doc.root()).iter().map(|&c| doc.key_of(c).into_owned()).collect();
        assert_eq!(keys, ["a", "b", "c", "d"]);
    }

    #[test]
    fn arrays_become_documents_keyed_by_position() {
        let bytes = bson_doc(r#"{"list":[10,20,30]}"#);
        let mut doc = Doc::new(Format::Bson);
        let parsed = bson_read::parse(&bytes, &mut doc).unwrap();
        doc.root = parsed.roots[0];
        let list = doc.children(doc.root())[0];
        assert_eq!(doc.node_at(list).kind, Kind::Array);
        assert_eq!(doc.children(list).len(), 3);

        // The wire form really does carry "0", "1", "2" as keys.
        assert!(bytes.windows(2).any(|w| w == b"0\0"));
        assert!(bytes.windows(2).any(|w| w == b"1\0"));
        assert!(bytes.windows(2).any(|w| w == b"2\0"));
    }

    #[test]
    fn nested_documents_nest_on_the_wire() {
        let bytes = bson_doc(r#"{"outer":{"inner":{"deep":[1,2]}}}"#);
        let again = round_trip(&bytes);
        assert_eq!(again, bytes);
    }

    #[test]
    fn every_bson_type_survives_a_byte_exact_round_trip() {
        let mut dbpointer = string("ns");
        dbpointer.extend_from_slice(&[0x22; 12]);
        let mut timestamp = u32(7);
        timestamp.extend_from_slice(&u32(5));
        let mut decimal = u64(15);
        decimal.extend_from_slice(&u64(0x303E_0000_0000_0000));

        let bytes = bson(&[
            ("double", 0x01, &f64(8.5)),
            ("string", 0x02, &string("hello")),
            ("binary", 0x05, &[3, 0, 0, 0, 0x80, 1, 2, 3]),
            ("undefined", 0x06, &[]),
            ("oid", 0x07, &[0x11; 12]),
            ("bool", 0x08, &[1]),
            ("datetime", 0x09, &i64(1_700_000_000_123)),
            ("null", 0x0A, &[]),
            ("regex", 0x0B, b"^ab\0ims\0"),
            ("dbpointer", 0x0C, &dbpointer),
            ("javascript", 0x0D, &string("x")),
            ("symbol", 0x0E, &string("sym")),
            ("int32", 0x10, &i32(-42)),
            ("timestamp", 0x11, &timestamp),
            ("int64", 0x12, &i64(-9_000_000_000)),
            ("decimal128", 0x13, &decimal),
            ("minkey", 0xFF, &[]),
            ("maxkey", 0x7F, &[]),
        ]);

        assert_eq!(round_trip(&bytes), bytes, "byte-exact round trip");

        // And the values read back as the types they were written with.
        let doc = parse_one(&bytes);
        let kinds: Vec<Kind> =
            doc.children(doc.root()).iter().map(|&c| doc.node_at(c).kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Double,
                Kind::Str,
                Kind::Binary,
                Kind::Undefined,
                Kind::ObjectId,
                Kind::Bool,
                Kind::DateTime,
                Kind::Null,
                Kind::Regex,
                Kind::DbPointer,
                Kind::JavaScript,
                Kind::Symbol,
                Kind::Int32,
                Kind::Timestamp,
                Kind::Int64,
                Kind::Decimal128,
                Kind::MinKey,
                Kind::MaxKey,
            ]
        );
    }

    #[test]
    fn code_with_scope_round_trips() {
        let scope = bson(&[("a", 0x10, &i32(1))]);
        let code = string("code");
        let mut payload = Vec::new();
        payload.extend_from_slice(&((4 + code.len() + scope.len()) as i32).to_le_bytes());
        payload.extend_from_slice(&code);
        payload.extend_from_slice(&scope);
        let bytes = bson(&[("fn", 0x0F, &payload)]);

        let again = round_trip(&bytes);
        assert_eq!(again, bytes);
        assert_eq!(again[4], 0x0F, "the element type byte survives");

        // The scope hangs off `aux` rather than being an ordinary child.
        let doc = parse_one(&bytes);
        let fn_node = doc.children(doc.root())[0];
        assert_eq!(doc.node_at(fn_node).kind, Kind::JavaScriptWithScope);
        assert_eq!(doc.text(doc.node_at(fn_node).val), "code");
        let scope_id = doc.node_at(fn_node).aux;
        assert_eq!(doc.key_of(doc.children(scope_id)[0]), "a");
    }

    #[test]
    fn a_document_stream_is_written_back_as_a_stream() {
        let one = bson_doc(r#"{"a":1}"#);
        let two = bson_doc(r#"{"b":2}"#);
        let mut stream = one.clone();
        stream.extend_from_slice(&two);

        let mut doc = Doc::new(Format::Bson);
        let parsed = bson_read::parse(&stream, &mut doc).unwrap();
        assert_eq!(parsed.roots.len(), 2);
        // Wrap the documents in the synthetic array root the loader would use.
        let array = doc.open(Kind::Array, crate::arena::Span::NONE);
        for &r in &parsed.roots {
            doc.push_child(r, array);
        }
        doc.close(array);
        doc.root = array;
        doc.synthetic_root = true;

        let mut out = Vec::new();
        write_bson(&doc, &mut out).unwrap();
        assert_eq!(out, stream);
    }

    #[test]
    fn a_top_level_array_cannot_be_saved_as_bson() {
        let doc = Doc::from_json(br#"[1,2,3]"#.to_vec()).unwrap();
        let mut out = Vec::new();
        let err = write_bson(&doc, &mut out).unwrap_err();
        assert!(matches!(err, Error::Serialize(msg) if msg.contains("must start with a document")));
    }

    #[test]
    fn out_of_range_integers_are_widened_with_a_warning() {
        let mut doc = Doc::from_json(br#"{"n":1}"#.to_vec()).unwrap();
        let child = doc.children(doc.root())[0];
        doc.nodes[child as usize].kind = Kind::Int32;
        doc.nodes[child as usize].i = 5_000_000_000;
        let mut out = Vec::new();
        let warnings = write_bson(&doc, &mut out).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("int64"));
        assert_eq!(out[4], 0x12, "written as int64");
    }

    #[test]
    fn a_uint64_past_i64_becomes_a_double() {
        let mut doc = Doc::from_json(br#"{"n":1}"#.to_vec()).unwrap();
        let child = doc.children(doc.root())[0];
        doc.nodes[child as usize].kind = Kind::UInt64;
        doc.nodes[child as usize].j = u64::MAX;
        let mut out = Vec::new();
        let warnings = write_bson(&doc, &mut out).unwrap();
        assert!(warnings[0].contains("double"), "{warnings:?}");
        assert_eq!(out[4], 0x01);
    }

    #[test]
    fn keys_containing_nul_are_refused() {
        let mut doc = Doc::from_json(br#"{"n":1}"#.to_vec()).unwrap();
        let child = doc.children(doc.root())[0];
        doc.nodes[child as usize].key = doc.push_blob(b"a\0b");
        let mut out = Vec::new();
        assert!(matches!(write_bson(&doc, &mut out), Err(Error::Serialize(_))));
    }

    #[test]
    fn an_object_id_of_the_wrong_length_is_refused() {
        let mut doc = Doc::from_json(br#"{"n":1}"#.to_vec()).unwrap();
        let child = doc.children(doc.root())[0];
        doc.nodes[child as usize].kind = Kind::ObjectId;
        doc.nodes[child as usize].val = doc.push_blob(&[1, 2, 3]);
        let mut out = Vec::new();
        assert!(matches!(write_bson(&doc, &mut out), Err(Error::Serialize(_))));
    }
}
