import { describe, expect, it } from 'vitest';
import type {
  AnalyticsSubagentNodeV1,
  AnalyticsSubagentTreePayloadV1,
} from '../../contracts/generated.ts';
import { ringCoreAlpha, ringKind, ringRadius } from './delegationRings.tsx';
import { timelineRecency } from './DelegationTimeline.tsx';
import { layoutDelegationTimeline, timelineTickLabel, timelineTicks } from './delegationTimeline.ts';
import { fitDelegationTopology, subtree, type TopologySessionMark } from './delegationTopology.ts';

/**
 * The ring and timeline views re-read one fitted topology. The
 * reading is the story fixture's shape: a three-level Codex tree whose
 * grandchild never recorded an end, a Claude session whose parent was never
 * ingested, and one flat Cursor session.
 */

const T0 = 1_760_000_000;

function node(overrides: Partial<AnalyticsSubagentNodeV1> & { session_id: string; depth: number }): AnalyticsSubagentNodeV1 {
  return {
    provider: 'codex',
    parent_session_id: null,
    agent: 'Codex',
    title: null,
    started_at: T0,
    ended_at: T0 + 100,
    is_subagent: overrides.depth > 0,
    parent_tool_use_id: null,
    descendants: 0,
    link: overrides.depth > 0 ? 'linked' : 'root',
    ...overrides,
  };
}

const READING: AnalyticsSubagentTreePayloadV1 = {
  available: true,
  source: 'sessions',
  error: null,
  nodes: [
    node({ session_id: 'root', depth: 0, descendants: 2, started_at: T0, ended_at: T0 + 3_600 }),
    node({ session_id: 'child', depth: 1, descendants: 1, parent_session_id: 'root', started_at: T0 + 600, ended_at: T0 + 2_000 }),
    node({ session_id: 'grandchild', depth: 2, parent_session_id: 'child', started_at: T0 + 900, ended_at: null }),
    node({
      session_id: 'orphan',
      depth: 0,
      provider: 'claude',
      agent: 'Claude',
      is_subagent: true,
      link: 'missing_parent',
      parent_session_id: 'never-ingested',
      started_at: T0 + 1_000,
      ended_at: T0 + 1_500,
    }),
    node({ session_id: 'solo', depth: 0, provider: 'cursor', agent: 'Cursor', started_at: T0 + 2_400, ended_at: T0 + 2_500 }),
  ],
  sessions_read: 5,
  root_count: 2,
  edge_count: 2,
  max_depth: 2,
  missing_parent_count: 1,
  cycle_count: 0,
  truncated: false,
};

const { model } = fitDelegationTopology(READING);
const mark = (id: string) => model.marks.find((candidate) => candidate.id.endsWith(`:${id}`))!;

describe('ring marks', () => {
  it('size by sessions beneath on a log band and name the reading kind', () => {
    expect(ringRadius(mark('root'), model.maxDescendants)).toBe(20);
    expect(ringRadius(mark('child'), model.maxDescendants)).toBeCloseTo(14.833, 3);
    expect(ringRadius(mark('grandchild'), model.maxDescendants)).toBe(6);
    expect(['root', 'child', 'grandchild', 'orphan', 'solo'].map((id) => ringKind(mark(id)))).toEqual([
      'origin',
      'delegate',
      'delegate',
      'cut',
      'origin',
    ]);
  });

  it('brighten the core with sessions beneath, and a leaf stays a whisper', () => {
    expect(ringCoreAlpha(mark('root'), model.maxDescendants)).toBe(0.36);
    expect(ringCoreAlpha(mark('child'), model.maxDescendants)).toBe(0.249);
    expect(ringCoreAlpha(mark('grandchild'), model.maxDescendants)).toBe(0.06);
  });

  it('lift a selection with its drawn subtree and nothing else', () => {
    const lifted = [...subtree(model, mark('child').id)].map((id) => id.split(':')[1]);
    expect(lifted).toEqual(['child', 'grandchild']);
    expect(subtree(model, 'codex:not-drawn').size).toBe(0);
  });
});

describe('layoutDelegationTimeline', () => {
  const timeline = layoutDelegationTimeline(model);

  it('reads rows in pre-order, one lane per top', () => {
    expect(timeline.rows.map((row) => [(row.mark as TopologySessionMark).node.session_id, row.lane])).toEqual([
      ['root', 0],
      ['child', 0],
      ['grandchild', 0],
      ['orphan', 1],
      ['solo', 2],
    ]);
    expect(timeline.domain).toEqual({ start: T0, end: T0 + 3_600 });
    expect(timeline.open).toBe(1);
    expect(timeline.unplaced).toBe(0);
  });

  it('brackets spawns at the child start and joins only where an end was recorded', () => {
    expect(timeline.brackets).toEqual([
      { kind: 'spawn', parentRow: 0, childRow: 1, at: T0 + 600 },
      { kind: 'join', parentRow: 0, childRow: 1, at: T0 + 2_000 },
      { kind: 'spawn', parentRow: 1, childRow: 2, at: T0 + 900 },
    ]);
  });

  it('counts lane concurrency over sessions with both ends, and says what it left out', () => {
    const lane = timeline.lanes[0]!;
    expect(lane.density).toEqual([
      { at: T0, count: 1 },
      { at: T0 + 600, count: 2 },
      { at: T0 + 2_000, count: 1 },
      { at: T0 + 3_600, count: 0 },
    ]);
    expect([lane.peak, lane.measured, lane.unmeasured]).toEqual([2, 2, 1]);
  });

  it('ramps bar fill by recency within the loaded page', () => {
    const domain = timeline.domain!;
    expect(timeline.rows.map((row) => timelineRecency(row, domain))).toEqual([0.4, 0.267, 0.4, 0.225, 0.308]);
  });

  it('spaces ticks by label width and prints them in UTC', () => {
    const ticks = timelineTicks({ start: T0, end: T0 + 3_600 }, 600);
    expect(ticks.step).toBe(600);
    expect(ticks.ticks).toHaveLength(6);
    expect(timelineTickLabel(ticks.ticks[0]!, ticks.step)).toBe('09:00');
  });
});
