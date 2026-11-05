//! Wire types handed to the webview.
//!
//! The tree is paged on purpose. A 12MB document holds close to two million
//! nodes; sending even one level of a large array in full would stall the
//! webview, so the frontend asks for the few hundred rows it is about to paint
//! and nothing more.

use doccore::arena::{Doc, Kind, Node, NO_NODE};
use doccore::numfmt;
use doccore::opts::{Format, JsonOptions, LoadOptions};
use doccore::Stats;
use serde::{Deserialize, Serialize};

/// Longest value text sent up for a single row. The full text is still one
/// `copySubtree` away; this is only what fits in a tree row.
const PREVIEW_BYTES: usize = 240;

/// An open tab: the id every other command addresses it by, plus its document
/// state.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabInfo {
    pub tab: u64,
    pub info: DocInfo,
}

/// Everything the status bar and the toolbar need about the open document.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocInfo {
    pub root: u32,
    /// Kind of the document root, so the text view knows whether to open with
    /// `{` or `[` without another round trip.
    pub root_kind: &'static str,
    /// The root's row preview, which is all a scalar root has to show.
    pub root_preview: String,
    pub path: Option<String>,
    pub format: Format,
    pub dirty: bool,
    pub expand_depth: u8,
    /// True when a multi-document BSON stream got an array root that is not
    /// really in the file and will not be written back out.
    pub synthetic_root: bool,
    /// The stripped magic-byte prefix, hex encoded, so the UI can show exactly
    /// what will be restored on save (requirement 1).
    pub magic: String,
    pub trailing_bytes: usize,
    pub warnings: Vec<String>,
    pub stats: Stats,
    /// Milliseconds the load took, shown next to the file size.
    pub load_ms: f64,
}

/// One row of the tree.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRow {
    pub id: u32,
    /// Position within the parent, so the UI can offer "insert before/after".
    pub index: u32,
    /// `None` for array elements, which are named by position.
    pub key: Option<String>,
    pub kind: &'static str,
    /// Kind after an embedded-JSON overlay is applied, so the text view opens
    /// the right bracket for a string presented as an object or array.
    pub presented_kind: &'static str,
    pub preview: String,
    /// Members of the node as presented, counting an embedded-JSON overlay.
    pub child_count: u32,
    pub expandable: bool,
    /// True when the value is a string that decoded into JSON (requirement 2).
    pub embedded_json: bool,
    /// How deep this string sits in the embedded-JSON chain, 1 for the outermost.
    pub overlay_level: u8,
    pub editable: bool,
}

/// One step of the path from the root to a match. The index is the node's
/// position inside its parent, which is what the frontend needs to page a
/// container far enough for the node to exist in the loaded rows.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchStep {
    pub node: u32,
    pub index: u32,
}

/// One hit from a full-document text search.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub node: u32,
    pub key: Option<String>,
    pub preview: String,
    /// Root down to the match, inclusive.
    pub path: Vec<SearchStep>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    pub matches: Vec<SearchMatch>,
    /// True when the limit cut the walk short, so there are more matches.
    pub truncated: bool,
}

/// A window onto one container's children.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildrenPage {
    pub parent: u32,
    pub offset: u32,
    pub total: u32,
    pub rows: Vec<NodeRow>,
}

/// What an edit command hands back: the affected node, plus fresh document
/// state so the status bar updates without a second round trip.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditResult {
    pub node: u32,
    pub info: DocInfo,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveResult {
    pub path: String,
    pub bytes_written: usize,
    pub format: Format,
    /// Fidelity notes, such as a JSON integer too large for BSON that had to
    /// widen to a double.
    pub warnings: Vec<String>,
}

/// Options the frontend may send. Everything defaults, so a partial object or
/// no object at all both work.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenOptions {
    pub expand_json_depth: u8,
    pub magic_len: Option<usize>,
    pub format: Option<Format>,
    /// Copy formatting, used by `copySubtree` and `exportText`.
    pub json: JsonOptions,
}

impl Default for OpenOptions {
    fn default() -> Self {
        OpenOptions {
            expand_json_depth: LoadOptions::default().expand_json_depth,
            magic_len: None,
            format: None,
            json: JsonOptions::default(),
        }
    }
}

impl OpenOptions {
    pub fn load(&self) -> LoadOptions {
        LoadOptions {
            expand_json_depth: self.expand_json_depth,
            magic_len: self.magic_len,
            format: self.format,
            ..LoadOptions::default()
        }
    }
}

/// Error shape the frontend receives. `kind` lets the UI react (a missing
/// document means "open something first"); `message` is what it shows.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub kind: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn no_document() -> ApiError {
        ApiError { kind: "noDocument", message: "no document is open".into() }
    }

    pub fn state(message: impl Into<String>) -> ApiError {
        ApiError { kind: "state", message: message.into() }
    }
}

impl From<doccore::Error> for ApiError {
    fn from(err: doccore::Error) -> ApiError {
        let kind = match err {
            doccore::Error::Io(_) => "io",
            doccore::Error::NoSuchNode(_) => "noNode",
            doccore::Error::Conflict(_) => "conflict",
            doccore::Error::Unsupported { .. } => "unsupported",
            _ => "core",
        };
        ApiError { kind, message: err.to_string() }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(err: std::io::Error) -> ApiError {
        ApiError { kind: "io", message: err.to_string() }
    }
}

pub type CmdResult<T> = Result<T, ApiError>;

impl DocInfo {
    pub fn of(doc: &Doc, load_ms: f64) -> DocInfo {
        let root = doc.node(doc.root()).ok();
        DocInfo {
            root: doc.root(),
            root_kind: root.map(|n| n.kind.type_name()).unwrap_or("unknown"),
            root_preview: root.map(|n| preview(doc, n)).unwrap_or_default(),
            path: doc.path.clone(),
            format: doc.format,
            dirty: doc.dirty,
            expand_depth: doc.expand_depth,
            synthetic_root: doc.synthetic_root,
            magic: numfmt::hex_encode(&doc.magic),
            trailing_bytes: doc.trailing.len(),
            warnings: doc.warnings.clone(),
            stats: doc.stats,
            load_ms,
        }
    }
}

/// Search every key and value in the document, in document order.
///
/// The walk follows the presented tree, so a string that decoded into JSON is
/// searched through its overlay rather than as an escaped literal. The query is
/// matched case-insensitively as a substring; an empty query matches nothing.
pub fn search(doc: &Doc, query: &str, limit: usize) -> SearchResults {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || limit == 0 {
        return SearchResults { matches: Vec::new(), truncated: false };
    }

    let mut matches = Vec::new();
    let mut path = Vec::new();
    walk(doc, doc.root(), 0, &needle, limit, &mut path, &mut matches);
    let truncated = matches.len() >= limit;
    SearchResults { matches, truncated }
}

/// Depth-first walk. Returns false once the limit is reached so the remaining
/// siblings are skipped.
fn walk(
    doc: &Doc,
    id: u32,
    index: u32,
    needle: &str,
    limit: usize,
    path: &mut Vec<SearchStep>,
    out: &mut Vec<SearchMatch>,
) -> bool {
    // Overlays make the presented tree deeper than the raw nesting limit, so
    // guard against a pathological chain rather than trusting the parser's cap.
    if path.len() > 4_096 {
        return true;
    }
    path.push(SearchStep { node: id, index });
    let n = match doc.node(id) {
        Ok(node) => *node,
        Err(_) => {
            path.pop();
            return true;
        }
    };
    if matches(doc, id, needle) {
        out.push(SearchMatch {
            node: id,
            key: if n.key.is_none() { None } else { Some(doc.key_of(id).into_owned()) },
            preview: preview(doc, &n),
            path: path.clone(),
        });
        if out.len() >= limit {
            path.pop();
            return false;
        }
    }

    for (i, &child) in doc.children(doc.presented(id)).iter().enumerate() {
        if !walk(doc, child, i as u32, needle, limit, path, out) {
            path.pop();
            return false;
        }
    }
    path.pop();
    true
}

/// Does the node's name or value contain the needle? Containers match by name
/// only; their summary text is not content.
fn matches(doc: &Doc, id: u32, needle: &str) -> bool {
    let Ok(n) = doc.node(id) else { return false };
    if !n.key.is_none() && doc.key_of(id).to_lowercase().contains(needle) {
        return true;
    }
    // A container's summary is not content, and an embedded-JSON string is
    // searched through its overlay so one visible value is not reported twice.
    if n.kind.is_container() || n.has_overlay() {
        return false;
    }
    let text = match n.kind {
        Kind::Str | Kind::Symbol | Kind::JavaScript | Kind::JavaScriptWithScope => {
            doc.text(n.val).to_lowercase()
        }
        _ => preview(doc, n).to_lowercase(),
    };
    text.contains(needle)
}

/// Build one window of rows for `parent`.
///
/// An embedded-JSON string is presented as the structure it decoded to, so the
/// child count and the rows both come from the overlay rather than the string.
pub fn page_children(doc: &Doc, parent: u32, offset: u32, limit: u32) -> CmdResult<ChildrenPage> {
    doc.node(parent)?;
    let shown = doc.presented(parent);
    let kids = doc.children(shown);
    let total = kids.len() as u32;
    let start = (offset as usize).min(kids.len());
    let end = start.saturating_add(limit as usize).min(kids.len());

    let rows = kids[start..end]
        .iter()
        .enumerate()
        .map(|(i, &id)| row(doc, id, start as u32 + i as u32))
        .collect();
    Ok(ChildrenPage { parent, offset: start as u32, total, rows })
}

fn row(doc: &Doc, id: u32, index: u32) -> NodeRow {
    // Ids come straight out of a live container's child list, so they are live
    // by construction; a tombstone here would mean the arena itself is corrupt.
    let n = *doc.node(id).expect("a child of a live container is live");
    let presented = doc.presented(id);
    let child_count = doc.child_count(presented);
    NodeRow {
        id,
        index,
        key: if n.key.is_none() { None } else { Some(doc.key_of(id).into_owned()) },
        kind: n.kind.type_name(),
        presented_kind: doc.node(presented).map(|p| p.kind.type_name()).unwrap_or(n.kind.type_name()),
        preview: preview(doc, &n),
        child_count,
        expandable: child_count > 0,
        embedded_json: n.has_overlay(),
        overlay_level: n.sub,
        editable: doccore::edit::can_edit(n.kind),
    }
}

/// Short human-readable form of a value, for the tree row.
fn preview(doc: &Doc, n: &Node) -> String {
    match n.kind {
        Kind::Object => format!("{{{} members}}", n.nkids),
        Kind::Array => format!("[{} items]", n.nkids),
        Kind::Str | Kind::Symbol | Kind::JavaScript => truncate(&doc.text(n.val)),
        Kind::Bool => {
            if n.sub != 0 {
                "true".into()
            } else {
                "false".into()
            }
        }
        Kind::Null => "null".into(),
        Kind::Undefined => "undefined".into(),
        Kind::Int32 | Kind::Int64 => n.i.to_string(),
        Kind::UInt64 => n.j.to_string(),
        Kind::Double => n.f.to_string(),
        Kind::Decimal128 => numfmt::decimal128_to_string(n.i as u64, n.j),
        Kind::DateTime => numfmt::iso8601(n.i),
        Kind::Timestamp => format!("{} #{}", numfmt::iso8601(n.i.saturating_mul(1000)), n.j),
        Kind::ObjectId => numfmt::hex_encode(doc.raw(n.val)),
        Kind::Binary => format!("<{} bytes, subtype {:#04x}>", n.val.len, n.sub),
        Kind::MinKey => "$minKey".into(),
        Kind::MaxKey => "$maxKey".into(),
        Kind::Regex => {
            let (pattern, options) = split_nul(doc.raw(n.val));
            let pattern = truncate(&String::from_utf8_lossy(pattern));
            format!("/{pattern}/{}", String::from_utf8_lossy(options))
        }
        Kind::JavaScriptWithScope => {
            let bindings = if n.aux == NO_NODE { 0 } else { doc.child_count(n.aux) };
            format!("{} (scope: {bindings})", truncate(&doc.text(n.val)))
        }
        Kind::DbPointer => {
            let (namespace, oid) = split_nul(doc.raw(n.val));
            format!("{} -> {}", String::from_utf8_lossy(namespace), numfmt::hex_encode(oid))
        }
    }
}

/// Split a two-part payload on its NUL separator.
fn split_nul(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&b| b == 0) {
        Some(at) => (&bytes[..at], &bytes[at + 1..]),
        None => (bytes, &[]),
    }
}

/// Cut a string down to something a tree row can show, on a character boundary.
fn truncate(text: &str) -> String {
    if text.len() <= PREVIEW_BYTES {
        return text.replace('\n', "\\n");
    }
    let mut end = PREVIEW_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].replace('\n', "\\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use doccore::document;

    fn loaded(text: &str) -> Doc {
        document::load(text.as_bytes().to_vec(), LoadOptions::default()).unwrap()
    }

    #[test]
    fn a_page_reports_the_true_total_but_only_the_requested_rows() {
        let items: Vec<String> = (0..500).map(|i| format!("{i}")).collect();
        let doc = loaded(&format!("[{}]", items.join(",")));

        let page = page_children(&doc, doc.root(), 0, 20).unwrap();
        assert_eq!(page.total, 500);
        assert_eq!(page.rows.len(), 20);
        assert_eq!(page.rows[0].index, 0);
        assert_eq!(page.rows[19].index, 19);
        assert_eq!(page.rows[0].preview, "0");

        let tail = page_children(&doc, doc.root(), 495, 20).unwrap();
        assert_eq!(tail.rows.len(), 5, "the last page is short");
        assert_eq!(tail.rows[0].index, 495);
        assert_eq!(tail.rows.last().unwrap().preview, "499");
    }

    #[test]
    fn paging_past_the_end_yields_an_empty_page() {
        let doc = loaded(r#"{"a":1}"#);
        let page = page_children(&doc, doc.root(), 99, 20).unwrap();
        assert!(page.rows.is_empty());
        assert_eq!(page.total, 1);
    }

    #[test]
    fn rows_describe_the_presentation_not_the_raw_node() {
        let doc = loaded(r#"{"s":"{\"x\":[1,2]}","a":[1,2,3],"o":{},"t":"plain"}"#);
        let page = page_children(&doc, doc.root(), 0, 10).unwrap();
        let by_key = |k: &str| page.rows.iter().find(|r| r.key.as_deref() == Some(k)).unwrap();

        let s = by_key("s");
        assert!(s.embedded_json, "the string decoded into JSON");
        assert_eq!(s.overlay_level, 1);
        assert_eq!(s.child_count, 1, "presented as the object inside it");
        assert!(s.expandable);
        assert_eq!(s.kind, "string", "but still a string as far as the file is concerned");
        assert_eq!(s.presented_kind, "object", "the text view opens a brace for it");

        assert_eq!(by_key("a").child_count, 3);
        assert!(!by_key("o").expandable, "an empty object has nothing to expand");
        assert!(!by_key("t").embedded_json);
        assert_eq!(by_key("t").preview, "plain");
    }

    #[test]
    fn array_elements_have_no_key_and_objects_always_do() {
        let doc = loaded(r#"{"a":[10]}"#);
        let outer = page_children(&doc, doc.root(), 0, 10).unwrap();
        let array = outer.rows[0].id;
        let inner = page_children(&doc, array, 0, 10).unwrap();

        assert_eq!(outer.rows[0].key.as_deref(), Some("a"));
        assert_eq!(inner.rows[0].key, None);
        assert_eq!(inner.rows[0].index, 0);
    }

    #[test]
    fn search_matches_keys_and_values_in_document_order() {
        let doc = loaded(r#"{"name":"alice","nested":{"NAME":"Bob"},"list":["alice",2]}"#);
        let results = search(&doc, "Alice", 50);
        assert_eq!(results.matches.len(), 2);
        assert!(!results.truncated);
        assert_eq!(results.matches[0].preview, "alice", "the first value comes first");
        assert_eq!(results.matches[1].preview, "alice");
        assert_eq!(results.matches[0].path.first().unwrap().node, doc.root());

        // The last step's index must address the match inside its parent.
        let last = &results.matches[1];
        let parent = last.path[last.path.len() - 2].node;
        let step = last.path.last().unwrap();
        assert_eq!(doc.children(parent)[step.index as usize], last.node);
    }

    #[test]
    fn search_matches_names_case_insensitively() {
        let doc = loaded(r#"{"Alpha":1,"beta":{"GAMMA":2}}"#);
        let results = search(&doc, "gamma", 10);
        assert_eq!(results.matches.len(), 1);
        assert_eq!(results.matches[0].key.as_deref(), Some("GAMMA"));
        assert_eq!(results.matches[0].preview, "2");
    }

    #[test]
    fn search_enters_embedded_json_overlays() {
        let doc = loaded(r#"{"s":"{\"inner\":\"needle\"}"}"#);
        let results = search(&doc, "needle", 10);
        assert_eq!(results.matches.len(), 1);
        assert_eq!(results.matches[0].key.as_deref(), Some("inner"));
        assert_eq!(results.matches[0].path.len(), 3, "root, the string, then its overlay");
    }

    #[test]
    fn search_stops_at_the_limit_and_reports_truncation() {
        let doc = loaded(r#"["a","a","a"]"#);
        let results = search(&doc, "a", 2);
        assert_eq!(results.matches.len(), 2);
        assert!(results.truncated);

        let empty = search(&doc, "  ", 10);
        assert!(empty.matches.is_empty());
        assert!(!empty.truncated);
    }

    /// Assemble a BSON document from `(name, type byte, payload)` triples. The
    /// wire order is type byte, key, NUL, payload.
    fn bson_doc(elements: &[(&str, u8, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (name, ty, payload) in elements {
            body.push(*ty);
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

    fn load_bson(bytes: Vec<u8>) -> Doc {
        let opts = LoadOptions { format: Some(Format::Bson), ..LoadOptions::default() };
        document::load(bytes, opts).unwrap()
    }

    #[test]
    fn previews_cover_the_bson_only_types() {
        let doc = load_bson(bson_doc(&[
            ("oid", 0x07, (0u8..12).collect()),
            ("when", 0x09, 0i64.to_le_bytes().to_vec()),
            ("flag", 0x08, vec![1]),
            ("blob", 0x05, [&4i32.to_le_bytes()[..], &[0x80], b"abcd"].concat()),
            // Decimal128 zero: the biased exponent sits in the high word.
            ("dec", 0x13, [0u64.to_le_bytes(), 0x3040_0000_0000_0000u64.to_le_bytes()].concat()),
            ("re", 0x0B, b"^a\0i\0".to_vec()),
            // BSON stores a timestamp as increment first, seconds second.
            ("ts", 0x11, [5u32.to_le_bytes(), 0u32.to_le_bytes()].concat()),
        ]));

        let page = page_children(&doc, doc.root(), 0, 20).unwrap();
        let preview = |k: &str| {
            page.rows.iter().find(|r| r.key.as_deref() == Some(k)).unwrap().preview.clone()
        };

        assert_eq!(preview("oid"), "000102030405060708090a0b");
        assert!(preview("when").starts_with("1970-01-01"), "{}", preview("when"));
        assert_eq!(preview("flag"), "true");
        assert_eq!(preview("blob"), "<4 bytes, subtype 0x80>");
        assert_eq!(preview("dec"), "0");
        assert_eq!(preview("re"), "/^a/i");
        assert!(preview("ts").ends_with("#5"), "{}", preview("ts"));

        // A bool is a value the editor can write; the BSON-only types are not,
        // so the tree must not offer an edit affordance for them.
        let editable =
            |k: &str| page.rows.iter().find(|r| r.key.as_deref() == Some(k)).unwrap().editable;
        assert!(editable("flag"));
        for kind in ["oid", "when", "blob", "dec", "re", "ts"] {
            assert!(!editable(kind), "{kind} should not be editable");
        }
    }

    #[test]
    fn binary_subtypes_are_shown_in_hex() {
        let doc =
            load_bson(bson_doc(&[("b", 0x05, [&0i32.to_le_bytes()[..], &[0], &[]].concat())]));
        let page = page_children(&doc, doc.root(), 0, 10).unwrap();
        assert_eq!(page.rows[0].preview, "<0 bytes, subtype 0x00>");
    }

    #[test]
    fn long_values_are_cut_on_a_character_boundary() {
        let doc = loaded(&format!(r#"{{"s":"{}é"}}"#, "x".repeat(PREVIEW_BYTES)));
        let page = page_children(&doc, doc.root(), 0, 10).unwrap();
        let preview = &page.rows[0].preview;

        assert!(preview.ends_with('…'));
        assert!(preview.len() <= PREVIEW_BYTES + "…".len(), "{}", preview.len());
        assert!(!preview.contains('é'), "the cut happened before the multi-byte character");
    }

    #[test]
    fn an_unknown_node_is_reported_rather_than_panicking() {
        let doc = loaded(r#"{"a":1}"#);
        let err = page_children(&doc, 4242, 0, 10).unwrap_err();
        assert_eq!(err.kind, "noNode");
    }

    /// The interfaces in `src/api.ts` are hand-written mirrors of these structs.
    /// A renamed field does not fail to compile — it arrives in the webview as
    /// `undefined` and the row quietly loses a badge, so the names are pinned.
    #[test]
    fn the_wire_shapes_are_what_the_frontend_declares() {
        /// Sorted key list of a serialized struct, for a stable comparison.
        fn shape<T: Serialize>(value: &T) -> String {
            let json = serde_json::to_value(value).unwrap();
            let mut keys: Vec<&str> = json
                .as_object()
                .expect("a struct serializes to an object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            keys.join(",")
        }

        let doc = loaded(r#"{"a":[1,2]}"#);
        let info = DocInfo::of(&doc, 1.5);
        assert_eq!(
            shape(&TabInfo { tab: 0, info: DocInfo::of(&doc, 0.0) }),
            "info,tab"
        );
        assert_eq!(
            shape(&info),
            "dirty,expandDepth,format,loadMs,magic,path,root,rootKind,rootPreview,stats,\
             syntheticRoot,trailingBytes,warnings"
        );
        assert_eq!(
            shape(&doc.stats),
            "blobBytes,childSlots,containerCount,documentCount,heapBytes,magicBytes,maxDepth,\
             nodeCount,overlayNodes,sourceBytes,trailingBytes"
        );

        let found = search(&doc, "a", 10);
        assert_eq!(shape(&found), "matches,truncated");
        assert_eq!(shape(&found.matches[0]), "key,node,path,preview");
        assert_eq!(shape(&found.matches[0].path[0]), "index,node");

        let page = page_children(&doc, doc.root(), 0, 10).unwrap();
        assert_eq!(shape(&page), "offset,parent,rows,total");
        assert_eq!(
            shape(&page.rows[0]),
            "childCount,editable,embeddedJson,expandable,id,index,key,kind,overlayLevel,\
             presentedKind,preview"
        );

        assert_eq!(shape(&EditResult { node: 1, info: DocInfo::of(&doc, 0.0) }), "info,node");
        assert_eq!(
            shape(&SaveResult {
                path: "x.json".into(),
                bytes_written: 1,
                format: Format::Json,
                warnings: Vec::new()
            }),
            "bytesWritten,format,path,warnings"
        );
        assert_eq!(shape(&ApiError::no_document()), "kind,message");

        // The frontend switches on these two literals to pick a file extension.
        assert_eq!(serde_json::to_value(Format::Json).unwrap(), "json");
        assert_eq!(serde_json::to_value(Format::Bson).unwrap(), "bson");
        assert_eq!(serde_json::to_value(info).unwrap()["format"], "json");
        let bson = load_bson(bson_doc(&[("n", 0x10, 1i32.to_le_bytes().to_vec())]));
        assert_eq!(serde_json::to_value(DocInfo::of(&bson, 0.0)).unwrap()["format"], "bson");
    }

    /// The inbound half of the contract: what the webview sends has to survive
    /// serde too, including the `uint` tag, which camelCase would otherwise
    /// spell `uInt`.
    #[test]
    fn the_frontend_can_address_every_editable_value_type() {
        use doccore::edit::Value;
        let cases = [
            (r#"{"type":"null"}"#, Value::Null),
            (r#"{"type":"bool","value":true}"#, Value::Bool(true)),
            (r#"{"type":"int","value":-5}"#, Value::Int(-5)),
            (r#"{"type":"uint","value":18446744073709551615}"#, Value::UInt(u64::MAX)),
            (r#"{"type":"double","value":1.5}"#, Value::Double(1.5)),
            (r#"{"type":"str","value":"x"}"#, Value::Str("x".into())),
            (r#"{"type":"object"}"#, Value::Object),
            (r#"{"type":"array"}"#, Value::Array),
        ];
        for (json, want) in cases {
            assert_eq!(serde_json::from_str::<Value>(json).unwrap(), want, "{json}");
        }

        // Every field of the open options is optional, so the frontend may send
        // a partial object or none at all.
        let partial: OpenOptions = serde_json::from_str(r#"{"expandJsonDepth":7}"#).unwrap();
        assert_eq!(partial.expand_json_depth, 7);
        assert_eq!(partial.load().expand_json_depth, 7);
        let empty: OpenOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.expand_json_depth, LoadOptions::default().expand_json_depth);
    }
}
