//! Structural editing: the operations the tree view exposes on a node.
//!
//! Node ids stay stable across every edit here. A deletion unlinks a node and
//! tombstones its subtree rather than renumbering anything, so the webview can
//! keep holding ids while the document changes underneath it. The one exception
//! is [`replace_with_json`], which splices in a freshly parsed subtree and so
//! hands back a new id.
//!
//! Every operation marks the document dirty and invalidates the embedded-JSON
//! strings enclosing the change, so serializing regenerates those strings from
//! their overlays instead of writing text that no longer matches the tree.

use serde::{Deserialize, Serialize};

use crate::arena::{Doc, Kind, Node, NO_NODE};
use crate::error::{Error, Result};
use crate::json_read::parse_json;

/// A value the editor can write into the arena.
///
/// Covers the types JSON can express. BSON-only scalars such as object ids,
/// timestamps and decimal128 are deliberately absent: guessing at their binary
/// layout from a text field would corrupt them, so they are reported as
/// unsupported rather than quietly mangled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Double(f64),
    Str(String),
    /// An empty object, ready to be filled in.
    Object,
    /// An empty array.
    Array,
}

/// Read a node's value back in the form the editor writes it, for prefilling an
/// edit field.
pub fn value_of(doc: &Doc, id: u32) -> Result<Value> {
    let n = doc.node(id)?;
    Ok(match n.kind {
        Kind::Null => Value::Null,
        Kind::Bool => Value::Bool(n.sub != 0),
        Kind::Int32 | Kind::Int64 => Value::Int(n.i),
        Kind::UInt64 => Value::UInt(n.j),
        Kind::Double => Value::Double(n.f),
        Kind::Str => Value::Str(doc.text(n.val).into_owned()),
        Kind::Object => Value::Object,
        Kind::Array => Value::Array,
        other => return Err(Error::Unsupported { action: "edit", kind: other.type_name() }),
    })
}

/// Replace a node's value in place, keeping its id and its name.
pub fn set_value(doc: &mut Doc, id: u32, value: Value) -> Result<()> {
    let old = *doc.node(id)?;
    let mut fresh = node_for(doc, &value);

    // Narrowing back to Int32 keeps a BSON element's type stable when the user
    // edits a small number that was stored as a 32-bit int.
    if old.kind == Kind::Int32 && fresh.kind == Kind::Int64 && i32::try_from(fresh.i).is_ok() {
        fresh.kind = Kind::Int32;
    }

    if old.is_container() && fresh.is_container() {
        // Converting an object to an array (or back) keeps its members; only
        // their names stop meaning anything.
        fresh.kids = old.kids;
        fresh.nkids = old.nkids;
    } else if old.is_container() {
        for kid in doc.children(id).to_vec() {
            doc.mark_removed(kid);
        }
    }
    if old.aux != NO_NODE {
        doc.mark_removed(old.aux);
    }
    if old.overlay != NO_NODE {
        doc.overlay_count -= 1;
        doc.mark_removed(old.overlay);
    }

    fresh.key = old.key;
    fresh.parent = old.parent;
    let became_container = fresh.is_container() && !old.is_container();
    doc.nodes[id as usize] = fresh;
    if became_container {
        doc.containers.push(id);
    }
    // A node that stops being a container keeps its stale entry in `containers`,
    // and its old child slots are simply never read again. Both are harmless:
    // the offset-fixing loops skip anything with no child list.
    finish(doc, id);
    Ok(())
}

/// Rename an object member. Array elements take their name from their position,
/// so they cannot be renamed.
pub fn rename(doc: &mut Doc, id: u32, key: &str) -> Result<()> {
    let target = *doc.node(id)?;
    let parent = target.parent;
    if parent == NO_NODE {
        return Err(Error::Conflict("the document root has no name".into()));
    }
    let parent_kind = doc.node_at(parent).kind;
    if parent_kind != Kind::Object {
        return Err(Error::Unsupported {
            action: "rename a member of",
            kind: parent_kind.type_name(),
        });
    }
    let span = doc.push_blob(key.as_bytes());
    doc.nodes[id as usize].key = span;
    finish(doc, id);
    Ok(())
}

/// Create a new node inside `parent` at `index` and return its id.
pub fn insert(
    doc: &mut Doc,
    parent: u32,
    index: usize,
    key: Option<String>,
    value: Value,
) -> Result<u32> {
    check_container(doc, parent, &key)?;
    let mut node = node_for(doc, &value);
    if let Some(name) = &key {
        node.key = doc.push_blob(name.as_bytes());
    }
    let id = doc.add_node(node);
    doc.insert_child(parent, index, id);
    finish(doc, id);
    Ok(id)
}

/// Parse `json` and insert the result as a new child of `parent`. This is the
/// "paste a whole structure here" path.
pub fn insert_json(
    doc: &mut Doc,
    parent: u32,
    index: usize,
    key: Option<String>,
    json: &str,
) -> Result<u32> {
    check_container(doc, parent, &key)?;
    let root = parse_speculative(doc, json)?;
    if let Some(name) = &key {
        let span = doc.push_blob(name.as_bytes());
        doc.nodes[root as usize].key = span;
    }
    doc.insert_child(parent, index, root);
    finish(doc, root);
    Ok(root)
}

/// Parse `json` and splice it in place of `id`, returning the new node's id.
pub fn replace_with_json(doc: &mut Doc, id: u32, json: &str) -> Result<u32> {
    let target = *doc.node(id)?;
    let root = parse_speculative(doc, json)?;
    doc.nodes[root as usize].key = target.key;

    if target.parent == NO_NODE {
        doc.mark_removed(doc.root);
        doc.root = root;
        finish(doc, root);
        return Ok(root);
    }
    let index = doc.index_of(target.parent, id).ok_or(Error::NoSuchNode(id))?;
    doc.insert_child(target.parent, index, root);
    // The replaced node is now one slot to the right of its replacement.
    let displaced = doc.remove_child(target.parent, index + 1)?;
    debug_assert_eq!(displaced, id, "the old node should have shifted right");
    doc.mark_removed(id);
    finish(doc, root);
    Ok(root)
}

/// Unlink a node from its parent and retire its whole subtree.
pub fn remove(doc: &mut Doc, id: u32) -> Result<()> {
    let target = *doc.node(id)?;
    if target.parent == NO_NODE {
        return Err(Error::Conflict("the document root cannot be deleted".into()));
    }
    let index = doc.index_of(target.parent, id).ok_or(Error::NoSuchNode(id))?;
    doc.remove_child(target.parent, index)?;
    doc.mark_removed(id);
    finish(doc, target.parent);
    Ok(())
}

/// Build the arena node for a [`Value`]. Containers start out empty.
fn node_for(doc: &mut Doc, value: &Value) -> Node {
    match value {
        Value::Null => Node::new(Kind::Null),
        Value::Bool(b) => {
            let mut n = Node::new(Kind::Bool);
            n.sub = u8::from(*b);
            n
        }
        Value::Int(i) => {
            let mut n = Node::new(Kind::Int64);
            n.i = *i;
            n
        }
        Value::UInt(j) => {
            // Staying unsigned keeps a value past `i64::MAX` exact.
            let mut n = Node::new(Kind::UInt64);
            n.j = *j;
            n
        }
        Value::Double(f) => {
            let mut n = Node::new(Kind::Double);
            n.f = *f;
            n
        }
        Value::Str(s) => {
            let mut n = Node::new(Kind::Str);
            n.val = doc.push_blob(s.as_bytes());
            n
        }
        Value::Object => Node::new(Kind::Object),
        Value::Array => Node::new(Kind::Array),
    }
}

/// Objects need a name for each member and arrays forbid one, so the two are
/// checked together before anything is written.
fn check_container(doc: &Doc, parent: u32, key: &Option<String>) -> Result<()> {
    let container = doc.node(parent)?;
    if !container.is_container() {
        return Err(Error::Unsupported { action: "insert into", kind: container.kind.type_name() });
    }
    match (container.kind, key) {
        (Kind::Object, None) => Err(Error::Conflict("an object member needs a name".into())),
        (Kind::Array, Some(_)) => {
            Err(Error::Conflict("an array element is named by its position".into()))
        }
        _ => Ok(()),
    }
}

/// Parse into a live arena, undoing every byte of it if the input is not JSON.
fn parse_speculative(doc: &mut Doc, json: &str) -> Result<u32> {
    let undo = doc.checkpoint();
    match parse_json(json.as_bytes(), doc) {
        Ok(root) => Ok(root),
        Err(err) => {
            undo.restore(doc);
            Err(err)
        }
    }
}

/// Common tail for every edit: the document changed, any embedded-JSON string
/// enclosing the change is now stale, and the status-bar counters moved.
fn finish(doc: &mut Doc, changed: u32) {
    doc.dirty = true;
    doc.mark_overlay_dirty(changed);
    doc.refresh_stats();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opts::{Format, JsonOptions, LoadOptions, SaveOptions};
    use crate::testutil::{bson, i32, parse_one, string};
    use crate::{document, expand};

    fn json(text: &str) -> Doc {
        let mut doc = Doc::new(Format::Json);
        doc.root = parse_json(text.as_bytes(), &mut doc).expect("valid json");
        doc.refresh_stats();
        doc
    }

    /// The root's member ids, taken in one go. Binding them up front is what
    /// lets a test hold ids while mutating the document underneath them.
    fn members(doc: &Doc) -> Vec<u32> {
        doc.children(doc.root()).to_vec()
    }

    fn compact(doc: &Doc) -> String {
        doc.to_json(doc.root(), &JsonOptions::compact()).unwrap()
    }

    #[test]
    fn a_scalar_can_be_rewritten_in_place() {
        let mut doc = json(r#"{"a":1,"b":"x","c":true}"#);
        let m = members(&doc);
        set_value(&mut doc, m[0], Value::Str("hello".into())).unwrap();
        set_value(&mut doc, m[1], Value::Double(2.5)).unwrap();
        set_value(&mut doc, m[2], Value::Null).unwrap();

        assert_eq!(compact(&doc), r#"{"a":"hello","b":2.5,"c":null}"#);
        assert_eq!(doc.node(m[0]).unwrap().kind, Kind::Str);
        assert!(doc.dirty);
    }

    #[test]
    fn rewriting_a_node_keeps_its_id_and_name() {
        let mut doc = json(r#"{"a":1}"#);
        let id = members(&doc)[0];
        set_value(&mut doc, id, Value::Object).unwrap();

        assert_eq!(members(&doc)[0], id, "the id must survive the edit");
        assert_eq!(doc.key_of(id), "a");
        assert_eq!(compact(&doc), r#"{"a":{}}"#);
    }

    #[test]
    fn replacing_a_container_retires_its_children() {
        let mut doc = json(r#"{"a":{"deep":[1,2,3]}}"#);
        let container = members(&doc)[0];
        let deep = doc.children(container)[0];
        let leaves: Vec<u32> = doc.children(deep).to_vec();
        let before = doc.stats.node_count;

        set_value(&mut doc, container, Value::Int(7)).unwrap();

        assert_eq!(compact(&doc), r#"{"a":7}"#);
        assert_eq!(doc.stats.node_count, before, "retiring a subtree adds no nodes");
        // The descendants are unreachable, so a stale id held by the webview
        // errors instead of resolving to someone else's node.
        assert!(matches!(doc.node(deep), Err(Error::NoSuchNode(_))));
        for leaf in leaves {
            assert!(matches!(doc.node(leaf), Err(Error::NoSuchNode(_))));
        }
    }

    #[test]
    fn converting_between_containers_keeps_the_members() {
        let mut doc = json(r#"{"a":[1,2]}"#);
        let array = members(&doc)[0];
        set_value(&mut doc, array, Value::Object).unwrap();

        assert_eq!(doc.node(array).unwrap().kind, Kind::Object);
        assert_eq!(doc.child_count(array), 2, "members survive the conversion");
    }

    #[test]
    fn a_small_int_written_over_an_int32_stays_an_int32() {
        let bytes = bson(&[("n", 0x10, &i32(1)), ("s", 0x02, &string("x"))]);
        let mut doc = parse_one(&bytes);
        let n = members(&doc)[0];
        assert_eq!(doc.node(n).unwrap().kind, Kind::Int32);

        set_value(&mut doc, n, Value::Int(42)).unwrap();
        assert_eq!(doc.node(n).unwrap().kind, Kind::Int32, "type must not widen");

        set_value(&mut doc, n, Value::Int(i64::from(i32::MAX) + 1)).unwrap();
        assert_eq!(doc.node(n).unwrap().kind, Kind::Int64, "no longer fits");
    }

    #[test]
    fn members_can_be_renamed() {
        let mut doc = json(r#"{"a":1,"b":2}"#);
        let b = members(&doc)[1];
        rename(&mut doc, b, "renamed").unwrap();
        assert_eq!(compact(&doc), r#"{"a":1,"renamed":2}"#);
    }

    #[test]
    fn array_elements_and_the_root_cannot_be_renamed() {
        let mut doc = json(r#"[1,2]"#);
        let root = doc.root();
        let element = doc.child_at(root, 0).unwrap();
        assert!(matches!(rename(&mut doc, element, "x"), Err(Error::Unsupported { .. })));
        assert!(matches!(rename(&mut doc, root, "x"), Err(Error::Conflict(_))));
    }

    #[test]
    fn inserting_into_an_object_needs_a_name() {
        let mut doc = json(r#"{"a":1}"#);
        let root = doc.root();
        assert!(matches!(insert(&mut doc, root, 0, None, Value::Null), Err(Error::Conflict(_))));
        insert(&mut doc, root, 0, Some("first".into()), Value::Bool(true)).unwrap();
        assert_eq!(compact(&doc), r#"{"first":true,"a":1}"#);
    }

    #[test]
    fn inserting_into_an_array_forbids_a_name() {
        let mut doc = json(r#"[1,2]"#);
        let root = doc.root();
        assert!(matches!(
            insert(&mut doc, root, 1, Some("x".into()), Value::Null),
            Err(Error::Conflict(_))
        ));
        insert(&mut doc, root, 1, None, Value::Str("mid".into())).unwrap();
        assert_eq!(compact(&doc), r#"[1,"mid",2]"#);
    }

    #[test]
    fn appending_past_the_end_lands_last() {
        let mut doc = json(r#"[1]"#);
        let root = doc.root();
        insert(&mut doc, root, 99, None, Value::Int(2)).unwrap();
        assert_eq!(compact(&doc), "[1,2]");
    }

    #[test]
    fn inserting_into_a_scalar_is_refused() {
        let mut doc = json(r#"{"a":1}"#);
        let scalar = members(&doc)[0];
        assert!(matches!(
            insert(&mut doc, scalar, 0, None, Value::Null),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn pasted_json_becomes_a_subtree() {
        let mut doc = json(r#"{"a":1,"b":2}"#);
        let root = doc.root();
        let id = insert_json(&mut doc, root, 1, Some("pasted".into()), r#"{"x":[1,2]}"#).unwrap();

        assert_eq!(compact(&doc), r#"{"a":1,"pasted":{"x":[1,2]},"b":2}"#);
        assert_eq!(doc.key_of(id), "pasted");
        assert_eq!(doc.child_count(id), 1);
    }

    #[test]
    fn a_subtree_can_be_replaced_in_place() {
        let mut doc = json(r#"{"a":{"old":1},"b":2}"#);
        let target = members(&doc)[0];
        let replacement = replace_with_json(&mut doc, target, r#"[7,8]"#).unwrap();

        assert_ne!(replacement, target, "the replacement is a fresh subtree");
        assert_eq!(compact(&doc), r#"{"a":[7,8],"b":2}"#);
        assert_eq!(doc.key_of(replacement), "a", "the name carries over");
        assert!(matches!(doc.node(target), Err(Error::NoSuchNode(_))));
    }

    #[test]
    fn the_root_can_be_replaced() {
        let mut doc = json(r#"{"a":1}"#);
        let old_root = doc.root();
        let root = replace_with_json(&mut doc, old_root, r#"[1,2,3]"#).unwrap();

        assert_eq!(doc.root(), root);
        assert_eq!(compact(&doc), "[1,2,3]");
        assert!(matches!(doc.node(old_root), Err(Error::NoSuchNode(_))));
    }

    #[test]
    fn invalid_pasted_json_leaves_the_document_untouched() {
        let mut doc = json(r#"{"a":1}"#);
        let root = doc.root();
        let target = members(&doc)[0];
        let before = doc.stats.node_count;

        assert!(replace_with_json(&mut doc, target, r#"{"broken":"#).is_err());
        assert!(insert_json(&mut doc, root, 0, Some("x".into()), "[1,").is_err());

        assert_eq!(compact(&doc), r#"{"a":1}"#);
        assert_eq!(doc.stats.node_count, before);
        assert!(!doc.dirty, "a rejected edit must not mark the document changed");
    }

    #[test]
    fn deleting_a_member_shifts_the_rest() {
        let mut doc = json(r#"{"a":1,"b":2,"c":3}"#);
        let root = doc.root();
        let b = members(&doc)[1];
        remove(&mut doc, b).unwrap();

        assert_eq!(compact(&doc), r#"{"a":1,"c":3}"#);
        assert_eq!(doc.child_count(root), 2);
    }

    #[test]
    fn the_root_cannot_be_deleted() {
        let mut doc = json(r#"{"a":1}"#);
        let root = doc.root();
        assert!(matches!(remove(&mut doc, root), Err(Error::Conflict(_))));
    }

    #[test]
    fn editing_inside_embedded_json_rewrites_the_string() {
        let text = r#"{"demo":"{\"a\":1,\"keep\":\"same\"}"}"#;
        let mut doc = document::load(text.as_bytes().to_vec(), LoadOptions::default()).unwrap();

        let demo = members(&doc)[0];
        let overlay = doc.presented(demo);
        let a = doc.child_at(overlay, 0).unwrap();
        assert!(!doc.overlay_is_authoritative(demo), "nothing edited yet");

        set_value(&mut doc, a, Value::Int(99)).unwrap();
        assert!(doc.overlay_is_authoritative(demo));
        assert_eq!(compact(&doc), r#"{"demo":"{\"a\":99,\"keep\":\"same\"}"}"#);
    }

    #[test]
    fn editing_deep_inside_a_nested_overlay_rewrites_every_enclosing_string() {
        let text = r#"{"demo":"{\"level1\":\"{\\\"a\\\":1}\"}"}"#;
        let mut doc = document::load(text.as_bytes().to_vec(), LoadOptions::default()).unwrap();

        let demo = members(&doc)[0];
        let level1 = doc.child_at(doc.presented(demo), 0).unwrap();
        let a = doc.child_at(doc.presented(level1), 0).unwrap();

        set_value(&mut doc, a, Value::Int(2)).unwrap();
        // Both strings in the chain have to be regenerated, or the outer one
        // would still serialize its stale text and swallow the change.
        assert!(doc.overlay_is_authoritative(level1));
        assert!(doc.overlay_is_authoritative(demo));
        assert_eq!(compact(&doc), r#"{"demo":"{\"level1\":\"{\\\"a\\\":2}\"}"}"#);
    }

    #[test]
    fn deleting_an_expanded_string_drops_its_overlay() {
        let text = r#"{"a":1,"demo":"{\"x\":1}"}"#;
        let mut doc = document::load(text.as_bytes().to_vec(), LoadOptions::default()).unwrap();
        assert_eq!(doc.stats.overlay_nodes, 1);

        let demo = members(&doc)[1];
        remove(&mut doc, demo).unwrap();
        assert_eq!(doc.stats.overlay_nodes, 0, "the overlay must be retired too");
        assert_eq!(compact(&doc), r#"{"a":1}"#);
    }

    #[test]
    fn overwriting_an_expanded_string_drops_its_overlay() {
        let text = r#"{"demo":"{\"x\":1}"}"#;
        let mut doc = document::load(text.as_bytes().to_vec(), LoadOptions::default()).unwrap();
        assert_eq!(doc.stats.overlay_nodes, 1);

        let demo = members(&doc)[0];
        set_value(&mut doc, demo, Value::Str("plain".into())).unwrap();
        assert_eq!(doc.stats.overlay_nodes, 0);
        assert!(!doc.node(demo).unwrap().has_overlay());
        assert_eq!(compact(&doc), r#"{"demo":"plain"}"#);
    }

    #[test]
    fn an_edit_elsewhere_leaves_an_overshadowing_string_alone() {
        let text = r#"{"a":1,"demo":"{\"x\":1}"}"#;
        let mut doc = document::load(text.as_bytes().to_vec(), LoadOptions::default()).unwrap();
        let m = members(&doc);

        set_value(&mut doc, m[0], Value::Int(2)).unwrap();

        assert!(doc.node(m[1]).unwrap().has_overlay(), "the overlay is untouched");
        assert!(!doc.overlay_is_authoritative(m[1]), "and still not regenerated");
        assert_eq!(compact(&doc), r#"{"a":2,"demo":"{\"x\":1}"}"#);
        assert_eq!(expand::expand(&mut doc, 3).overlays_created, 0, "nothing new to find");
    }

    #[test]
    fn a_newly_written_string_can_be_unfolded_by_expanding_again() {
        // Edits deliberately do not expand on their own: walking every node
        // after each keystroke would be ruinous on a large document. Re-running
        // the pass picks up new strings and leaves existing overlays alone.
        let mut doc = json(r#"{"a":1}"#);
        let root = doc.root();
        insert(&mut doc, root, 1, Some("s".into()), Value::Str(r#"{"x":[1,2]}"#.into())).unwrap();
        let s = members(&doc)[1];
        assert!(!doc.node(s).unwrap().has_overlay());

        doc.expand_embedded_json(3);
        assert!(doc.node(s).unwrap().has_overlay());
        assert_eq!(doc.child_count(doc.presented(s)), 1);
    }

    #[test]
    fn values_read_back_in_the_shape_the_editor_writes() {
        let doc = json(r#"{"n":null,"b":true,"i":-5,"d":1.5,"s":"text","o":{},"a":[]}"#);
        let expected = [
            Value::Null,
            Value::Bool(true),
            Value::Int(-5),
            Value::Double(1.5),
            Value::Str("text".into()),
            Value::Object,
            Value::Array,
        ];
        let m = members(&doc);
        for (index, want) in expected.into_iter().enumerate() {
            assert_eq!(value_of(&doc, m[index]).unwrap(), want, "member {index}");
        }
    }

    #[test]
    fn bson_only_scalars_report_as_unsupported_rather_than_guessing() {
        let bytes = bson(&[("id", 0x07, &[0u8; 12]), ("n", 0x10, &i32(1))]);
        let doc = parse_one(&bytes);
        let oid = members(&doc)[0];

        assert_eq!(doc.node(oid).unwrap().kind, Kind::ObjectId);
        assert!(matches!(
            value_of(&doc, oid),
            Err(Error::Unsupported { action: "edit", kind: "objectId" })
        ));
    }

    #[test]
    fn edits_survive_a_bson_round_trip() {
        let bytes = bson(&[("n", 0x10, &i32(1)), ("s", 0x02, &string("old"))]);
        let mut doc = parse_one(&bytes);
        let root = doc.root();
        let m = members(&doc);

        set_value(&mut doc, m[0], Value::Int(9)).unwrap();
        set_value(&mut doc, m[1], Value::Str("new".into())).unwrap();
        rename(&mut doc, m[1], "renamed").unwrap();
        insert(&mut doc, root, 2, Some("added".into()), Value::Bool(true)).unwrap();

        let saved = doc.save(&SaveOptions::default()).unwrap();
        let again = document::load(saved.bytes, LoadOptions::default()).unwrap();
        let after = members(&again);
        let keys: Vec<String> = after.iter().map(|&c| again.key_of(c).into_owned()).collect();

        assert_eq!(keys, ["n", "renamed", "added"]);
        assert_eq!(again.node(after[0]).unwrap().i, 9);
        assert_eq!(again.text(again.node(after[1]).unwrap().val), "new");
        assert_eq!(again.node(after[2]).unwrap().sub, 1, "the bool stayed true");
    }
}
