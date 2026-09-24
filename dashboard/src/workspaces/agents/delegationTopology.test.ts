import { describe, expect, it } from 'vitest';
import type {
  AnalyticsSubagentNodeV1,
  AnalyticsSubagentTreePayloadV1,
} from '../../contracts/generated.ts';
import {
  FANOUT_LIMIT,
  edgePath,
  fieldSize,
  fitDelegationTopology,
  layoutDelegationTopology,
  markPosition,
  neighbourhood,
  type TopologyBundleMark,
  type TopologySessionMark,
} from './delegationTopology.ts';
import { ringRadius } from './delegationRings.tsx';

function node(overrides: Partial<AnalyticsSubagentNodeV1> & { session_id: string; depth: number }): AnalyticsSubagentNodeV1 {
  return {
    provider: 'codex',
    parent_session_id: null,
    agent: 'Codex',
    title: null,
    started_at: 1_760_000_000,
    ended_at: 1_760_000_100,
    is_subagent: overrides.depth > 0,
    parent_tool_use_id: overrides.depth > 0 ? `toolu_${overrides.session_id}` : null,
    descendants: 0,
    link: overrides.depth > 0 ? 'linked' : 'root',
    ...overrides,
  };
}

function payload(nodes: AnalyticsSubagentNodeV1[]): AnalyticsSubagentTreePayloadV1 {
  return {
    available: true,
    source: 'sessions',
    error: null,
    nodes,
    sessions_read: nodes.length,
    root_count: nodes.filter((entry) => entry.link === 'root').length,
    edge_count: nodes.filter((entry) => entry.link === 'linked').length,
    max_depth: Math.max(0, ...nodes.map((entry) => entry.depth)),
    missing_parent_count: nodes.filter((entry) => entry.link === 'missing_parent').length,
    cycle_count: nodes.filter((entry) => entry.link === 'cycle').length,
    truncated: false,
  };
}

/** The daemon fixture shape: a two-level tree, an orphan, and a solo root. */
function fixtureTree(): AnalyticsSubagentTreePayloadV1 {
  return payload([
    node({ session_id: 'root', depth: 0, descendants: 3, title: 'RC sweep' }),
    node({ session_id: 'child-a', depth: 1, parent_session_id: 'root', descendants: 1 }),
    node({ session_id: 'grandchild', depth: 2, parent_session_id: 'child-a', ended_at: null }),
    node({ session_id: 'child-b', depth: 1, parent_session_id: 'root', agent: null, title: 'untitled' }),
    node({
      session_id: 'orphan',
      depth: 0,
      provider: 'claude',
      agent: 'Claude',
      parent_session_id: 'never-ingested',
      parent_tool_use_id: 'toolu_claude_07',
      link: 'missing_parent',
    }),
    node({ session_id: 'solo', depth: 0, provider: 'cursor', agent: 'Cursor' }),
  ]);
}

const sessions = (model: ReturnType<typeof layoutDelegationTopology>) =>
  model.marks.filter((mark): mark is TopologySessionMark => mark.kind === 'session');
const bundles = (model: ReturnType<typeof layoutDelegationTopology>) =>
  model.marks.filter((mark): mark is TopologyBundleMark => mark.kind === 'bundle');

describe('layoutDelegationTopology', () => {
  it('reads generations, edges and cut edges from the pre-order alone', () => {
    const model = layoutDelegationTopology(fixtureTree());

    expect(model.columns).toBe(3);
    expect(model.generations.map((generation) => generation.sessions)).toEqual([3, 2, 1]);
    expect(model.edges.map((edge) => `${edge.from}->${edge.to}`)).toEqual([
      'codex:root->codex:child-a',
      'codex:root->codex:child-b',
      'codex:child-a->codex:grandchild',
    ]);
    expect(model.edges[0]?.toolUseId).toBe('toolu_child-a');
    expect(model.stubs).toEqual([
      {
        id: 'stub:claude:orphan',
        to: 'claude:orphan',
        kind: 'missing_parent',
        parentSessionId: 'never-ingested',
      },
    ]);
    // Every session is drawn or bundled, never both and never neither.
    expect(model.drawnSessions + model.bundledSessions).toBe(model.totalSessions);
    expect(model.bundledSessions).toBe(0);
  });

  it('centres a parent over its children and gives every leaf its own row', () => {
    const model = layoutDelegationTopology(fixtureTree());
    const byId = new Map(model.marks.map((mark) => [mark.id, mark]));

    // Leaves take rows in pre-order: grandchild 0, child-b 1, orphan 2, solo 3.
    expect(byId.get('codex:grandchild')?.row).toBe(0);
    expect(byId.get('codex:child-b')?.row).toBe(1);
    expect(byId.get('claude:orphan')?.row).toBe(2);
    expect(byId.get('cursor:solo')?.row).toBe(3);
    // child-a sits on its only child; root on the mean of its two children.
    expect(byId.get('codex:child-a')?.row).toBe(0);
    expect(byId.get('codex:root')?.row).toBe(0.5);
    expect(model.rows).toBe(4);

    // Marks come back in column-major reading order.
    expect(model.marks.map((mark) => mark.id)).toEqual([
      'codex:root',
      'claude:orphan',
      'cursor:solo',
      'codex:child-a',
      'codex:child-b',
      'codex:grandchild',
    ]);
  });

  it('bundles fan-out past the limit by agent and reconciles the counts', () => {
    const children = Array.from({ length: 12 }, (_, index) =>
      node({
        session_id: `child-${index}`,
        depth: 1,
        parent_session_id: 'root',
        agent: index < 7 ? 'Explorer' : index < 11 ? 'Reviewer' : 'Writer',
        descendants: index === 0 ? 2 : 0,
      }),
    );
    // The first child's two grandchildren must fold in with it.
    const grandchildren = [
      node({ session_id: 'gc-0', depth: 2, parent_session_id: 'child-0', descendants: 1 }),
      node({ session_id: 'gc-1', depth: 3, parent_session_id: 'gc-0' }),
    ];
    const nodes = [
      node({ session_id: 'root', depth: 0, descendants: 14 }),
      children[0]!,
      ...grandchildren,
      ...children.slice(1),
    ];
    const model = layoutDelegationTopology(payload(nodes));

    expect(children.length).toBeGreaterThan(FANOUT_LIMIT);
    const folded = bundles(model);
    expect(folded.map((bundle) => [bundle.label, bundle.sessions, bundle.descendants])).toEqual([
      ['Explorer', 7, 2],
      ['Reviewer', 4, 0],
    ]);
    // The lone Writer stays an individual mark beside the two bundles.
    expect(sessions(model).map((mark) => mark.id)).toEqual(['codex:root', 'codex:child-11']);
    expect(model.drawnSessions).toBe(2);
    expect(model.bundledSessions).toBe(13);
    expect(model.drawnSessions + model.bundledSessions).toBe(model.totalSessions);
    expect(model.columns).toBe(2);
    expect(model.edges.filter((edge) => edge.kind === 'bundle')).toHaveLength(2);
    expect(model.generations[1]).toEqual({ generation: 1, marks: 3, sessions: 1, bundled: 11 });
  });

  it('unfolds an expanded bundle into its members and their subtrees', () => {
    const children = Array.from({ length: 10 }, (_, index) =>
      node({
        session_id: `child-${index}`,
        depth: 1,
        parent_session_id: 'root',
        agent: index < 9 ? 'Explorer' : 'Writer',
        descendants: index === 0 ? 1 : 0,
      }),
    );
    const nodes = [
      node({ session_id: 'root', depth: 0, descendants: 11 }),
      children[0]!,
      node({ session_id: 'gc-0', depth: 2, parent_session_id: 'child-0' }),
      ...children.slice(1),
    ];
    const folded = layoutDelegationTopology(payload(nodes));
    const bundle = bundles(folded)[0];
    expect(bundle?.id).toBe('bundle:codex:root:agent:Explorer');

    const opened = layoutDelegationTopology(payload(nodes), { expanded: new Set([bundle!.id]) });
    expect(bundles(opened)).toHaveLength(0);
    expect(opened.drawnSessions).toBe(nodes.length);
    expect(opened.bundledSessions).toBe(0);
    expect(opened.columns).toBe(3);
    expect(opened.edges.some((edge) => edge.to === 'codex:gc-0')).toBe(true);
    // The parent remembers what was opened beneath it, so it can be folded.
    const root = sessions(opened).find((mark) => mark.id === 'codex:root');
    expect(root?.openedBundles).toEqual([
      { id: 'bundle:codex:root:agent:Explorer', label: 'Explorer', sessions: 9 },
    ]);
    expect(opened.openedTopBundles).toEqual([]);
  });

  it('reports an opened top-level bundle on the model, since it has no parent mark', () => {
    const tops = Array.from({ length: 9 }, (_, index) =>
      node({ session_id: `top-${index}`, depth: 0, agent: 'Codex' }),
    );
    const folded = layoutDelegationTopology(payload(tops));
    expect(folded.marks).toHaveLength(1);
    const opened = layoutDelegationTopology(payload(tops), {
      expanded: new Set(['bundle:source:agent:Codex']),
    });
    expect(opened.marks).toHaveLength(9);
    expect(opened.openedTopBundles).toEqual([
      { id: 'bundle:source:agent:Codex', label: 'Codex', sessions: 9 },
    ]);
  });

  it('orders labels by code point, not by locale', () => {
    const children = ['B', 'a', 'ä'].flatMap((label) =>
      Array.from({ length: 3 }, (_, index) =>
        node({ session_id: `${label}-${index}`, depth: 1, parent_session_id: 'root', agent: label }),
      ),
    );
    const model = layoutDelegationTopology(
      payload([node({ session_id: 'root', depth: 0, descendants: 9 }), ...children]),
    );
    expect(bundles(model).map((bundle) => bundle.label)).toEqual(['B', 'a', 'ä']);
  });

  it('folds surplus agent groups into one remainder bundle', () => {
    const children = Array.from({ length: 11 }, (_, index) =>
      node({ session_id: `child-${index}`, depth: 1, parent_session_id: 'root', agent: `Agent ${index}` }),
    );
    const model = layoutDelegationTopology(
      payload([node({ session_id: 'root', depth: 0, descendants: 11 }), ...children]),
    );
    const marks = model.marks.filter((mark) => mark.generation === 1);
    expect(marks).toHaveLength(FANOUT_LIMIT);
    const remainder = bundles(model);
    expect(remainder).toHaveLength(1);
    expect(remainder[0]).toMatchObject({
      basis: 'remainder',
      sessions: 4,
      id: 'bundle:codex:root:remainder',
      label: '4 more sessions',
    });
    expect(model.drawnSessions + model.bundledSessions).toBe(12);
  });

  it('groups unlabelled siblings by provider rather than inventing an agent', () => {
    const children = Array.from({ length: 9 }, (_, index) =>
      node({
        session_id: `child-${index}`,
        depth: 1,
        parent_session_id: 'root',
        agent: null,
        provider: index < 5 ? 'codex' : 'claude',
      }),
    );
    const model = layoutDelegationTopology(
      payload([node({ session_id: 'root', depth: 0, descendants: 9 }), ...children]),
    );
    expect(bundles(model).map((bundle) => [bundle.basis, bundle.label, bundle.sessions])).toEqual([
      ['provider', 'codex', 5],
      ['provider', 'claude', 4],
    ]);
  });

  it('stubs a cycle top and files a depth skip as a top instead of guessing', () => {
    const model = layoutDelegationTopology(
      payload([
        node({ session_id: 'loop', depth: 0, parent_session_id: 'loop', link: 'cycle' }),
        // Depth jumps from 0 to 2 with nothing at depth 1: a contradiction.
        node({ session_id: 'skipped', depth: 2, parent_session_id: 'ghost' }),
      ]),
    );
    expect(model.stubs).toEqual([
      { id: 'stub:codex:loop', to: 'codex:loop', kind: 'cycle', parentSessionId: 'loop' },
    ]);
    expect(sessions(model).map((mark) => [mark.id, mark.parentId, mark.generation])).toEqual([
      ['codex:loop', null, 0],
      ['codex:skipped', null, 2],
    ]);
    expect(model.edges).toEqual([]);
  });

  it('lays out an empty reading as an empty field', () => {
    const model = layoutDelegationTopology(payload([]));
    expect(model).toMatchObject({ marks: [], edges: [], stubs: [], generations: [], rows: 0, columns: 0 });
    expect(fieldSize(model)).toEqual({ width: 176, height: 72 });
  });

  it('folds children past the depth limit into their parent and counts them', () => {
    const model = layoutDelegationTopology(fixtureTree(), { depthLimit: 1 });
    const byId = new Map(model.marks.map((mark) => [mark.id, mark]));
    expect(byId.has('codex:grandchild')).toBe(false);
    expect(byId.get('codex:child-a')).toMatchObject({ drawnChildren: 0, foldedDescendants: 1 });
    expect(byId.get('codex:root')).toMatchObject({ drawnChildren: 2, foldedDescendants: 0 });
    expect(model.columns).toBe(2);
    expect(model.drawnSessions).toBe(5);
    expect(model.bundledSessions).toBe(1);
    expect(model.drawnSessions + model.bundledSessions).toBe(model.totalSessions);
  });

  it('draws a folded session past the limit when the reader expanded it', () => {
    const model = layoutDelegationTopology(fixtureTree(), {
      depthLimit: 1,
      expanded: new Set(['codex:child-a']),
    });
    expect(model.marks.some((mark) => mark.id === 'codex:grandchild')).toBe(true);
    expect(model.bundledSessions).toBe(0);
    const childA = sessions(model).find((mark) => mark.id === 'codex:child-a');
    expect(childA).toMatchObject({ depthOpened: true, foldedDescendants: 0 });
    // A session drawn within the limit is not "opened" even if its id is in
    // the set: nothing about it was the reader's act.
    const root = sessions(model).find((mark) => mark.id === 'codex:root');
    expect(root?.depthOpened).toBe(false);
  });

  it('counts folded sessions from the pre-order, not from a descendants claim', () => {
    const contradictory = payload([
      // Claims eleven beneath it; the pre-order holds one.
      node({ session_id: 'root', depth: 0, descendants: 11 }),
      node({ session_id: 'only', depth: 1, parent_session_id: 'root' }),
    ]);
    const model = layoutDelegationTopology(contradictory, { depthLimit: 0 });
    expect(model.marks[0]).toMatchObject({ id: 'codex:root', foldedDescendants: 1 });
    expect(model.drawnSessions + model.bundledSessions).toBe(2);
  });
});

describe('fitDelegationTopology', () => {
  /** A chain of roots each fanning to three children, deep enough to overflow. */
  function wideDeep(roots: number, depth: number): AnalyticsSubagentTreePayloadV1 {
    const nodes: AnalyticsSubagentNodeV1[] = [];
    for (let root = 0; root < roots; root += 1) {
      const push = (id: string, parent: string | null, level: number) => {
        nodes.push(
          node({
            session_id: id,
            depth: level,
            parent_session_id: parent,
            descendants: 0,
            agent: `Agent ${level}`,
          }),
        );
        if (level < depth) {
          for (let child = 0; child < 3; child += 1) push(`${id}.${child}`, id, level + 1);
        }
      };
      push(`r${root}`, null, 0);
    }
    return payload(nodes);
  }

  it('draws the whole reading when it fits the row budget', () => {
    const fit = fitDelegationTopology(fixtureTree());
    expect(fit.depthLimit).toBe(Number.POSITIVE_INFINITY);
    expect(fit.maxDepth).toBe(2);
    expect(fit.model.bundledSessions).toBe(0);
  });

  it('folds from the deepest generation inward until the rows fit', () => {
    const reading = wideDeep(4, 3);
    // 4 roots × 27 leaves = 108 leaf rows unfolded.
    expect(layoutDelegationTopology(reading).rows).toBe(108);
    const fit = fitDelegationTopology(reading);
    expect(fit.maxDepth).toBe(3);
    expect(fit.depthLimit).toBe(2);
    expect(fit.model.rows).toBe(36);
    expect(fit.model.rows).toBeLessThanOrEqual(40);
    expect(fit.model.drawnSessions + fit.model.bundledSessions).toBe(reading.nodes.length);
    expect(fit.model.marks.every((mark) => mark.generation <= 2)).toBe(true);
  });

  it('keeps a reader-expanded session drawn past the fitted limit', () => {
    const reading = wideDeep(4, 3);
    const fit = fitDelegationTopology(reading, new Set(['codex:r0.0.0']));
    expect(fit.depthLimit).toBe(2);
    // Only the expanded session's children reach generation 3.
    const third = fit.model.marks.filter((mark) => mark.generation === 3);
    expect(third.map((mark) => mark.id)).toEqual([
      'codex:r0.0.0.0',
      'codex:r0.0.0.1',
      'codex:r0.0.0.2',
    ]);
    expect(fit.model.drawnSessions + fit.model.bundledSessions).toBe(reading.nodes.length);
  });

  it('lets the fan-out rule fold a wide top generation before depth folds anything', () => {
    const reading = wideDeep(60, 1);
    const fit = fitDelegationTopology(reading);
    // Sixty roots share one agent label, so they bundle and the field fits
    // without folding a generation.
    expect(fit.depthLimit).toBe(Number.POSITIVE_INFINITY);
    expect(fit.model.marks).toHaveLength(1);
    expect(fit.model.marks[0]).toMatchObject({ kind: 'bundle', sessions: 60, descendants: 180 });
    expect(fit.model.drawnSessions + fit.model.bundledSessions).toBe(reading.nodes.length);
  });
});

describe('topology geometry', () => {
  it('scales ring radii from measured descendants and never below the floor', () => {
    const model = layoutDelegationTopology(fixtureTree());
    const byId = new Map(model.marks.map((mark) => [mark.id, mark]));
    expect(ringRadius(byId.get('codex:root')!, model.maxDescendants)).toBe(20);
    expect(ringRadius(byId.get('codex:grandchild')!, model.maxDescendants)).toBe(6);
    // A solo root with nothing beneath is a leaf, not a source.
    expect(ringRadius(byId.get('cursor:solo')!, model.maxDescendants)).toBe(6);
    const mid = ringRadius(byId.get('codex:child-a')!, model.maxDescendants);
    expect(mid).toBeGreaterThan(6);
    expect(mid).toBeLessThan(20);
  });

  it('draws an edge from trailing edge to leading edge through the mid column', () => {
    const model = layoutDelegationTopology(fixtureTree());
    const byId = new Map(model.marks.map((mark) => [mark.id, mark]));
    const from = markPosition(byId.get('codex:root')!);
    const to = markPosition(byId.get('codex:child-b')!);
    expect(from).toEqual({ x: 88, y: 56 });
    expect(to).toEqual({ x: 264, y: 76 });
    expect(edgePath(from, to, 16, 5)).toBe('M104,56 C181.5,56 181.5,76 259,76');
  });

  it('isolates the path to the top plus the drawn children', () => {
    const model = layoutDelegationTopology(fixtureTree());
    expect([...neighbourhood(model, 'codex:child-a')].sort()).toEqual([
      'codex:child-a',
      'codex:grandchild',
      'codex:root',
    ]);
    expect([...neighbourhood(model, 'cursor:solo')]).toEqual(['cursor:solo']);
  });
});
