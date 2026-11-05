/**
 * Application shell: owns the open tabs, the tree of each tab, and the dialogs.
 *
 * Every tab is a backend session addressed by id, so several documents can stay
 * open at once without sharing node ids. A tab keeps its own tree, selection and
 * expand depth; only the active one is visible, but the others stay mounted so
 * their scroll position and expansion survive a switch.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import * as api from "./api";
import type { DocInfo, JsonOptions, NodeRow, SaveOptions, TabInfo, Value } from "./api";
import { EmptyState, StatusBar, TabBar, Toolbar } from "./components/Chrome";
import { ConfirmDialog, NodeDialog, PasteDialog } from "./components/Dialogs";
import type { NodeSubmission } from "./components/Dialogs";
import { TreeView } from "./components/TreeView";
import { bytes } from "./format";
import { useTree } from "./useTree";
import type { Tree } from "./useTree";

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

/** Depth a newly opened document starts at. */
const DEFAULT_DEPTH = 3;

interface Tab {
  id: number;
  info: DocInfo;
  /** Bumped when the document is rebuilt and cached pages must be dropped. */
  generation: number;
  /** Last depth the user asked for; the document reports its own after a load. */
  depth: number;
  selected: number | null;
}

type Dialog =
  | { kind: "paste" }
  | {
      kind: "edit";
      tab: number;
      row: NodeRow;
      parentId: number;
      initial: Value | null;
      initialJson: string;
    }
  | { kind: "insert"; tab: number; row: NodeRow; parentId: number }
  | { kind: "delete"; tab: number; row: NodeRow; parentId: number }
  | { kind: "close"; tab: number }
  | null;

export default function App() {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeId, setActiveId] = useState<number | null>(null);
  const [dialog, setDialog] = useState<Dialog>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [version, setVersion] = useState<string | null>(null);

  // The tree hook lives inside each tab panel, so its object is parked here for
  // the event handlers that need to refresh the active tab.
  const trees = useRef(new Map<number, Tree>());

  const active = tabs.find((tab) => tab.id === activeId) ?? null;

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

  const patchInfo = useCallback((id: number, info: DocInfo) => {
    setTabs((prev) =>
      prev.map((tab) => (tab.id === id ? { ...tab, info, depth: info.expandDepth } : tab)),
    );
  }, []);

  /** Adopt a document that was rebuilt from its original bytes. */
  const adopt = useCallback((id: number, info: DocInfo) => {
    setTabs((prev) =>
      prev.map((tab) =>
        tab.id === id
          ? {
              ...tab,
              info,
              depth: info.expandDepth,
              selected: null,
              generation: tab.generation + 1,
            }
          : tab,
      ),
    );
  }, []);

  const addTab = useCallback((opened: TabInfo) => {
    setTabs((prev) => [
      ...prev,
      {
        id: opened.tab,
        info: opened.info,
        generation: 0,
        depth: opened.info.expandDepth,
        selected: null,
      },
    ]);
    setActiveId(opened.tab);
  }, []);

  // `bson-editor <path>` loads a document during setup, before this webview
  // exists, so ask which tabs are already open. An empty list is the ordinary
  // "nothing open" case.
  useEffect(() => {
    api
      .listTabs()
      .then((open) => {
        if (open.length === 0) return;
        setTabs(
          open.map((item) => ({
            id: item.tab,
            info: item.info,
            generation: 0,
            depth: item.info.expandDepth,
            selected: null,
          })),
        );
        setActiveId(open[0].tab);
      })
      .catch(() => undefined);
    api.coreVersion().then(setVersion).catch(() => setVersion(null));
  }, []);

  const toClipboard = useCallback(async (text: string, label: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setNotice(`${label} · ${bytes(text.length)} copied`);
    } catch {
      setFailure("The clipboard is not available");
    }
  }, []);

  const onOpen = useCallback(async () => {
    try {
      const picked = await openFileDialog({
        multiple: false,
        filters: [{ name: "Documents", extensions: ["json", "bson", "ndjson", "txt"] }],
      });
      const path = typeof picked === "string" ? picked : null;
      if (!path) return;
      addTab(await api.openFile(path, openOptions(DEFAULT_DEPTH)));
    } catch (cause) {
      fail(cause);
    }
  }, [addTab, fail, openOptions]);

  const onLoadText = useCallback(
    async (text: string) => {
      try {
        addTab(await api.loadText(text, openOptions(DEFAULT_DEPTH)));
        setDialog(null);
      } catch (cause) {
        fail(cause);
      }
    },
    [addTab, fail, openOptions],
  );

  const writeOut = useCallback(async (tab: Tab, path: string | null) => {
    const options: SaveOptions = { format: null, json: fileJson, newlineTerminated: true };
    const result = await api.saveFile(tab.id, path, options);
    setTabs((prev) =>
      prev.map((item) =>
        item.id === tab.id
          ? { ...item, info: { ...item.info, dirty: false, path: result.path } }
          : item,
      ),
    );
    setNotice(`Saved ${bytes(result.bytesWritten)} to ${result.path}`);
    if (result.warnings.length > 0) {
      setFailure(result.warnings.join("\n"));
    }
  }, []);

  const onSaveAs = useCallback(
    async (tab: Tab) => {
      try {
        const extension = tab.info.format === "bson" ? "bson" : "json";
        const picked = await saveFileDialog({
          defaultPath: tab.info.path ?? `document.${extension}`,
          filters: [{ name: extension.toUpperCase(), extensions: [extension] }],
        });
        const path = typeof picked === "string" ? picked : null;
        if (path) await writeOut(tab, path);
      } catch (cause) {
        fail(cause);
      }
    },
    [fail, writeOut],
  );

  const onSave = useCallback(
    async (tab: Tab) => {
      if (!tab.info.path) {
        await onSaveAs(tab);
        return;
      }
      try {
        await writeOut(tab, null);
      } catch (cause) {
        fail(cause);
      }
    },
    [fail, onSaveAs, writeOut],
  );

  const onCopyAll = useCallback(
    async (tab: Tab) => {
      try {
        const options: SaveOptions = { format: "json", json: copyJson, newlineTerminated: false };
        await toClipboard(await api.exportText(tab.id, options), "Document");
      } catch (cause) {
        fail(cause);
      }
    },
    [fail, toClipboard],
  );

  const onDepth = useCallback(
    async (tab: Tab, next: number) => {
      setTabs((prev) =>
        prev.map((item) => (item.id === tab.id ? { ...item, depth: next } : item)),
      );
      try {
        adopt(tab.id, await api.setExpandDepth(tab.id, next));
      } catch (cause) {
        fail(cause);
      }
    },
    [adopt, fail],
  );

  const onReexpand = useCallback(
    async (tab: Tab) => {
      try {
        // Ids survive a re-scan, but rows do not: a string that just gained an
        // overlay becomes expandable, so the cached pages have to go.
        adopt(tab.id, await api.reexpand(tab.id));
      } catch (cause) {
        fail(cause);
      }
    },
    [adopt, fail],
  );

  const onCopyRow = useCallback(
    async (tabId: number, row: NodeRow) => {
      try {
        const text = await api.copySubtree(tabId, row.id, copyJson);
        await toClipboard(text, row.key ?? `[${row.index}]`);
      } catch (cause) {
        fail(cause);
      }
    },
    [fail, toClipboard],
  );

  const onEditRow = useCallback(
    async (tabId: number, row: NodeRow, parentId: number) => {
      try {
        const [initial, initialJson] = await Promise.all([
          api.nodeValue(tabId, row.id),
          api.copySubtree(tabId, row.id, copyJson),
        ]);
        setDialog({ kind: "edit", tab: tabId, row, parentId, initial, initialJson });
      } catch (cause) {
        fail(cause);
      }
    },
    [fail],
  );

  const onSubmitNode = useCallback(
    async (submission: NodeSubmission) => {
      if (dialog?.kind !== "edit" && dialog?.kind !== "insert") return;
      const { tab: tabId, row, parentId } = dialog;
      const editing = dialog.kind === "edit";
      try {
        let result: api.EditResult;
        if (editing) {
          result =
            submission.kind === "value"
              ? await api.setNodeValue(tabId, row.id, submission.value)
              : await api.replaceNode(tabId, row.id, submission.json);
        } else {
          const index = row.index + 1;
          result =
            submission.kind === "value"
              ? await api.insertNode(tabId, parentId, index, submission.key, submission.value)
              : await api.pasteJson(tabId, parentId, index, submission.key, submission.json);
        }
        patchInfo(tabId, result.info);
        setTabs((prev) =>
          prev.map((item) => (item.id === tabId ? { ...item, selected: result.node } : item)),
        );
        // A pasted structure lands under a new id, so rename that one.
        if (submission.key !== null && submission.key !== row.key) {
          const renamed = await api.renameNode(tabId, result.node, submission.key);
          patchInfo(tabId, renamed.info);
        }
        await trees.current.get(tabId)?.refresh(parentId);
        setDialog(null);
      } catch (cause) {
        fail(cause);
      }
    },
    [dialog, fail, patchInfo],
  );

  const onConfirmDelete = useCallback(async () => {
    if (dialog?.kind !== "delete") return;
    const { tab: tabId, row, parentId } = dialog;
    setDialog(null);
    try {
      const result = await api.removeNode(tabId, row.id);
      patchInfo(tabId, result.info);
      setTabs((prev) =>
        prev.map((item) => (item.id === tabId ? { ...item, selected: null } : item)),
      );
      await trees.current.get(tabId)?.refresh(parentId);
    } catch (cause) {
      fail(cause);
    }
  }, [dialog, fail, patchInfo]);

  const doClose = useCallback(
    async (id: number) => {
      try {
        await api.closeTab(id);
      } catch {
        // The backend may already have dropped it; the UI state is what matters.
      }
      trees.current.delete(id);
      const index = tabs.findIndex((tab) => tab.id === id);
      const next = tabs.filter((tab) => tab.id !== id);
      setTabs(next);
      setActiveId((current) =>
        current === id ? (next[Math.min(index, next.length - 1)]?.id ?? null) : current,
      );
    },
    [tabs],
  );

  const onCloseTab = useCallback(
    (id: number) => {
      const tab = tabs.find((item) => item.id === id);
      if (!tab) return;
      if (tab.info.dirty) {
        setDialog({ kind: "close", tab: id });
        return;
      }
      void doClose(id);
    },
    [doClose, tabs],
  );

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
        const tab = tabs.find((item) => item.id === activeId);
        if (tab) void onSave(tab);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [activeId, onOpen, onSave, tabs]);

  const actionsFor = (tabId: number) => ({
    onCopy: (row: NodeRow) => void onCopyRow(tabId, row),
    onEdit: (row: NodeRow, parentId: number) => void onEditRow(tabId, row, parentId),
    onInsert: (row: NodeRow, parentId: number) =>
      setDialog({ kind: "insert", tab: tabId, row, parentId }),
    onDelete: (row: NodeRow, parentId: number) =>
      setDialog({ kind: "delete", tab: tabId, row, parentId }),
  });

  return (
    <main className="app">
      <TabBar
        tabs={tabs}
        activeId={activeId}
        onActivate={setActiveId}
        onClose={onCloseTab}
        onNew={() => void onOpen()}
      />

      <Toolbar
        info={active?.info ?? null}
        onOpen={() => void onOpen()}
        onPaste={() => setDialog({ kind: "paste" })}
        onSave={() => active && void onSave(active)}
        onSaveAs={() => active && void onSaveAs(active)}
        onCopyAll={() => active && void onCopyAll(active)}
        onReexpand={() => active && void onReexpand(active)}
        onDepth={(next) => active && void onDepth(active, next)}
      />

      <div className="panes">
        {tabs.map((tab) => (
          <div key={tab.id} className={tab.id === activeId ? "pane" : "pane hidden"}>
            <TabPanel
              tab={tab}
              trees={trees}
              actions={actionsFor(tab.id)}
              onSelect={(selected) =>
                setTabs((prev) =>
                  prev.map((item) => (item.id === tab.id ? { ...item, selected } : item)),
                )
              }
            />
          </div>
        ))}
        {tabs.length === 0 && (
          <EmptyState onOpen={() => void onOpen()} onPaste={() => setDialog({ kind: "paste" })} />
        )}
      </div>

      <StatusBar info={active?.info ?? null} version={version} />

      {(notice || failure) && (
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
      {dialog?.kind === "close" && (
        <ConfirmDialog
          title="Close this tab?"
          body="It has unsaved changes. Closing discards them."
          confirmLabel="Discard and close"
          danger
          onClose={() => setDialog(null)}
          onConfirm={() => {
            const id = dialog.tab;
            setDialog(null);
            void doClose(id);
          }}
        />
      )}
    </main>
  );
}

/** One mounted tab. It owns the tree hook so a switch does not throw the paged
 *  rows and expansion state away. */
function TabPanel({
  tab,
  trees,
  actions,
  onSelect,
}: {
  tab: Tab;
  trees: { current: Map<number, Tree> };
  actions: {
    onCopy: (row: NodeRow, parentId: number) => void;
    onEdit: (row: NodeRow, parentId: number) => void;
    onInsert: (row: NodeRow, parentId: number) => void;
    onDelete: (row: NodeRow, parentId: number) => void;
  };
  onSelect: (id: number | null) => void;
}) {
  const tree = useTree(tab.id, tab.info.root, tab.generation);
  trees.current.set(tab.id, tree);

  return (
    <>
      <TreeView tree={tree} actions={actions} selected={tab.selected} onSelect={onSelect} />
      {tree.error && (
        <div className="toast-wrap">
          <div className="toast error">
            <span>{tree.error}</span>
            <button className="quiet" onClick={tree.clearError}>
              Dismiss
            </button>
          </div>
        </div>
      )}
    </>
  );
}

function label(row: NodeRow): string {
  return row.key ?? `[${row.index}]`;
}
