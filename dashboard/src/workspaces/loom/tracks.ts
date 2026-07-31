/** Loom track model: pure data + layout, no DOM. The canvas renderer consumes
 * this; tests and future virtualization reason about it directly.
 *
 * Everything here is derived from real spans. Nothing in this module invents a
 * span, a track, or a magnitude — the density profile and the lane packing are
 * projections of the same session rows the canvas draws.
 */

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

/* -------------------------------------------------------------------------
 * Rhythm. Lanes are thin because the interesting axis is horizontal; a track
 * grows by stacking sub-lanes only when its spans genuinely overlap.
 * ---------------------------------------------------------------------- */

/** One packed sub-lane inside a track. */
export const LANE_HEIGHT = 16;
/** Lanes stretch to fill a tall pane, but never become slabs. */
export const MAX_LANE_HEIGHT = 46;
export const LANE_GAP = 3;
/** Breathing room above/below a track's stack of lanes. */
export const TRACK_PAD = 7;
/** Two-tier axis: a day-band row over a fine tick row. */
export const AXIS_HEIGHT = 34;

/** Minimum on-screen width of a mark, in CSS pixels. */
export const MIN_MARK_PX = 3;

export interface TrackLayout {
  track: LoomTrack;
  /** Spans packed into non-overlapping sub-lanes, earliest first. */
  lanes: LoomSpan[][];
  /** Top edge of the track band, relative to the top of the axis. */
  top: number;
  height: number;
  /** Resolved lane height — lanes stretch to fill a tall viewport. */
  laneHeight: number;
}

/* -------------------------------------------------------------------------
 * Windows
 * ---------------------------------------------------------------------- */

/** Full data extent across every track, or null when nothing is timestamped. */
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

/** The extent with a small margin, so the first and last mark are not welded
 * to the frame. This is the default viewport and the "fit" target. */
export function fittedWindow(extent: LoomWindow): LoomWindow {
  const pad = Math.max((extent.end - extent.start) * 0.02, 60);
  return { start: extent.start - pad, end: extent.end + pad };
}

/** Never zoom past a minute of span, nor further out than 8x the data. */
function windowLimits(extent: LoomWindow): { min: number; max: number } {
  const full = Math.max(extent.end - extent.start, 60);
  return { min: 60, max: full * 8 };
}

export function clampWindow(view: LoomWindow, extent: LoomWindow): LoomWindow {
  const { min, max } = windowLimits(extent);
  const span = Math.min(Math.max(view.end - view.start, min), max);
  const full = fittedWindow(extent);
  // Keep at least a third of the viewport over real data at every zoom level:
  // panning can never strand the eye in empty time.
  const slack = span * (2 / 3);
  let start = view.start;
  if (start > full.end - span + slack) start = full.end - span + slack;
  if (start < full.start - slack) start = full.start - slack;
  return { start, end: start + span };
}

export function zoomWindow(
  view: LoomWindow,
  extent: LoomWindow,
  factor: number,
  focusTime: number,
): LoomWindow {
  const span = view.end - view.start;
  const nextSpan = span * factor;
  const ratio = span === 0 ? 0.5 : (focusTime - view.start) / span;
  return clampWindow(
    { start: focusTime - ratio * nextSpan, end: focusTime + (1 - ratio) * nextSpan },
    extent,
  );
}

/** True when the viewport is (near enough) the whole fitted extent. */
export function isFitted(view: LoomWindow, extent: LoomWindow): boolean {
  const full = fittedWindow(extent);
  const tolerance = (full.end - full.start) * 0.005;
  return (
    Math.abs(view.start - full.start) <= tolerance &&
    Math.abs(view.end - full.end) <= tolerance
  );
}

export function xFor(time: number, window: LoomWindow, width: number): number {
  return ((time - window.start) / (window.end - window.start)) * width;
}

/* -------------------------------------------------------------------------
 * Lane packing
 * ---------------------------------------------------------------------- */

/** Greedy interval packing: each span drops into the first sub-lane whose last
 * mark has already ended (plus a pixel-sized gap, so two marks that would touch
 * on screen are still separated). Overlap becomes structure instead of mud. */
export function packTrack(spans: LoomSpan[], minGapSeconds = 0): LoomSpan[][] {
  const ordered = [...spans].sort((a, b) => a.start - b.start || a.end - b.end);
  const lanes: LoomSpan[][] = [];
  const lastEnd: number[] = [];
  for (const span of ordered) {
    const end = Math.max(span.end, span.start + minGapSeconds);
    let placed = false;
    for (let lane = 0; lane < lanes.length; lane += 1) {
      if ((lastEnd[lane] ?? -Infinity) + minGapSeconds <= span.start) {
        lanes[lane]!.push(span);
        lastEnd[lane] = end;
        placed = true;
        break;
      }
    }
    if (!placed) {
      lanes.push([span]);
      lastEnd.push(end);
    }
  }
  return lanes;
}

/** Lane height that spends whatever vertical room the viewport can spare,
 * within a legible band. A three-track weave in a tall pane should breathe;
 * a thirty-lane weave should stay thin and scroll. */
export function laneHeightFor(totalLanes: number, availableHeight: number): number {
  if (totalLanes <= 0 || availableHeight <= 0) return LANE_HEIGHT;
  const free = availableHeight - AXIS_HEIGHT - totalLanes * LANE_GAP;
  return Math.min(Math.max(free / totalLanes, LANE_HEIGHT), MAX_LANE_HEIGHT);
}

/** Lay every track out vertically, packing each one against the current
 * viewport so lane count reflects what actually collides on screen. */
export function layoutTracks(
  tracks: LoomTrack[],
  view: LoomWindow,
  width: number,
  availableHeight = 0,
): TrackLayout[] {
  const minGap = width > 0 ? ((view.end - view.start) / width) * MIN_MARK_PX : 0;
  const packed = tracks.map((track) => packTrack(track.spans, minGap));
  const totalLanes = packed.reduce((sum, lanes) => sum + Math.max(lanes.length, 1), 0);
  const laneHeight = laneHeightFor(totalLanes, availableHeight);
  const layouts: TrackLayout[] = [];
  let top = 0;
  tracks.forEach((track, index) => {
    const lanes = packed[index] ?? [];
    const laneCount = Math.max(lanes.length, 1);
    const height = TRACK_PAD * 2 + laneCount * laneHeight + (laneCount - 1) * LANE_GAP;
    layouts.push({ track, lanes, top, height, laneHeight });
    top += height;
  });
  return layouts;
}

export function layoutHeight(layouts: TrackLayout[]): number {
  const last = layouts[layouts.length - 1];
  return last ? last.top + last.height : 0;
}

/** Pick the span under a canvas-space point, if any. `y` is measured from the
 * top of the canvas (axis included). */
export function pick(
  layouts: TrackLayout[],
  view: LoomWindow,
  width: number,
  x: number,
  y: number,
): { track: LoomTrack; span: LoomSpan } | null {
  const localY = y - AXIS_HEIGHT;
  const layout = layouts.find(
    (candidate) => localY >= candidate.top && localY < candidate.top + candidate.height,
  );
  if (!layout) return null;
  const laneY = localY - layout.top - TRACK_PAD;
  const lane = Math.floor(laneY / (layout.laneHeight + LANE_GAP));
  const spans = layout.lanes[lane];
  if (!spans) return null;
  let best: LoomSpan | null = null;
  for (const span of spans) {
    const x0 = xFor(span.start, view, width);
    const x1 = Math.max(xFor(span.end, view, width), x0 + MIN_MARK_PX);
    if (x >= x0 - 2 && x <= x1 + 2) best = span;
  }
  return best ? { track: layout.track, span: best } : null;
}

/* -------------------------------------------------------------------------
 * Axis
 * ---------------------------------------------------------------------- */

export interface AxisTick {
  x: number;
  time: number;
  label: string;
}

/** A 1/2/5-style ladder that stays calendar-honest at the day scale and above,
 * so a five-day window ticks in hours and a five-month window ticks in weeks
 * instead of both collapsing onto the same five candidates. */
const TICK_STEPS = [
  1, 2, 5, 10, 15, 30,
  60, 120, 300, 600, 900, 1800,
  3600, 2 * 3600, 3 * 3600, 6 * 3600, 12 * 3600,
  86_400, 2 * 86_400, 7 * 86_400, 14 * 86_400,
  30 * 86_400, 90 * 86_400, 180 * 86_400, 365 * 86_400,
] as const;

export function tickStepFor(spanSeconds: number, width: number): number {
  const maxTicks = Math.min(Math.max(Math.floor(width / 96), 2), 14);
  const target = spanSeconds / maxTicks;
  const step =
    TICK_STEPS.find((candidate) => candidate >= target) ??
    TICK_STEPS[TICK_STEPS.length - 1]!;
  // The two axis tiers must never say the same thing twice: the fine row wants
  // to sit below whatever the calendar band row is showing. That preference
  // never outranks legibility, though — dropping under the band unit can cost
  // several rungs, and a bare "one rung below the band" rule prints 64 ticks
  // across a two-year window. Take the LARGEST rung under the band (fewest
  // ticks), and only if it still keeps ticks no closer than a ~48px pitch;
  // otherwise the tiers share a unit and the band row carries the coarser one.
  const ceiling = bandCeiling(spanSeconds);
  if (step < ceiling) return step;
  const denseLimit = Math.max(maxTicks, Math.floor(width / 48));
  const finer = [...TICK_STEPS]
    .reverse()
    .find((candidate) => candidate < ceiling && spanSeconds / candidate <= denseLimit);
  return finer ?? step;
}

/** Largest fine step allowed under the current calendar band scale. */
function bandCeiling(spanSeconds: number): number {
  const scale = bandScale(spanSeconds);
  if (scale === 'hour') return 3600;
  if (scale === 'day') return 86_400;
  return 30 * 86_400;
}

/** Which calendar unit the upper axis tier bands by. */
export function bandScale(spanSeconds: number): 'hour' | 'day' | 'month' {
  if (spanSeconds <= 36 * 3600) return 'hour';
  if (spanSeconds <= 70 * 86_400) return 'day';
  return 'month';
}

/** Fine ticks: clock time inside a day, calendar dates above it. */
export function axisTicks(view: LoomWindow, width: number): AxisTick[] {
  if (width <= 0) return [];
  const step = tickStepFor(view.end - view.start, width);
  const ticks: AxisTick[] = [];
  const first = Math.ceil(view.start / step) * step;
  for (let t = first; t <= view.end; t += step) {
    ticks.push({ x: xFor(t, view, width), time: t, label: tickLabel(t, step) });
    if (ticks.length > 64) break;
  }
  return ticks;
}

function tickLabel(epochSeconds: number, step: number): string {
  const date = new Date(epochSeconds * 1000);
  if (step < 60) {
    return date.toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    });
  }
  if (step < 86_400) {
    return date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  }
  if (step < 30 * 86_400) {
    return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  }
  return date.toLocaleDateString(undefined, { month: 'short', year: '2-digit' });
}

export interface DayBand {
  x0: number;
  x1: number;
  /** Local midnight that opens the band. */
  time: number;
  label: string;
  /** Alternating parity, so consecutive days can be tinted apart. */
  odd: boolean;
}

/** Calendar bands behind the ticks: hours inside a day, days inside a season,
 * months beyond that. The bands are the weave's warp — the eye reads their
 * rhythm before it reads any single mark. */
export function dayBands(view: LoomWindow, width: number): DayBand[] {
  if (width <= 0) return [];
  const spanSeconds = view.end - view.start;
  const scale = bandScale(spanSeconds);
  const bands: DayBand[] = [];
  const cursor = new Date(view.start * 1000);
  if (scale === 'hour') cursor.setMinutes(0, 0, 0);
  else if (scale === 'day') cursor.setHours(0, 0, 0, 0);
  else cursor.setDate(1), cursor.setHours(0, 0, 0, 0);
  const hourStride = Math.max(1, Math.round(spanSeconds / 3600 / 8));
  let guard = 0;
  while (cursor.getTime() / 1000 < view.end && guard < 400) {
    guard += 1;
    const time = cursor.getTime() / 1000;
    const next = new Date(cursor);
    if (scale === 'hour') next.setHours(next.getHours() + hourStride);
    else if (scale === 'day') next.setDate(next.getDate() + 1);
    else next.setMonth(next.getMonth() + 1);
    const nextTime = next.getTime() / 1000;
    if (nextTime > view.start) {
      bands.push({
        x0: xFor(Math.max(time, view.start), view, width),
        x1: xFor(Math.min(nextTime, view.end), view, width),
        time,
        label: bandLabel(time, scale),
        odd: bands.length % 2 === 1,
      });
    }
    cursor.setTime(next.getTime());
  }
  return bands;
}

function bandLabel(epochSeconds: number, scale: 'hour' | 'day' | 'month'): string {
  const date = new Date(epochSeconds * 1000);
  if (scale === 'hour') {
    return date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  }
  if (scale === 'day') {
    return date.toLocaleDateString(undefined, {
      weekday: 'short',
      month: 'short',
      day: 'numeric',
    });
  }
  return date.toLocaleDateString(undefined, { month: 'long', year: 'numeric' });
}

/* -------------------------------------------------------------------------
 * Density
 * ---------------------------------------------------------------------- */

/** Weight-per-bucket across the full extent: the overview profile drawn in the
 * minimap. Each span contributes its weight spread evenly over the buckets it
 * covers, so a long quiet session does not out-shout a short dense one. */
export function densityProfile(
  tracks: LoomTrack[],
  extent: LoomWindow,
  buckets: number,
): number[] {
  const out = new Array<number>(Math.max(buckets, 1)).fill(0);
  const span = extent.end - extent.start;
  if (span <= 0) return out;
  const width = span / out.length;
  for (const track of tracks) {
    for (const mark of track.spans) {
      const from = Math.max(0, Math.floor((mark.start - extent.start) / width));
      const to = Math.min(out.length - 1, Math.floor((mark.end - extent.start) / width));
      const covered = Math.max(1, to - from + 1);
      const share = mark.weight / covered;
      for (let i = from; i <= to; i += 1) out[i] = (out[i] ?? 0) + share;
    }
  }
  return out;
}

/** Highest number of marks alive at once in a track — the honest headline for
 * "how tangled is this lane". */
export function peakConcurrency(track: LoomTrack): number {
  const edges: Array<[number, number]> = [];
  for (const span of track.spans) {
    edges.push([span.start, 1], [Math.max(span.end, span.start), -1]);
  }
  edges.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  let live = 0;
  let peak = 0;
  for (const [, delta] of edges) {
    live += delta;
    if (live > peak) peak = live;
  }
  return peak;
}

/* -------------------------------------------------------------------------
 * Formatting
 * ---------------------------------------------------------------------- */

/**
 * A span length in Loom's short vocabulary. The unit is in the name on
 * purpose: Brain's `activitySummary.ts` exports `formatDurationMs`, and both
 * take a bare `number`, so a bare `formatDuration` would let either module's
 * formatter be imported into the other's call sites and print a duration off
 * by 1000x with no type error — a wrong number on screen rather than a build
 * failure. Loom's clock is epoch seconds throughout (`formatMoment` multiplies
 * by 1000 to build a `Date`, and `weave.ts` clamps spans against 3600).
 */
export function formatDurationSeconds(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '—';
  if (seconds < 90) return `${Math.round(seconds)}s`;
  if (seconds < 5400) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 36 * 3600) {
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
  }
  return `${Math.round(seconds / 86_400)}d`;
}

export function formatMoment(epochSeconds: number): string {
  return new Date(epochSeconds * 1000).toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}
