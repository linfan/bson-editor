# BSON Editor

A desktop reader and editor for JSON and BSON documents, built with Tauri v2 and React.

It opens plain JSON, plain BSON, and BSON wrapped in a magic-byte prefix. Strings that
turn out to contain JSON unfold into real structure, as deep as you ask. The tree is
virtualized and lazy, so a 10 MB document opens in tens of milliseconds and stays
responsive while you scroll it.

## Running it

```sh
pnpm install
pnpm tauri dev              # develop with hot reload
pnpm tauri build            # produce an installable bundle
cargo build -p bson-editor  # just the binary
./target/debug/bson-editor path/to/document.bson
```

The last form opens straight into the document: the file is parsed during startup, before
the window exists, and the webview picks it up on mount.

| Script | What it does |
| --- | --- |
| `pnpm dev` | Vite dev server on its own, no Tauri shell |
| `pnpm build` | Typecheck and bundle the frontend into `dist/` |
| `pnpm typecheck` | `tsc --noEmit` |
| `pnpm test:core` | The engine's Rust test suite |
| `pnpm perf [mb]` | The performance harness, on a synthetic document of that many megabytes |

`cargo test --workspace` runs everything: 135 tests across the engine and the IPC layer.

## What it does

**Magic-byte framing.** A BSON file often carries a prefix that is not part of the
document — a container header, a length, a signature. Detection takes a fast path when the
file's own leading length declares exactly the whole file, so there is no prefix at all.
Otherwise it tries every offset up to a 256-byte cap and takes the first where the
remainder parses as one or more well-formed BSON documents; whatever follows the last
document is kept as trailing bytes. A prefix longer than the cap is not guessed at and has
to be forced. Whatever sits in front is kept verbatim and written back on save, and so is
the trailing run.

**Nested JSON strings.** A string whose content parses as JSON gets an *overlay*: a second
tree hanging off it, shown in place of the text and badged `json×N` for the Nth level of
nesting. This recurses, so

```json
{"demo":"{\"level1\":\"{\\\"level2\\\":\\\"value\\\"}\"}"}
```

opens as three levels of real structure. The toolbar's stepper sets how many levels to
unfold; each level roughly doubles the escaping, so the cost is bounded by the input and
the control caps out at 12. Overlays are lazy on the way out, too — a string nobody edited
serializes from its original bytes, so untouched embedded JSON stays byte-identical.
Editing inside an overlay marks every enclosing string stale, so the change is not
swallowed by a parent that still held the old text.

**Structured tree.** Rows arrive a page at a time (400 per request, 5,000 per IPC call at
most) and only the rows inside the viewport are ever mounted. Expanding a container reads
its first page; scrolling near the end of a loaded page pulls the next. Every row has a
Copy action that puts that subtree on the clipboard as indented JSON — at any depth,
including the root. The clipboard expands embedded strings, because that is what is on
screen; a file on disk keeps them as strings, because expanding them would write something
that no longer parses back to the same bytes.

**Editing.** Values can be rewritten in place, members renamed, siblings inserted,
subtrees pasted in as JSON, and nodes deleted. Node ids are stable across all of it: a
deletion unlinks and tombstones rather than renumbering, so an edit refreshes only the
container it touched instead of reloading the document. Numbers edited in the typed form
keep their width — a small integer written over an `int32` stays an `int32` rather than
silently widening the element's BSON type. `Cmd/Ctrl+O` opens, `Cmd/Ctrl+S` saves.

Types BSON has that JSON cannot express — `objectId`, `datetime`, `decimal128`, `binary`,
`regex`, `timestamp` — are read and displayed but not editable. Reconstructing their binary
layout from a text field would corrupt them, so the editor declines rather than guesses.

## Performance

`pnpm perf` builds a synthetic document and measures the real paths. Apple Silicon,
release build:

| | 12 MB | 48 MB |
| --- | --- | --- |
| parse | 34 ms (354 MB/s) | 119 ms (403 MB/s) |
| unfold embedded JSON | 19 ms (41,889 overlays) | 81 ms (176,961 overlays) |
| format, minified | 26 ms (459 MB/s) | 103 ms (466 MB/s) |
| format, indented | 39 ms (832 MB/s) | 141 ms (897 MB/s) |
| encode to BSON | 15 ms (1.2 GB/s) | 60 ms (1.2 GB/s) |
| parse that BSON back | 44 ms (400 MB/s) | 210 ms (330 MB/s) |

Copying the largest subtree in the fixture — 150,001 nodes, 1.8 MB of indented JSON —
takes 2.6 ms. JSON and BSON both round-trip byte-exactly, asserted at the end of every run.

The 23 MB fixture opens in the real application at 406 MB resident and then idles at 0%
CPU, because nothing is held in the webview except the rows on screen.

## How it is put together

```
crates/doccore     the engine — no Tauri, no DOM, so it tests in milliseconds
src-tauri          the IPC shell: a document store and the wire types
src                React: the tree, the dialogs, the chrome
```

`doccore` holds the whole model in one arena. Nodes are a flat `Vec<Node>` at exactly 64
bytes each — one cache line — with child lists in a compressed-sparse-row `Vec<u32>` and
every key, string and binary payload in a single `Vec<u8>` addressed by offset and length.
Parsing drives `serde_json`'s `Deserializer` with a `Visitor` that appends straight into
that arena, so there is no intermediate `Value` tree to allocate and then drop; writing
goes into a caller-supplied byte buffer with `itoa` and `ryu` for numbers.

The IPC surface is paged rather than streaming, and the frontend keeps a `generation`
counter that invalidates cached pages whenever the document is rebuilt from its original
bytes — which is the one kind of change that can hand back the same root id for entirely
different nodes. The interfaces in `src/api.ts` mirror the Rust wire types by hand, so a
test pins every field name they emit: a rename would otherwise arrive in the webview as
`undefined` rather than failing to build.

## Known limits

- Changing the expand depth rebuilds from the original bytes, so it is refused while there
  are unsaved edits rather than quietly discarding them. `Re-scan` is the in-place
  alternative: it looks for JSON strings written since load without touching your edits.
- A file holding several concatenated BSON documents gets a synthetic array root so the
  tree can show them all. That root is not written back out.
- A prefix longer than the 256-byte scan cap is not auto-detected. `OpenOptions.magicLen`
  forces one, but no control in the UI exposes it yet.
- The typed edit form rejects integers past JavaScript's safe-integer range. The dialog's
  Raw JSON tab parses them exactly on the Rust side, which is why it exists.
