//! The IPC surface: one command per thing the UI can ask for.
//!
//! The document itself never crosses the bridge. A 12MB file becomes close to
//! two million arena nodes, and serializing all of that for the webview would
//! take longer than parsing it did, so every command hands back only the slice
//! the UI is about to paint.

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use doccore::edit::{self, Value};
use doccore::opts::{Format, JsonOptions, LoadOptions, SaveOptions};
use doccore::{document, Doc, Saved};
use tauri::State;

use crate::dto::{
    page_children, ApiError, ChildrenPage, CmdResult, DocInfo, EditResult, OpenOptions, SaveResult,
    TabInfo,
};

/// One open document, plus the options it was loaded with so the expand depth
/// can be changed later without asking the user for the file again.
pub struct Session {
    pub doc: Doc,
    pub opts: doccore::LoadOptions,
    pub load_ms: f64,
}

/// Every open tab. A `BTreeMap` keeps the tab strip in the order the documents
/// were opened, which the frontend relies on when it restores a session.
#[derive(Default)]
pub struct Sessions {
    next_id: u64,
    open: BTreeMap<u64, Session>,
}

/// The app holds one session per open tab, addressed by a stable id.
#[derive(Default)]
pub struct Store(pub Mutex<Sessions>);

/// Largest window the UI may ask for in one call. Paging exists to keep huge
/// arrays off the bridge, so an unbounded limit would defeat the point.
const MAX_PAGE: u32 = 5_000;

fn lock<'a>(state: &'a State<'_, Store>) -> CmdResult<MutexGuard<'a, Sessions>> {
    state.0.lock().map_err(|_| ApiError::state("the document store is wedged; restart the app"))
}

fn read<R>(
    state: &State<'_, Store>,
    tab: u64,
    f: impl FnOnce(&Session) -> CmdResult<R>,
) -> CmdResult<R> {
    let guard = lock(state)?;
    f(guard.open.get(&tab).ok_or_else(ApiError::no_document)?)
}

fn write<R>(
    state: &State<'_, Store>,
    tab: u64,
    f: impl FnOnce(&mut Session) -> CmdResult<R>,
) -> CmdResult<R> {
    let mut guard = lock(state)?;
    f(guard.open.get_mut(&tab).ok_or_else(ApiError::no_document)?)
}

fn millis(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Publish a freshly loaded document as a new tab.
fn install(
    state: &State<'_, Store>,
    doc: Doc,
    options: OpenOptions,
    load_ms: f64,
) -> CmdResult<TabInfo> {
    let info = DocInfo::of(&doc, load_ms);
    let mut guard = lock(state)?;
    let tab = guard.next_id;
    guard.next_id += 1;
    guard.open.insert(tab, Session { doc, opts: options.load(), load_ms });
    Ok(TabInfo { tab, info })
}

/// Load a file straight into the store, for the `bson-editor <path>` form.
///
/// This runs during setup, before there is a webview to answer, so a failure is
/// reported on stderr and the app simply opens empty.
pub fn open_into(store: &Store, path: &str) {
    let started = Instant::now();
    let opts = LoadOptions::default();
    let loaded = fs::read(path)
        .map_err(doccore::Error::from)
        .and_then(|bytes| document::load(bytes, opts))
        .map(|mut doc| {
            doc.path = Some(path.to_string());
            doc
        });
    match loaded {
        Ok(doc) => {
            let load_ms = millis(started);
            if let Ok(mut guard) = store.0.lock() {
                let tab = guard.next_id;
                guard.next_id += 1;
                guard.open.insert(tab, Session { doc, opts, load_ms });
            }
        }
        Err(err) => eprintln!("could not open {path}: {err}"),
    }
}

/// Bytes ready to write, plus where they go. Split out so the disk write happens
/// outside the lock.
struct Prepared {
    target: String,
    saved: Saved,
    format: Format,
}

#[tauri::command]
pub fn core_version() -> String {
    doccore::version().to_string()
}

/// Read a file from disk and parse it (requirements 1, 3 and 4).
#[tauri::command]
pub fn open_file(
    state: State<'_, Store>,
    path: String,
    options: Option<OpenOptions>,
) -> CmdResult<TabInfo> {
    let options = options.unwrap_or_default();
    let started = Instant::now();
    let bytes = fs::read(&path)?;
    let mut doc = document::load(bytes, options.load())?;
    let load_ms = millis(started);
    doc.path = Some(path);
    install(&state, doc, options, load_ms)
}

/// Parse text the user pasted, which may be JSON or hex/base64-encoded BSON
/// (requirement 3).
#[tauri::command]
pub fn load_text(
    state: State<'_, Store>,
    text: String,
    options: Option<OpenOptions>,
) -> CmdResult<TabInfo> {
    let options = options.unwrap_or_default();
    let started = Instant::now();
    let doc = document::load_text(&text, options.load())?;
    install(&state, doc, options, millis(started))
}

/// Every open tab, in the order it was opened. The frontend calls this once on
/// mount to pick up a document that `bson-editor <path>` loaded during setup.
#[tauri::command]
pub fn list_tabs(state: State<'_, Store>) -> CmdResult<Vec<TabInfo>> {
    let guard = lock(&state)?;
    Ok(guard
        .open
        .iter()
        .map(|(&tab, s)| TabInfo { tab, info: DocInfo::of(&s.doc, s.load_ms) })
        .collect())
}

/// Drop a tab and everything it holds. Large documents make this worth doing
/// eagerly rather than waiting for the process to exit.
#[tauri::command]
pub fn close_tab(state: State<'_, Store>, tab: u64) -> CmdResult<()> {
    let mut guard = lock(&state)?;
    guard.open.remove(&tab).map(|_| ()).ok_or_else(ApiError::no_document)
}

/// One window of a container's children. This is the tree's only data source,
/// which is what keeps a two-million-node document scrollable.
#[tauri::command]
pub fn children(
    state: State<'_, Store>,
    tab: u64,
    node: u32,
    offset: u32,
    limit: u32,
) -> CmdResult<ChildrenPage> {
    read(&state, tab, |s| page_children(&s.doc, node, offset, limit.min(MAX_PAGE)))
}

/// The current value of a node, for prefilling an edit field.
#[tauri::command]
pub fn node_value(state: State<'_, Store>, tab: u64, node: u32) -> CmdResult<Value> {
    read(&state, tab, |s| Ok(edit::value_of(&s.doc, node)?))
}

/// Serialize any subtree as formatted JSON text, for the clipboard
/// (requirement 3). `expandStrings` decides whether an embedded JSON string
/// comes out as the structure the user sees or as the escaped literal on disk.
#[tauri::command]
pub fn copy_subtree(
    state: State<'_, Store>,
    tab: u64,
    node: u32,
    options: Option<JsonOptions>,
) -> CmdResult<String> {
    let options = options.unwrap_or_default();
    read(&state, tab, |s| Ok(s.doc.to_json(node, &options)?))
}

#[tauri::command]
pub fn set_node_value(
    state: State<'_, Store>,
    tab: u64,
    node: u32,
    value: Value,
) -> CmdResult<EditResult> {
    write(&state, tab, |s| {
        edit::set_value(&mut s.doc, node, value)?;
        Ok(EditResult { node, info: DocInfo::of(&s.doc, s.load_ms) })
    })
}

#[tauri::command]
pub fn rename_node(
    state: State<'_, Store>,
    tab: u64,
    node: u32,
    key: String,
) -> CmdResult<EditResult> {
    write(&state, tab, |s| {
        edit::rename(&mut s.doc, node, &key)?;
        Ok(EditResult { node, info: DocInfo::of(&s.doc, s.load_ms) })
    })
}

#[tauri::command]
pub fn insert_node(
    state: State<'_, Store>,
    tab: u64,
    parent: u32,
    index: u32,
    key: Option<String>,
    value: Value,
) -> CmdResult<EditResult> {
    write(&state, tab, |s| {
        let id = edit::insert(&mut s.doc, parent, index as usize, key, value)?;
        Ok(EditResult { node: id, info: DocInfo::of(&s.doc, s.load_ms) })
    })
}

/// Insert a whole pasted structure as a new member or element.
#[tauri::command]
pub fn paste_json(
    state: State<'_, Store>,
    tab: u64,
    parent: u32,
    index: u32,
    key: Option<String>,
    json: String,
) -> CmdResult<EditResult> {
    write(&state, tab, |s| {
        let id = edit::insert_json(&mut s.doc, parent, index as usize, key, &json)?;
        Ok(EditResult { node: id, info: DocInfo::of(&s.doc, s.load_ms) })
    })
}

/// Overwrite a node with a pasted structure. Returns the new node's id, which
/// differs from the old one because the replacement is freshly parsed.
#[tauri::command]
pub fn replace_node(
    state: State<'_, Store>,
    tab: u64,
    node: u32,
    json: String,
) -> CmdResult<EditResult> {
    write(&state, tab, |s| {
        let id = edit::replace_with_json(&mut s.doc, node, &json)?;
        Ok(EditResult { node: id, info: DocInfo::of(&s.doc, s.load_ms) })
    })
}

/// Delete a node. The result names the parent, which is what the UI has to
/// re-read.
#[tauri::command]
pub fn remove_node(state: State<'_, Store>, tab: u64, node: u32) -> CmdResult<EditResult> {
    write(&state, tab, |s| {
        let parent = s.doc.parent_of(node);
        edit::remove(&mut s.doc, node)?;
        Ok(EditResult { node: parent, info: DocInfo::of(&s.doc, s.load_ms) })
    })
}

/// Re-run embedded-JSON expansion (requirement 2).
///
/// Edits do not expand on their own, so a string written after load stays
/// folded until this runs. Deepening the depth also goes through here; lowering
/// it needs `set_expand_depth`, which rebuilds from the original bytes.
#[tauri::command]
pub fn reexpand(state: State<'_, Store>, tab: u64) -> CmdResult<DocInfo> {
    write(&state, tab, |s| {
        let depth = s.doc.expand_depth;
        s.doc.expand_embedded_json(depth);
        Ok(DocInfo::of(&s.doc, s.load_ms))
    })
}

/// Change how many levels of nested JSON string to unfold. Refused while there
/// are unsaved edits, because rebuilding would discard them.
#[tauri::command]
pub fn set_expand_depth(state: State<'_, Store>, tab: u64, depth: u8) -> CmdResult<DocInfo> {
    write(&state, tab, |s| {
        s.opts.expand_json_depth = depth;
        let opts = s.opts;
        s.doc.set_expand_depth(depth, &opts)?;
        Ok(DocInfo::of(&s.doc, s.load_ms))
    })
}

/// Write the document to disk. For BSON this restores the magic-byte prefix and
/// any trailing bytes that were stripped on load (requirement 1).
#[tauri::command]
pub fn save_file(
    state: State<'_, Store>,
    tab: u64,
    path: Option<String>,
    options: Option<SaveOptions>,
) -> CmdResult<SaveResult> {
    let options = options.unwrap_or_default();
    let prepared = read(&state, tab, |s| {
        let target = match path.clone() {
            Some(p) => p,
            None => s.doc.path.clone().ok_or_else(|| {
                ApiError::state("this document has no path; choose where to save")
            })?,
        };
        let saved = s.doc.save(&options)?;
        Ok(Prepared { target, saved, format: options.format.unwrap_or(s.doc.format) })
    })?;

    fs::write(&prepared.target, &prepared.saved.bytes)?;
    let bytes_written = prepared.saved.bytes.len();
    let target = prepared.target.clone();
    // Only clear the dirty flag once the bytes are actually on disk.
    write(&state, tab, |s| {
        s.doc.dirty = false;
        s.doc.path = Some(target);
        Ok(())
    })?;

    Ok(SaveResult {
        path: prepared.target,
        bytes_written,
        format: prepared.format,
        warnings: prepared.saved.warnings,
    })
}

/// Serialize without touching the disk, for "copy the whole document".
#[tauri::command]
pub fn export_text(
    state: State<'_, Store>,
    tab: u64,
    options: Option<SaveOptions>,
) -> CmdResult<String> {
    let options = options.unwrap_or_default();
    read(&state, tab, |s| {
        let saved = s.doc.save(&options)?;
        String::from_utf8(saved.bytes).map_err(|_| {
            ApiError::state("this document is BSON; pass format: json or use saveFile")
        })
    })
}
