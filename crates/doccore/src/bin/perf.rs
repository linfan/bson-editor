//! Performance harness for requirement 4.
//!
//! Generates a large synthetic document, then reports parse and format
//! throughput plus the arena's memory footprint. Run with:
//!
//! ```text
//! cargo run --release -p doccore --bin perf [target-megabytes]
//! ```

use std::time::{Duration, Instant};

use doccore::opts::{Flavor, Format, Indent, JsonOptions, LoadOptions, SaveOptions};
use doccore::{document, Doc, Kind};

/// Number of elements in the one deliberately large nested array, so the
/// subtree-copy measurement has something substantial to chew on.
const BIG_NESTED_ARRAY: u64 = 50_000;

fn main() {
    let target_mb: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(12);
    let target = target_mb * 1024 * 1024;

    let started = Instant::now();
    let fixture = build_json(target);
    println!("fixture       {:>7.2} MB  built in {}", mb(fixture.len()), show(started.elapsed()));

    // Parse with expansion off, then expand separately, so the cost of
    // requirement 2 shows up on its own line instead of hiding inside the parse
    // number.
    let parse_opts = LoadOptions { expand_json_depth: 0, ..Default::default() };
    let started = Instant::now();
    let mut doc =
        document::load(fixture.as_bytes().to_vec(), parse_opts).expect("fixture must parse");
    let parse = started.elapsed();
    println!(
        "parse         {:>7.2} MB/s  in {}  -> {} nodes, depth {}",
        throughput(fixture.len(), parse),
        show(parse),
        doc.stats.node_count,
        doc.stats.max_depth
    );

    let started = Instant::now();
    let expansion = doc.expand_embedded_json(LoadOptions::default().expand_json_depth);
    let expand_t = started.elapsed();
    println!(
        "expand        {:>7} overlays  in {}  -> {} strings scanned, +{} nodes",
        expansion.overlays_created,
        show(expand_t),
        expansion.strings_examined,
        expansion.nodes_added
    );

    println!(
        "arena         {:>7.2} MB  ({} nodes x 64 B, {:.2} MB blob, {} child slots)",
        mb(doc.stats.heap_bytes - doc.stats.source_bytes),
        doc.stats.node_count,
        mb(doc.stats.blob_bytes),
        doc.stats.child_slots
    );

    let started = Instant::now();
    let compact = doc.to_json(doc.root(), &JsonOptions::compact()).unwrap();
    let compact_t = started.elapsed();
    println!(
        "format min    {:>7.2} MB/s  in {}  -> {:.2} MB",
        throughput(compact.len(), compact_t),
        show(compact_t),
        mb(compact.len())
    );

    let started = Instant::now();
    let pretty = doc
        .to_json(doc.root(), &JsonOptions { indent: Indent::Spaces(2), ..Default::default() })
        .unwrap();
    let pretty_t = started.elapsed();
    println!(
        "format pretty {:>7.2} MB/s  in {}  -> {:.2} MB",
        throughput(pretty.len(), pretty_t),
        show(pretty_t),
        mb(pretty.len())
    );

    let started = Instant::now();
    let extended = doc
        .to_json(doc.root(), &JsonOptions { flavor: Flavor::Extended, ..JsonOptions::compact() })
        .unwrap();
    let extended_t = started.elapsed();
    println!(
        "format ext    {:>7.2} MB/s  in {}  -> {:.2} MB",
        throughput(extended.len(), extended_t),
        show(extended_t),
        mb(extended.len())
    );

    // "Copy as formatted JSON" is the interactive hot path in the tree view.
    // Measure it on the largest nested subtree and on a single record.
    for label in ["largest subtree", "single record"] {
        let subtree = if label == "largest subtree" {
            largest_nested_array(&doc)
        } else {
            // The second record: the first carries the deliberately oversized
            // history array, so it would not represent a typical copy.
            let records = doc.child_at(doc.root(), 0).expect("fixture has a records array");
            doc.child_at(records, 1).expect("fixture has more than one record")
        };
        let nodes = doc.subtree_size(subtree);
        let started = Instant::now();
        let copied = doc
            .to_json(subtree, &JsonOptions { indent: Indent::Spaces(2), ..Default::default() })
            .unwrap();
        println!(
            "copy {label:<11} {:>7.2} KB  in {}  ({} nodes)",
            copied.len() as f64 / 1024.0,
            show(started.elapsed()),
            nodes
        );
    }

    // Correctness gate: re-parsing the minified output must reproduce it byte
    // for byte, so formatting is stable and lossless.
    let started = Instant::now();
    let reparsed = document::load(compact.as_bytes().to_vec(), LoadOptions::default()).unwrap();
    let again = reparsed.to_json(reparsed.root(), &JsonOptions::compact()).unwrap();
    assert_eq!(again, compact, "round-trip changed the document");
    assert_eq!(reparsed.stats.node_count, doc.stats.node_count);
    println!("json round-trip {:>5}     verified in {}", "ok", show(started.elapsed()));

    // The same exercise for BSON, which is the format this editor exists for.
    let started = Instant::now();
    let encoded = doc
        .save(&SaveOptions { format: Some(Format::Bson), ..Default::default() })
        .expect("fixture must be representable as BSON");
    let encode = started.elapsed();
    assert!(encoded.warnings.is_empty(), "{:?}", encoded.warnings);
    println!(
        "bson encode   {:>7.2} MB/s  in {}  -> {:.2} MB",
        throughput(encoded.bytes.len(), encode),
        show(encode),
        mb(encoded.bytes.len())
    );

    let bson_bytes = encoded.bytes;
    let started = Instant::now();
    let bson_doc =
        document::load(bson_bytes.clone(), LoadOptions::default()).expect("bson must parse");
    let bson_parse = started.elapsed();
    println!(
        "bson parse    {:>7.2} MB/s  in {}  -> {} nodes",
        throughput(bson_bytes.len(), bson_parse),
        show(bson_parse),
        bson_doc.stats.node_count
    );

    let started = Instant::now();
    let reencoded = bson_doc.save(&SaveOptions::default()).expect("bson must re-encode");
    assert_eq!(reencoded.bytes, bson_bytes, "BSON round trip must be byte exact");
    println!("bson round-trip {:>5}     verified in {}", "ok", show(started.elapsed()));
}

/// Build a JSON object holding an array of records with a realistic mix of
/// value shapes. The object wrapper is deliberate: BSON has no top-level array,
/// so a bare array fixture could not be measured on the BSON path.
fn build_json(target_bytes: usize) -> String {
    let mut out = String::with_capacity(target_bytes + 4 * 1024 * 1024);
    out.push_str("{\"records\":[");
    let mut index = 0u64;
    while out.len() < target_bytes {
        if index > 0 {
            out.push(',');
        }
        // Requirement 2 at scale: a JSON object stored as a string, plus one
        // nested two levels deep. Built with serde_json because hand-writing
        // the escaping for a string inside a string is how bugs hide. Both
        // results are already quoted JSON string literals.
        let payload = serde_json::json!({
            "kind": "event",
            "n": index,
            "tags": ["a", "b"],
            "meta": { "ok": true },
        })
        .to_string();
        let wrapped =
            serde_json::json!({ "lvl": serde_json::json!({ "deep": index }).to_string() })
                .to_string();
        let payload = serde_json::to_string(&payload).unwrap();
        let nested = serde_json::to_string(&wrapped).unwrap();
        out.push_str(&format!(
            r#"{{"id":{index},"name":"user-{index}\ttabbed","email":"user{index}@example.com","score":{score:.6},"active":{active},"tags":["alpha","beta","gamma-{index}"],"balance":null,"created":"2024-0{month}-1{day}T12:34:56Z","meta":{{"level":{level},"flags":[true,false,true],"nested":{{"deep":{{"deeper":[{index},{a},{b}]}}}}}},"payload":{payload},"nested":{nested},"history":["#,
            index = index,
            score = (index as f64) * 0.0001234,
            active = index.is_multiple_of(2),
            month = index % 9 + 1,
            day = index % 9,
            level = index % 5,
            a = index * 7,
            b = index * 11,
            payload = payload,
            nested = nested,
        ));
        let history_len = if index == 0 { BIG_NESTED_ARRAY } else { index % 50 };
        for h in 0..history_len {
            if h > 0 {
                out.push(',');
            }
            out.push_str(&format!("{{\"t\":{h},\"v\":{}}}", (h * 37) % 1000));
        }
        out.push_str("]}");
        index += 1;
    }
    out.push_str("]}");
    out
}

/// The largest array at least two levels below the root, standing in for "copy
/// this subtree" in the UI. The top-level container is skipped because it holds
/// every record, so measuring it would repeat the whole-document numbers.
fn largest_nested_array(doc: &Doc) -> u32 {
    let root = doc.root();
    let mut best = root;
    let mut best_size = 0usize;
    let mut work: Vec<u32> =
        doc.children(root).iter().flat_map(|&child| doc.children(child).to_vec()).collect();
    while let Some(id) = work.pop() {
        let node = doc.node(id).expect("live node");
        if node.is_container() {
            let size = doc.child_count(id) as usize;
            if node.kind == Kind::Array && size > best_size {
                best_size = size;
                best = id;
            }
            work.extend(doc.children(id));
        }
    }
    best
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn throughput(bytes: usize, elapsed: Duration) -> f64 {
    mb(bytes) / elapsed.as_secs_f64()
}

fn show(d: Duration) -> String {
    if d.as_secs_f64() < 1.0 {
        format!("{:.1} ms", d.as_secs_f64() * 1000.0)
    } else {
        format!("{:.3} s", d.as_secs_f64())
    }
}
