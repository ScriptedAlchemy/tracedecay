import { describe, expect, it } from 'vitest';
import type { AnalyticsSubagentNodeV1, AnalyticsSubagentTreePayloadV1 } from '../../contracts/generated.ts';
import {
  groupSubagentTrees,
  subagentElapsedSeconds,
  subagentLabel,
  subagentTreeCensus,
} from './subagentTree.ts';

function node(overrides: Partial<AnalyticsSubagentNodeV1>): AnalyticsSubagentNodeV1 {
  return {
    provider: 'codex',
    session_id: 'session.a',
    parent_session_id: null,
    agent: null,
    title: null,
    started_at: null,
    ended_at: null,
    is_subagent: false,
    parent_tool_use_id: null,
    depth: 0,
    descendants: 0,
    link: 'root',
    ...overrides,
  };
}

function payload(
  nodes: AnalyticsSubagentNodeV1[],
  overrides: Partial<AnalyticsSubagentTreePayloadV1> = {},
): AnalyticsSubagentTreePayloadV1 {
  return {
    available: true,
    source: 'sessions',
    error: null,
    nodes,
    sessions_read: nodes.length,
    root_count: nodes.filter((entry) => entry.link === 'root').length,
    edge_count: nodes.filter((entry) => entry.link === 'linked').length,
    max_depth: nodes.reduce((deepest, entry) => Math.max(deepest, entry.depth), 0),
    missing_parent_count: nodes.filter((entry) => entry.link === 'missing_parent').length,
    cycle_count: nodes.filter((entry) => entry.link === 'cycle').length,
    truncated: false,
    ...overrides,
  };
}

describe('groupSubagentTrees', () => {
  it('splits the pre-order list at every depth-0 entry', () => {
    const groups = groupSubagentTrees([
      node({ session_id: 'a', depth: 0, descendants: 2 }),
      node({ session_id: 'a.1', depth: 1, link: 'linked', parent_session_id: 'a' }),
      node({ session_id: 'a.1.1', depth: 2, link: 'linked', parent_session_id: 'a.1' }),
      node({ session_id: 'b', depth: 0 }),
    ]);

    expect(groups.map((group) => group.top.session_id)).toEqual(['a', 'b']);
    expect(groups[0]?.nodes.map((entry) => entry.session_id)).toEqual(['a', 'a.1', 'a.1.1']);
    expect(groups[1]?.nodes.map((entry) => entry.session_id)).toEqual(['b']);
  });

  it('partitions rather than filters, so no session is lost on the way to the screen', () => {
    const nodes = [
      node({ session_id: 'a', depth: 0 }),
      node({ session_id: 'a.1', depth: 1, link: 'linked' }),
      node({ session_id: 'orphan', depth: 0, link: 'missing_parent' }),
      node({ session_id: 'cycled', depth: 0, link: 'cycle' }),
    ];
    const groups = groupSubagentTrees(nodes);
    const flattened = groups.flatMap((group) => group.nodes.map((entry) => entry.session_id));

    expect(flattened).toEqual(nodes.map((entry) => entry.session_id));
  });

  it('answers an empty reading with no groups', () => {
    expect(groupSubagentTrees([])).toEqual([]);
  });
});

describe('subagentLabel', () => {
  it('prefers the agent, then the title, and never invents a name', () => {
    expect(subagentLabel(node({ agent: 'Codex', title: 'sweep' }))).toBe('Codex');
    expect(subagentLabel(node({ agent: null, title: 'sweep' }))).toBe('sweep');
    expect(subagentLabel(node({ agent: null, title: null, session_id: 'session.z' }))).toBe(
      'session.z',
    );
  });
});

describe('subagentElapsedSeconds', () => {
  it('subtracts the store\'s SECOND stamps without rescaling them', () => {
    expect(
      subagentElapsedSeconds(node({ started_at: 1_760_000_000, ended_at: 1_760_003_600 })),
    ).toBe(3_600);
  });

  it('keeps an unrecorded end apart from a zero-length session', () => {
    expect(subagentElapsedSeconds(node({ started_at: 1_760_000_000, ended_at: null }))).toBeNull();
    expect(subagentElapsedSeconds(node({ started_at: null, ended_at: 1_760_000_000 }))).toBeNull();
    expect(
      subagentElapsedSeconds(node({ started_at: 1_760_000_000, ended_at: 1_760_000_000 })),
    ).toBe(0);
  });

  it('refuses a negative span rather than drawing a backwards duration', () => {
    expect(
      subagentElapsedSeconds(node({ started_at: 1_760_000_100, ended_at: 1_760_000_000 })),
    ).toBeNull();
  });
});

describe('subagentTreeCensus', () => {
  it('carries the daemon\'s counts through without recomputing them', () => {
    const census = subagentTreeCensus(
      payload(
        [
          node({ session_id: 'a', depth: 0 }),
          node({ session_id: 'a.1', depth: 1, link: 'linked' }),
        ],
        { sessions_read: 9, truncated: true },
      ),
    );

    expect(census.nodes).toBe(2);
    // The denominator is the store's, not the drawn row count.
    expect(census.sessionsRead).toBe(9);
    expect(census.edges).toBe(1);
    expect(census.maxDepth).toBe(1);
    expect(census.truncated).toBe(true);
  });

  it('calls a reading flat only when it holds sessions and no edge', () => {
    expect(subagentTreeCensus(payload([node({ session_id: 'a' })])).flat).toBe(true);
    expect(
      subagentTreeCensus(
        payload([node({ session_id: 'a' }), node({ session_id: 'a.1', depth: 1, link: 'linked' })]),
      ).flat,
    ).toBe(false);
    // An empty reading is not flat: there is nothing to be flat about.
    expect(subagentTreeCensus(payload([])).flat).toBe(false);
  });
});
