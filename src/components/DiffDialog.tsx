/**
 * Compare two open tabs, side by side.
 *
 * Both documents are serialized to the JSON the app would copy out (embedded
 * strings expanded, two-space indent) and diffed line by line in the webview.
 * That keeps the comparison independent of the paged tree, and it means the two
 * trees may be at different expand depths without affecting the result.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import * as api from "../api";
import type { DocInfo, JsonOptions, SaveOptions } from "../api";
import { diffTexts } from "../diff";
import type { Diff, DiffSide } from "../diff";
import { fileName } from "../format";
import { Modal } from "./Dialogs";

const DIFF_HEIGHT = 20;
const OVERSCAN = 12;

const diffJson: JsonOptions = {
  indent: { kind: "spaces", width: 2 },
  flavor: "plain",
  expandStrings: true,
  maxDepth: null,
};

const diffSave: SaveOptions = { format: "json", json: diffJson, newlineTerminated: false };

interface TabOption {
  id: number;
  info: DocInfo;
}

export function DiffDialog({
  tabs,
  initialLeft,
  initialRight,
  onClose,
}: {
  tabs: TabOption[];
  initialLeft: number | null;
  initialRight: number | null;
  onClose: () => void;
}) {
  const [leftId, setLeftId] = useState<number | null>(initialLeft ?? tabs[0]?.id ?? null);
  const [rightId, setRightId] = useState<number | null>(
    initialRight ?? tabs.find((tab) => tab.id !== (initialLeft ?? tabs[0]?.id))?.id ?? null,
  );
  const [diff, setDiff] = useState<Diff | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (leftId === null || rightId === null || leftId === rightId) {
      setDiff(null);
      setError(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    Promise.all([api.exportText(leftId, diffSave), api.exportText(rightId, diffSave)])
      .then(([left, right]) => {
        if (!cancelled) setDiff(diffTexts(left, right));
      })
      .catch((cause) => {
        if (!cancelled) {
          setDiff(null);
          setError(api.errorMessage(cause));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [leftId, rightId]);

  const same = leftId !== null && leftId === rightId;

  return (
    <Modal
      title="Compare tabs"
      subtitle="Differences are computed on the JSON the app would copy out."
      onClose={onClose}
      wide
      diff
    >
      <div className="diff-pickers">
        <label>
          <span>Left</span>
          <select
            value={leftId ?? ""}
            onChange={(event) => setLeftId(event.target.value ? Number(event.target.value) : null)}
          >
            {tabs.map((tab) => (
              <option key={tab.id} value={tab.id}>
                {label(tab)}
              </option>
            ))}
          </select>
        </label>
        <button
          className="quiet"
          onClick={() => {
            setLeftId(rightId);
            setRightId(leftId);
          }}
          title="Swap sides"
        >
          ⇄
        </button>
        <label>
          <span>Right</span>
          <select
            value={rightId ?? ""}
            onChange={(event) =>
              setRightId(event.target.value ? Number(event.target.value) : null)
            }
          >
            {tabs.map((tab) => (
              <option key={tab.id} value={tab.id}>
                {label(tab)}
              </option>
            ))}
          </select>
        </label>
        <div className="spacer" />
        {diff && !loading && (
          <span className="diff-stats">
            {diff.stats.added + diff.stats.removed + diff.stats.changed === 0
              ? "identical"
              : `+${diff.stats.added} added · −${diff.stats.removed} removed · ~${diff.stats.changed} changed`}
          </span>
        )}
      </div>

      {same && <p className="diff-note">Pick two different tabs to compare.</p>}
      {!same && loading && <p className="diff-note">Comparing…</p>}
      {!same && error && <p className="error diff-error">{error}</p>}
      {!same && !loading && diff && <DiffBody diff={diff} />}
    </Modal>
  );
}

/** Virtualized body: a document pair can be tens of thousands of lines and only
 *  the rows in the viewport are worth mounting. */
function DiffBody({ diff }: { diff: Diff }) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [viewport, setViewport] = useState(500);
  const [scrollTop, setScrollTop] = useState(0);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setViewport(el.clientHeight));
    observer.observe(el);
    setViewport(el.clientHeight);
    return () => observer.disconnect();
  }, []);

  const total = diff.rows.length;
  const last = Math.max(0, total - 1);
  const scrollRow = Math.min(Math.floor(scrollTop / DIFF_HEIGHT), last);
  const start = Math.max(0, scrollRow - OVERSCAN);
  const end = Math.min(total, start + Math.ceil(viewport / DIFF_HEIGHT) + OVERSCAN * 2);

  const visible = useMemo(() => diff.rows.slice(start, end), [diff.rows, start, end]);

  return (
    <div
      className="diff-body"
      ref={scrollRef}
      onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
    >
      <div className="diff-canvas" style={{ height: total * DIFF_HEIGHT }}>
        {visible.map((row, offset) => (
          <div
            key={row.key}
            className="diff-row"
            style={{ top: (start + offset) * DIFF_HEIGHT, height: DIFF_HEIGHT }}
          >
            <Cell side={row.left} />
            <Cell side={row.right} />
          </div>
        ))}
      </div>
    </div>
  );
}

function Cell({ side }: { side: DiffSide | null }) {
  const kind = side?.kind ?? "empty";
  return (
    <div className={`diff-cell ${kind}`}>
      <span className="diff-no">{side?.no ?? ""}</span>
      <span className="diff-text" title={side?.text}>
        {side && withHighlight(side)}
      </span>
    </div>
  );
}

function withHighlight(side: DiffSide) {
  const span = side.highlight;
  if (!span || span.start >= span.end) return side.text;
  return (
    <>
      {side.text.slice(0, span.start)}
      <mark>{side.text.slice(span.start, span.end)}</mark>
      {side.text.slice(span.end)}
    </>
  );
}

function label(tab: TabOption): string {
  return `${tab.info.path ? fileName(tab.info.path) : `pasted #${tab.id}`} (${tab.info.format})`;
}
