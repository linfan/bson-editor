/**
 * The raw value of one node, in a large read-only window.
 *
 * A container (or a string that decoded into JSON) is shown as formatted JSON
 * with syntax highlighting. Any other string is shown as its decoded text, so
 * `\n` and `\t` in the stored value become real line breaks and tabs.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import * as api from "../api";
import type { NodeRow } from "../api";
import { bytes } from "../format";
import { FindBar } from "./Chrome";
import { Modal } from "./Dialogs";

/** Above this the tokenizer would make the window crawl; the value is still
 *  shown, just without colour. */
const HIGHLIGHT_LIMIT = 500_000;
const MATCH_LIMIT = 500;

interface Content {
  mode: "json" | "text";
  text: string;
}

interface Match {
  start: number;
  end: number;
}

interface Results {
  matches: Match[];
  truncated: boolean;
}

interface Segment {
  start: number;
  end: number;
  className?: string;
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
  const [findOpen, setFindOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [focusToken, setFocusToken] = useState(0);
  const [results, setResults] = useState<Results>({ matches: [], truncated: false });
  const [current, setCurrent] = useState(0);
  const [searching, setSearching] = useState(false);
  const [revealToken, setRevealToken] = useState(0);
  const bodyRef = useRef<HTMLPreElement>(null);

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

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "f") {
        event.preventDefault();
        event.stopImmediatePropagation();
        setFindOpen(true);
        setFocusToken((value) => value + 1);
      } else if (event.key === "Escape" && findOpen) {
        event.preventDefault();
        event.stopImmediatePropagation();
        setFindOpen(false);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [findOpen]);

  useEffect(() => {
    if (!findOpen || !content || query.trim().length === 0) {
      setResults({ matches: [], truncated: false });
      setCurrent(0);
      setSearching(false);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const timer = setTimeout(() => {
      if (cancelled) return;
      setResults(findMatches(content.text, query.trim()));
      setCurrent(0);
      setSearching(false);
      setRevealToken((value) => value + 1);
    }, 180);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [content, findOpen, query]);

  useEffect(() => {
    if (!findOpen || results.matches.length === 0) return;
    bodyRef.current
      ?.querySelector(".raw-match.active")
      ?.scrollIntoView({ block: "center", inline: "nearest" });
  }, [current, findOpen, results.matches, revealToken]);

  const stepMatch = (delta: number) => {
    const count = results.matches.length;
    if (count === 0) return;
    setCurrent((value) => (value + delta + count) % count);
    setRevealToken((value) => value + 1);
  };

  const name = row.key ?? `[${row.index}]`;
  const subtitle = content
    ? `${name} · ${row.kind} · ${bytes(content.text.length)}${content.mode === "text" ? " text" : ""}`
    : `${name} · ${row.kind}`;

  const tooLarge = content !== null && content.text.length > HIGHLIGHT_LIMIT;
  const matches = findOpen ? results.matches : [];

  return (
    <Modal title="RAW JSON" subtitle={subtitle} onClose={onClose} raw closeLabel="Close">
      {findOpen && (
        <FindBar
          query={query}
          onQuery={setQuery}
          count={results.matches.length}
          current={current}
          truncated={results.truncated}
          busy={searching}
          focusToken={focusToken}
          onNext={() => stepMatch(1)}
          onPrev={() => stepMatch(-1)}
          onClose={() => setFindOpen(false)}
        />
      )}
      {error ? (
        <p className="raw-note error">{error}</p>
      ) : !content ? (
        <p className="raw-note">Reading…</p>
      ) : (
        <>
          {content.mode === "json" && tooLarge && (
            <p className="raw-hint">Syntax highlighting is off for this large value.</p>
          )}
          <pre ref={bodyRef} className="raw-body">
            {content.mode === "text" || tooLarge ? (
              renderSegments(
                content.text,
                [{ start: 0, end: content.text.length }],
                matches,
                current,
              )
            ) : (
              <Highlighted text={content.text} matches={matches} current={current} />
            )}
          </pre>
        </>
      )}
    </Modal>
  );
}

const TOKEN =
  /("(?:[^"\\]|\\.)*")|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)|\b(true|false)\b|\b(null)\b|([{}\[\],:])/g;

function Highlighted({
  text,
  matches,
  current,
}: {
  text: string;
  matches: Match[];
  current: number;
}) {
  const segments = useMemo(() => tokenize(text), [text]);
  return <>{renderSegments(text, segments, matches, current)}</>;
}

/** Wrap each JSON token in a colour span, leaving whitespace as plain text. */
function tokenize(text: string): Segment[] {
  const out: Segment[] = [];
  const re = new RegExp(TOKEN.source, "g");
  let last = 0;
  let match: RegExpExecArray | null;
  while ((match = re.exec(text)) !== null) {
    if (match.index > last) out.push({ start: last, end: match.index });
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
    out.push({ start: match.index, end: re.lastIndex, className });
    last = re.lastIndex;
  }
  if (last < text.length) out.push({ start: last, end: text.length });
  return out;
}

function findMatches(text: string, needle: string): Results {
  const matches: Match[] = [];
  const escaped = needle.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const re = new RegExp(escaped, "giu");
  let match: RegExpExecArray | null;
  while ((match = re.exec(text)) !== null) {
    if (matches.length === MATCH_LIMIT) return { matches, truncated: true };
    matches.push({ start: match.index, end: match.index + match[0].length });
  }
  return { matches, truncated: false };
}

function renderSegments(
  text: string,
  segments: Segment[],
  matches: Match[],
  current: number,
): ReactNode[] {
  const out: ReactNode[] = [];
  const emit = (start: number, end: number, className?: string, matchIndex?: number) => {
    if (end <= start) return;
    const value = text.slice(start, end);
    const key = `${start}:${end}`;
    if (matchIndex !== undefined) {
      out.push(
        <mark key={key} className={`raw-match${matchIndex === current ? " active" : ""}`}>
          {className ? <span className={className}>{value}</span> : value}
        </mark>,
      );
    } else if (className) {
      out.push(
        <span key={key} className={className}>
          {value}
        </span>,
      );
    } else {
      out.push(value);
    }
  };

  let firstMatch = 0;
  for (const segment of segments) {
    while (firstMatch < matches.length && matches[firstMatch].end <= segment.start) {
      firstMatch += 1;
    }
    let cursor = segment.start;
    let matchIndex = firstMatch;
    while (matchIndex < matches.length && matches[matchIndex].start < segment.end) {
      const match = matches[matchIndex];
      const start = Math.max(segment.start, match.start);
      const end = Math.min(segment.end, match.end);
      emit(cursor, start, segment.className);
      emit(start, end, segment.className, matchIndex);
      cursor = end;
      if (match.end > segment.end) break;
      matchIndex += 1;
    }
    emit(cursor, segment.end, segment.className);
    firstMatch = matchIndex;
  }
  return out;
}
