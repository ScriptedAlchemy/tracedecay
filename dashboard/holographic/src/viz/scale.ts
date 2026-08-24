/**
 * Tiny hand-rolled scale/binning helpers shared by the holographic charts.
 * Deliberately dependency-free (no d3): linear scales, "nice" tick generation,
 * extents, and histogram binning are all the charts need.
 */

export interface LinearScale {
  (value: number): number;
  invert: (px: number) => number;
  domain: [number, number];
  range: [number, number];
  ticks: (count?: number) => number[];
}

export function extent(values: number[]): [number, number] {
  let lo = Infinity;
  let hi = -Infinity;
  for (const v of values) {
    if (!Number.isFinite(v)) continue;
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  }
  if (lo === Infinity) return [0, 1];
  if (lo === hi) {
    // Degenerate domain (e.g. all HRR similarities identical): widen it so the
    // scale stays invertible and bins have nonzero width.
    const pad = Math.abs(lo) * 1e-3 || 0.5;
    return [lo - pad, hi + pad];
  }
  return [lo, hi];
}

/** Pad a numeric domain by a fraction on both ends. */
export function padDomain([lo, hi]: [number, number], frac = 0.06): [number, number] {
  const span = hi - lo || 1;
  return [lo - span * frac, hi + span * frac];
}

function tickStep(lo: number, hi: number, count: number): number {
  const span = Math.abs(hi - lo);
  const raw = span / Math.max(1, count);
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const norm = raw / mag;
  const step = norm >= 5 ? 10 : norm >= 2 ? 5 : norm >= 1 ? 2 : 1;
  return step * mag;
}

export function ticks(lo: number, hi: number, count = 5): number[] {
  if (!Number.isFinite(lo) || !Number.isFinite(hi) || lo === hi) return [lo];
  const step = tickStep(lo, hi, count);
  const start = Math.ceil(lo / step) * step;
  const out: number[] = [];
  for (let v = start; v <= hi + step * 1e-9; v += step) {
    // Snap floating point noise (0.30000000000000004 -> 0.3).
    out.push(Number(v.toPrecision(12)));
  }
  return out;
}

export function scaleLinear(
  domain: [number, number],
  range: [number, number],
): LinearScale {
  const [d0, d1] = domain;
  const [r0, r1] = range;
  const dd = d1 - d0 || 1;
  const fn = ((value: number) => r0 + ((value - d0) / dd) * (r1 - r0)) as LinearScale;
  fn.invert = (px: number) => d0 + ((px - r0) / ((r1 - r0) || 1)) * dd;
  fn.domain = domain;
  fn.range = range;
  fn.ticks = (count = 5) => ticks(d0, d1, count);
  return fn;
}

export interface Bin {
  x0: number;
  x1: number;
  count: number;
}

/** Equal-width histogram bins over the (auto-widened) extent of `values`. */
export function binValues(values: number[], binCount = 24): Bin[] {
  const [lo, hi] = extent(values);
  const width = (hi - lo) / binCount;
  const bins: Bin[] = Array.from({ length: binCount }, (_, i) => ({
    x0: lo + i * width,
    x1: lo + (i + 1) * width,
    count: 0,
  }));
  for (const v of values) {
    if (!Number.isFinite(v)) continue;
    let idx = Math.floor((v - lo) / width);
    if (idx >= binCount) idx = binCount - 1;
    if (idx < 0) idx = 0;
    bins[idx].count += 1;
  }
  return bins;
}

/** Format an axis tick compactly given the domain magnitude. */
export function formatTick(value: number, span: number): string {
  if (span >= 100) return String(Math.round(value));
  if (span >= 1) return value.toFixed(1).replace(/\.0$/, "");
  if (span >= 0.01) return value.toFixed(2);
  return value.toPrecision(3);
}
