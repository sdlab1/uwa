export function fmtMs(ms: number | undefined | null): string {
  if (ms == null || !isFinite(ms)) return '—';
  if (ms < 1) return '<1 ms';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

export function fmtNum(n: number | undefined | null): string {
  if (n == null) return '—';
  return n.toLocaleString();
}

export function fmtPercent(v: number | undefined | null, digits = 1): string {
  if (v == null || !isFinite(v)) return '—';
  return `${(v * 100).toFixed(digits)}%`;
}

export function fmtRel(secs: number | undefined | null): string {
  if (secs == null) return '—';
  if (secs < 60) return `${Math.round(secs)}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

/** Small bar-chart bar width used by MiniBars/PoolPanel. */
export function pct(n: number, max: number): number {
  if (max <= 0) return 0;
  return Math.min(100, (n / max) * 100);
}
