/**
 * Layout for the TRANSIT MAP candidate (plan 11b Surface 2, inside Trace).
 *
 * Columns are hop distance (callers' side left, callees' side right); bands
 * are the MEASURED dependency depth of each symbol's FILE from the strata
 * read, depth 0 at the top. Symbols are stations in their file's band, and
 * each drawn channel is a line routed through the corridor between two
 * columns, parallel runs bundled lane by lane. A call into a shallower file
 * climbs: it is drawn straight up in the error hue, and the caption says a
 * climb is a boundary-crossing observation, not proof of a bug.
 *
 * A depth the strata read did not measure is never guessed: that station
 * sits in a hatched DEPTH UNMEASURED band, and its lines are dashed because
 * nobody can say whether they climb. Depths the neighbourhood never enters
 * are hatched NO STATION bands, so a skipped layer is visible as skipped.
 *
 * Pure and DOM-free.
 */
import type { TraceChannel, TraceModel, TraceNode } from './types.ts';
import { channelKey } from './variants.ts';

export interface TransitDepths {
  /** Measured file depth by path, or null when strata is not measured. */
  readonly byPath: ReadonlyMap<string, number> | null;
  readonly maxDepth: number | null;
  /** The scan was capped, so every depth is a floor. */
  readonly floor: boolean;
}

export type TransitBandKind = 'station' | 'empty' | 'unmeasured';

export interface TransitBand {
  readonly key: string;
  readonly kind: TransitBandKind;
  readonly depth: number | null;
  readonly y: number;
  readonly height: number;
}

export interface TransitStation {
  readonly node: TraceNode;
  readonly x: number;
  readonly y: number;
  readonly depth: number | null;
  readonly column: number;
}

export type TransitLineKind = 'descend' | 'level' | 'climb' | 'unmeasured';

export interface TransitLine {
  readonly key: string;
  readonly channel: TraceChannel;
  readonly kind: TransitLineKind;
  /** Callee depth minus caller depth; null when either end is unmeasured. */
  readonly delta: number | null;
  readonly d: string;
  readonly width: number;
}

export interface TransitGap {
  readonly x: number;
  readonly deltas: readonly number[];
  readonly climbs: number;
  readonly unmeasured: number;
}

export interface TransitLayout {
  readonly width: number;
  readonly height: number;
  readonly compact: boolean;
  readonly gutter: number;
  readonly columns: readonly { ring: number; x: number; title: string }[];
  readonly bands: readonly TransitBand[];
  readonly stations: readonly TransitStation[];
  readonly lines: readonly TransitLine[];
  readonly gaps: readonly TransitGap[];
  readonly rulerY: number;
  readonly labelChars: number;
}

/** Below this width station names are shown on inspect only. */
export const TRANSIT_COMPACT_BELOW = 560;
const HEAD = 34;
const STACK = 22;
const EMPTY_BAND = 20;
const LANE = 3.5;
const CHAR_PX = 6.7;

export function lineWidth(calls: number): number {
  return 1.25 + Math.sqrt(Math.max(0, calls)) * 0.55;
}

function columnTitle(ring: number): string {
  if (ring === 0) return 'focus';
  const hops = Math.abs(ring);
  return `${hops} ${hops === 1 ? 'hop' : 'hops'} ${ring < 0 ? 'up' : 'down'}`;
}

export function layoutTransit(
  model: TraceModel,
  depths: TransitDepths,
  width: number,
): TransitLayout {
  const W = Math.max(240, width);
  const compact = W < TRANSIT_COMPACT_BELOW;
  const gutter = compact ? 44 : 92;
  const rings = [...new Set(model.nodes.map((node) => node.ring))].sort((a, b) => a - b);
  const colW = (W - gutter - 12) / rings.length;
  const columns = rings.map((ring, i) => ({
    ring,
    x: Math.round(gutter + colW * i + (compact ? colW / 2 : 14)),
    title: columnTitle(ring),
  }));
  const colOf = new Map(rings.map((ring, i) => [ring, i]));

  const depthOf = (node: TraceNode): number | null =>
    node.filePath === null || depths.byPath === null ? null : (depths.byPath.get(node.filePath) ?? null);

  /* ---- bands ------------------------------------------------------------ */
  const cells = new Map<string, TraceNode[]>();
  const cellKey = (depth: number | null, column: number) => `${depth ?? 'u'}:${column}`;
  const callSites = new Map<string, number>();
  for (const channel of model.channels) {
    callSites.set(channel.a, (callSites.get(channel.a) ?? 0) + channel.calls);
    callSites.set(channel.b, (callSites.get(channel.b) ?? 0) + channel.calls);
  }
  for (const node of [...model.nodes].sort(
    (a, b) => (callSites.get(b.id) ?? 0) - (callSites.get(a.id) ?? 0) || a.name.localeCompare(b.name),
  )) {
    const key = cellKey(depthOf(node), colOf.get(node.ring)!);
    cells.set(key, [...(cells.get(key) ?? []), node]);
  }
  const stackIn = (depth: number | null) =>
    Math.max(0, ...rings.map((_, c) => cells.get(cellKey(depth, c))?.length ?? 0));

  const measured = model.nodes.map(depthOf).filter((d): d is number => d !== null);
  const maxDepth = depths.maxDepth ?? (measured.length ? Math.max(...measured) : null);
  const bands: TransitBand[] = [];
  let y = HEAD;
  if (maxDepth !== null && depths.byPath !== null) {
    for (let depth = 0; depth <= maxDepth; depth += 1) {
      const stack = stackIn(depth);
      const height = stack === 0 ? EMPTY_BAND : 14 + stack * STACK;
      bands.push({ key: `d${depth}`, kind: stack === 0 ? 'empty' : 'station', depth, y, height });
      y += height;
    }
  }
  const unmeasuredStack = stackIn(null);
  if (unmeasuredStack > 0) {
    y += 6;
    bands.push({ key: 'u', kind: 'unmeasured', depth: null, y, height: 30 + unmeasuredStack * STACK });
    y += 30 + unmeasuredStack * STACK;
  }

  /* ---- stations --------------------------------------------------------- */
  const bandY = new Map(bands.map((band) => [band.depth ?? 'u', band]));
  const stations: TransitStation[] = [];
  for (const [key, nodes] of cells) {
    const [depthText, columnText] = key.split(':');
    const depth = depthText === 'u' ? null : Number(depthText);
    const band = bandY.get(depth ?? 'u')!;
    const column = Number(columnText);
    nodes.forEach((node, k) => {
      stations.push({
        node,
        x: columns[column]!.x,
        y: band.y + (band.kind === 'unmeasured' ? 28 : 12) + k * STACK + 6,
        depth,
        column,
      });
    });
  }
  const stationOf = new Map(stations.map((station) => [station.node.id, station]));

  /* ---- lines ------------------------------------------------------------ */
  // Corridor for a line: the gap to the right of its left end's column,
  // hugging the next column so station labels keep their side of the gap.
  const gapLines = new Map<number, Array<{ channel: TraceChannel; top: number; bottom: number }>>();
  const gapIndex = (channel: TraceChannel): number => {
    const a = stationOf.get(channel.a)!;
    const b = stationOf.get(channel.b)!;
    const left = Math.min(a.column, b.column);
    return a.column === b.column && left === columns.length - 1 ? left - 1 : left;
  };
  for (const channel of model.channels) {
    const a = stationOf.get(channel.a);
    const b = stationOf.get(channel.b);
    if (!a || !b) continue;
    const gap = gapIndex(channel);
    const list = gapLines.get(gap) ?? [];
    list.push({ channel, top: Math.min(a.y, b.y), bottom: Math.max(a.y, b.y) });
    gapLines.set(gap, list);
  }
  const laneX = new Map<string, number>();
  for (const [gap, list] of gapLines) {
    // Lines that span less sit nearer their stations, so short runs do not
    // cross long ones: sort by top, then by span.
    list.sort((p, q) => p.top - q.top || p.bottom - q.bottom);
    const right = columns[gap + 1]?.x ?? columns[gap]!.x + colW;
    const edge = right - (compact ? colW / 2 - 6 : 14);
    // Lanes compress rather than spill out of their corridor into a column.
    const pitch = Math.min(LANE, (compact ? colW - 12 : colW * 0.4) / Math.max(1, list.length));
    list.forEach((entry, i) => {
      laneX.set(channelKey(entry.channel), edge - (list.length - i) * pitch);
    });
  }

  const lines: TransitLine[] = [];
  const gapAcc = columns.slice(0, -1).map(() => ({ deltas: [] as number[], climbs: 0, unmeasured: 0 }));
  const r = (n: number) => Math.round(n * 10) / 10;
  for (const channel of model.channels) {
    const caller = stationOf.get(channel.a);
    const callee = stationOf.get(channel.b);
    if (!caller || !callee) continue;
    const key = channelKey(channel);
    const delta =
      caller.depth === null || callee.depth === null ? null : callee.depth - caller.depth;
    const kind: TransitLineKind =
      delta === null ? 'unmeasured' : delta < 0 ? 'climb' : delta === 0 ? 'level' : 'descend';
    const [left, right] = caller.x <= callee.x ? [caller, callee] : [callee, caller];
    const lane = laneX.get(key)!;
    let d: string;
    if (left.column === right.column) {
      d = `M${left.x},${left.y} H${r(lane)} V${right.y} H${right.x}`;
    } else if (kind === 'climb' || left.y === right.y) {
      d = `M${left.x},${left.y} H${r(lane)} V${right.y} H${right.x}`;
    } else {
      // Ease across the corridor at no steeper than 45°, so a climb, drawn
      // straight up, is the steepest thing on the map.
      const run = Math.min(Math.abs(right.y - left.y) / 2, 16);
      d = `M${left.x},${left.y} H${r(lane - run)} L${r(lane + run)},${right.y} H${right.x}`;
      if (lane + run > right.x) d = `M${left.x},${left.y} H${r(lane)} V${right.y} H${right.x}`;
    }
    lines.push({ key, channel, kind, delta, d, width: lineWidth(channel.calls) });
    const gap = gapAcc[gapIndex(channel)];
    if (gap) {
      if (delta === null) gap.unmeasured += 1;
      else gap.deltas.push(delta);
      if (kind === 'climb') gap.climbs += 1;
    }
  }
  // Heaviest last, so a one-site line never hides the trunk it crosses.
  lines.sort((p, q) => p.channel.calls - q.channel.calls);

  const gaps: TransitGap[] = gapAcc.map((acc, i) => ({
    ...acc,
    x: (columns[i]!.x + columns[i + 1]!.x) / 2,
  }));
  const labelChars = Math.max(6, Math.floor((colW - 44) / CHAR_PX));
  return {
    width: W,
    height: y + 62,
    compact,
    gutter,
    columns,
    bands,
    stations,
    lines,
    gaps,
    rulerY: y + 18,
    labelChars,
  };
}

/** The foot-ruler reading for one corridor. */
export function gapReading(gap: TransitGap): string {
  const parts: string[] = [];
  if (gap.deltas.length > 0) {
    const low = Math.min(...gap.deltas);
    const high = Math.max(...gap.deltas);
    const sign = (n: number) => (n > 0 ? `+${n}` : String(n));
    parts.push(low === high ? `Δ ${sign(low)}` : `Δ ${sign(low)}…${sign(high)}`);
  }
  if (gap.climbs > 0) parts.push(`${gap.climbs} ${gap.climbs === 1 ? 'climb' : 'climbs'}`);
  if (gap.unmeasured > 0) parts.push(`${gap.unmeasured} unmeasured`);
  return parts.length ? parts.join(' · ') : 'no lines';
}
