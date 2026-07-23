/** Loom track model: pure data + layout, no DOM. The canvas renderer consumes
 * this; tests and future virtualization reason about it directly. */

export interface LoomSpan {
  id: string;
  /** Seconds since epoch. */
  start: number;
  /** Seconds since epoch; instants render as minimum-width marks. */
  end: number;
  label: string;
  /** Secondary magnitude (e.g. message count) mapped to mark height. */
  weight: number;
}

export interface LoomTrack {
  id: string;
  label: string;
  spans: LoomSpan[];
}

export interface LoomWindow {
  start: number;
  end: number;
}

/** Track rows are fixed-height lanes (calm density; no packing puzzle). */
export const TRACK_HEIGHT = 28;
export const TRACK_GAP = 6;
export const AXIS_HEIGHT = 22;

export function windowOf(tracks: LoomTrack[]): LoomWindow | null {
  let start = Infinity;
  let end = -Infinity;
  for (const track of tracks) {
    for (const span of track.spans) {
      if (span.start < start) start = span.start;
      if (span.end > end) end = span.end;
    }
  }
  if (!Number.isFinite(start) || !Number.isFinite(end)) return null;
  if (end - start < 3600) end = start + 3600;
  return { start, end };
}

export function xFor(time: number, window: LoomWindow, width: number): number {
  return ((time - window.start) / (window.end - window.start)) * width;
}

export function timeFor(x: number, window: LoomWindow, width: number): number {
  return window.start + (x / width) * (window.end - window.start);
}

/** Pick the span under a canvas-space point, if any. */
export function pick(
  tracks: LoomTrack[],
  window: LoomWindow,
  width: number,
  x: number,
  y: number,
): { track: LoomTrack; span: LoomSpan } | null {
  const row = Math.floor((y - AXIS_HEIGHT) / (TRACK_HEIGHT + TRACK_GAP));
  const track = tracks[row];
  if (!track) return null;
  for (const span of track.spans) {
    const x0 = xFor(span.start, window, width);
    const x1 = Math.max(xFor(span.end, window, width), x0 + 3);
    if (x >= x0 - 2 && x <= x1 + 2) return { track, span };
  }
  return null;
}

/** Human tick labels for the axis at a sensible cadence for the window. */
export function axisTicks(window: LoomWindow, width: number): Array<{ x: number; label: string }> {
  const spanSeconds = window.end - window.start;
  const stepCandidates = [
    3600,
    6 * 3600,
    24 * 3600,
    7 * 24 * 3600,
    30 * 24 * 3600,
  ];
  const target = spanSeconds / Math.max(3, Math.floor(width / 120));
  const step =
    stepCandidates.find((candidate) => candidate >= target) ??
    stepCandidates[stepCandidates.length - 1]!;
  const ticks: Array<{ x: number; label: string }> = [];
  const first = Math.ceil(window.start / step) * step;
  for (let t = first; t <= window.end; t += step) {
    const date = new Date(t * 1000);
    const label =
      step >= 24 * 3600
        ? date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
        : date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
    ticks.push({ x: xFor(t, window, width), label });
  }
  return ticks;
}
