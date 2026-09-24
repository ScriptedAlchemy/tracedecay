import type { AnalyticsSubagentNodeV1 } from '../../contracts/generated.ts';
import type { DelegationTopologyModel, TopologyMark } from './delegationTopology.ts';

/**
 * The delegation timeline: the fitted topology re-read with recorded time on
 * x and the hierarchy on y.
 *
 * Rows are the tree in pre-order, a parent directly above its children, so y
 * is the hierarchy and nothing else. Each top opens a lane. X is the store's
 * own `started_at`/`ended_at` in Unix seconds and nothing is interpolated:
 *
 *   - a session with no recorded start has no x and is counted as unplaced;
 *   - a session with a start and no end is open, its bar runs to the reading's
 *     last recorded instant and is marked open rather than given an end;
 *   - a spawn bracket joins a drawn parent to a child at the child's recorded
 *     start. The parent/child relation is the daemon's; the instant is the
 *     child's own.
 *   - a join bracket marks the child's recorded end on the parent's row. The
 *     store records that the child ended, not that it reported back, so the
 *     join is an inference from the end time and is drawn as one.
 *
 * Lane density is concurrency over sessions whose start and end are both
 * recorded; open and unplaced sessions are counted beside it, never guessed
 * into it.
 */

export interface TimelineRow {
  readonly mark: TopologyMark;
  readonly index: number;
  readonly lane: number;
  /** Seconds, or null when the reading recorded no start. */
  readonly start: number | null;
  /** Seconds, or null when no end is recorded. */
  readonly end: number | null;
}

export interface TimelineBracket {
  readonly kind: 'spawn' | 'join';
  readonly parentRow: number;
  readonly childRow: number;
  readonly at: number;
}

export interface TimelineDensityStep {
  readonly at: number;
  readonly count: number;
}

export interface TimelineLane {
  readonly index: number;
  readonly topId: string;
  readonly firstRow: number;
  readonly rows: number;
  /** Concurrency steps: from `at` until the next step, `count` sessions ran. */
  readonly density: readonly TimelineDensityStep[];
  readonly peak: number;
  /** Sessions in the lane with both ends recorded, the population `density` counts. */
  readonly measured: number;
  /** Sessions in the lane left out of `density` because an end is missing. */
  readonly unmeasured: number;
}

export interface DelegationTimelineModel {
  readonly rows: readonly TimelineRow[];
  readonly brackets: readonly TimelineBracket[];
  readonly lanes: readonly TimelineLane[];
  /** The recorded extent in seconds, or null when nothing drawn has a start. */
  readonly domain: { readonly start: number; readonly end: number } | null;
  readonly unplaced: number;
  readonly open: number;
}

function span(nodes: readonly AnalyticsSubagentNodeV1[]): { start: number | null; end: number | null } {
  let start: number | null = null;
  let end: number | null = null;
  let allEnded = true;
  for (const node of nodes) {
    if (node.started_at != null) start = start === null ? node.started_at : Math.min(start, node.started_at);
    if (node.ended_at == null) allEnded = false;
    else end = end === null ? node.ended_at : Math.max(end, node.ended_at);
  }
  return { start, end: allEnded ? end : null };
}

function markNodes(mark: TopologyMark): readonly AnalyticsSubagentNodeV1[] {
  return mark.kind === 'bundle' ? mark.members : [mark.node];
}

function density(intervals: readonly (readonly [number, number])[]): TimelineDensityStep[] {
  const events: [number, number][] = [];
  for (const [start, end] of intervals) {
    events.push([start, 1], [end, -1]);
  }
  // Ends before starts at one instant: a hand-over is not an overlap.
  events.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  const steps: TimelineDensityStep[] = [];
  let count = 0;
  for (const [at, delta] of events) {
    count += delta;
    const last = steps[steps.length - 1];
    if (last !== undefined && last.at === at) steps[steps.length - 1] = { at, count };
    else steps.push({ at, count });
  }
  return steps;
}

export function layoutDelegationTimeline(model: DelegationTopologyModel): DelegationTimelineModel {
  const children = new Map<string | null, TopologyMark[]>();
  for (const mark of model.marks) {
    const bucket = children.get(mark.parentId);
    if (bucket) bucket.push(mark);
    else children.set(mark.parentId, [mark]);
  }
  for (const bucket of children.values()) bucket.sort((a, b) => a.row - b.row);

  const rows: TimelineRow[] = [];
  const rowOf = new Map<string, number>();
  const lanes: TimelineLane[] = [];
  const visit = (mark: TopologyMark, lane: number) => {
    const { start, end } = span(markNodes(mark));
    const index = rows.length;
    rows.push({ mark, index, lane, start, end });
    rowOf.set(mark.id, index);
    for (const child of children.get(mark.id) ?? []) visit(child, lane);
  };
  for (const top of children.get(null) ?? []) {
    const firstRow = rows.length;
    visit(top, lanes.length);
    const laneRows = rows.slice(firstRow);
    const intervals: [number, number][] = [];
    let unmeasured = 0;
    for (const row of laneRows) {
      for (const node of markNodes(row.mark)) {
        if (node.started_at != null && node.ended_at != null && node.ended_at >= node.started_at) {
          intervals.push([node.started_at, node.ended_at]);
        } else {
          unmeasured += 1;
        }
      }
    }
    const steps = density(intervals);
    lanes.push({
      index: lanes.length,
      topId: top.id,
      firstRow,
      rows: laneRows.length,
      density: steps,
      peak: steps.reduce((max, step) => Math.max(max, step.count), 0),
      measured: intervals.length,
      unmeasured,
    });
  }

  const brackets: TimelineBracket[] = [];
  for (const row of rows) {
    if (row.mark.kind !== 'session' || row.mark.parentId === null) continue;
    const parentRow = rowOf.get(row.mark.parentId);
    if (parentRow === undefined) continue;
    if (row.start !== null) brackets.push({ kind: 'spawn', parentRow, childRow: row.index, at: row.start });
    if (row.end !== null) brackets.push({ kind: 'join', parentRow, childRow: row.index, at: row.end });
  }

  let start = Number.POSITIVE_INFINITY;
  let end = Number.NEGATIVE_INFINITY;
  for (const row of rows) {
    if (row.start === null) continue;
    start = Math.min(start, row.start);
    end = Math.max(end, row.end ?? row.start);
  }
  return {
    rows,
    brackets,
    lanes,
    domain: Number.isFinite(start) ? { start, end: Math.max(end, start + 1) } : null,
    unplaced: rows.filter((row) => row.start === null).length,
    open: rows.filter((row) => row.start !== null && row.end === null).length,
  };
}

const TICK_STEPS = [1, 5, 10, 15, 30, 60, 300, 600, 900, 1_800, 3_600, 7_200, 10_800, 21_600, 43_200, 86_400, 172_800, 604_800];

/** Tick instants for a domain drawn `width` pixels wide, at least `minPitch`
 * apart, the pitch the widest label this step prints needs. */
export function timelineTicks(
  domain: { start: number; end: number },
  width: number,
  minPitch = 96,
): { step: number; ticks: number[] } {
  const seconds = domain.end - domain.start;
  const step =
    TICK_STEPS.find((candidate) => (candidate / seconds) * width >= minPitch) ??
    TICK_STEPS[TICK_STEPS.length - 1]!;
  const ticks: number[] = [];
  for (let at = Math.ceil(domain.start / step) * step; at <= domain.end; at += step) ticks.push(at);
  return { step, ticks };
}

/** A tick label in UTC: the time of day under a day, the date from a day up. */
export function timelineTickLabel(at: number, step: number): string {
  const iso = new Date(at * 1000).toISOString();
  if (step >= 86_400) return iso.slice(5, 10);
  if (step < 60) return iso.slice(11, 19);
  return iso.slice(11, 16);
}
