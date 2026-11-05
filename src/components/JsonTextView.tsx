/**
 * The document as indented JSON text, with the same lazy tree behind it.
 *
 * Every visible node becomes one line: containers open on their own line and
 * contribute a synthesized closing line when their children end. Folding a
 * container goes through the same `tree.toggle` as the table view, so a large
 * document still only grows the flat row list by what is unfolded. The list is
 * virtualized, which is what keeps a fully expanded file paintable.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import type { NodeRow } from "../api";
import type { Tree, TreeRow } from "../useTree";

const LINE_HEIGHT = 22;
const OVERSCAN = 12;
const INDENT = 18;

interface Props {
  tree: Tree;
  rootKind: string;
  rootPreview: string;
  selected: number | null;
  onSelect: (id: number) => void;
  /** Jump to a node once its line exists (search navigation). */
  scrollTo?: { node: number; token: number } | null;
}

interface TextLine {
  key: string;
  indent: number;
  /** The node this line selects and folds, when it has one. */
  node: number | null;
  /** Set on the "more rows" sentinel, so scrolling can pull the next page. */
  parentId: number | null;
  foldable: boolean;
  open: boolean;
  prefix: string | null;
  value: string;
  valueKind: string;
  comma: boolean;
  more: boolean;
}

export function JsonTextView({
  tree,
  rootKind,
  rootPreview,
  selected,
  onSelect,
  scrollTo,
}: Props) {
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

  const lines = useMemo(
    () =>
      tree.rows.length === 0 && tree.busy
        ? []
        : buildLines(tree.rows, tree.expanded, rootKind, rootPreview),
    [tree.rows, tree.expanded, tree.busy, rootKind, rootPreview],
  );

  const total = lines.length;
  const lastLine = Math.max(0, total - 1);
  const scrollLine = Math.min(Math.floor(scrollTop / LINE_HEIGHT), lastLine);
  const start = Math.max(0, scrollLine - OVERSCAN);
  const end = Math.min(total, start + Math.ceil(viewport / LINE_HEIGHT) + OVERSCAN * 2);

  // A reveal is asynchronous, so its line may only appear on a later render.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || !scrollTo || handledScroll.current === scrollTo.token) return;
    const index = lines.findIndex((line) => line.node === scrollTo.node);
    if (index < 0) return;
    handledScroll.current = scrollTo.token;
    el.scrollTop = Math.max(0, index * LINE_HEIGHT - el.clientHeight / 3);
    setScrollTop(el.scrollTop);
  }, [lines, scrollTo]);

  const pending = useMemo(() => {
    const parents = new Set<number>();
    for (let i = start; i < end; i += 1) {
      const parent = lines[i].parentId;
      if (lines[i].more && parent !== null) parents.add(parent);
    }
    return [...parents];
  }, [lines, start, end]);

  const { loadMore } = tree;
  useEffect(() => {
    for (const parentId of pending) loadMore(parentId);
  }, [pending, loadMore]);

  return (
    <div
      className="jsonview"
      ref={scrollRef}
      onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
    >
      <div className="json-canvas" style={{ height: total * LINE_HEIGHT }}>
        {lines.slice(start, end).map((line, offset) => {
          const top = (start + offset) * LINE_HEIGHT;
          const isSelected = line.node !== null && line.node === selected;
          return (
            <div
              key={line.key}
              className={`json-line${isSelected ? " selected" : ""}${line.more ? " more" : ""}`}
              style={{ top, height: LINE_HEIGHT }}
              onClick={() => line.node !== null && onSelect(line.node)}
            >
              <span className="indent" style={{ width: line.indent * INDENT }} />
              <button
                className={`chevron${line.foldable ? "" : " none"}`}
                onClick={(event) => {
                  event.stopPropagation();
                  if (line.foldable && line.node !== null) tree.toggle(line.node);
                }}
                aria-label={line.open ? "Collapse" : "Expand"}
              >
                {line.foldable ? (line.open ? "▾" : "▸") : ""}
              </button>
              {line.prefix && <span className="key json-key">{line.prefix}</span>}
              <span className={`v-${line.valueKind}`}>{line.value}</span>
              {line.comma && <span className="punctuation">,</span>}
            </div>
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

/** Flatten the visible tree rows into JSON text lines.
 *
 * `depth` on a row is its indent inside the root; the root's own braces are one
 * more level out. A container that is open contributes a closing line when the
 * walk leaves its children, which is where its comma belongs. */
function buildLines(
  rows: TreeRow[],
  expanded: Set<number>,
  rootKind: string,
  rootPreview: string,
): TextLine[] {
  const out: TextLine[] = [];
  const stack: { indent: number; close: string; comma: boolean; node: number }[] = [];
  let closeSeq = 0;

  const closeTop = () => {
    const top = stack.pop()!;
    out.push({
      key: `close-${top.node}-${closeSeq++}`,
      indent: top.indent,
      node: null,
      parentId: null,
      foldable: false,
      open: false,
      prefix: null,
      value: top.close,
      valueKind: "punct",
      comma: top.comma,
      more: false,
    });
  };

  if (rootKind !== "object" && rootKind !== "array") {
    return [
      {
        key: "root",
        indent: 0,
        node: null,
        parentId: null,
        foldable: false,
        open: false,
        prefix: null,
        value: rootPreview || rootKind,
        valueKind: slug(rootKind),
        comma: false,
        more: false,
      },
    ];
  }

  const rootOpen = rootKind === "array" ? "[" : "{";
  const rootClose = rootKind === "array" ? "]" : "}";
  out.push({
    key: "root-open",
    indent: 0,
    node: null,
    parentId: null,
    foldable: false,
    open: true,
    prefix: null,
    value: rootOpen,
    valueKind: "punct",
    comma: false,
    more: false,
  });

  for (const item of rows) {
    const depth = item.depth + 1;
    while (stack.length > 0 && depth <= stack[stack.length - 1].indent) closeTop();

    if (item.row === undefined) {
      out.push({
        key: item.key,
        indent: depth,
        node: null,
        parentId: item.parentId,
        foldable: false,
        open: false,
        prefix: null,
        value: `… ${item.remaining?.toLocaleString() ?? 0} more`,
        valueKind: "muted",
        comma: false,
        more: true,
      });
      continue;
    }

    const row = item.row;
    const prefix = row.key === null ? null : `${JSON.stringify(row.key)}: `;
    const comma = row.index + 1 < item.siblingTotal;
    const container = row.presentedKind === "object" || row.presentedKind === "array";
    const expandedContainer = container && row.expandable && expanded.has(row.id);

    if (expandedContainer) {
      const array = row.presentedKind === "array";
      out.push({
        key: `${item.key}-open`,
        indent: depth,
        node: row.id,
        parentId: item.parentId,
        foldable: true,
        open: true,
        prefix,
        value: array ? "[" : "{",
        valueKind: "punct",
        comma: false,
        more: false,
      });
      stack.push({ indent: depth, close: array ? "]" : "}", comma, node: row.id });
    } else if (container) {
      const array = row.presentedKind === "array";
      out.push({
        key: item.key,
        indent: depth,
        node: row.id,
        parentId: item.parentId,
        foldable: row.expandable,
        open: false,
        prefix,
        value: row.expandable ? (array ? "[…]" : "{…}") : array ? "[]" : "{}",
        valueKind: "punct",
        comma,
        more: false,
      });
    } else {
      out.push({
        key: item.key,
        indent: depth,
        node: row.id,
        parentId: item.parentId,
        foldable: false,
        open: false,
        prefix,
        ...scalar(row),
        comma,
        more: false,
      });
    }
  }

  while (stack.length > 0) closeTop();
  out.push({
    key: "root-close",
    indent: 0,
    node: null,
    parentId: null,
    foldable: false,
    open: false,
    prefix: null,
    value: rootClose,
    valueKind: "punct",
    comma: false,
    more: false,
  });
  return out;
}

function scalar(row: NodeRow): { value: string; valueKind: string } {
  if (row.kind === "string" || row.kind === "symbol" || row.kind === "javascript") {
    return { value: JSON.stringify(row.preview), valueKind: slug(row.kind) };
  }
  return { value: row.preview, valueKind: slug(row.kind) };
}

function slug(kind: string): string {
  return kind.replace(/[^a-z0-9]/gi, "-").toLowerCase();
}
