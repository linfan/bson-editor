/** Small display formatters shared by the toolbar, rows and status bar. */

const UNITS = ["KB", "MB", "GB", "TB"];

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  let value = n / 1024;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

export function duration(ms: number): string {
  if (ms < 1) return "<1 ms";
  return ms < 1000 ? `${ms.toFixed(ms < 10 ? 1 : 0)} ms` : `${(ms / 1000).toFixed(2)} s`;
}

export function count(n: number): string {
  return n.toLocaleString("en-US");
}

export function fileName(path: string | null): string {
  if (!path) return "untitled";
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}
