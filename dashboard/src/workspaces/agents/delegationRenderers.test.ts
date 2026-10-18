import { describe, expect, it } from 'vitest';
import type {
  AnalyticsSubagentNodeV1,
  AnalyticsSubagentTreePayloadV1,
} from '../../contracts/generated.ts';
import { ringCoreAlpha, ringKind, ringRadius } from './delegationRings.tsx';
import { fitDelegationTopology, subtree, type TopologySessionMark } from './delegationTopology.ts';

/**
 * The ring view re-reads one fitted topology. The
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
