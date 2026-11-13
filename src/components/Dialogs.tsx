/**
 * Modal dialogs: load pasted text, edit or insert a node, confirm a delete.
 *
 * Editing has two tabs on purpose. The typed form covers the ordinary case;
 * the JSON tab is the escape hatch for anything it cannot express, notably a
 * `uint64` past JavaScript's safe-integer range, which the Rust side parses
 * exactly from text while a JS number would silently lose precision.
 */
import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import type { NodeRow, Value } from "../api";

export function Modal({
  title,
  subtitle,
  onClose,
  children,
  wide,
  diff,
  raw,
  closeLabel,
}: {
  title: string;
  subtitle?: string;
  onClose: () => void;
  children: ReactNode;
  wide?: boolean;
  /** The comparison view needs the full window width, not a dialog width. */
  diff?: boolean;
  /** A large read-only viewer, taller than an ordinary dialog. */
  raw?: boolean;
  closeLabel?: string;
}) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        className={`modal${wide ? " wide" : ""}${diff ? " diff" : ""}${raw ? " raw" : ""}`}
        role="dialog"
        aria-modal="true"
      >
        <header>
          <div>
            <h2>{title}</h2>
            {subtitle && <p className="subtitle">{subtitle}</p>}
          </div>
          <button className="quiet" onClick={onClose}>
            {closeLabel ?? "Cancel"}
          </button>
        </header>
        {children}
      </div>
    </div>
  );
}

/** Ask for JSON or hex/base64-encoded BSON to open as a document. */
export function PasteDialog({
  onClose,
  onSubmit,
}: {
  onClose: () => void;
  onSubmit: (text: string) => void;
}) {
  const [text, setText] = useState("");
  const trimmed = text.trim();

  return (
    <Modal
      title="Open pasted content"
      subtitle="JSON text, or BSON as hex or base64"
      onClose={onClose}
      wide
    >
      <textarea
        className="paste"
        autoFocus
        spellCheck={false}
        value={text}
        placeholder={'{"hello": "world"}'}
        onChange={(event) => setText(event.target.value)}
      />
      <footer>
        <button className="primary" disabled={trimmed.length === 0} onClick={() => onSubmit(trimmed)}>
          Open
        </button>
      </footer>
    </Modal>
  );
}

export function ConfirmDialog({
  title,
  body,
  confirmLabel,
  danger,
  onClose,
  onConfirm,
}: {
  title: string;
  body: string;
  confirmLabel: string;
  danger?: boolean;
  onClose: () => void;
  onConfirm: () => void;
}) {
  return (
    <Modal title={title} onClose={onClose}>
      <p className="dialog-body">{body}</p>
      <footer>
        <button className={danger ? "danger" : "primary"} autoFocus onClick={onConfirm}>
          {confirmLabel}
        </button>
      </footer>
    </Modal>
  );
}

/** What the node dialog hands back to the caller. */
export type NodeSubmission =
  | { kind: "value"; key: string | null; value: Value }
  | { kind: "json"; key: string | null; json: string };

type ValueKind = "null" | "bool" | "int" | "uint" | "double" | "str" | "object" | "array";

const KINDS: ValueKind[] = ["str", "int", "double", "bool", "null", "object", "array", "uint"];

/** Types the form can fill in without any input at all. */
const INPUTLESS: ValueKind[] = ["null", "object", "array"];

function kindOf(value: Value): ValueKind {
  return value.type === "uint" ? "uint" : (value.type as ValueKind);
}

function textOf(value: Value): string {
  return "value" in value ? String(value.value) : "";
}

/** Turn the form's text into a wire value, or explain why it cannot. */
function toValue(kind: ValueKind, text: string): { value?: Value; error?: string } {
  switch (kind) {
    case "null":
      return { value: { type: "null" } };
    case "object":
      return { value: { type: "object" } };
    case "array":
      return { value: { type: "array" } };
    case "bool":
      return { value: { type: "bool", value: text === "true" } };
    case "str":
      return { value: { type: "str", value: text } };
    case "int":
    case "uint": {
      const trimmed = text.trim();
      if (!/^-?\d+$/.test(trimmed)) return { error: "expected a whole number" };
      const parsed = Number(trimmed);
      if (!Number.isSafeInteger(parsed)) {
        return { error: "beyond JavaScript's safe integer range — use the JSON tab" };
      }
      if (kind === "uint" && parsed < 0) return { error: "expected a non-negative integer" };
      return { value: { type: kind, value: parsed } };
    }
    case "double": {
      const trimmed = text.trim();
      if (trimmed.length === 0) return { error: "expected a number" };
      const parsed = Number(trimmed);
      if (Number.isNaN(parsed)) return { error: "not a number" };
      return { value: { type: "double", value: parsed } };
    }
  }
}

export function NodeDialog({
  mode,
  row,
  named,
  initial,
  initialJson,
  onCancel,
  onSubmit,
}: {
  mode: "edit" | "insert";
  /** The row being edited, or the row a new sibling goes after. */
  row: NodeRow | null;
  /** True when the parent is an object, so the member needs a name. */
  named: boolean;
  initial: Value | null;
  initialJson: string;
  onCancel: () => void;
  onSubmit: (submission: NodeSubmission) => void;
}) {
  const [tab, setTab] = useState<"value" | "json">("value");
  const [key, setKey] = useState(row?.key ?? "");
  const [kind, setKind] = useState<ValueKind>(initial ? kindOf(initial) : "str");
  const [text, setText] = useState(initial ? textOf(initial) : "");
  const [json, setJson] = useState(initialJson);
  const [error, setError] = useState<string | null>(null);

  const title = mode === "edit" ? "Edit value" : "Insert after";
  const needsInput = !INPUTLESS.includes(kind);
  const keyField = named ? key : null;

  const submit = () => {
    if (tab === "json") {
      if (json.trim().length === 0) {
        setError("nothing to parse");
        return;
      }
      onSubmit({ kind: "json", key: keyField, json });
      return;
    }
    if (named && key.length === 0) {
      setError("an object member needs a name");
      return;
    }
    const result = toValue(kind, text);
    if (result.error) {
      setError(result.error);
      return;
    }
    onSubmit({ kind: "value", key: keyField, value: result.value! });
  };

  return (
    <Modal
      title={title}
      subtitle={row ? `${row.key ?? `[${row.index}]`} · ${row.kind}` : undefined}
      onClose={onCancel}
      wide={tab === "json"}
    >
      <div className="tabs">
        <button className={tab === "value" ? "on" : ""} onClick={() => { setTab("value"); setError(null); }}>
          Typed value
        </button>
        <button className={tab === "json" ? "on" : ""} onClick={() => { setTab("json"); setError(null); }}>
          Raw JSON
        </button>
      </div>

      {tab === "value" ? (
        <div className="fields">
          {named && (
            <label>
              <span>Name</span>
              <input value={key} onChange={(event) => setKey(event.target.value)} autoFocus />
            </label>
          )}
          <label>
            <span>Type</span>
            <select value={kind} onChange={(event) => setKind(event.target.value as ValueKind)}>
              {KINDS.map((option) => (
                <option key={option} value={option}>
                  {option}
                </option>
              ))}
            </select>
          </label>
          {kind === "bool" ? (
            <label>
              <span>Value</span>
              <select value={text === "false" ? "false" : "true"} onChange={(event) => setText(event.target.value)}>
                <option value="true">true</option>
                <option value="false">false</option>
              </select>
            </label>
          ) : (
            needsInput && (
              <label>
                <span>Value</span>
                <input
                  className="mono"
                  value={text}
                  spellCheck={false}
                  autoFocus={!named}
                  onChange={(event) => setText(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") submit();
                  }}
                />
              </label>
            )
          )}
        </div>
      ) : (
        <textarea
          className="paste"
          autoFocus
          spellCheck={false}
          value={json}
          placeholder={'{"or": ["paste", "any", "json"]}' }
          onChange={(event) => setJson(event.target.value)}
        />
      )}

      {error && <p className="error">{error}</p>}
      <footer>
        <button className="primary" onClick={submit}>
          {mode === "edit" ? "Apply" : "Insert"}
        </button>
      </footer>
    </Modal>
  );
}
