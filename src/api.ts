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
  preview: string;
  childCount: number;
  expandable: boolean;
  embeddedJson: boolean;
  overlayLevel: number;
  editable: boolean;
}

export interface ChildrenPage {
  parent: number;
  offset: number;
  total: number;
  rows: NodeRow[];
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
  invoke<DocInfo>("open_file", { path, options });

export const loadText = (text: string, options?: OpenOptions) =>
  invoke<DocInfo>("load_text", { text, options });

export const docInfo = () => invoke<DocInfo>("doc_info");

export const children = (node: number, offset: number, limit: number) =>
  invoke<ChildrenPage>("children", { node, offset, limit });

export const nodeValue = (node: number) => invoke<Value>("node_value", { node });

export const copySubtree = (node: number, options?: JsonOptions) =>
  invoke<string>("copy_subtree", { node, options });

export const setNodeValue = (node: number, value: Value) =>
  invoke<EditResult>("set_node_value", { node, value });

export const renameNode = (node: number, key: string) =>
  invoke<EditResult>("rename_node", { node, key });

export const insertNode = (parent: number, index: number, key: string | null, value: Value) =>
  invoke<EditResult>("insert_node", { parent, index, key, value });

export const pasteJson = (parent: number, index: number, key: string | null, json: string) =>
  invoke<EditResult>("paste_json", { parent, index, key, json });

export const replaceNode = (node: number, json: string) =>
  invoke<EditResult>("replace_node", { node, json });

export const removeNode = (node: number) => invoke<EditResult>("remove_node", { node });

export const reexpand = () => invoke<DocInfo>("reexpand");

export const setExpandDepth = (depth: number) => invoke<DocInfo>("set_expand_depth", { depth });

export const saveFile = (path: string | null, options?: SaveOptions) =>
  invoke<SaveResult>("save_file", { path, options });

export const exportText = (options?: SaveOptions) => invoke<string>("export_text", { options });
