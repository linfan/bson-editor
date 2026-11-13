/**
 * The raw value of one node, in a large read-only window.
 *
 * A container (or a string that decoded into JSON) is shown as formatted JSON
 * with syntax highlighting. Any other string is shown as its decoded text, so
 * `\n` and `\t` in the stored value become real line breaks and tabs.
 */
import { useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import * as api from "../api";
import type { NodeRow } from "../api";
import { bytes } from "../format";
import { Modal } from "./Dialogs";

/** Above this the tokenizer would make the window crawl; the value is still
 *  shown, just without colour. */
const HIGHLIGHT_LIMIT = 500_000;

interface Content {
  mode: "json" | "text";
  text: string;
}

export function RawDialog({
  tab,
  row,
  onClose,
}: {
  tab: number;
  row: NodeRow;
  onClose: () => void;
}) {
  const [content, setContent] = useState<Content | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = async (): Promise<Content> => {
      // An embedded-JSON string presents as the structure it decoded to, which
      // is the JSON the user is looking at in the tree.
      if (row.presentedKind === "object" || row.presentedKind === "array") {
        return { mode: "json", text: await api.copySubtree(tab, row.id, api.prettyJson) };
      }
      if (row.kind === "string") {
        const value = await api.nodeValue(tab, row.id);
        if (value.type !== "str") throw new Error(`unexpected value type ${value.type}`);
        return { mode: "text", text: value.value };
      }
      return { mode: "json", text: await api.copySubtree(tab, row.id, api.prettyJson) };
    };
    load()
      .then((loaded) => {
        if (!cancelled) setContent(loaded);
      })
      .catch((cause) => {
        if (!cancelled) setError(api.errorMessage(cause));
      });
    return () => {
      cancelled = true;
    };
  }, [tab, row]);

  const name = row.key ?? `[${row.index}]`;
  const subtitle = content
    ? `${name} · ${row.kind} · ${bytes(content.text.length)}${content.mode === "text" ? " text" : ""}`
    : `${name} · ${row.kind}`;

  const tooLarge = content !== null && content.text.length > HIGHLIGHT_LIMIT;

  return (
    <Modal title="RAW JSON" subtitle={subtitle} onClose={onClose} raw closeLabel="Close">
      {error ? (
        <p className="raw-note error">{error}</p>
      ) : !content ? (
        <p className="raw-note">Reading…</p>
      ) : (
        <>
          {content.mode === "json" && tooLarge && (
            <p className="raw-hint">Syntax highlighting is off for this large value.</p>
          )}
          <pre className="raw-body">
            {content.mode === "text" || tooLarge ? (
              content.text
            ) : (
              <Highlighted text={content.text} />
            )}
          </pre>
        </>
      )}
    </Modal>
  );
}

const TOKEN =
  /("(?:[^"\\]|\\.)*")|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)|\b(true|false)\b|\b(null)\b|([{}\[\],:])/g;

function Highlighted({ text }: { text: string }) {
  const parts = useMemo(() => tokenize(text), [text]);
  return <>{parts}</>;
}

/** Wrap each JSON token in a colour span, leaving whitespace as plain text. */
function tokenize(text: string): ReactNode[] {
  const out: ReactNode[] = [];
  const re = new RegExp(TOKEN.source, "g");
  let last = 0;
  let match: RegExpExecArray | null;
  while ((match = re.exec(text)) !== null) {
    if (match.index > last) out.push(text.slice(last, match.index));
    const [, str, num, bool, nul] = match;
    let className = "hl-punct";
    if (str !== undefined) {
      // A string followed by a colon is a member name, not a value.
      className = /^[ \t]*:/.test(text.slice(re.lastIndex)) ? "hl-key" : "hl-string";
    } else if (num !== undefined) {
      className = "hl-number";
    } else if (bool !== undefined) {
      className = "hl-bool";
    } else if (nul !== undefined) {
      className = "hl-null";
    }
    out.push(
      <span key={match.index} className={className}>
        {match[0]}
      </span>,
    );
    last = re.lastIndex;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}
