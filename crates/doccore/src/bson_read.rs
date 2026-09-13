//! BSON → arena parser, plus an allocation-free structural scanner.
//!
//! Two passes exist over the same byte layout on purpose. [`scan`] validates
//! structure and measures element sizes without touching the arena, which is
//! what makes magic-byte detection cheap: candidate offsets can be rejected
//! without building anything. [`parse`] then fills the arena once the framing
//! is known.

use crate::arena::{Doc, Kind, Node, Span, MAX_DEPTH};
use crate::error::{Error, Result};

/// Smallest legal BSON document: a 4-byte length plus the terminator.
pub(crate) const MIN_DOC_LEN: usize = 5;

/// Outcome of walking a BSON document stream.
#[derive(Clone, Copy, Debug)]
pub struct Scan {
    /// Offset just past the last complete document.
    pub end: usize,
    pub documents: usize,
}

/// Outcome of parsing a BSON document stream into the arena.
pub struct Parsed {
    /// Root node of each document, in file order.
    pub roots: Vec<u32>,
    pub scan: Scan,
}

// ------------------------------------------------------------------- scanning

/// Validate a BSON document stream starting at `start` without allocating.
///
/// Stops at the first position that cannot begin another complete document, so
/// a file made of a prefix plus documents plus a trailer reports where the
/// documents end. Fails only when not even one document parses.
pub fn scan(bytes: &[u8], start: usize) -> Result<Scan> {
    let mut pos = start;
    let mut documents = 0;
    while let Some(declared) = declared_length(bytes, pos) {
        match scan_document(bytes, pos, declared, 0) {
            Ok(next) => {
                pos = next;
                documents += 1;
            }
            // Nesting past the cap is a property of the file, not a sign that
            // the prefix guess was wrong, so report it rather than searching on.
            Err(err @ Error::TooDeep(_)) => return Err(err),
            Err(_) => break,
        }
    }
    if documents == 0 {
        return Err(Error::bson("no valid BSON document found", start));
    }
    Ok(Scan { end: pos, documents })
}

/// The document length declared at `pos`, if a document could start there.
fn declared_length(bytes: &[u8], pos: usize) -> Option<usize> {
    let raw = bytes.get(pos..pos + 4)?;
    let len = i32::from_le_bytes(raw.try_into().ok()?) as i64;
    if len < MIN_DOC_LEN as i64 {
        return None;
    }
    let len = len as usize;
    if pos.checked_add(len)? > bytes.len() {
        return None;
    }
    Some(len)
}

/// True when a BSON document starts at offset 0 and its declared length covers
/// the whole buffer exactly. Lets framing detection skip a validation pass on
/// the overwhelmingly common single-document file, since `parse` checks the
/// structure anyway.
pub fn declares_exact_document(bytes: &[u8]) -> bool {
    declared_length(bytes, 0) == Some(bytes.len())
}

fn scan_document(bytes: &[u8], start: usize, len: usize, depth: u32) -> Result<usize> {
    if depth >= MAX_DEPTH {
        return Err(Error::TooDeep(MAX_DEPTH));
    }
    let end = start + len;
    let mut pos = start + 4;
    loop {
        if pos >= end {
            return Err(Error::bson("document is missing its 0x00 terminator", pos));
        }
        let tag = bytes[pos];
        pos += 1;
        if tag == 0 {
            break;
        }
        let kind = Kind::from_bson_byte(tag)
            .ok_or_else(|| Error::bson(format!("unknown element type {tag:#04x}"), pos))?;
        pos = scan_cstring(bytes, pos)?;
        pos = scan_value(bytes, pos, kind, depth)?;
    }
    if pos != end {
        return Err(Error::bson("declared length does not match the elements", start));
    }
    Ok(end)
}

fn scan_value(bytes: &[u8], pos: usize, kind: Kind, depth: u32) -> Result<usize> {
    match kind {
        Kind::Double | Kind::DateTime | Kind::Int64 | Kind::Timestamp => advance(bytes, pos, 8),
        Kind::Int32 => advance(bytes, pos, 4),
        Kind::Decimal128 => advance(bytes, pos, 16),
        Kind::ObjectId => advance(bytes, pos, 12),
        Kind::Bool => {
            let value = *bytes.get(pos).ok_or_else(|| truncated(pos))?;
            if value > 1 {
                return Err(Error::bson(format!("boolean byte must be 0 or 1, got {value}"), pos));
            }
            Ok(pos + 1)
        }
        Kind::Undefined | Kind::Null | Kind::MinKey | Kind::MaxKey => Ok(pos),
        Kind::Str | Kind::Symbol | Kind::JavaScript => scan_string(bytes, pos),
        Kind::Object | Kind::Array => {
            let len = declared_length(bytes, pos)
                .ok_or_else(|| Error::bson("nested document length is out of range", pos))?;
            scan_document(bytes, pos, len, depth + 1)
        }
        Kind::Binary => {
            // The int32 counts the data bytes only; the subtype byte is extra.
            let len = payload_length(bytes, pos, 0)?;
            advance(bytes, pos + 4, 1 + len)
        }
        Kind::Regex => {
            let after_pattern = scan_cstring(bytes, pos)?;
            scan_cstring(bytes, after_pattern)
        }
        Kind::DbPointer => {
            let after_name = scan_string(bytes, pos)?;
            advance(bytes, after_name, 12)
        }
        Kind::JavaScriptWithScope => {
            let start = pos;
            let total = read_i32(bytes, pos)?;
            if total < 4 {
                return Err(Error::bson("code-with-scope length is below its minimum", start));
            }
            let end = start + total as usize;
            if end > bytes.len() {
                return Err(Error::bson("code-with-scope is truncated", start));
            }
            let after_code = scan_string(bytes, start + 4)?;
            let scope_len = declared_length(bytes, after_code)
                .ok_or_else(|| Error::bson("scope document length is out of range", after_code))?;
            let after_scope = scan_document(bytes, after_code, scope_len, depth + 1)?;
            if after_scope != end {
                return Err(Error::bson("code-with-scope length does not match its parts", start));
            }
            Ok(end)
        }
        // Not a BSON type; `Kind::from_bson_byte` never yields it.
        Kind::UInt64 => Err(Error::bson("uint64 is not a BSON element type", pos)),
    }
}

/// A BSON string: an int32 length that counts its own NUL terminator.
fn scan_string(bytes: &[u8], pos: usize) -> Result<usize> {
    let len = payload_length(bytes, pos, 1)?;
    let end = pos + 4 + len;
    if bytes[end - 1] != 0 {
        return Err(Error::bson("string is not NUL-terminated", pos));
    }
    Ok(end)
}

fn scan_cstring(bytes: &[u8], pos: usize) -> Result<usize> {
    bytes[pos..]
        .iter()
        .position(|&b| b == 0)
        .map(|i| pos + i + 1)
        .ok_or_else(|| Error::bson("unterminated cstring", pos))
}

/// Read an int32 payload length and check that the body it describes fits.
fn payload_length(bytes: &[u8], pos: usize, minimum: i32) -> Result<usize> {
    let len = read_i32(bytes, pos)?;
    if len < minimum {
        return Err(Error::bson(format!("payload length {len} is below {minimum}"), pos));
    }
    let end = pos
        .checked_add(4)
        .and_then(|p| p.checked_add(len as usize))
        .ok_or_else(|| Error::bson("payload length overflows", pos))?;
    if end > bytes.len() {
        return Err(Error::bson("payload is truncated", pos));
    }
    Ok(len as usize)
}

fn read_i32(bytes: &[u8], pos: usize) -> Result<i32> {
    let raw = bytes.get(pos..pos + 4).ok_or_else(|| truncated(pos))?;
    Ok(i32::from_le_bytes(raw.try_into().expect("four bytes")))
}

fn advance(bytes: &[u8], pos: usize, n: usize) -> Result<usize> {
    let end = pos.checked_add(n).ok_or_else(|| truncated(pos))?;
    if end > bytes.len() {
        return Err(truncated(pos));
    }
    Ok(end)
}

fn truncated(pos: usize) -> Error {
    Error::bson("unexpected end of input", pos)
}

// --------------------------------------------------------------------- parsing

struct Reader<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn read_u8(&mut self) -> Result<u8> {
        let b = *self.input.get(self.pos).ok_or_else(|| truncated(self.pos))?;
        self.pos += 1;
        Ok(b)
    }

    fn read_i32(&mut self) -> Result<i32> {
        let v = read_i32(self.input, self.pos)?;
        self.pos += 4;
        Ok(v)
    }

    fn read_u32(&mut self) -> Result<u32> {
        Ok(self.read_i32()? as u32)
    }

    fn read_i64(&mut self) -> Result<i64> {
        let raw = self.take(8)?;
        Ok(i64::from_le_bytes(raw.try_into().expect("eight bytes")))
    }

    fn read_u64(&mut self) -> Result<u64> {
        let raw = self.take(8)?;
        Ok(u64::from_le_bytes(raw.try_into().expect("eight bytes")))
    }

    fn read_f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| truncated(self.pos))?;
        let slice = self.input.get(self.pos..end).ok_or_else(|| truncated(self.pos))?;
        self.pos = end;
        Ok(slice)
    }

    fn cstring(&mut self) -> Result<&'a [u8]> {
        let start = self.pos;
        let end = scan_cstring(self.input, start)?;
        self.pos = end;
        Ok(&self.input[start..end - 1])
    }

    fn bson_string(&mut self) -> Result<&'a [u8]> {
        let start = self.pos;
        let len = payload_length(self.input, start, 1)?;
        self.pos += 4;
        let body = self.take(len)?;
        if body[len - 1] != 0 {
            return Err(Error::bson("string is not NUL-terminated", start));
        }
        Ok(&body[..len - 1])
    }
}

/// Parse a BSON document stream into `doc`.
///
/// Parsing is lenient about what follows the last complete document: real dumps
/// often carry a checksum or padding, and refusing to open them would be worse
/// than preserving the extra bytes verbatim.
pub fn parse(bytes: &[u8], doc: &mut Doc) -> Result<Parsed> {
    doc.reserve_for(bytes.len());
    let mut r = Reader { input: bytes, pos: 0 };
    let mut roots = Vec::new();
    while let Some(len) = declared_length(bytes, r.pos) {
        // A failed attempt must not leave half-built containers behind, or the
        // arena's open/close stack and depth counter would be corrupted for
        // every later operation.
        let checkpoint =
            (doc.nodes.len(), doc.children.len(), doc.blob.len(), doc.containers.len());
        match parse_document(&mut r, doc, Span::NONE, 0, Kind::Object, len) {
            Ok(id) => roots.push(id),
            Err(err) => {
                if roots.is_empty() {
                    return Err(err);
                }
                doc.nodes.truncate(checkpoint.0);
                doc.children.truncate(checkpoint.1);
                doc.blob.truncate(checkpoint.2);
                doc.containers.truncate(checkpoint.3);
                doc.stack.clear();
                doc.depth = 0;
                doc.warnings.push(format!("stopped after {} document(s): {}", roots.len(), err));
                break;
            }
        }
    }
    if roots.is_empty() {
        return Err(Error::bson("no BSON document found", 0));
    }
    let documents = roots.len();
    Ok(Parsed { roots, scan: Scan { end: r.pos, documents } })
}

fn parse_document(
    r: &mut Reader,
    doc: &mut Doc,
    key: Span,
    depth: u32,
    kind: Kind,
    len: usize,
) -> Result<u32> {
    if depth >= MAX_DEPTH {
        return Err(Error::TooDeep(MAX_DEPTH));
    }
    let start = r.pos;
    let end = start + len;
    r.pos += 4;
    let id = doc.open(kind, key);
    parse_elements(r, doc, id, end, depth, kind == Kind::Array)?;
    if r.pos != end {
        return Err(Error::bson("declared length does not match the elements", start));
    }
    doc.close(id);
    Ok(id)
}

fn parse_elements(
    r: &mut Reader,
    doc: &mut Doc,
    id: u32,
    end: usize,
    depth: u32,
    as_array: bool,
) -> Result<()> {
    let mut index = 0u32;
    let mut counter = itoa::Buffer::new();
    let mut keys_discarded = false;
    loop {
        if r.pos >= end {
            return Err(Error::bson("document is missing its 0x00 terminator", r.pos));
        }
        let tag = r.read_u8()?;
        if tag == 0 {
            break;
        }
        let tag_pos = r.pos - 1;
        let kind = Kind::from_bson_byte(tag)
            .ok_or_else(|| Error::bson(format!("unknown element type {tag:#04x}"), tag_pos))?;
        let name = r.cstring()?;
        // BSON arrays store their positions as string keys. The arena derives
        // them from child order instead, so the keys are dropped — but only
        // after checking they really were positions.
        let key = if as_array {
            if counter.format(index).as_bytes() != name {
                keys_discarded = true;
            }
            Span::NONE
        } else {
            doc.push_blob(name)
        };
        let child = parse_value(r, doc, kind, key, depth)?;
        doc.push_child(child, id);
        index += 1;
    }
    if keys_discarded {
        doc.warnings.push(
            "array element keys were not sequential indexes; positions now come from order"
                .to_string(),
        );
    }
    Ok(())
}

fn parse_value(r: &mut Reader, doc: &mut Doc, kind: Kind, key: Span, depth: u32) -> Result<u32> {
    let mut n = Node::new(kind);
    n.key = key;
    match kind {
        Kind::Double => n.f = r.read_f64()?,
        Kind::Str | Kind::Symbol | Kind::JavaScript => {
            let text = r.bson_string()?;
            n.val = doc.push_blob(text);
        }
        Kind::Object => {
            let len = declared_length(r.input, r.pos)
                .ok_or_else(|| Error::bson("nested document length is out of range", r.pos))?;
            return parse_document(r, doc, key, depth + 1, Kind::Object, len);
        }
        Kind::Array => {
            let len = declared_length(r.input, r.pos)
                .ok_or_else(|| Error::bson("array length is out of range", r.pos))?;
            return parse_document(r, doc, key, depth + 1, Kind::Array, len);
        }
        Kind::Binary => {
            // The int32 counts the data bytes only; the subtype byte is extra.
            let len = payload_length(r.input, r.pos, 0)?;
            r.pos += 4;
            let subtype = r.read_u8()?;
            let data = r.take(len)?;
            n.sub = subtype;
            n.val = doc.push_blob(data);
        }
        Kind::Undefined | Kind::Null | Kind::MinKey | Kind::MaxKey => {}
        Kind::ObjectId => {
            let oid = r.take(12)?;
            n.val = doc.push_blob(oid);
        }
        Kind::Bool => {
            let value = r.read_u8()?;
            if value > 1 {
                return Err(Error::bson(
                    format!("boolean byte must be 0 or 1, got {value}"),
                    r.pos,
                ));
            }
            n.sub = value;
        }
        Kind::DateTime => n.i = r.read_i64()?,
        Kind::Regex => {
            let pattern = r.cstring()?;
            let options = r.cstring()?;
            n.val = doc.push_joined(&[pattern, options]);
        }
        Kind::DbPointer => {
            let namespace = r.bson_string()?;
            let oid = r.take(12)?;
            n.val = doc.push_joined(&[namespace, oid]);
        }
        Kind::JavaScriptWithScope => {
            let start = r.pos;
            let total = r.read_i32()?;
            if total < 4 {
                return Err(Error::bson("code-with-scope length is below its minimum", start));
            }
            let end = start + total as usize;
            if end > r.input.len() {
                return Err(Error::bson("code-with-scope is truncated", start));
            }
            let code = r.bson_string()?;
            n.val = doc.push_blob(code);
            let scope_len = declared_length(r.input, r.pos)
                .ok_or_else(|| Error::bson("scope document length is out of range", r.pos))?;
            let scope = parse_document(r, doc, Span::NONE, depth + 1, Kind::Object, scope_len)?;
            if r.pos != end {
                return Err(Error::bson("code-with-scope length does not match its parts", start));
            }
            n.aux = scope;
            let id = doc.add_node(n);
            doc.nodes[scope as usize].parent = id;
            return Ok(id);
        }
        Kind::Int32 => n.i = r.read_i32()? as i64,
        // BSON stores a timestamp as an increment followed by the seconds.
        Kind::Timestamp => {
            let increment = r.read_u32()?;
            let seconds = r.read_u32()?;
            n.i = seconds as i64;
            n.j = increment as u64;
        }
        Kind::Int64 => n.i = r.read_i64()?,
        Kind::Decimal128 => {
            n.i = r.read_u64()? as i64;
            n.j = r.read_u64()?;
        }
        Kind::UInt64 => return Err(Error::bson("uint64 is not a BSON element type", r.pos)),
    }
    Ok(doc.add_node(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numfmt::hex_encode;
    use crate::opts::Format;
    use crate::testutil::{bson, i32, i64, nest, parse_one, string};

    #[test]
    fn parses_a_document_of_every_scalar_type() {
        let bytes = bson(&[
            ("d", 0x01, &8.5f64.to_le_bytes()),
            ("s", 0x02, &string("hello")),
            ("b", 0x05, &[3, 0, 0, 0, 0x80, 1, 2, 3]),
            ("u", 0x06, &[]),
            ("o", 0x07, &[0xaa; 12]),
            ("t", 0x08, &[1]),
            ("dt", 0x09, &i64(1_700_000_000_123)),
            ("n", 0x0A, &[]),
            ("r", 0x0B, b"^ab\0i\0"),
            ("i", 0x10, &i32(-42)),
            ("ts", 0x11, &[7, 0, 0, 0, 5, 0, 0, 0]),
            ("l", 0x12, &i64(-9_000_000_000)),
            ("dec", 0x13, &[15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x3e, 0x30]),
            ("mn", 0xFF, &[]),
            ("mx", 0x7F, &[]),
        ]);
        let doc = parse_one(&bytes);
        let kids = doc.children(doc.root());
        assert_eq!(kids.len(), 15);

        let by_name = |name: &str| -> u32 {
            *kids.iter().find(|&&k| doc.key_of(k) == name).expect("key present")
        };
        assert_eq!(doc.node_at(by_name("d")).f, 8.5);
        assert_eq!(doc.text(doc.node_at(by_name("s")).val), "hello");
        let bin = doc.node_at(by_name("b"));
        assert_eq!(bin.sub, 0x80);
        assert_eq!(doc.raw(bin.val), &[1, 2, 3]);
        assert_eq!(doc.node_at(by_name("u")).kind, Kind::Undefined);
        assert_eq!(hex_encode(doc.value_bytes(by_name("o"))), "aaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(doc.node_at(by_name("t")).sub, 1);
        assert_eq!(doc.node_at(by_name("dt")).i, 1_700_000_000_123);
        assert_eq!(doc.node_at(by_name("n")).kind, Kind::Null);
        let re = doc.node_at(by_name("r"));
        assert_eq!(doc.raw(re.val), b"^ab\0i");
        assert_eq!(doc.node_at(by_name("i")).i, -42);
        let ts = doc.node_at(by_name("ts"));
        assert_eq!((ts.i, ts.j), (5, 7), "timestamp is increment then seconds");
        assert_eq!(doc.node_at(by_name("l")).i, -9_000_000_000);
        let dec = doc.node_at(by_name("dec"));
        assert_eq!(crate::numfmt::decimal128_to_string(dec.i as u64, dec.j), "1.5");
        assert_eq!(doc.node_at(by_name("mn")).kind, Kind::MinKey);
        assert_eq!(doc.node_at(by_name("mx")).kind, Kind::MaxKey);
    }

    #[test]
    fn parses_nested_documents_and_arrays() {
        let inner = bson(&[("x", 0x10, &i32(1))]);
        let array = bson(&[("0", 0x10, &i32(10)), ("1", 0x02, &string("two"))]);
        let bytes = bson(&[("sub", 0x03, &inner), ("arr", 0x04, &array)]);
        let doc = parse_one(&bytes);
        let kids = doc.children(doc.root());
        let sub = kids[0];
        assert_eq!(doc.node_at(sub).kind, Kind::Object);
        assert_eq!(doc.key_of(doc.children(sub)[0]), "x");
        let arr = kids[1];
        assert_eq!(doc.node_at(arr).kind, Kind::Array);
        let items = doc.children(arr);
        assert_eq!(items.len(), 2);
        assert!(doc.node_at(items[0]).key.is_none(), "array positions are not stored as keys");
        assert_eq!(doc.node_at(items[0]).i, 10);
        assert!(doc.warnings.is_empty());
    }

    #[test]
    fn array_keys_that_are_not_indexes_are_reported() {
        let array = bson(&[("0", 0x10, &i32(10)), ("wrong", 0x10, &i32(11))]);
        let bytes = bson(&[("arr", 0x04, &array)]);
        let doc = parse_one(&bytes);
        assert_eq!(doc.warnings.len(), 1, "{:?}", doc.warnings);
        let arr = doc.children(doc.root())[0];
        assert_eq!(doc.children(arr).len(), 2, "elements are kept regardless");
    }

    #[test]
    fn parses_code_with_scope_and_links_the_scope_as_aux() {
        let scope = bson(&[("a", 0x10, &i32(1))]);
        let mut payload = Vec::new();
        let total = 4 + (4 + "code".len() + 1) + scope.len();
        payload.extend_from_slice(&(total as i32).to_le_bytes());
        payload.extend_from_slice(&string("code"));
        payload.extend_from_slice(&scope);
        let bytes = bson(&[("fn", 0x0F, &payload)]);

        let doc = parse_one(&bytes);
        let fn_node = doc.children(doc.root())[0];
        assert_eq!(doc.node_at(fn_node).kind, Kind::JavaScriptWithScope);
        assert_eq!(doc.text(doc.node_at(fn_node).val), "code");
        let scope_id = doc.node_at(fn_node).aux;
        assert_ne!(scope_id, crate::arena::NO_NODE);
        assert_eq!(doc.parent_of(scope_id), fn_node);
        assert_eq!(doc.key_of(doc.children(scope_id)[0]), "a");
        // The scope must not also appear as an ordinary child.
        assert_eq!(doc.children(fn_node).len(), 0);
    }

    #[test]
    fn parses_a_stream_of_documents() {
        let one = bson(&[("a", 0x10, &i32(1))]);
        let two = bson(&[("b", 0x10, &i32(2))]);
        let mut stream = one.clone();
        stream.extend_from_slice(&two);

        let mut doc = Doc::new(Format::Bson);
        let parsed = parse(&stream, &mut doc).unwrap();
        assert_eq!(parsed.roots.len(), 2);
        assert_eq!(parsed.scan.end, stream.len());
        assert_eq!(doc.key_of(doc.children(parsed.roots[0])[0]), "a");
        assert_eq!(doc.key_of(doc.children(parsed.roots[1])[0]), "b");
    }

    #[test]
    fn a_partial_document_after_a_complete_one_becomes_trailing_bytes() {
        let one = bson(&[("a", 0x10, &i32(1))]);
        let mut stream = one.clone();
        stream.extend_from_slice(&[0x01, 0x02, 0x03]);

        let mut doc = Doc::new(Format::Bson);
        let parsed = parse(&stream, &mut doc).unwrap();
        assert_eq!(parsed.roots.len(), 1);
        assert_eq!(parsed.scan.end, one.len());
        assert_eq!(doc.warnings.len(), 0, "a short remainder is not worth warning about");
    }

    #[test]
    fn corrupt_documents_are_rejected_with_an_offset() {
        // Declared length longer than the buffer.
        let mut truncated = bson(&[("a", 0x10, &i32(1))]);
        let real = i32::from_le_bytes(truncated[0..4].try_into().unwrap());
        truncated[0..4].copy_from_slice(&(real + 100).to_le_bytes());
        assert!(matches!(parse(&truncated, &mut Doc::new(Format::Bson)), Err(Error::Bson { .. })));

        // Unknown element type, reported at the byte that holds the bad tag.
        let mut bad_type = bson(&[("a", 0x10, &i32(1))]);
        bad_type[4] = 0x42;
        assert!(matches!(
            parse(&bad_type, &mut Doc::new(Format::Bson)),
            Err(Error::Bson { offset: 4, .. })
        ));

        // Missing terminator: declared length runs past the elements.
        let no_term = vec![9, 0, 0, 0, 0x10, b'a', 0, 1, 0];
        assert!(matches!(parse(&no_term, &mut Doc::new(Format::Bson)), Err(Error::Bson { .. })));
    }

    #[test]
    fn deeply_nested_documents_hit_the_depth_cap() {
        // 200 levels is past MAX_DEPTH; both passes must refuse it instead of
        // overflowing the stack.
        let too_deep = nest(200);
        assert!(matches!(parse(&too_deep, &mut Doc::new(Format::Bson)), Err(Error::TooDeep(_))));
        assert!(matches!(scan(&too_deep, 0), Err(Error::TooDeep(_))));

        // Just under the cap still parses.
        let shallow_enough = nest(MAX_DEPTH as usize - 2);
        assert!(parse(&shallow_enough, &mut Doc::new(Format::Bson)).is_ok());
    }

    #[test]
    fn scan_matches_the_parser_on_where_documents_end() {
        let one = bson(&[("a", 0x10, &i32(1))]);
        let two = bson(&[("b", 0x02, &string("x"))]);
        let mut stream = one.clone();
        stream.extend_from_slice(&two);
        stream.extend_from_slice(&[0xDE, 0xAD]);

        let scanned = scan(&stream, 0).unwrap();
        assert_eq!(scanned.documents, 2);
        assert_eq!(scanned.end, one.len() + two.len());

        let mut doc = Doc::new(Format::Bson);
        let parsed = parse(&stream, &mut doc).unwrap();
        assert_eq!(parsed.scan.end, scanned.end, "scanner and parser must agree");
        assert_eq!(parsed.roots.len(), scanned.documents);
    }

    #[test]
    fn scan_rejects_input_that_is_not_bson() {
        assert!(scan(b"not bson at all", 0).is_err());
        assert!(scan(b"", 0).is_err());
        assert!(scan(&[4, 0, 0, 0, 0], 0).is_err(), "length below the 5-byte minimum");
    }
}
