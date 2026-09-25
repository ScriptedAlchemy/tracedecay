/**
 * Per-lane density summaries over the current window: the aggregation layer
 * a renderer draws when one pixel holds more records than a glyph can carry.
 *
 * Pure arithmetic over the projection and the laid-out model, so every
 * renderer summarizes the same counts. A bin counts only what a record says:
 * a member session whose measured extent covers the bin is `active`; one that
 * began with no recorded end is `open` (begun, extent unknown), never active;
 * `events` are dated records in the bin. Undated records have no bin and are
 * counted only in the totals. Nothing after a dated playback cursor is binned.
 */
import { timeToX } from './layout.ts';
import type {
  JourneyEvent,
  JourneyEventKind,
  JourneyLane,
  JourneyProjection,
  RevealBoundary,
  TemporalSceneModel,
} from './types.ts';

export interface DensityBin {
  readonly x0: number;
  readonly x1: number;
  readonly active: number;
  readonly open: number;
  readonly starts: number;
  readonly events: number;
}

export interface LaneDensity {
  readonly laneId: string;
  readonly bins: readonly DensityBin[];
  readonly peak: { readonly active: number; readonly open: number; readonly events: number };
  /** Exact counts over every member session in the loaded page, not the window. */
  readonly totals: {
    readonly sessions: number;
    readonly messages: number;
    readonly commits: number;
    readonly events: number;
    readonly undated: number;
    readonly openEnded: number;
  };
  /** Smallest pixel gap between two drawn marks sharing a row of the lane
   * (its line or its undated gutter); `Infinity` when no row holds two. */
  readonly minGap: number;
}

export interface SceneDensity {
  readonly binPx: number;
  readonly lanes: ReadonlyMap<string, LaneDensity>;
  /** The oldest session start in the loaded page. */
  readonly headTime: number | null;
  /** The newest recorded time in the loaded page: what `NOW` names. */
  readonly tailTime: number | null;
  /** `tailTime` on the axis, or null when it lies outside the window. */
  readonly tailX: number | null;
}

export interface DensityOptions {
  readonly reveal: RevealBoundary | null;
  readonly hiddenKinds: ReadonlySet<JourneyEventKind>;
}

const DEFAULT_BIN_PX = 6;

/** Newest dated record in the page: a session start or end, a dated event, or
 * an interval end. Null when the page holds no dated record at all. */
export function newestLoadedTime(projection: JourneyProjection): number | null {
  let newest = -Infinity;
  for (const lane of projection.lanes) {
    newest = Math.max(newest, lane.start, lane.end ?? -Infinity);
  }
  for (const event of projection.events) {
    if (event.time !== null) newest = Math.max(newest, event.time);
  }
  for (const interval of projection.intervals) newest = Math.max(newest, interval.end);
  return Number.isFinite(newest) ? newest : null;
}

interface LaneIndex {
  /** Revealed member extents; `end` null when unrecorded. */
  readonly members: readonly { readonly start: number; readonly end: number | null }[];
  /** Revealed dated event times, unsorted. */
  readonly eventTimes: readonly number[];
  readonly totals: LaneDensity['totals'];
}

/** The window-independent half of the density summary: who belongs to each
 * scene lane and which of their records are revealed. Rebuilt only when the
 * loaded page, the bundle membership, the filters or the cursor change. */
export interface DensityIndex {
  readonly lanes: ReadonlyMap<string, LaneIndex>;
  readonly revealTime: number | null;
  readonly headTime: number | null;
  readonly tailTime: number | null;
}

/** Identity of the scene's lane and bundle membership; equal across window
 * changes, different once a branch opens or closes. */
export function membershipKey(model: TemporalSceneModel): string {
  return `${model.lanes.map((lane) => lane.id).join('\n')}\u0000${model.clusters.map((cluster) => cluster.laneId).join('\n')}`;
}

export function densityIndex(projection: JourneyProjection, model: TemporalSceneModel, options: DensityOptions): DensityIndex {
  const { reveal, hiddenKinds } = options;
  const revealTime = reveal !== null ? reveal.time : null;
  const laneById = new Map(projection.lanes.map((lane) => [lane.id, lane] as const));
  const eventsByLane = new Map<string, JourneyEvent[]>();
  for (const event of projection.events) {
    const bucket = eventsByLane.get(event.laneId);
    if (bucket) bucket.push(event);
    else eventsByLane.set(event.laneId, [event]);
  }
  const membersOf = new Map<string, readonly string[]>();
  for (const cluster of model.clusters) membersOf.set(cluster.laneId, cluster.memberLaneIds);
  const withheld = (event: JourneyEvent): boolean => {
    if (reveal === null) return false;
    if (event.time !== null && revealTime !== null && event.time > revealTime) return true;
    return event.laneId === reveal.laneId && event.source === 'transcript' && event.sequence > reveal.sequence;
  };
  const lanes = new Map<string, LaneIndex>();
  for (const sceneLane of model.lanes) {
    const memberIds = [sceneLane.id, ...(membersOf.get(sceneLane.id) ?? [])];
    const members = memberIds.map((id) => laneById.get(id)).filter((lane): lane is JourneyLane => lane !== undefined);
    const extents: { start: number; end: number | null }[] = [];
    const eventTimes: number[] = [];
    let messages = 0;
    let commits = 0;
    let events = 0;
    let undated = 0;
    let openEnded = 0;
    for (const member of members) {
      messages += member.messages;
      if (member.end === null) openEnded += 1;
      for (const event of eventsByLane.get(member.id) ?? []) {
        if (hiddenKinds.has(event.kind)) continue;
        events += 1;
        if (event.kind === 'commit') commits += 1;
        if (event.time === null) undated += 1;
        else if (!withheld(event)) eventTimes.push(event.time);
      }
      if (revealTime === null || member.start <= revealTime) extents.push({ start: member.start, end: member.end });
    }
    lanes.set(sceneLane.id, {
      members: extents,
      eventTimes,
      totals: { sessions: members.length, messages, commits, events, undated, openEnded },
    });
  }
  const headTime = projection.lanes.reduce<number | null>(
    (oldest, lane) => (oldest === null || lane.start < oldest ? lane.start : oldest),
    null,
  );
  return { lanes, revealTime, headTime, tailTime: newestLoadedTime(projection) };
}

/** Bins an index over the model's window, and measures mark spacing. */
export function layoutDensity(index: DensityIndex, model: TemporalSceneModel, binPx = DEFAULT_BIN_PX): SceneDensity {
  const { viewport } = model;
  const pitch = Math.max(1, binPx);
  const fieldX0 = viewport.left;
  const fieldX1 = viewport.width - viewport.right;
  const binCount = Math.max(1, Math.ceil((fieldX1 - fieldX0) / pitch));
  const { revealTime } = index;
  /** Bin index containing `time`, clamped; null when outside the window. */
  const binOf = (time: number): number | null => {
    const x = timeToX(viewport, time);
    if (x < fieldX0 || x > fieldX1) return null;
    return Math.min(binCount - 1, Math.floor((x - fieldX0) / pitch));
  };
  const revealBin = revealTime === null ? binCount - 1 : binOf(revealTime) ?? (revealTime < viewport.window.start ? -1 : binCount - 1);
  // Marks collide only within one row: the lane's own line or its undated gutter.
  const rowXs = new Map<string, Map<number, number[]>>();
  for (const node of model.nodes) {
    let rows = rowXs.get(node.laneId);
    if (!rows) rowXs.set(node.laneId, (rows = new Map()));
    const bucket = rows.get(node.y);
    if (bucket) bucket.push(node.x);
    else rows.set(node.y, [node.x]);
  }

  const lanes = new Map<string, LaneDensity>();
  for (const sceneLane of model.lanes) {
    const laneIndex = index.lanes.get(sceneLane.id);
    if (!laneIndex) continue;
    const active = new Array<number>(binCount).fill(0);
    const open = new Array<number>(binCount).fill(0);
    const starts = new Array<number>(binCount).fill(0);
    const events = new Array<number>(binCount).fill(0);
    for (const time of laneIndex.eventTimes) {
      const bin = binOf(time);
      if (bin !== null && bin <= revealBin) events[bin] = (events[bin] ?? 0) + 1;
    }
    for (const member of laneIndex.members) {
      const startBin = binOf(member.start);
      if (startBin !== null && startBin <= revealBin) starts[startBin] = (starts[startBin] ?? 0) + 1;
      const first = member.start < viewport.window.start ? 0 : startBin;
      if (first === null) continue;
      if (member.end === null) {
        for (let bin = first; bin <= revealBin; bin += 1) open[bin] = (open[bin] ?? 0) + 1;
        continue;
      }
      const last = member.end > viewport.window.end ? binCount - 1 : binOf(member.end);
      if (last === null) continue;
      for (let bin = first; bin <= Math.min(last, revealBin); bin += 1) active[bin] = (active[bin] ?? 0) + 1;
    }
    const bins: DensityBin[] = active.map((count, bin) => ({
      x0: fieldX0 + bin * pitch,
      x1: Math.min(fieldX1, fieldX0 + (bin + 1) * pitch),
      active: count,
      open: open[bin] ?? 0,
      starts: starts[bin] ?? 0,
      events: events[bin] ?? 0,
    }));
    let minGap = Infinity;
    for (const row of rowXs.get(sceneLane.id)?.values() ?? []) {
      const xs = [...row].sort((a, b) => a - b);
      for (let i = 1; i < xs.length; i += 1) minGap = Math.min(minGap, xs[i]! - xs[i - 1]!);
    }
    lanes.set(sceneLane.id, {
      laneId: sceneLane.id,
      bins,
      peak: { active: Math.max(0, ...active), open: Math.max(0, ...open), events: Math.max(0, ...events) },
      totals: laneIndex.totals,
      minGap,
    });
  }

  const tailX = index.tailTime === null ? null : timeToX(viewport, index.tailTime);
  return {
    binPx: pitch,
    lanes,
    headTime: index.headTime,
    tailTime: index.tailTime,
    tailX: tailX !== null && tailX >= fieldX0 && tailX <= fieldX1 ? tailX : null,
  };
}
