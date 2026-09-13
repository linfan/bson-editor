//! Requirement 2: unfold JSON that is hiding inside a string value.
//!
//! A field like `{"demo":"{\"a\":1}"}` is a string as far as the codec is
//! concerned. This pass re-parses those strings into the same arena and hangs
//! the result off [`overlay`](crate::arena::Node::overlay), so the tree can show
//! the structure while the serializer keeps writing the original escaped text
//! until the user edits something inside it.
//!
//! Nesting is handled by the same walk: an overlay's own strings are candidates
//! one level down, which is what makes
//! `{"demo":"{\"level1\":\"{\\\"level2\\\":\\\"value\\\"}\"}"}` unfold twice.
//!
//! Cost is bounded by the input. Every overlay is parsed out of bytes that are
//! already inside the document, and each level of nesting is escaped one more
//! time than the level above it, so the payloads strictly shrink. There is no
//! node budget to tune.

use crate::arena::{Doc, Kind, NO_NODE};
use crate::json_read::parse_json;

/// What one expansion pass did. Returned to the caller for logging and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Expansion {
    /// String nodes that were old enough and shallow enough to consider.
    pub strings_examined: usize,
    /// How many of those decoded into a JSON object or array.
    pub overlays_created: usize,
    /// Arena nodes the pass added.
    pub nodes_added: usize,
}

/// Parse embedded JSON out of every string in `doc`, up to `max_depth` levels of
/// nesting. `0` disables expansion.
pub fn expand(doc: &mut Doc, max_depth: u8) -> Expansion {
    let mut report = Expansion::default();
    if max_depth == 0 || doc.root == NO_NODE {
        return report;
    }
    let base = doc.nodes.len();
    // The payload has to be copied out before parsing: the parse appends to the
    // blob, which may reallocate the buffer the payload points into. One buffer
    // is reused across the whole pass.
    let mut scratch: Vec<u8> = Vec::new();
    // (node id, how many overlay levels sit above it)
    let mut work: Vec<(u32, u8)> = vec![(doc.root, 0)];

    while let Some((id, level)) = work.pop() {
        let node = doc.nodes[id as usize];
        if node.kind.is_container() {
            work.extend(doc.children(id).iter().map(|&c| (c, level)));
            // The scope document of code-with-scope is a sibling tree, not a
            // child, so it has to be walked explicitly.
            if node.aux != NO_NODE {
                work.push((node.aux, level));
            }
            continue;
        }
        if !expandable(node.kind) || level >= max_depth {
            continue;
        }
        if node.overlay != NO_NODE {
            work.push((node.overlay, level + 1));
            continue;
        }
        report.strings_examined += 1;
        if !starts_container(doc.raw(node.val)) {
            continue;
        }
        scratch.clear();
        scratch.extend_from_slice(doc.raw(node.val));

        // Most strings are not JSON, so a failed parse is the common case and
        // has to leave no trace: undo every node, child slot and blob byte the
        // attempt allocated.
        let undo = doc.checkpoint();
        match parse_json(&scratch, doc) {
            Ok(root) => {
                report.overlays_created += 1;
                doc.overlay_count += 1;
                let owner = &mut doc.nodes[id as usize];
                owner.overlay = root;
                owner.sub = level + 1;
                // Linking the overlay back to its string keeps `path_to` and
                // `mark_overlay_dirty` working across the whole chain.
                doc.nodes[root as usize].parent = id;
                work.push((root, level + 1));
            }
            Err(_) => undo.restore(doc),
        }
    }

    report.nodes_added = doc.nodes.len() - base;
    report
}

/// Only the two string types can hold JSON. JavaScript source looks like JSON
/// sometimes but is not, so it is left alone.
#[inline]
fn expandable(kind: Kind) -> bool {
    matches!(kind, Kind::Str | Kind::Symbol)
}

/// Cheap pre-filter. A string whose first meaningful byte is neither brace can
/// never parse to a container, which skips the parse and the payload copy for
/// almost every string in a typical document.
fn starts_container(bytes: &[u8]) -> bool {
    matches!(bytes.iter().find(|b| !b.is_ascii_whitespace()), Some(b'{') | Some(b'['))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opts::{Format, JsonOptions};
    use crate::testutil::parse_one;

    fn json(text: &str) -> Doc {
        let mut doc = Doc::new(Format::Json);
        doc.root = parse_json(text.as_bytes(), &mut doc).expect("valid json");
        doc
    }

    fn expand_json(text: &str, depth: u8) -> Doc {
        let mut doc = json(text);
        expand(&mut doc, depth);
        doc
    }

    /// Key of the single member of an object node.
    fn only_key(doc: &Doc, object: u32) -> String {
        let kids = doc.children(object);
        assert_eq!(kids.len(), 1, "expected exactly one member");
        doc.key_of(kids[0]).into_owned()
    }

    #[test]
    fn a_json_string_becomes_a_structure() {
        let doc = expand_json(r#"{"demo":"{\"a\":1}"}"#, 3);
        let demo = doc.children(doc.root())[0];
        assert_eq!(doc.text(doc.node_at(demo).val), r#"{"a":1}"#, "raw text is untouched");

        let overlay = doc.presented(demo);
        assert_ne!(overlay, demo);
        assert_eq!(doc.node_at(overlay).kind, Kind::Object);
        assert_eq!(only_key(&doc, overlay), "a");
        assert_eq!(doc.node_at(doc.children(overlay)[0]).i, 1);
        assert_eq!(doc.node_at(demo).sub, 1);
    }

    #[test]
    fn nested_json_strings_unfold_one_level_at_a_time() {
        let text = r#"{"demo":"{\"level1\":\"{\\\"level2\\\":\\\"value\\\"}\"}"}"#;

        let shallow = expand_json(text, 1);
        let demo = shallow.children(shallow.root())[0];
        let level1 = shallow.presented(demo);
        assert_eq!(only_key(&shallow, level1), "level1");
        let inner = shallow.children(level1)[0];
        assert!(!shallow.node_at(inner).has_overlay(), "depth 1 must stop at the second string");

        let deep = expand_json(text, 2);
        let demo = deep.children(deep.root())[0];
        let inner = deep.children(deep.presented(demo))[0];
        assert_eq!(deep.node_at(inner).sub, 2);
        let level2 = deep.presented(inner);
        assert_eq!(only_key(&deep, level2), "level2");
        assert_eq!(deep.text(deep.node_at(deep.children(level2)[0]).val), "value");
    }

    #[test]
    fn depth_zero_disables_expansion() {
        let doc = expand_json(r#"{"demo":"{\"a\":1}"}"#, 0);
        let demo = doc.children(doc.root())[0];
        assert!(!doc.node_at(demo).has_overlay());
        assert_eq!(doc.nodes.len(), 2, "no arena growth");
    }

    #[test]
    fn plain_strings_are_left_alone() {
        let mut doc = json(r#"{"s":"hello","e":"","x":"[1,","y":"{\"a\":1","z":"  spaced out"}"#);
        let report = expand(&mut doc, 3);

        assert_eq!(report.strings_examined, 5);
        assert_eq!(report.overlays_created, 0);
        assert_eq!(report.nodes_added, 0);
        for &id in doc.children(doc.root()) {
            assert!(!doc.node_at(id).has_overlay(), "{} gained an overlay", doc.key_of(id));
        }
    }

    #[test]
    fn a_scalar_json_string_does_not_count_as_a_structure() {
        // `123` is valid JSON but not a structure, so there is nothing to show.
        let report = {
            let mut doc = json(r#"{"n":"123","b":"true"}"#);
            expand(&mut doc, 3)
        };
        assert_eq!(report.strings_examined, 2);
        assert_eq!(report.overlays_created, 0);
        assert_eq!(report.nodes_added, 0, "a rolled-back parse must not grow the arena");
    }

    #[test]
    fn broken_json_leaves_the_arena_untouched() {
        let mut doc = json(r#"{"a":"{\"x\":","b":"[1,","c":"fine"}"#);
        let before = doc.nodes.len();
        let blob_before = doc.blob.len();
        let report = expand(&mut doc, 3);

        assert_eq!(report.overlays_created, 0);
        assert_eq!(doc.nodes.len(), before);
        assert_eq!(doc.blob.len(), blob_before);
        assert_eq!(doc.containers.len(), 1, "only the root object survives the rollback");

        // The rollback has to leave the arena usable: parse the next document
        // into the same one and check the child list is still right.
        let after = parse_json(br#"{"z":[1,2]}"#, &mut doc).unwrap();
        assert_eq!(only_key(&doc, after), "z");
        assert_eq!(doc.children(doc.children(after)[0]).len(), 2);
    }

    #[test]
    fn expansion_does_not_change_what_gets_serialized() {
        let text = r#"{"demo":"{\"level1\":\"{\\\"level2\\\":\\\"value\\\"}\"}","n":1}"#;
        let mut doc = json(text);
        expand(&mut doc, 8);
        let mut out = Vec::new();
        crate::json_write::write_json(&doc, doc.root(), &JsonOptions::compact(), &mut out).unwrap();
        assert_eq!(out, text.as_bytes(), "an untouched overlay must serialize the original bytes");
    }

    #[test]
    fn overlay_parent_links_reach_back_to_the_document() {
        let doc = expand_json(r#"{"demo":"{\"level1\":\"{\\\"a\\\":1}\"}"}"#, 4);
        let demo = doc.children(doc.root())[0];
        let level1 = doc.children(doc.presented(demo))[0];
        let leaf = doc.children(doc.presented(level1))[0];

        assert_eq!(doc.parent_of(doc.presented(demo)), demo);
        assert_eq!(doc.parent_of(doc.presented(level1)), level1);
        assert_eq!(
            doc.path_to(leaf).unwrap(),
            vec![doc.root(), demo, doc.presented(demo), level1, doc.presented(level1), leaf]
        );
    }

    #[test]
    fn arrays_of_json_strings_are_expanded_too() {
        let doc = expand_json(r#"["{\"a\":1}","{\"b\":2}"]"#, 3);
        let kids = doc.children(doc.root());
        assert_eq!(kids.len(), 2);
        assert_eq!(only_key(&doc, doc.presented(kids[0])), "a");
        assert_eq!(only_key(&doc, doc.presented(kids[1])), "b");
    }

    #[test]
    fn bson_strings_are_expanded() {
        use crate::testutil::{bson, i32, string};
        let bytes = bson(&[("payload", 0x02, &string(r#"{"a":1}"#)), ("n", 0x10, &i32(2))]);
        let mut doc = parse_one(&bytes);
        let report = expand(&mut doc, 3);

        assert_eq!(report.overlays_created, 1);
        let payload = doc.children(doc.root())[0];
        assert_eq!(only_key(&doc, doc.presented(payload)), "a");
    }

    #[test]
    fn deeply_nested_json_still_expands_safely() {
        // One string per level. Eight levels is plenty to prove the chain
        // recurses past the default depth of 3, and it has to stay small: each
        // wrap re-escapes the level below it, so the text roughly doubles.
        let mut text = String::from(r#"{"leaf":1}"#);
        for _ in 0..8 {
            text = format!(r#"{{"n":{}}}"#, serde_json::to_string(&text).unwrap());
        }
        let mut doc = json(&text);
        let report = expand(&mut doc, 64);
        assert_eq!(report.overlays_created, 8);
    }
}
