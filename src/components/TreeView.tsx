/**
 * The virtualized tree.
 *
 * Only the rows inside the viewport are ever mounted, so the cost of painting
 * is fixed no matter how large the document is. Rows arrive a page at a time
 * from the Rust side; scrolling near the end of a loaded page pulls the next.
 */
import { memo, useEffect, useMemo, useRef, useState } from "react";
import type { NodeRow } from "../api";
import type { Tree, TreeRow } from "../useTree";

const ROW_HEIGHT = 26;
const OVERSCAN = 10;
const INDENT = 16;

export interface RowActions {
  onCopy: (row: NodeRow, parentId: number) => void;
  onEdit: (row: NodeRow, parentId: number) => void;
  onInsert: (row: NodeRow, parentId: number) => void;
  onDelete: (row: NodeRow, parentId: number) => void;
}

interface Props {
  tree: Tree;
  actions: RowActions;
  selected: number | null;
  onSelect: (id: number) => void;
  /** Jump to a node once its row has been loaded (search navigation). */
  scrollTo?: { node: number; token: number } | null;
}

export function TreeView({ tree, actions, selected, onSelect, scrollTo }: Props) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [viewport, setViewport] = useState(600);
  const [scrollTop, setScrollTop] = useState(0);
  const handledScroll = useRef(-1);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setViewport(el.clientHeight));
    observer.observe(el);
    setViewport(el.clientHeight);
    return () => observer.disconnect();
  }, []);

  const total = tree.rows.length;
  // The scroll offset is state, so it can outlive the rows it referred to:
  // collapsing a subtree or adopting a smaller document shrinks the canvas, and
  // the browser clamps the element without firing a scroll event. Clamping the
  // row index keeps the window inside the list instead of painting nothing.
  const lastRow = Math.max(0, total - 1);
  const scrollRow = Math.min(Math.floor(scrollTop / ROW_HEIGHT), lastRow);
  const start = Math.max(0, scrollRow - OVERSCAN);
  const end = Math.min(total, start + Math.ceil(viewport / ROW_HEIGHT) + OVERSCAN * 2);
  const { rows } = tree;

  // Containers whose "load more" sentinel has scrolled into view. Memoized on
  // the window bounds rather than on the sliced array, whose identity changes
  // every render.
  const pending = useMemo(() => {
    const parents = new Set<number>();
    for (let i = start; i < end; i += 1) {
      const item = rows[i];
      if (item.remaining !== undefined) parents.add(item.parentId);
    }
    return [...parents];
  }, [rows, start, end]);

  const { loadMore } = tree;
  useEffect(() => {
    for (const parentId of pending) loadMore(parentId);
  }, [pending, loadMore]);

  // A reveal is asynchronous, so the row may not exist on the first render
  // after the request. Leave the token unhandled until it does.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || !scrollTo || handledScroll.current === scrollTo.token) return;
    const index = rows.findIndex((item) => item.row?.id === scrollTo.node);
    if (index < 0) return;
    handledScroll.current = scrollTo.token;
    el.scrollTop = Math.max(0, index * ROW_HEIGHT - el.clientHeight / 3);
    setScrollTop(el.scrollTop);
  }, [rows, scrollTo]);

  const visible: TreeRow[] = [];
  for (let i = start; i < end; i += 1) visible.push(rows[i]);

  return (
    <div
      className="tree"
      ref={scrollRef}
      onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
    >
      <div className="tree-canvas" style={{ height: total * ROW_HEIGHT }}>
        {visible.map((item, offset) => {
          const top = (start + offset) * ROW_HEIGHT;
          if (item.row === undefined) {
            return (
              <div key={item.key} className="row more" style={{ top, height: ROW_HEIGHT }}>
                <span style={{ marginLeft: item.depth * INDENT + 22 }}>
                  {tree.busy ? "loading…" : `${item.remaining?.toLocaleString()} more`}
                </span>
              </div>
            );
          }
          return (
            <Row
              key={item.key}
              item={item}
              top={top}
              open={tree.expanded.has(item.row.id)}
              selected={selected === item.row.id}
              onToggle={tree.toggle}
              onSelect={onSelect}
              actions={actions}
            />
          );
        })}
        {total === 0 && (
          <div className="tree-empty">
            {tree.busy ? "Reading…" : "This document is empty."}
          </div>
        )}
      </div>
    </div>
  );
}

interface RowProps {
  item: TreeRow;
  top: number;
  open: boolean;
  selected: boolean;
  onToggle: (id: number) => void;
  onSelect: (id: number) => void;
  actions: RowActions;
}

const Row = memo(function Row({
  item,
  top,
  open,
  selected,
  onToggle,
  onSelect,
  actions,
}: RowProps) {
  const row = item.row!;
  const isContainer = row.expandable || row.kind === "object" || row.kind === "array";

  return (
    <div
      className={`row${selected ? " selected" : ""}`}
      style={{ top, height: ROW_HEIGHT }}
      onClick={() => onSelect(row.id)}
    >
      <span className="indent" style={{ width: item.depth * INDENT }} />
      <button
        className={`chevron${isContainer ? "" : " none"}`}
        onClick={(event) => {
          event.stopPropagation();
          if (isContainer) onToggle(row.id);
        }}
        aria-label={open ? "Collapse" : "Expand"}
      >
        {isContainer ? (open ? "▾" : "▸") : ""}
      </button>

      {row.key === null ? (
        <span className="index">{row.index}</span>
      ) : (
        <span className="key" title={row.key}>
          {row.key}
        </span>
      )}
      <span className="colon">:</span>
      <span className={`value v-${slug(row.kind)}`} title={row.preview}>
        {row.preview}
      </span>
      <span className={`badge b-${slug(row.kind)}`}>{row.kind}</span>
      {row.embeddedJson && (
        <span className="badge b-embedded" title="This string decoded into JSON">
          json×{row.overlayLevel}
        </span>
      )}

      <span className="actions" onClick={(event) => event.stopPropagation()}>
        <button onClick={() => actions.onCopy(row, item.parentId)} title="Copy as formatted JSON">
          Copy
        </button>
        {row.editable && (
          <button onClick={() => actions.onEdit(row, item.parentId)} title="Edit value">
            Edit
          </button>
        )}
        <button
          onClick={() => actions.onInsert(row, item.parentId)}
          title="Insert a sibling after this one"
        >
          Add
        </button>
        <button className="danger" onClick={() => actions.onDelete(row, item.parentId)} title="Delete">
          Del
        </button>
      </span>
    </div>
  );
});

/** CSS-safe fragment of a type name, for the per-type colour classes. */
function slug(kind: string): string {
  return kind.replace(/[^a-z0-9]/gi, "-").toLowerCase();
}
