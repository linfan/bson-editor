/**
 * Line diffing for the tab comparison view.
 *
 * Myers' algorithm gives the edit script in O((N+M)·D), where D is the number
 * of changed lines. That is fast for the usual case — two versions of the same
 * document — and the cap keeps a pair of unrelated files from making the trace
 * grow without bound; past it the middle is reported as a wholesale change.
 */

type Op = "equal" | "delete" | "insert";

/** Longest edit script the trace is allowed to grow to. */
const MAX_D = 1_000;

/** A side of one aligned diff row. */
export interface DiffSide {
  /** 1-based line number in its document. */
  no: number;
  text: string;
  kind: "equal" | "del" | "ins" | "change";
  /** Range inside `text` that actually differs, for intra-line highlighting. */
  highlight?: { start: number; end: number };
}

export interface DiffRow {
  key: string;
  left: DiffSide | null;
  right: DiffSide | null;
}

export interface DiffStats {
  added: number;
  removed: number;
  changed: number;
}

export interface Diff {
  rows: DiffRow[];
  stats: DiffStats;
}

/** Build aligned rows for two JSON texts. */
export function diffTexts(left: string, right: string): Diff {
  return buildRows(left.split("\n"), right.split("\n"));
}

export function buildRows(a: string[], b: string[]): Diff {
  const ops = diffLines(a, b);
  const rows: DiffRow[] = [];
  const stats: DiffStats = { added: 0, removed: 0, changed: 0 };

  let i = 0;
  while (i < ops.length) {
    const op = ops[i];
    if (op.type === "equal") {
      rows.push({
        key: `e${op.ai}`,
        left: { no: op.ai + 1, text: a[op.ai], kind: "equal" },
        right: { no: op.bi + 1, text: b[op.bi], kind: "equal" },
      });
      i += 1;
      continue;
    }

    // A run of deletions and insertions is one change block. Pairing them in
    // order gives a readable side-by-side view instead of two disconnected
    // halves.
    const dels: number[] = [];
    const ins: number[] = [];
    while (i < ops.length && ops[i].type !== "equal") {
      if (ops[i].type === "delete") dels.push(ops[i].ai);
      else ins.push(ops[i].bi);
      i += 1;
    }

    const paired = Math.min(dels.length, ins.length);
    for (let j = 0; j < paired; j += 1) {
      const ai = dels[j];
      const bi = ins[j];
      const span = changeSpan(a[ai], b[bi]);
      rows.push({
        key: `c${ai}-${bi}`,
        left: {
          no: ai + 1,
          text: a[ai],
          kind: "change",
          highlight: { start: span.aStart, end: span.aEnd },
        },
        right: {
          no: bi + 1,
          text: b[bi],
          kind: "change",
          highlight: { start: span.bStart, end: span.bEnd },
        },
      });
      stats.changed += 1;
    }
    for (let j = paired; j < dels.length; j += 1) {
      const ai = dels[j];
      rows.push({
        key: `d${ai}`,
        left: { no: ai + 1, text: a[ai], kind: "del", highlight: { start: 0, end: a[ai].length } },
        right: null,
      });
      stats.removed += 1;
    }
    for (let j = paired; j < ins.length; j += 1) {
      const bi = ins[j];
      rows.push({
        key: `i${bi}`,
        left: null,
        right: { no: bi + 1, text: b[bi], kind: "ins", highlight: { start: 0, end: b[bi].length } },
      });
      stats.added += 1;
    }
  }

  return { rows, stats };
}

interface LineOp {
  type: Op;
  ai: number;
  bi: number;
}

/** Trim the common ends, diff what is left, and put the indices back. */
function diffLines(a: string[], b: string[]): LineOp[] {
  let prefix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix += 1;
  let suffix = 0;
  while (
    suffix < a.length - prefix &&
    suffix < b.length - prefix &&
    a[a.length - 1 - suffix] === b[b.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  const types = diffMiddle(
    a.slice(prefix, a.length - suffix),
    b.slice(prefix, b.length - suffix),
  );

  const ops: LineOp[] = [];
  for (let i = 0; i < prefix; i += 1) ops.push({ type: "equal", ai: i, bi: i });
  let ai = prefix;
  let bi = prefix;
  for (const type of types) {
    if (type === "equal") {
      ops.push({ type, ai, bi });
      ai += 1;
      bi += 1;
    } else if (type === "delete") {
      ops.push({ type, ai, bi });
      ai += 1;
    } else {
      ops.push({ type, ai, bi });
      bi += 1;
    }
  }
  for (let i = 0; i < suffix; i += 1) {
    ops.push({ type: "equal", ai: a.length - suffix + i, bi: b.length - suffix + i });
  }
  return ops;
}

function diffMiddle(a: string[], b: string[]): Op[] {
  const n = a.length;
  const m = b.length;
  if (n === 0) return b.map(() => "insert" as const);
  if (m === 0) return a.map(() => "delete" as const);

  const max = n + m;
  const offset = max + 1;
  const v = new Int32Array(2 * max + 3).fill(-1);
  v[offset + 1] = 0;
  const trace: Int32Array[] = [];

  for (let d = 0; d <= Math.min(max, MAX_D); d += 1) {
    trace.push(v.slice());
    for (let k = -d; k <= d; k += 2) {
      const at = offset + k;
      let x: number;
      if (k === -d || (k !== d && v[at - 1] < v[at + 1])) {
        x = v[at + 1];
      } else {
        x = v[at - 1] + 1;
      }
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x += 1;
        y += 1;
      }
      v[at] = x;
      if (x >= n && y >= m) {
        return backtrack(trace, a.length, b.length, d, offset);
      }
    }
  }

  // Too different to trace within the cap; call it a wholesale replacement of
  // the middle. The grouping step still pairs the lines side by side.
  return [...a.map(() => "delete" as const), ...b.map(() => "insert" as const)];
}

function backtrack(
  trace: Int32Array[],
  nx: number,
  ny: number,
  d: number,
  offset: number,
): Op[] {
  const types: Op[] = [];
  let x = nx;
  let y = ny;
  for (let depth = d; depth > 0; depth -= 1) {
    const v = trace[depth];
    const k = x - y;
    let prevK: number;
    if (k === -depth || (k !== depth && v[offset + k - 1] < v[offset + k + 1])) {
      prevK = k + 1;
    } else {
      prevK = k - 1;
    }
    const prevX = v[offset + prevK];
    const prevY = prevX - prevK;
    while (x > prevX && y > prevY) {
      types.push("equal");
      x -= 1;
      y -= 1;
    }
    if (x === prevX) {
      types.push("insert");
      y -= 1;
    } else {
      types.push("delete");
      x -= 1;
    }
  }
  while (x > 0 && y > 0) {
    types.push("equal");
    x -= 1;
    y -= 1;
  }
  return types.reverse();
}

/** Common prefix and suffix of a changed pair, so only the middle is marked. */
function changeSpan(a: string, b: string): { aStart: number; aEnd: number; bStart: number; bEnd: number } {
  let prefix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix += 1;
  let suffix = 0;
  while (
    suffix < a.length - prefix &&
    suffix < b.length - prefix &&
    a[a.length - 1 - suffix] === b[b.length - 1 - suffix]
  ) {
    suffix += 1;
  }
  return {
    aStart: prefix,
    aEnd: a.length - suffix,
    bStart: prefix,
    bEnd: b.length - suffix,
  };
}
