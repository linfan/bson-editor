/** Toolbar, status bar and the empty state — the chrome around the tree. */
import type { DocInfo } from "../api";
import { bytes, count, duration, fileName } from "../format";

/** Deepest embedded-JSON chain the stepper offers. Deeper chains exist but each
 *  level roughly doubles the escaping, so past this the text is unreadable. */
const MAX_DEPTH = 12;

export function Toolbar({
  info,
  onOpen,
  onPaste,
  onSave,
  onSaveAs,
  onCopyAll,
  onReexpand,
  onDepth,
}: {
  info: DocInfo | null;
  onOpen: () => void;
  onPaste: () => void;
  onSave: () => void;
  onSaveAs: () => void;
  onCopyAll: () => void;
  onReexpand: () => void;
  onDepth: (depth: number) => void;
}) {
  const depth = info?.expandDepth ?? 0;
  // Changing the depth rebuilds the document from its original bytes, which
  // would throw away unsaved edits, so the control is disabled while dirty.
  const locked = info?.dirty ?? false;
  const lockReason = locked ? "Save or discard your edits first" : undefined;

  return (
    <div className="toolbar">
      <div className="group">
        <button onClick={onOpen}>Open…</button>
        <button onClick={onPaste}>Paste…</button>
      </div>
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
