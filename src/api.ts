/**
 * Typed wrappers around the Rust command surface.
 *
 * Every shape here mirrors a struct in `src-tauri/src/dto.rs`, which serde
 * emits in camelCase. Nothing in the app calls `invoke` directly, so a rename
 * on either side of the bridge shows up here in one place.
 */
import { invoke } from "@tauri-apps/api/core";

/** Rows requested per page. Small enough to stay instant, large enough that
 *  scrolling a big array does not turn into a stream of round trips. */
export const PAGE_SIZE = 400;

export interface Stats {
  nodeCount: number;
  containerCount: number;
  maxDepth: number;
  blobBytes: number;
  childSlots: number;
  sourceBytes: number;
  documentCount: number;
  magicBytes: number;
  trailingBytes: number;
  overlayNodes: number;
  heapBytes: number;
}

export type Format = "json" | "bson";

export interface DocInfo {
  root: number;
  /** Kind of the document root, used to open the JSON text with `{` or `[`. */
  rootKind: string;
  /** Preview of the root, which is all a scalar root has to show. */
  rootPreview: string;
  path: string | null;
  format: Format;
  dirty: boolean;
  expandDepth: number;
  syntheticRoot: boolean;
  /** Hex of the stripped magic-byte prefix, restored verbatim on save. */
  magic: string;
  trailingBytes: number;
  warnings: string[];
  stats: Stats;
  loadMs: number;
}

export interface NodeRow {
  id: number;
  index: number;
  key: string | null;
  kind: string;
  /** Kind after an embedded-JSON overlay, so its brackets are right. */
  presentedKind: string;
  preview: string;
  childCount: number;
  expandable: boolean;
  embeddedJson: boolean;
  overlayLevel: number;
  editable: boolean;
}

/** An open tab: the id every command addresses it by, plus its document state. */
export interface TabInfo {
  tab: number;
  info: DocInfo;
}

export interface ChildrenPage {
  parent: number;
  offset: number;
  total: number;
  rows: NodeRow[];
}

/** One step of the path from the root to a search match. */
export interface SearchStep {
  node: number;
  index: number;
}

export interface SearchMatch {
  node: number;
  key: string | null;
  preview: string;
  /** Root down to the match, inclusive. */
  path: SearchStep[];
}

export interface SearchResults {
  matches: SearchMatch[];
  /** True when the limit cut the walk short, so there are more matches. */
  truncated: boolean;
}

export interface EditResult {
  node: number;
  info: DocInfo;
}

export interface SaveResult {
  path: string;
  bytesWritten: number;
  format: Format;
  warnings: string[];
}

/** A value the editor can write. BSON-only types are not representable, which
 *  is why `NodeRow.editable` exists. */
export type Value =
  | { type: "null" }
  | { type: "bool"; value: boolean }
  | { type: "int"; value: number }
  | { type: "uint"; value: number }
  | { type: "double"; value: number }
  | { type: "str"; value: string }
  | { type: "object" }
  | { type: "array" };

export type Indent = { kind: "compact" } | { kind: "spaces"; width: number } | { kind: "tab" };

export interface JsonOptions {
  indent: Indent;
  flavor: "plain" | "extended";
  /** Emit an embedded JSON string as the structure it decoded to. */
  expandStrings: boolean;
  maxDepth: number | null;
}

export interface OpenOptions {
  expandJsonDepth: number;
  magicLen: number | null;
  format: Format | null;
  json: JsonOptions;
}

export interface SaveOptions {
  format: Format | null;
  json: JsonOptions;
  newlineTerminated: boolean;
}

export const prettyJson: JsonOptions = {
  indent: { kind: "spaces", width: 2 },
  flavor: "plain",
  expandStrings: true,
  maxDepth: null,
};

export const rawJson: JsonOptions = {
  indent: { kind: "spaces", width: 2 },
  flavor: "plain",
  expandStrings: false,
  maxDepth: null,
};

/** What the Rust side rejects a command with. */
export interface ApiError {
  kind: "noDocument" | "noNode" | "conflict" | "unsupported" | "io" | "state" | "core";
  message: string;
}

export function isApiError(error: unknown): error is ApiError {
  return typeof error === "object" && error !== null && "message" in error;
}

export function errorMessage(error: unknown): string {
  return isApiError(error) ? error.message : String(error);
}

export const coreVersion = () => invoke<string>("core_version");

export const openFile = (path: string, options?: OpenOptions) =>
  invoke<TabInfo>("open_file", { path, options });

export const loadText = (text: string, options?: OpenOptions) =>
  invoke<TabInfo>("load_text", { text, options });

/** Every open tab, including one loaded from the command line before mount. */
export const listTabs = () => invoke<TabInfo[]>("list_tabs");

export const closeTab = (tab: number) => invoke<void>("close_tab", { tab });

export const children = (tab: number, node: number, offset: number, limit: number) =>
  invoke<ChildrenPage>("children", { tab, node, offset, limit });

export const search = (tab: number, query: string, limit: number) =>
  invoke<SearchResults>("search", { tab, query, limit });

export const nodeValue = (tab: number, node: number) => invoke<Value>("node_value", { tab, node });

export const copySubtree = (tab: number, node: number, options?: JsonOptions) =>
  invoke<string>("copy_subtree", { tab, node, options });

export const setNodeValue = (tab: number, node: number, value: Value) =>
  invoke<EditResult>("set_node_value", { tab, node, value });

export const renameNode = (tab: number, node: number, key: string) =>
  invoke<EditResult>("rename_node", { tab, node, key });

export const insertNode = (
  tab: number,
  parent: number,
  index: number,
  key: string | null,
  value: Value,
) => invoke<EditResult>("insert_node", { tab, parent, index, key, value });

export const pasteJson = (
  tab: number,
  parent: number,
  index: number,
  key: string | null,
  json: string,
) => invoke<EditResult>("paste_json", { tab, parent, index, key, json });

export const replaceNode = (tab: number, node: number, json: string) =>
  invoke<EditResult>("replace_node", { tab, node, json });

export const removeNode = (tab: number, node: number) =>
  invoke<EditResult>("remove_node", { tab, node });

export const reexpand = (tab: number) => invoke<DocInfo>("reexpand", { tab });

export const setExpandDepth = (tab: number, depth: number) =>
  invoke<DocInfo>("set_expand_depth", { tab, depth });

export const saveFile = (tab: number, path: string | null, options?: SaveOptions) =>
  invoke<SaveResult>("save_file", { tab, path, options });

export const exportText = (tab: number, options?: SaveOptions) =>
  invoke<string>("export_text", { tab, options });
