/**
 * The tree's data model: a flat list of the rows currently visible.
 *
 * Nothing is fetched until the user expands it, and a container's children are
 * fetched a page at a time. That is what lets a document with a 50,000-element
 * array open instantly: the flat list only ever grows to cover what has actually
 * been scrolled to, and the renderer paints a window of it.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "./api";
import type { NodeRow } from "./api";

export interface TreeRow {
  key: string;
  depth: number;
  /** The container this row sits in, which is what an edit has to refresh. */
  parentId: number;
  row?: NodeRow;
  /** Number of siblings, so the text view knows whether a comma follows. */
  siblingTotal: number;
  /** Set on the sentinel row that pulls the next page of a large container. */
  remaining?: number;
}

interface Segment {
  total: number;
  rows: NodeRow[];
}

export interface Tree {
  rows: TreeRow[];
  expanded: Set<number>;
  busy: boolean;
  error: string | null;
  toggle: (id: number) => void;
  loadMore: (parentId: number) => void;
  /** Re-read one container after an edit shifted or rewrote its members. */
  refresh: (parentId: number) => Promise<void>;
  /** Open every ancestor of a node and page its container until the row exists. */
  reveal: (path: api.SearchStep[]) => Promise<void>;
  clearError: () => void;
}

export function useTree(tab: number, root: number | null, generation: number): Tree {
  const [segments, setSegments] = useState<Map<number, Segment>>(() => new Map());
  const [expanded, setExpanded] = useState<Set<number>>(() => new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Pages already requested, so a sentinel sitting in the viewport cannot fire
  // the same fetch once per frame.
  const inFlight = useRef(new Set<string>());
  // Read-only mirror of `segments` for handlers that need the current page
  // offset. Assigned during render, which is safe because it only mirrors.
  const current = useRef(segments);
  current.current = segments;

  const fetchPage = useCallback(
    async (parentId: number, offset: number, limit: number, append: boolean) => {
      const tag = `${parentId}@${offset}`;
      if (inFlight.current.has(tag)) return null;
      inFlight.current.add(tag);
      setBusy(true);
      try {
        const page = await api.children(tab, parentId, offset, limit);
        setSegments((prev) => {
          const next = new Map(prev);
          const rows = append ? [...(next.get(parentId)?.rows ?? []), ...page.rows] : page.rows;
          next.set(parentId, { total: page.total, rows });
          return next;
        });
        return page;
      } catch (cause) {
        setError(api.errorMessage(cause));
        return null;
      } finally {
        inFlight.current.delete(tag);
        setBusy(false);
      }
    },
    [tab],
  );

  // Opening a document reads the first page of the root and leaves it expanded,
  // so the tree is never empty on arrival. `generation` is what catches a
  // rebuild at a new expand depth: that re-parses from the original bytes and
  // can hand back the same root id for completely different node ids.
  useEffect(() => {
    if (root === null) return;
    setSegments(new Map());
    setExpanded(new Set([root]));
    void fetchPage(root, 0, api.PAGE_SIZE, false);
  }, [root, generation, fetchPage]);

  const toggle = useCallback(
    (id: number) => {
      if (expanded.has(id)) {
        setExpanded((prev) => {
          const next = new Set(prev);
          next.delete(id);
          return next;
        });
        return;
      }
      setExpanded((prev) => new Set(prev).add(id));
      // Re-read every time rather than serving a cached page: an edit elsewhere
      // may have changed this subtree, and one page is cheap.
      void fetchPage(id, 0, api.PAGE_SIZE, false);
    },
    [expanded, fetchPage],
  );

  const loadMore = useCallback(
    (parentId: number) => {
      const offset = current.current.get(parentId)?.rows.length ?? 0;
      void fetchPage(parentId, offset, api.PAGE_SIZE, true);
    },
    [fetchPage],
  );

  const refresh = useCallback(
    async (parentId: number) => {
      // Keep however many pages were already loaded, so a long array the user
      // scrolled through does not collapse back to its first page.
      const loaded = current.current.get(parentId)?.rows.length ?? 0;
      await fetchPage(parentId, 0, Math.max(loaded, api.PAGE_SIZE), false);
    },
    [fetchPage],
  );

  const reveal = useCallback(
    async (path: api.SearchStep[]) => {
      // Counts are tracked locally because state updates inside the loop do not
      // reach this closure; without them the next page request would repeat the
      // same offset.
      const counts = new Map<number, number>();
      for (let i = 0; i + 1 < path.length; i += 1) {
        const parent = path[i].node;
        const childIndex = path[i + 1].index;
        setExpanded((prev) => (prev.has(parent) ? prev : new Set(prev).add(parent)));
        let count = counts.get(parent) ?? current.current.get(parent)?.rows.length ?? 0;
        let retries = 0;
        while (count <= childIndex) {
          const page = await fetchPage(parent, count, api.PAGE_SIZE, count > 0);
          if (!page) {
            // A page for this offset is already in flight (the initial root
            // load, say). Give it a moment and pick up however much landed.
            if (retries >= 5) break;
            retries += 1;
            await new Promise((resolve) => setTimeout(resolve, 40));
            count = Math.max(count, current.current.get(parent)?.rows.length ?? 0);
            continue;
          }
          if (page.rows.length === 0) break;
          count += page.rows.length;
          if (count >= page.total) break;
        }
        counts.set(parent, count);
      }
    },
    [fetchPage],
  );

  const rows = useMemo(() => {
    if (root === null) return [] as TreeRow[];
    const out: TreeRow[] = [];
    const walk = (parentId: number, depth: number) => {
      const segment = segments.get(parentId);
      if (!segment) return;
      for (const row of segment.rows) {
        out.push({ key: `${parentId}/${row.id}`, depth, parentId, row, siblingTotal: segment.total });
        if (expanded.has(row.id)) walk(row.id, depth + 1);
      }
      if (segment.rows.length < segment.total) {
        out.push({
          key: `${parentId}/more`,
          depth,
          parentId,
          siblingTotal: segment.total,
          remaining: segment.total - segment.rows.length,
        });
      }
    };
    walk(root, 0);
    return out;
  }, [root, segments, expanded]);

  return {
    rows,
    expanded,
    busy,
    error,
    toggle,
    loadMore,
    refresh,
    reveal,
    clearError: () => setError(null),
  };
}
