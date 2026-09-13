//! A flat, index-based representation of a parsed document.
//!
//! Everything lives in three contiguous vectors:
//!
//! * [`Doc::nodes`] — one fixed-size [`Node`] per value, 64 bytes each.
//! * [`Doc::children`] — child ids of every container, packed back to back
//!   (compressed sparse row). A node stores the start offset and length of its
//!   own slice, so reading a child is O(1) and walking a container is a
//!   sequential scan.
//! * [`Doc::blob`] — one byte buffer holding every key, string, and binary
//!   payload, referenced by [`Span`]s. Parsing a 10MB document performs a
//!   single memcpy per string instead of an allocation per string.
//!
//! Node ids are stable for the lifetime of a document: deletions unlink a node
//! from its parent but never renumber anything, so the webview can hold on to
//! ids while the document is edited underneath it.

use std::borrow::Cow;
use std::collections::HashSet;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::opts::Format;

/// Sentinel for "no node". Doubles as the parser's open-container marker, which
/// is safe because a real id can never reach `u32::MAX`.
pub const NO_NODE: u32 = u32::MAX;
/// Sentinel for "this node has no child list allocated yet".
const NO_POS: u32 = u32::MAX;

/// Deepest nesting the codecs will follow before giving up. Matches
/// `serde_json`'s built-in recursion limit so JSON and BSON reject hostile
/// input at the same depth.
pub const MAX_DEPTH: u32 = 128;

/// A byte range inside [`Doc::blob`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Span {
    pub off: u32,
    pub len: u32,
}

impl Span {
    pub const NONE: Span = Span { off: NO_NODE, len: 0 };

    #[inline]
    pub fn is_none(self) -> bool {
        self.off == NO_NODE
    }

    #[inline]
    pub fn slice(self, blob: &[u8]) -> &[u8] {
        if self.is_none() {
            &[]
        } else {
            &blob[self.off as usize..(self.off + self.len) as usize]
        }
    }
}

/// Value type. The discriminants are the BSON element type codes, so the BSON
/// writer can emit `kind as u8` directly.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize)]
#[repr(u8)]
pub enum Kind {
    Double = 0x01,
    Str = 0x02,
    Object = 0x03,
    Array = 0x04,
    Binary = 0x05,
    Undefined = 0x06,
    ObjectId = 0x07,
    Bool = 0x08,
    DateTime = 0x09,
    Null = 0x0A,
    Regex = 0x0B,
    DbPointer = 0x0C,
    JavaScript = 0x0D,
    Symbol = 0x0E,
    JavaScriptWithScope = 0x0F,
    Int32 = 0x10,
    Timestamp = 0x11,
    Int64 = 0x12,
    Decimal128 = 0x13,
    MaxKey = 0x7F,
    MinKey = 0xFF,
    /// JSON only: an integer too large for [`Kind::Int64`]. Not a BSON type, so
    /// exporting one to BSON widens it to a double.
    UInt64 = 0xFE,
}

impl Kind {
    pub fn from_bson_byte(b: u8) -> Option<Kind> {
        Some(match b {
            0x01 => Kind::Double,
            0x02 => Kind::Str,
            0x03 => Kind::Object,
            0x04 => Kind::Array,
            0x05 => Kind::Binary,
            0x06 => Kind::Undefined,
            0x07 => Kind::ObjectId,
            0x08 => Kind::Bool,
            0x09 => Kind::DateTime,
            0x0A => Kind::Null,
            0x0B => Kind::Regex,
            0x0C => Kind::DbPointer,
            0x0D => Kind::JavaScript,
            0x0E => Kind::Symbol,
            0x0F => Kind::JavaScriptWithScope,
            0x10 => Kind::Int32,
            0x11 => Kind::Timestamp,
            0x12 => Kind::Int64,
            0x13 => Kind::Decimal128,
            0x7F => Kind::MaxKey,
            0xFF => Kind::MinKey,
            _ => return None,
        })
    }

    #[inline]
    pub fn is_container(self) -> bool {
        matches!(self, Kind::Object | Kind::Array)
    }

    #[inline]
    pub fn is_number(self) -> bool {
        matches!(self, Kind::Double | Kind::Int32 | Kind::Int64 | Kind::UInt64 | Kind::Decimal128)
    }

    /// Short label shown as a badge in the tree.
    pub fn type_name(self) -> &'static str {
        match self {
            Kind::Double => "double",
            Kind::Str => "string",
            Kind::Object => "object",
            Kind::Array => "array",
            Kind::Binary => "binary",
            Kind::Undefined => "undefined",
            Kind::ObjectId => "objectId",
            Kind::Bool => "bool",
            Kind::DateTime => "datetime",
            Kind::Null => "null",
            Kind::Regex => "regex",
            Kind::DbPointer => "dbPointer",
            Kind::JavaScript => "javascript",
            Kind::Symbol => "symbol",
            Kind::JavaScriptWithScope => "code+scope",
            Kind::Int32 => "int32",
            Kind::Timestamp => "timestamp",
            Kind::Int64 => "int64",
            Kind::Decimal128 => "decimal128",
            Kind::MaxKey => "maxKey",
            Kind::MinKey => "minKey",
            Kind::UInt64 => "uint64",
        }
    }
}

/// One value in the document.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub kind: Kind,
    /// Multi-purpose discriminator:
    /// * [`Kind::Bool`] — `1` for true, `0` for false.
    /// * [`Kind::Binary`] — the BSON binary subtype.
    /// * [`Kind::Str`] with an overlay — how deeply nested the embedded JSON
    ///   is: `1` for a string in the document itself, `2` for a string found
    ///   inside a first-level overlay, and so on. Shown as a badge in the tree.
    pub sub: u8,
    /// Bit flags, see [`Node::OVERLAY_DIRTY`].
    pub flags: u8,
    /// Root of the parsed structure hidden inside a string value, or
    /// [`NO_NODE`]. Requirement 2.
    pub overlay: u32,
    /// Auxiliary node id (the scope document of [`Kind::JavaScriptWithScope`]).
    pub aux: u32,
    /// Owning container, or [`NO_NODE`] for a root.
    pub parent: u32,
    /// Start of this node's slice of [`Doc::children`], or `NO_POS`.
    pub kids: u32,
    pub nkids: u32,
    /// Member name. [`Span::NONE`] for array elements and document roots.
    pub key: Span,
    /// Payload bytes. Meaning depends on `kind`:
    /// * `Str`/`Symbol`/`JavaScript` — the text.
    /// * `Binary`/`ObjectId` — the raw bytes.
    /// * `Regex` — `pattern`, a NUL separator, then `options`.
    /// * `DbPointer` — namespace, a NUL separator, then the 12-byte id.
    pub val: Span,
    /// `Double` payload.
    pub f: f64,
    /// Integer payload: `Int32`/`Int64` value, `DateTime` milliseconds,
    /// `Timestamp` seconds, `Decimal128` low 64 bits.
    pub i: i64,
    /// Second integer payload: `UInt64` value, `Timestamp` increment,
    /// `Decimal128` high 64 bits.
    pub j: u64,
}

impl Node {
    /// Marks a string node whose embedded-JSON overlay has been edited, so
    /// serialization must regenerate the string from the overlay instead of
    /// writing the stale raw text.
    pub const OVERLAY_DIRTY: u8 = 1;

    #[inline]
    pub fn new(kind: Kind) -> Node {
        Node {
            kind,
            sub: 0,
            flags: 0,
            overlay: NO_NODE,
            aux: NO_NODE,
            parent: NO_NODE,
            kids: NO_POS,
            nkids: 0,
            key: Span::NONE,
            val: Span::NONE,
            f: 0.0,
            i: 0,
            j: 0,
        }
    }

    #[inline]
    pub fn is_container(&self) -> bool {
        self.kind.is_container()
    }

    /// Is this a string whose content decoded into a nested JSON structure?
    #[inline]
    pub fn has_overlay(&self) -> bool {
        self.overlay != NO_NODE
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub node_count: usize,
    pub container_count: usize,
    pub max_depth: u32,
    pub blob_bytes: usize,
    pub child_slots: usize,
    pub source_bytes: usize,
    pub document_count: usize,
    pub magic_bytes: usize,
    pub trailing_bytes: usize,
    pub overlay_nodes: usize,
    pub heap_bytes: usize,
}

/// A parsed document, plus everything needed to write it back out byte for
/// byte.
pub struct Doc {
    pub(crate) nodes: Vec<Node>,
    pub(crate) children: Vec<u32>,
    pub(crate) blob: Vec<u8>,
    /// Ids of every container node, so a structural edit rescans containers
    /// rather than the whole arena.
    pub(crate) containers: Vec<u32>,
    /// Parser scratch: pending child ids with [`NO_NODE`] markers where each
    /// open container began.
    pub(crate) stack: Vec<u32>,
    pub(crate) depth: u32,
    pub(crate) max_depth: u32,
    /// Nodes unlinked by an edit. Their ids stay reserved so a stale id in the
    /// webview resolves to an error instead of someone else's node.
    pub(crate) removed: HashSet<u32>,
    /// How many string nodes currently own an embedded-JSON overlay. Tracked
    /// rather than derived: [`Doc::refresh_stats`] runs after every edit, and
    /// scanning 1.7M nodes to count them would make each keystroke visible.
    pub(crate) overlay_count: usize,

    pub(crate) root: u32,
    pub format: Format,
    /// Requirement 1: bytes stripped from the front of the file before parsing.
    /// Written back verbatim on save.
    pub magic: Vec<u8>,
    /// Bytes found after the last BSON document, also written back verbatim.
    pub trailing: Vec<u8>,
    /// Original file content, kept so the expand depth can be changed without
    /// asking the user for the file again.
    pub(crate) source: Vec<u8>,
    /// True when the root is a synthetic array wrapping several BSON documents.
    pub synthetic_root: bool,
    /// How many levels of embedded JSON the current overlays were built with.
    /// Requirement 2.
    pub expand_depth: u8,
    pub document_count: usize,
    pub stats: Stats,
    pub warnings: Vec<String>,
    pub dirty: bool,
    /// Path the document was loaded from, if any.
    pub path: Option<String>,
}

impl Doc {
    pub(crate) fn new(format: Format) -> Doc {
        Doc {
            nodes: Vec::new(),
            children: Vec::new(),
            blob: Vec::new(),
            containers: Vec::new(),
            stack: Vec::new(),
            depth: 0,
            max_depth: 0,
            removed: HashSet::new(),
            overlay_count: 0,
            root: NO_NODE,
            format,
            magic: Vec::new(),
            trailing: Vec::new(),
            source: Vec::new(),
            synthetic_root: false,
            expand_depth: 0,
            document_count: 0,
            stats: Stats::default(),
            warnings: Vec::new(),
            dirty: false,
            path: None,
        }
    }

    /// Grow the vectors up front from an input size estimate. Avoids repeated
    /// reallocation and the peak-memory spike that comes with it on 10MB+
    /// input.
    pub(crate) fn reserve_for(&mut self, source_len: usize) {
        let nodes = source_len / 12 + 16;
        self.nodes.reserve(nodes);
        self.children.reserve(nodes);
        self.blob.reserve(source_len / 3);
        self.containers.reserve(nodes / 8);
    }

    // ---------------------------------------------------------------- reading

    #[inline]
    pub fn root(&self) -> u32 {
        self.root
    }

    #[inline]
    pub fn node(&self, id: u32) -> Result<&Node> {
        match self.nodes.get(id as usize) {
            Some(n) if !self.removed.contains(&id) => Ok(n),
            _ => Err(Error::NoSuchNode(id)),
        }
    }

    /// Like [`Doc::node`] for internal callers that know the id is live.
    #[inline]
    pub(crate) fn node_at(&self, id: u32) -> &Node {
        &self.nodes[id as usize]
    }

    #[inline]
    pub fn raw(&self, span: Span) -> &[u8] {
        span.slice(&self.blob)
    }

    /// Span contents as text, replacing invalid UTF-8 rather than failing. BSON
    /// in the wild is not always valid UTF-8 and the viewer must never panic.
    pub fn text(&self, span: Span) -> Cow<'_, str> {
        let bytes = self.raw(span);
        match std::str::from_utf8(bytes) {
            Ok(s) => Cow::Borrowed(s),
            Err(_) => String::from_utf8_lossy(bytes),
        }
    }

    #[inline]
    pub fn key_of(&self, id: u32) -> Cow<'_, str> {
        let n = self.node_at(id);
        self.text(n.key)
    }

    #[inline]
    pub fn value_bytes(&self, id: u32) -> &[u8] {
        let n = self.node_at(id);
        self.raw(n.val)
    }

    /// Child ids of a container, in document order.
    #[inline]
    pub fn children(&self, id: u32) -> &[u32] {
        let n = self.node_at(id);
        if n.kids == NO_POS {
            return &[];
        }
        &self.children[n.kids as usize..(n.kids + n.nkids) as usize]
    }

    #[inline]
    pub fn child_count(&self, id: u32) -> u32 {
        self.node_at(id).nkids
    }

    pub fn child_at(&self, id: u32, index: usize) -> Option<u32> {
        self.children(id).get(index).copied()
    }

    #[inline]
    pub fn parent_of(&self, id: u32) -> u32 {
        self.node_at(id).parent
    }

    /// The node whose children the viewer should show for `id`: an embedded
    /// JSON string is presented as the structure it decodes to.
    #[inline]
    pub fn presented(&self, id: u32) -> u32 {
        let n = self.node_at(id);
        if n.overlay != NO_NODE {
            n.overlay
        } else {
            id
        }
    }

    /// Index of `child` inside `parent`, or `None`.
    pub fn index_of(&self, parent: u32, child: u32) -> Option<usize> {
        self.children(parent).iter().position(|&c| c == child)
    }

    /// Ids from the root down to `node`, inclusive.
    pub fn path_to(&self, node: u32) -> Result<Vec<u32>> {
        self.node(node)?;
        let mut chain = Vec::new();
        let mut cur = node;
        while cur != NO_NODE {
            chain.push(cur);
            cur = self.node_at(cur).parent;
            if chain.len() > self.nodes.len() {
                return Err(Error::NoSuchNode(node));
            }
        }
        chain.reverse();
        Ok(chain)
    }

    /// Number of nodes in a subtree, used to warn before copying something huge.
    pub fn subtree_size(&self, id: u32) -> usize {
        let mut count = 0usize;
        let mut work = vec![id];
        while let Some(n) = work.pop() {
            count += 1;
            let node = self.node_at(n);
            if node.overlay != NO_NODE {
                work.push(node.overlay);
            }
            if node.aux != NO_NODE {
                work.push(node.aux);
            }
            work.extend(self.children(n));
        }
        count
    }

    // --------------------------------------------------------------- building

    /// Append bytes to the blob and return their span.
    #[inline]
    pub(crate) fn push_blob(&mut self, bytes: &[u8]) -> Span {
        let off = self.blob.len() as u32;
        self.blob.extend_from_slice(bytes);
        Span { off, len: bytes.len() as u32 }
    }

    /// Pack several byte slices into one blob region separated by NUL bytes and
    /// return the span covering all of them. This is how the two-part payloads
    /// share a single `val` span: regex pattern plus options, and DBPointer
    /// namespace plus object id.
    pub(crate) fn push_joined(&mut self, parts: &[&[u8]]) -> Span {
        let off = self.blob.len() as u32;
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                self.blob.push(0);
            }
            self.blob.extend_from_slice(part);
        }
        Span { off, len: self.blob.len() as u32 - off }
    }

    /// Flag every string node that owns an overlay containing `node`.
    ///
    /// An edit inside embedded JSON has to invalidate the whole chain of
    /// enclosing strings, not just the nearest one, or an outer string would
    /// still serialize its stale raw text and silently swallow the change.
    pub fn mark_overlay_dirty(&mut self, node: u32) {
        let mut cur = node;
        let mut guard = 0usize;
        while cur != NO_NODE {
            let n = &mut self.nodes[cur as usize];
            if n.overlay != NO_NODE {
                n.flags |= Node::OVERLAY_DIRTY;
            }
            cur = n.parent;
            guard += 1;
            if guard > self.nodes.len() {
                break;
            }
        }
    }

    /// Does this string node's overlay take precedence over its raw text when
    /// serializing?
    #[inline]
    pub fn overlay_is_authoritative(&self, id: u32) -> bool {
        let n = self.node_at(id);
        n.overlay != NO_NODE && n.flags & Node::OVERLAY_DIRTY != 0
    }

    #[inline]
    pub(crate) fn add_node(&mut self, n: Node) -> u32 {
        let id = self.nodes.len() as u32;
        self.nodes.push(n);
        if n.kind.is_container() {
            self.containers.push(id);
        }
        id
    }

    /// Start a container. Call [`Doc::push_child`] for each member, then
    /// [`Doc::close`].
    pub(crate) fn open(&mut self, kind: Kind, key: Span) -> u32 {
        let mut n = Node::new(kind);
        n.key = key;
        let id = self.add_node(n);
        self.stack.push(NO_NODE);
        self.depth += 1;
        if self.depth > self.max_depth {
            self.max_depth = self.depth;
        }
        id
    }

    /// Record a finished child of the innermost open container.
    #[inline]
    pub(crate) fn push_child(&mut self, child: u32, parent: u32) {
        self.nodes[child as usize].parent = parent;
        self.stack.push(child);
    }

    /// Finish the innermost open container, moving its pending children into the
    /// CSR array.
    pub(crate) fn close(&mut self, parent: u32) {
        let mark = self
            .stack
            .iter()
            .rposition(|&x| x == NO_NODE)
            .expect("close() without a matching open()");
        let count = self.stack.len() - mark - 1;
        let node = &mut self.nodes[parent as usize];
        if count == 0 {
            node.kids = NO_POS;
        } else {
            node.kids = self.children.len() as u32;
            self.children.extend_from_slice(&self.stack[mark + 1..]);
        }
        node.nkids = count as u32;
        self.stack.truncate(mark);
        self.depth -= 1;
    }

    /// Attach an already-built child list to a container. Used by edits, which
    /// do not run through the open/push/close dance.
    pub fn set_children(&mut self, parent: u32, ids: &[u32]) {
        let node = &mut self.nodes[parent as usize];
        if ids.is_empty() {
            node.kids = NO_POS;
            node.nkids = 0;
            return;
        }
        node.kids = self.children.len() as u32;
        node.nkids = ids.len() as u32;
        self.children.extend_from_slice(ids);
        for &id in ids {
            self.nodes[id as usize].parent = parent;
        }
    }

    pub(crate) fn refresh_stats(&mut self) {
        self.stats = Stats {
            node_count: self.nodes.len(),
            container_count: self.containers.len(),
            max_depth: self.max_depth,
            blob_bytes: self.blob.len(),
            child_slots: self.children.len(),
            source_bytes: self.source.len(),
            document_count: self.document_count.max(1),
            magic_bytes: self.magic.len(),
            trailing_bytes: self.trailing.len(),
            overlay_nodes: self.overlay_count,
            heap_bytes: self.nodes.len() * std::mem::size_of::<Node>()
                + self.children.len() * 4
                + self.blob.len()
                + self.source.len(),
        };
    }

    // ----------------------------------------------------------------- editing

    /// Insert `child` into `parent` at `index`.
    ///
    /// Child ranges are kept contiguous, so an insertion shifts every following
    /// range by one slot. That is a linear scan over containers; edits are
    /// user-driven, so the cost tracks interaction speed rather than document
    /// size in practice.
    pub fn insert_child(&mut self, parent: u32, index: usize, child: u32) {
        let (start, len) = {
            let p = self.node_at(parent);
            (p.kids, p.nkids as usize)
        };
        self.nodes[child as usize].parent = parent;
        if start == NO_POS {
            let at = self.children.len() as u32;
            self.children.push(child);
            let p = &mut self.nodes[parent as usize];
            p.kids = at;
            p.nkids = 1;
            return;
        }
        let at = start as usize + index.min(len);
        self.children.insert(at, child);
        self.nodes[parent as usize].nkids += 1;
        for idx in 0..self.containers.len() {
            let c = self.containers[idx];
            if c == parent {
                continue;
            }
            let n = &mut self.nodes[c as usize];
            if n.kids != NO_POS && n.kids as usize >= at {
                n.kids += 1;
            }
        }
    }

    /// Unlink the child at `index` from `parent` and return its id.
    pub fn remove_child(&mut self, parent: u32, index: usize) -> Result<u32> {
        let (start, len) = {
            let p = self.node_at(parent);
            (p.kids, p.nkids as usize)
        };
        if start == NO_POS || index >= len {
            return Err(Error::NoSuchNode(parent));
        }
        let at = start as usize + index;
        let child = self.children.remove(at);
        let p = &mut self.nodes[parent as usize];
        p.nkids -= 1;
        if p.nkids == 0 {
            p.kids = NO_POS;
        }
        for idx in 0..self.containers.len() {
            let c = self.containers[idx];
            if c == parent {
                continue;
            }
            let n = &mut self.nodes[c as usize];
            if n.kids != NO_POS && n.kids as usize > at {
                n.kids -= 1;
            }
        }
        Ok(child)
    }

    /// Snapshot the arena so a speculative parse can be undone.
    #[inline]
    pub(crate) fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            nodes: self.nodes.len(),
            children: self.children.len(),
            blob: self.blob.len(),
            containers: self.containers.len(),
            overlay_count: self.overlay_count,
            max_depth: self.max_depth,
        }
    }

    /// Mark a whole subtree unreachable without renumbering anything.
    pub fn mark_removed(&mut self, root: u32) {
        let mut work = vec![root];
        while let Some(id) = work.pop() {
            if !self.removed.insert(id) {
                continue;
            }
            let n = *self.node_at(id);
            if n.overlay != NO_NODE {
                self.overlay_count -= 1;
                work.push(n.overlay);
            }
            if n.aux != NO_NODE {
                work.push(n.aux);
            }
            work.extend(self.children(id));
        }
    }
}

/// Arena sizes before a speculative parse, so a failure can be rolled back.
///
/// Both the embedded-JSON expander and the editor parse into a live arena and
/// have to leave no trace when the input turns out not to be JSON.
pub(crate) struct Checkpoint {
    nodes: usize,
    children: usize,
    blob: usize,
    containers: usize,
    overlay_count: usize,
    max_depth: u32,
}

impl Checkpoint {
    pub(crate) fn restore(self, doc: &mut Doc) {
        doc.nodes.truncate(self.nodes);
        doc.children.truncate(self.children);
        doc.blob.truncate(self.blob);
        doc.containers.truncate(self.containers);
        doc.overlay_count = self.overlay_count;
        doc.max_depth = self.max_depth;
        // The parse died with containers still open, so its half-finished work
        // is still sitting on the stack. Leaving it there would make the next
        // `close()` bind children to the wrong parent.
        doc.stack.clear();
        doc.depth = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trips_through_bson_codes() {
        for byte in [
            0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10, 0x11, 0x12, 0x13, 0x7F, 0xFF,
        ] {
            let kind = Kind::from_bson_byte(byte).expect("valid bson type");
            assert_eq!(kind as u8, byte, "{} code mismatch", kind.type_name());
        }
        assert_eq!(Kind::from_bson_byte(0x00), None);
        assert_eq!(Kind::from_bson_byte(0x42), None);
    }

    #[test]
    fn node_is_exactly_one_cache_line() {
        // The arena's footprint for a 10MB document scales with this, and 64
        // bytes is exactly one cache line, so a sequential scan over `nodes`
        // never straddles two.
        assert_eq!(std::mem::size_of::<Node>(), 64);
    }

    fn int_node(doc: &mut Doc, value: i64) -> u32 {
        let mut n = Node::new(Kind::Int32);
        n.i = value;
        doc.add_node(n)
    }

    #[test]
    fn empty_containers_share_no_child_range() {
        let mut doc = Doc::new(Format::Json);
        let a = doc.open(Kind::Object, Span::NONE);
        doc.close(a);
        let b = doc.open(Kind::Array, Span::NONE);
        doc.close(b);
        assert!(doc.children(a).is_empty());
        assert!(doc.children(b).is_empty());

        doc.insert_child(a, 0, b);
        assert_eq!(doc.children(a), &[b]);
        assert!(doc.children(b).is_empty(), "b must stay empty after a gains a child");
        assert_eq!(doc.parent_of(b), a);
    }

    #[test]
    fn inserting_in_the_middle_keeps_sibling_ranges_intact() {
        let mut doc = Doc::new(Format::Json);
        let root = doc.open(Kind::Object, Span::NONE);
        let k1 = doc.push_blob(b"first");
        let arr1 = doc.open(Kind::Array, k1);
        let v = int_node(&mut doc, 1);
        doc.push_child(v, arr1);
        doc.close(arr1);
        doc.push_child(arr1, root);
        let k2 = doc.push_blob(b"second");
        let arr2 = doc.open(Kind::Array, k2);
        let w = int_node(&mut doc, 2);
        doc.push_child(w, arr2);
        doc.close(arr2);
        doc.push_child(arr2, root);
        doc.close(root);
        doc.root = root;

        let fresh = doc.add_node(Node::new(Kind::Null));
        doc.insert_child(arr1, 0, fresh);

        assert_eq!(doc.children(arr1), &[fresh, v]);
        assert_eq!(doc.children(arr2), &[w], "later range must follow the insert");
        assert_eq!(doc.children(root), &[arr1, arr2]);
        assert_eq!(doc.path_to(v).unwrap(), vec![root, arr1, v]);
    }

    #[test]
    fn removal_marks_the_subtree_dead_but_keeps_ids_stable() {
        let mut doc = Doc::new(Format::Json);
        let root = doc.open(Kind::Array, Span::NONE);
        let child = doc.open(Kind::Object, Span::NONE);
        let leaf = doc.add_node(Node::new(Kind::Str));
        doc.push_child(leaf, child);
        doc.close(child);
        doc.push_child(child, root);
        let sibling = doc.add_node(Node::new(Kind::Null));
        doc.push_child(sibling, root);
        doc.close(root);
        doc.root = root;

        let removed = doc.remove_child(root, 0).unwrap();
        assert_eq!(removed, child);
        doc.mark_removed(removed);

        assert_eq!(doc.children(root), &[sibling]);
        assert!(matches!(doc.node(child), Err(Error::NoSuchNode(_))));
        assert!(matches!(doc.node(leaf), Err(Error::NoSuchNode(_))));
        assert!(doc.node(sibling).is_ok());
    }

    #[test]
    fn invalid_utf8_is_replaced_not_rejected() {
        let mut doc = Doc::new(Format::Bson);
        let span = doc.push_blob(&[b'a', 0xFF, b'z']);
        assert_eq!(doc.text(span), "a\u{fffd}z");
    }
}
