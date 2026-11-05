/** Tab strip, toolbar, status bar and the empty state — the chrome around the tree. */
import { useEffect, useRef, useState } from "react";
import type { DocInfo } from "../api";
import { bytes, count, duration, fileName } from "../format";

/** One document per tab. The strip scrolls horizontally rather than wrapping, so
 *  the rows below keep their full height. Workspace-level actions live here too:
 *  the `+` menu adds a tab, and Compare pairs two of them. */
export function TabBar({
  tabs,
  activeId,
  onActivate,
  onClose,
  onOpen,
  onPaste,
  canCompare,
  onCompare,
}: {
  tabs: { id: number; info: DocInfo }[];
  activeId: number | null;
  onActivate: (id: number) => void;
  onClose: (id: number) => void;
  onOpen: () => void;
  onPaste: () => void;
  canCompare: boolean;
  onCompare: () => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menu = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (event: PointerEvent) => {
      if (!menu.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    window.addEventListener("pointerdown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  return (
    <div className="tabbar">
      <div className="tablist">
        {tabs.map((tab) => (
          <div
            key={tab.id}
            className={`tab${tab.id === activeId ? " on" : ""}`}
            onClick={() => onActivate(tab.id)}
            title={tab.info.path ?? "Pasted content"}
          >
            <span className={`dot${tab.info.dirty ? " dirty" : ""}`} />
            <span className="tab-name">{tab.info.path ? fileName(tab.info.path) : "pasted"}</span>
            <span className={`badge b-${tab.info.format}`}>{tab.info.format}</span>
            <button
              className="tab-close"
              title="Close tab"
              onClick={(event) => {
                event.stopPropagation();
                onClose(tab.id);
              }}
            >
              ×
            </button>
          </div>
        ))}
      </div>
      <div className="tabbar-actions">
        {tabs.length > 0 && (
          <button onClick={onCompare} disabled={!canCompare} title="Diff two open tabs">
            Compare…
          </button>
        )}
        <div className="newtab" ref={menu}>
          <button
            className="tab-new"
            title="New tab"
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            onClick={() => setMenuOpen((open) => !open)}
          >
            +
          </button>
          {menuOpen && (
            <div className="menu" role="menu">
              <button
                role="menuitem"
                onClick={() => {
                  setMenuOpen(false);
                  onOpen();
                }}
              >
                Open file…
              </button>
              <button
                role="menuitem"
                onClick={() => {
                  setMenuOpen(false);
                  onPaste();
                }}
              >
                Paste JSON or BSON…
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/** Deepest embedded-JSON chain the stepper offers. Deeper chains exist but each
 *  level roughly doubles the escaping, so past this the text is unreadable. */
const MAX_DEPTH = 12;

export function Toolbar({
  info,
  onSave,
  onSaveAs,
  onCopyAll,
  onReexpand,
  onDepth,
  view,
  onView,
  onExpandAll,
  onCollapseAll,
}: {
  info: DocInfo | null;
  onSave: () => void;
  onSaveAs: () => void;
  onCopyAll: () => void;
  onReexpand: () => void;
  onDepth: (depth: number) => void;
  view: "tree" | "text";
  onView: (view: "tree" | "text") => void;
  onExpandAll: () => void;
  onCollapseAll: () => void;
}) {
  const depth = info?.expandDepth ?? 0;
  // Changing the depth rebuilds the document from its original bytes, which
  // would throw away unsaved edits, so the control is disabled while dirty.
  const locked = info?.dirty ?? false;
  const lockReason = locked ? "Save or discard your edits first" : undefined;

  return (
    <div className="toolbar">
      <div className="group">
        <button onClick={onSave} disabled={!info} title={lockReason}>
          Save
        </button>
        <button onClick={onSaveAs} disabled={!info}>
          Save as…
        </button>
        <button onClick={onCopyAll} disabled={!info}>
          Copy JSON
        </button>
      </div>

      <div className="group depth" title="How many levels of JSON hidden inside a string to unfold">
        <span className="label">Nested JSON</span>
        <button
          className="step"
          disabled={!info || locked || depth === 0}
          title={lockReason}
          onClick={() => onDepth(depth - 1)}
        >
          −
        </button>
        <span className="depth-value">{depth}</span>
        <button
          className="step"
          disabled={!info || locked || depth >= MAX_DEPTH}
          title={lockReason}
          onClick={() => onDepth(depth + 1)}
        >
          +
        </button>
        <button onClick={onReexpand} disabled={!info} title="Look for JSON strings written since load">
          Re-scan
        </button>
      </div>

      <div className="group tree">
        <button
          onClick={onExpandAll}
          disabled={!info || view !== "tree"}
          title="Expand every container"
        >
          Expand all
        </button>
        <button
          onClick={onCollapseAll}
          disabled={!info || view !== "tree"}
          title="Collapse every container"
        >
          Collapse all
        </button>
      </div>

      <div className="group view" title="How the document is presented">
        <span className="label">View</span>
        <div className="segmented" role="group" aria-label="View">
          <button
            className={view === "tree" ? "on" : ""}
            disabled={!info}
            aria-pressed={view === "tree"}
            onClick={() => onView("tree")}
          >
            Tree
          </button>
          <button
            className={view === "text" ? "on" : ""}
            disabled={!info}
            aria-pressed={view === "text"}
            onClick={() => onView("text")}
          >
            JSON
          </button>
        </div>
      </div>

      <div className="spacer" />

      {info && (
        <div className="document">
          <span className={`dot${info.dirty ? " dirty" : ""}`} title={info.dirty ? "Unsaved changes" : "Saved"} />
          <span className="name" title={info.path ?? undefined}>
            {fileName(info.path)}
          </span>
          <span className={`badge b-${info.format}`}>{info.format}</span>
          {info.stats.magicBytes > 0 && (
            <span className="badge b-magic" title={`Restored verbatim on save: ${info.magic}`}>
              magic {info.stats.magicBytes}B
            </span>
          )}
        </div>
      )}
    </div>
  );
}

/** The find bar. Enter and Shift+Enter step through matches; Escape closes. */
export function FindBar({
  query,
  onQuery,
  count,
  current,
  truncated,
  busy,
  focusToken,
  onNext,
  onPrev,
  onClose,
}: {
  query: string;
  onQuery: (query: string) => void;
  count: number;
  current: number;
  truncated: boolean;
  busy: boolean;
  /** Bumped by Ctrl/Cmd+F to pull focus back to the field. */
  focusToken: number;
  onNext: () => void;
  onPrev: () => void;
  onClose: () => void;
}) {
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, [focusToken]);

  const status =
    query.trim().length === 0
      ? ""
      : busy
        ? "searching…"
        : count === 0
          ? "no matches"
          : `${current + 1} / ${count}${truncated ? "+" : ""}`;

  return (
    <div className="findbar">
      <input
        ref={input}
        className="mono"
        placeholder="Find in document"
        spellCheck={false}
        value={query}
        onChange={(event) => onQuery(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            if (event.shiftKey) onPrev();
            else onNext();
          } else if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
      />
      <span className="find-status">{status}</span>
      <button onClick={onPrev} disabled={count === 0} title="Previous match (Shift+Enter)">
        ↑
      </button>
      <button onClick={onNext} disabled={count === 0} title="Next match (Enter)">
        ↓
      </button>
      <button className="quiet" onClick={onClose} title="Close (Escape)">
        ×
      </button>
    </div>
  );
}

export function StatusBar({
  info,
  version,
}: {
  info: DocInfo | null;
  version: string | null;
}) {
  const engine = version ? <span className="muted">doccore v{version}</span> : null;
  if (!info) {
    return (
      <div className="statusbar">
        <span className="muted">No document open</span>
        <div className="spacer" />
        {engine}
      </div>
    );
  }
  const { stats } = info;
  return (
    <div className="statusbar">
      <Stat label="nodes" value={count(stats.nodeCount)} />
      <Stat label="depth" value={String(stats.maxDepth)} />
      <Stat label="source" value={bytes(stats.sourceBytes)} />
      <Stat label="in memory" value={bytes(stats.heapBytes)} />
      {stats.documentCount > 1 && <Stat label="documents" value={count(stats.documentCount)} />}
      {stats.overlayNodes > 0 && <Stat label="json strings" value={count(stats.overlayNodes)} />}
      {stats.trailingBytes > 0 && <Stat label="trailing" value={bytes(stats.trailingBytes)} />}
      <Stat label="parsed in" value={duration(info.loadMs)} />
      <div className="spacer" />
      {info.warnings.length > 0 && (
        <span className="warn" title={info.warnings.join("\n")}>
          {info.warnings.length} warning{info.warnings.length === 1 ? "" : "s"}
        </span>
      )}
      {engine}
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <span className="stat">
      <b>{value}</b> {label}
    </span>
  );
}

export function EmptyState({ onOpen, onPaste }: { onOpen: () => void; onPaste: () => void }) {
  return (
    <div className="empty">
      <h1>JSON / BSON reader and editor</h1>
      <p>
        Opens JSON and BSON, including BSON behind a magic-byte prefix, which is stripped on load and
        written back unchanged on save. Strings that turn out to hold JSON unfold into real structure,
        nested as deep as you ask.
      </p>
      <div className="group">
        <button className="primary" onClick={onOpen}>
          Open a file…
        </button>
        <button onClick={onPaste}>Paste JSON or BSON…</button>
      </div>
      <p className="muted">
        Large files are fine: a 12MB document parses in tens of milliseconds and the tree only ever
        holds the rows on screen.
      </p>
    </div>
  );
}
