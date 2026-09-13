//! Performance harness for requirement 4.
//!
//! Generates a large synthetic document, then reports parse and format
//! throughput plus the arena's memory footprint. Run with:
//!
//! ```text
//! cargo run --release -p doccore --bin perf [target-megabytes]
//! ```

use std::time::{Duration, Instant};

use doccore::opts::{Flavor, Indent, JsonOptions, LoadOptions};
use doccore::{document, Doc, Kind};

/// Number of elements in the one deliberately large nested array, so the
/// subtree-copy measurement has something substantial to chew on.
const BIG_NESTED_ARRAY: u64 = 50_000;

fn main() {
    let target_mb: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(12);
    let target = target_mb * 1024 * 1024;

    let started = Instant::now();
    let fixture = build_json(target);
    println!(
        "fixture       {:>7.2} MB  built in {}",
        mb(fixture.len()),
        show(started.elapsed())
    );

    let started = Instant::now();
    let doc = document::load(fixture.as_bytes().to_vec(), LoadOptions::default())
        .expect("fixture must parse");
    let parse = started.elapsed();
    println!(
        "parse         {:>7.2} MB/s  in {}  -> {} nodes, depth {}",
        throughput(fixture.len(), parse),
        show(parse),
        doc.stats.node_count,
        doc.stats.max_depth
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
        .to_json(
            doc.root(),
            &JsonOptions {
                indent: Indent::Spaces(2),
                ..Default::default()
            },
        )
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
        .to_json(
            doc.root(),
            &JsonOptions {
                flavor: Flavor::Extended,
                ..JsonOptions::compact()
            },
        )
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
            doc.child_at(doc.root(), 0).expect("fixture has records")
        };
        let nodes = doc.subtree_size(subtree);
        let started = Instant::now();
        let copied = doc
            .to_json(
                subtree,
                &JsonOptions {
                    indent: Indent::Spaces(2),
                    ..Default::default()
                },
            )
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
    let again = reparsed
        .to_json(reparsed.root(), &JsonOptions::compact())
        .unwrap();
    assert_eq!(again, compact, "round-trip changed the document");
    assert_eq!(reparsed.stats.node_count, doc.stats.node_count);
    println!(
        "round-trip    {:>7}       verified in {}",
        "ok",
        show(started.elapsed())
    );
}

/// Build a JSON array of records with a realistic mix of value shapes.
fn build_json(target_bytes: usize) -> String {
    let mut out = String::with_capacity(target_bytes + 4 * 1024 * 1024);
    out.push('[');
    let mut index = 0u64;
    while out.len() < target_bytes {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            r#"{{"id":{index},"name":"user-{index}\ttabbed","email":"user{index}@example.com","score":{score:.6},"active":{active},"tags":["alpha","beta","gamma-{index}"],"balance":null,"created":"2024-0{month}-1{day}T12:34:56Z","meta":{{"level":{level},"flags":[true,false,true],"nested":{{"deep":{{"deeper":[{index},{a},{b}]}}}}}},"history":["#,
            index = index,
            score = (index as f64) * 0.0001234,
            active = index.is_multiple_of(2),
            month = index % 9 + 1,
            day = index % 9,
            level = index % 5,
            a = index * 7,
            b = index * 11,
        ));
        let history_len = if index == 0 {
            BIG_NESTED_ARRAY
        } else {
            index % 50
        };
        for h in 0..history_len {
            if h > 0 {
                out.push(',');
            }
            out.push_str(&format!("{{\"t\":{h},\"v\":{}}}", (h * 37) % 1000));
        }
        out.push_str("]}");
        index += 1;
    }
    out.push(']');
    out
}

/// The largest array below the root. The root is excluded because the fixture's
/// root is the whole document, which the format measurements already cover.
fn largest_nested_array(doc: &Doc) -> u32 {
    let root = doc.root();
    let mut best = root;
    let mut best_size = 0usize;
    let mut work = doc.children(root).to_vec();
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
