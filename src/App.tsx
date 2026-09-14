/**
 * Application shell: owns the open document, the tree, and the dialogs.
 *
 * The tree is the only place node ids live. Every edit refreshes just the
 * container it touched rather than reloading the document, because node ids are
 * stable across an edit. The exception is anything that rebuilds the document
 * from its original bytes — opening a file, or changing the expand depth — which
 * bumps `generation` so the cached pages are dropped.
 */
import { useCallback, useEffect, useState } from "react";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import * as api from "./api";
import type { DocInfo, JsonOptions, NodeRow, SaveOptions, Value } from "./api";
import { EmptyState, StatusBar, Toolbar } from "./components/Chrome";
import { ConfirmDialog, NodeDialog, PasteDialog } from "./components/Dialogs";
import type { NodeSubmission } from "./components/Dialogs";
import { TreeView } from "./components/TreeView";
import { bytes } from "./format";
import { useTree } from "./useTree";

/** Formatting for the clipboard: what the user sees, so embedded JSON strings
 *  come out as the structure they were unfolded into. */
const copyJson: JsonOptions = {
  indent: { kind: "spaces", width: 2 },
  flavor: "plain",
  expandStrings: true,
  maxDepth: null,
};

/** Formatting for a file on disk. Embedded strings stay strings: expanding them
 *  here would rewrite the document into something that no longer parses back to
 *  the same bytes. */
const fileJson: JsonOptions = { ...copyJson, expandStrings: false };

type Dialog =
  | { kind: "paste" }
  | { kind: "edit"; row: NodeRow; parentId: number; initial: Value | null; initialJson: string }
  | { kind: "insert"; row: NodeRow; parentId: number }
  | { kind: "delete"; row: NodeRow; parentId: number }
  | null;

export default function App() {
  const [info, setInfo] = useState<DocInfo | null>(null);
  const [generation, setGeneration] = useState(0);
  const [depth, setDepth] = useState(3);
  const [selected, setSelected] = useState<number | null>(null);
  const [dialog, setDialog] = useState<Dialog>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [version, setVersion] = useState<string | null>(null);

  const tree = useTree(info?.root ?? null, generation);

  // A confirmation fades by itself; an error waits to be dismissed, because
  // reading it is the whole point.
  useEffect(() => {
    if (!notice) return;
    const timer = setTimeout(() => setNotice(null), 2400);
    return () => clearTimeout(timer);
  }, [notice]);

  const fail = useCallback((cause: unknown) => setFailure(api.errorMessage(cause)), []);

  const openOptions = useCallback(
    (level: number): api.OpenOptions => ({
      expandJsonDepth: level,
      magicLen: null,
      format: null,
      json: fileJson,
    }),
    [],
  );

  /** Take ownership of a freshly parsed document and drop every cached page. */
  const adopt = useCallback((next: DocInfo) => {
    setInfo(next);
    setDepth(next.expandDepth);
    setSelected(null);
    setGeneration((current) => current + 1);
  }, []);

  // `bson-editor <path>` loads a document during setup, before this webview
  // exists, so ask whether one is already there. Rejecting is the ordinary
  // "nothing open" case and is ignored on purpose.
  useEffect(() => {
    api.docInfo().then(adopt).catch(() => undefined);
    api.coreVersion().then(setVersion).catch(() => setVersion(null));
  }, [adopt]);

  const toClipboard = useCallback(
    async (text: string, label: string) => {
      try {
        await navigator.clipboard.writeText(text);
        setNotice(`${label} · ${bytes(text.length)} copied`);
      } catch {
        setFailure("The clipboard is not available");
      }
    },
    [],
  );

  const onOpen = useCallback(async () => {
    try {
      const picked = await openFileDialog({
        multiple: false,
        filters: [{ name: "Documents", extensions: ["json", "bson", "ndjson", "txt"] }],
      });
      const path = typeof picked === "string" ? picked : null;
      if (!path) return;
      adopt(await api.openFile(path, openOptions(depth)));
    } catch (cause) {
      fail(cause);
    }
  }, [adopt, depth, fail, openOptions]);

  const onLoadText = useCallback(
    async (text: string) => {
      try {
        adopt(await api.loadText(text, openOptions(depth)));
        setDialog(null);
      } catch (cause) {
        fail(cause);
      }
    },
    [adopt, depth, fail, openOptions],
  );

  const writeOut = useCallback(
    async (path: string | null) => {
      const options: SaveOptions = { format: null, json: fileJson, newlineTerminated: true };
      const result = await api.saveFile(path, options);
      setInfo((prev) => (prev ? { ...prev, dirty: false, path: result.path } : prev));
      setNotice(`Saved ${bytes(result.bytesWritten)} to ${result.path}`);
      if (result.warnings.length > 0) {
        setFailure(result.warnings.join("\n"));
      }
    },
    [],
  );

  const onSaveAs = useCallback(async () => {
    if (!info) return;
    try {
      const extension = info.format === "bson" ? "bson" : "json";
      const picked = await saveFileDialog({
        defaultPath: info.path ?? `document.${extension}`,
        filters: [{ name: extension.toUpperCase(), extensions: [extension] }],
      });
      const path = typeof picked === "string" ? picked : null;
      if (path) await writeOut(path);
    } catch (cause) {
      fail(cause);
    }
  }, [fail, info, writeOut]);

  const onSave = useCallback(async () => {
    if (!info) return;
    if (!info.path) {
      await onSaveAs();
      return;
    }
    try {
      await writeOut(null);
    } catch (cause) {
      fail(cause);
    }
  }, [fail, info, onSaveAs, writeOut]);

  const onCopyAll = useCallback(async () => {
    try {
      const options: SaveOptions = { format: "json", json: copyJson, newlineTerminated: false };
      await toClipboard(await api.exportText(options), "Document");
    } catch (cause) {
      fail(cause);
    }
  }, [fail, toClipboard]);

  const onDepth = useCallback(
    async (next: number) => {
      setDepth(next);
      if (!info) return;
      try {
        adopt(await api.setExpandDepth(next));
      } catch (cause) {
        fail(cause);
      }
    },
    [adopt, fail, info],
  );

  const onReexpand = useCallback(async () => {
    try {
      // Ids survive a re-scan, but rows do not: a string that just gained an
      // overlay becomes expandable, so the cached pages have to go.
      adopt(await api.reexpand());
    } catch (cause) {
      fail(cause);
    }
  }, [adopt, fail]);

  const onCopyRow = useCallback(
    async (row: NodeRow) => {
      try {
        const text = await api.copySubtree(row.id, copyJson);
        await toClipboard(text, row.key ?? `[${row.index}]`);
      } catch (cause) {
        fail(cause);
      }
    },
    [fail, toClipboard],
  );

  const onEditRow = useCallback(
    async (row: NodeRow, parentId: number) => {
      try {
        const [initial, initialJson] = await Promise.all([
          api.nodeValue(row.id),
          api.copySubtree(row.id, copyJson),
        ]);
        setDialog({ kind: "edit", row, parentId, initial, initialJson });
      } catch (cause) {
        fail(cause);
      }
    },
    [fail],
  );

  const onSubmitNode = useCallback(
    async (submission: NodeSubmission) => {
      if (dialog?.kind !== "edit" && dialog?.kind !== "insert") return;
      const { row, parentId } = dialog;
      const editing = dialog.kind === "edit";
      try {
        let result: api.EditResult;
        if (editing) {
          result =
            submission.kind === "value"
              ? await api.setNodeValue(row.id, submission.value)
              : await api.replaceNode(row.id, submission.json);
        } else {
          const index = row.index + 1;
          result =
            submission.kind === "value"
              ? await api.insertNode(parentId, index, submission.key, submission.value)
              : await api.pasteJson(parentId, index, submission.key, submission.json);
        }
        setInfo(result.info);
        setSelected(result.node);
        // A pasted structure lands under a new id, so rename that one.
        if (submission.key !== null && submission.key !== row.key) {
          const renamed = await api.renameNode(result.node, submission.key);
          setInfo(renamed.info);
        }
        await tree.refresh(parentId);
        setDialog(null);
      } catch (cause) {
        fail(cause);
      }
    },
    [dialog, fail, tree],
  );

  const onConfirmDelete = useCallback(async () => {
    if (dialog?.kind !== "delete") return;
    const { parentId } = dialog;
    setDialog(null);
    try {
      const result = await api.removeNode(dialog.row.id);
      setInfo(result.info);
      setSelected(null);
      await tree.refresh(parentId);
    } catch (cause) {
      fail(cause);
    }
  }, [dialog, fail, tree]);

  // Cmd/Ctrl+O and Cmd/Ctrl+S, which a desktop app is expected to honour.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return;
      const key = event.key.toLowerCase();
      if (key === "o") {
        event.preventDefault();
        void onOpen();
      } else if (key === "s") {
        event.preventDefault();
        void onSave();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onOpen, onSave]);

  const actions = {
    onCopy: (row: NodeRow) => void onCopyRow(row),
    onEdit: (row: NodeRow, parentId: number) => void onEditRow(row, parentId),
    onInsert: (row: NodeRow, parentId: number) => setDialog({ kind: "insert", row, parentId }),
    onDelete: (row: NodeRow, parentId: number) => setDialog({ kind: "delete", row, parentId }),
  };

  return (
    <main className="app">
      <Toolbar
        info={info}
        onOpen={() => void onOpen()}
        onPaste={() => setDialog({ kind: "paste" })}
        onSave={() => void onSave()}
        onSaveAs={() => void onSaveAs()}
        onCopyAll={() => void onCopyAll()}
        onReexpand={() => void onReexpand()}
        onDepth={(next) => void onDepth(next)}
      />

      {info ? (
        <TreeView tree={tree} actions={actions} selected={selected} onSelect={setSelected} />
      ) : (
        <EmptyState onOpen={() => void onOpen()} onPaste={() => setDialog({ kind: "paste" })} />
      )}

      <StatusBar info={info} version={version} />

      {(notice || failure || tree.error) && (
        <div className="toast-wrap">
          {notice && <div className="toast">{notice}</div>}
          {failure && (
            <div className="toast error">
              <span>{failure}</span>
              <button className="quiet" onClick={() => setFailure(null)}>
                Dismiss
              </button>
            </div>
          )}
          {tree.error && (
            <div className="toast error">
              <span>{tree.error}</span>
              <button className="quiet" onClick={tree.clearError}>
                Dismiss
              </button>
            </div>
          )}
        </div>
      )}

      {dialog?.kind === "paste" && (
        <PasteDialog onClose={() => setDialog(null)} onSubmit={(text) => void onLoadText(text)} />
      )}
      {(dialog?.kind === "edit" || dialog?.kind === "insert") && (
        <NodeDialog
          mode={dialog.kind}
          row={dialog.row}
          named={dialog.row.key !== null}
          initial={dialog.kind === "edit" ? dialog.initial : null}
          initialJson={dialog.kind === "edit" ? dialog.initialJson : ""}
          onCancel={() => setDialog(null)}
          onSubmit={(submission) => void onSubmitNode(submission)}
        />
      )}
      {dialog?.kind === "delete" && (
        <ConfirmDialog
          title="Delete this node?"
          body={`${label(dialog.row)} and everything below it will be removed. Saving writes the change to disk.`}
          confirmLabel="Delete"
          danger
          onClose={() => setDialog(null)}
          onConfirm={() => void onConfirmDelete()}
        />
      )}
    </main>
  );
}

function label(row: NodeRow): string {
  return row.key ?? `[${row.index}]`;
}
