import { describe, expect, it } from 'vitest';
import type {
  AnalyticsSubagentNodeV1,
  AnalyticsSubagentTreePayloadV1,
  FeedbackProximityEncounterV1,
  LcmMessageV1,
  LoomBranchSpanV1,
  LoomCommitV1,
  LoomSessionRowV1,
  LoomTemporalPayloadV1,
} from '../../contracts/generated.ts';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { laneIdOf, orderMessages, projectJourney, type JourneySources } from './journey.ts';

/**
 * The projection runs in the plain `node` project: it joins wire rows and
 * decides nothing about pixels, so every honesty rule below is checked on the
 * joined records themselves.
 */

const T0 = 1_784_700_000;

function session(over: Partial<LoomSessionRowV1> = {}): LoomSessionRowV1 {
  return {
    session_id: 'root',
    provider: 'cursor',
    title: null,
    started_at: T0,
    ended_at: null,
    last_message_at: null,
    messages: 10,
    is_subagent: false,
    edited_files_recorded: false,
    models: [],
    ...over,
  };
}

function node(over: Partial<AnalyticsSubagentNodeV1> = {}): AnalyticsSubagentNodeV1 {
  return {
    agent: null,
    depth: 0,
    descendants: 0,
    ended_at: null,
    is_subagent: false,
    link: 'root',
    parent_session_id: null,
    parent_tool_use_id: null,
    provider: 'cursor',
    session_id: 'root',
    started_at: T0,
    title: null,
    ...over,
  };
}

function tree(
  nodes: AnalyticsSubagentNodeV1[],
  over: Partial<AnalyticsSubagentTreePayloadV1> = {},
): AnalyticsSubagentTreePayloadV1 {
  return {
    available: true,
    cycle_count: 0,
    edge_count: 0,
    error: null,
    max_depth: 0,
    missing_parent_count: 0,
    nodes,
    root_count: 0,
    sessions_read: nodes.length,
    source: 'test',
    truncated: false,
    ...over,
  };
}

function commit(over: Partial<LoomCommitV1> = {}): LoomCommitV1 {
  return {
    branch: 'master',
    commit_sha: 'abcdef1234567890',
    committed_at: T0 + 600,
    confidence: 0.83,
    evidence: 'transcript',
    provider: 'cursor',
    relation: 'authored',
    session_id: 'root',
    span_overlap_kind: null,
    worktree: '/w',
    ...over,
  };
}

function branchSpan(over: Partial<LoomBranchSpanV1> = {}): LoomBranchSpanV1 {
  return {
    branch: 'feature/x',
    event_count: 3,
    first_at: T0 + 100,
    last_at: T0 + 400,
    provider: 'cursor',
    session_id: 'root',
    source: 'git',
    worktree: '/w',
    ...over,
  };
}

function temporal(over: Partial<LoomTemporalPayloadV1> = {}): LoomTemporalPayloadV1 {
  return {
    available: true,
    branch_spans: [],
    commits: [],
    edited_files: [],
    sessions: [session()],
    source_statuses: [],
    temporal_refresh: {
      active_generations: 0,
      authority: 'test',
      latest_activated_at_micros: null,
      state: 'ready',
    },
    total: over.sessions?.length ?? 1,
    ...over,
  };
}

function message(over: Partial<LcmMessageV1> = {}): LcmMessageV1 {
  return {
    content: 'hello world',
    message_id: 'm1',
    metadata_json: null,
    ordinal: 0,
    pinned: null,
    role: 'assistant',
    session_id: 'root',
    snippet: null,
    source: null,
    storage_kind: null,
    store_id: null,
    summary_node_ids: [],
    timestamp: null,
    token_count: null,
    token_count_provenance: null,
    tool_name: null,
    ...over,
  };
}

const scope = {
  project_id: 'project',
  repository_id: 'repository',
  worktree_id: 'worktree',
  branch_ref: 'refs/heads/master',
  head_commit_id: 'commit',
};

function participant(provider: string, sessionId: string) {
  return {
    access: 'write' as const,
    activity: { start: (T0 + 10) * 1_000_000, end: (T0 + 20) * 1_000_000 },
    address: { scope, file: 'file', span: null, symbol: null },
    agent_id: `agent.${sessionId}`,
    branch_ref: null,
    head_revision: null,
    source: { provider, session_id: sessionId, source_key: null },
    worktree_id: null,
    worktree_root: `/tmp/${sessionId}`,
  };
}

function encounter(
  relation: FeedbackProximityEncounterV1['relation'],
  participants: FeedbackProximityEncounterV1['participants'],
  id = 'enc-1',
): FeedbackProximityEncounterV1 {
  return {
    coverage: 'complete',
    encounter_id: id,
    expires_at: (T0 + 60) * 1_000_000,
    interval: { start: (T0 + 10) * 1_000_000, end: (T0 + 20) * 1_000_000 },
    observed_at: (T0 + 20) * 1_000_000,
    participants,
    relation,
    scope,
  };
}

function sources(over: Partial<JourneySources> = {}): JourneySources {
  return {
    temporal: temporal(),
    hierarchy: tree([node()]),
    hierarchyState: 'loaded',
    selected: null,
    encounters: [],
    ...over,
  };
}

const ROOT = laneIdOf('cursor', 'root');
const CHILD = laneIdOf('cursor', 'child');

function parentChild(childStart = T0 + 60) {
  return sources({
    temporal: temporal({
      sessions: [
        session(),
        session({ session_id: 'child', started_at: childStart, is_subagent: true, messages: 3 }),
      ],
    }),
    hierarchy: tree([
      node(),
      node({
        session_id: 'child',
        link: 'linked',
        parent_session_id: 'root',
        parent_tool_use_id: 'toolu_01',
        is_subagent: true,
        depth: 1,
        agent: 'explorer',
      }),
    ]),
  });
}

/** Deterministic pseudo-shuffle so the test never depends on Math.random. */
function rotate<T>(items: readonly T[], by: number): T[] {
  const shift = by % Math.max(items.length, 1);
  return [...items.slice(shift), ...items.slice(0, shift)].reverse();
}

describe('orderMessages', () => {
  it('orders by the ordinal when the store served one', () => {
    const ordered = orderMessages([
      message({ message_id: 'c', ordinal: 2 }),
      message({ message_id: 'a', ordinal: 0 }),
      message({ message_id: 'b', ordinal: 1 }),
    ]);
    expect(ordered.map((m) => m.message_id)).toEqual(['a', 'b', 'c']);
  });

  it('falls back to wire order when ordinals are absent', () => {
    const ordered = orderMessages([
      message({ message_id: 'x', ordinal: null }),
      message({ message_id: 'y', ordinal: null }),
    ]);
    expect(ordered.map((m) => m.message_id)).toEqual(['x', 'y']);
  });

  it('is stable for equal ordinals', () => {
    const ordered = orderMessages([
      message({ message_id: 'first', ordinal: 1 }),
      message({ message_id: 'second', ordinal: 1 }),
    ]);
    expect(ordered.map((m) => m.message_id)).toEqual(['first', 'second']);
  });
});

describe('projectJourney lanes', () => {
  it('drops undated session rows and counts them', () => {
    const projection = projectJourney(
      sources({
        temporal: temporal({
          sessions: [
            session(),
            session({ session_id: 'zero', started_at: 0 }),
            session({ session_id: 'null', started_at: null }),
          ],
        }),
      }),
    );
    expect(projection.lanes.map((lane) => lane.sessionId)).toEqual(['root']);
    expect(projection.stats.undated).toBe(2);
  });

  it('prefers a recorded session end over the last message and never counts an equal instant', () => {
    const projection = projectJourney(
      sources({
        temporal: temporal({
          sessions: [
            session({ session_id: 'both', ended_at: T0 + 300, last_message_at: T0 + 900 }),
            session({ session_id: 'msg', ended_at: null, last_message_at: T0 + 120 }),
            session({ session_id: 'same', ended_at: T0, last_message_at: T0 }),
          ],
        }),
      }),
    );
    const byId = new Map(projection.lanes.map((lane) => [lane.sessionId, lane]));
    expect(byId.get('both')).toMatchObject({ end: T0 + 300, endSource: 'session_end' });
    expect(byId.get('msg')).toMatchObject({ end: T0 + 120, endSource: 'last_message' });
    expect(byId.get('same')).toMatchObject({ end: null, endSource: null });
    expect(projection.gaps.filter((gap) => gap.kind === 'extent_unknown')).toHaveLength(1);
    expect(projection.stats.openEnded).toBe(1);
  });

  it('labels, models, edited files and agent come from the records that carry them', () => {
    const projection = projectJourney(
      sources({
        temporal: temporal({
          sessions: [
            session({
              title: '  Verify scheduler ',
              models: [{ model: null }, { model: 'gpt-5' }, { model: 'gpt-5' }, { model: 'sonnet' }],
              edited_files_recorded: true,
            }),
          ],
          edited_files: [
            { path: 'a.ts', provider: 'cursor', session_id: 'root', change_type: null, hunks: null },
            { path: 'a.ts', provider: 'cursor', session_id: 'root', change_type: null, hunks: 2 },
            { path: 'b.ts', provider: 'cursor', session_id: 'root', change_type: null, hunks: null },
            { path: 'c.ts', provider: 'codex', session_id: 'root', change_type: null, hunks: null },
          ],
        }),
        hierarchy: tree([node({ agent: 'lead' })]),
      }),
    );
    expect(projection.lanes[0]).toMatchObject({
      id: ROOT,
      label: 'Verify scheduler',
      models: ['gpt-5', 'sonnet'],
      editedFilesRecorded: true,
      editedFileCount: 2,
      agent: 'lead',
    });
    // The rollup carries no edit time, so the edits are a typed absence, not marks.
    expect(projection.gaps.filter((gap) => gap.kind === 'edit_time_unrecorded')).toEqual([
      {
        id: `gap:edit_time_unrecorded:${ROOT}`,
        laneId: ROOT,
        kind: 'edit_time_unrecorded',
        grade: 'unavailable',
        detail: '2 edited files recorded · no edit time in this read',
      },
    ]);
  });

  it('projects identically regardless of wire order', () => {
    const rows = [
      session({ session_id: 'a', provider: 'cursor', started_at: T0 }),
      session({ session_id: 'b', provider: 'codex', started_at: T0 + 10 }),
      session({ session_id: 'c', provider: 'cursor', started_at: T0 + 20, is_subagent: true }),
      session({ session_id: 'd', provider: 'cursor', started_at: T0 + 30, is_subagent: true }),
      session({ session_id: 'e', provider: 'claude', started_at: T0 + 5 }),
    ];
    const nodes = [
      node({ session_id: 'a' }),
      node({ session_id: 'b', provider: 'codex' }),
      node({ session_id: 'c', link: 'linked', parent_session_id: 'a', parent_tool_use_id: 't1' }),
      node({ session_id: 'd', link: 'linked', parent_session_id: 'c', parent_tool_use_id: 't2' }),
      node({ session_id: 'e', provider: 'claude' }),
    ];
    const commits = [
      commit({ session_id: 'a', commit_sha: '1111111aaaa', committed_at: T0 + 50 }),
      commit({ session_id: 'a', commit_sha: '2222222bbbb', committed_at: T0 + 40 }),
      commit({ session_id: 'c', commit_sha: '3333333cccc', committed_at: T0 + 25 }),
    ];
    const spans = [
      branchSpan({ session_id: 'a', worktree: '/w1' }),
      branchSpan({ session_id: 'a', worktree: '/w2', first_at: T0 + 5 }),
    ];
    const build = (shift: number) =>
      projectJourney(
        sources({
          temporal: temporal({
            sessions: rotate(rows, shift),
            commits: rotate(commits, shift),
            branch_spans: rotate(spans, shift),
          }),
          hierarchy: tree(rotate(nodes, shift)),
        }),
      );
    const baseline = projectJourney(
      sources({
        temporal: temporal({ sessions: rows, commits, branch_spans: spans }),
        hierarchy: tree(nodes),
      }),
    );
    for (const shift of [1, 2, 3, 4]) expect(build(shift)).toEqual(baseline);
    // Provider groups by lane count, then roots by start, then the subtree.
    expect(baseline.lanes.map((lane) => lane.sessionId)).toEqual(['a', 'c', 'd', 'e', 'b']);
    expect(baseline.lanes.map((lane) => lane.depth)).toEqual([0, 1, 2, 0, 0]);
    expect(baseline.stats.providers).toEqual([
      { id: 'cursor', lanes: 3, messages: 30 },
      { id: 'claude', lanes: 1, messages: 10 },
      { id: 'codex', lanes: 1, messages: 10 },
    ]);
  });
});

describe('projectJourney parentage', () => {
  it('emits an inferred fork at the child start and a parent-lane event only for a linked pair in the page', () => {
    const projection = projectJourney(parentChild());
    expect(projection.relations).toHaveLength(1);
    const relation = projection.relations[0];
    expect(relation).toMatchObject({
      kind: 'spawn',
      fromLaneId: ROOT,
      toLaneId: CHILD,
      time: T0 + 60,
      grade: 'inferred',
    });
    // No parent transcript is loaded, so the fork cannot sit on the parent's
    // tool call and says so.
    expect(relation?.basis).toBe(
      'subagent tree · parent_session_id · parent_tool_use_id toolu_01 · fork placed at the child start: the parent transcript is not loaded',
    );
    const child = projection.lanes.find((lane) => lane.id === CHILD);
    expect(child).toMatchObject({ parentId: ROOT, depth: 1, agent: 'explorer' });
    const spawn = projection.events.find((event) => event.kind === 'spawn');
    expect(spawn).toMatchObject({
      laneId: ROOT,
      time: T0 + 60,
      grade: 'inferred',
      source: 'parentage',
      label: 'child',
      ref: CHILD,
      id: `spawn:${CHILD}`,
    });
    expect(projection.stats).toMatchObject({ lanes: 2, roots: 1, subagents: 1 });
  });

  describe('parentage from the session row and the subagent tree', () => {
    const rows = (child: Partial<LoomSessionRowV1>, extra: LoomSessionRowV1[] = []) =>
      temporal({
        sessions: [
          session({ ended_at: T0 + 3600 }),
          session({ session_id: 'other', started_at: T0 + 10 }),
          session({ session_id: 'child', started_at: T0 + 60, is_subagent: true, ...child }),
          ...extra,
        ],
      });
    const linked = (parent: string, tool: string | null) =>
      node({ session_id: 'child', link: 'linked', parent_session_id: parent, parent_tool_use_id: tool, is_subagent: true, depth: 1 });

    it('forks from the row column alone when the tree is silent', () => {
      const projection = projectJourney(sources({ temporal: rows({ parent_session_id: 'root', parent_tool_use_id: 'toolu_09' }) }));
      expect(projection.relations.filter((r) => r.kind === 'spawn')).toEqual([
        {
          id: `rel:spawn:${CHILD}`,
          kind: 'spawn',
          fromLaneId: ROOT,
          toLaneId: CHILD,
          time: T0 + 60,
          grade: 'inferred',
          basis: 'sessions row · parent_session_id · parent_tool_use_id toolu_09 · fork placed at the child start: the parent transcript is not loaded',
        },
      ]);
      expect(projection.lanes.find((lane) => lane.id === CHILD)).toMatchObject({ parentId: ROOT, depth: 1 });
    });

    const parentPage = (messages: LcmMessageV1[]) => ({ laneId: ROOT, messages });
    const taskCall = message({ message_id: 'root:b-task', ordinal: 3, role: 'assistant', tool_name: 'Task', tool_use_id: 'toolu_09', timestamp: T0 + 55 });

    it('forks exactly from the loaded tool call whose id the child recorded', () => {
      const projection = projectJourney(
        sources({
          temporal: rows({ parent_session_id: 'root', parent_tool_use_id: 'toolu_09' }),
          selected: parentPage([message({ message_id: 'root:b-read', ordinal: 2, tool_name: 'Read', tool_use_id: 'toolu_08', timestamp: T0 + 50 }), taskCall]),
        }),
      );
      expect(projection.relations.filter((r) => r.kind === 'spawn')).toEqual([
        {
          id: `rel:spawn:${CHILD}`,
          kind: 'spawn',
          fromLaneId: ROOT,
          toLaneId: CHILD,
          time: T0 + 55,
          grade: 'exact',
          basis: 'sessions row · parent_session_id · parent_tool_use_id toolu_09 · fork placed on the spawning tool call Task',
          fromEventId: `msg:${ROOT}:root:b-task`,
        },
      ]);
      // The tool-call glyph is the fork's mark: no second spawn mark.
      expect(projection.events.filter((event) => event.kind === 'spawn')).toEqual([]);
      expect(projection.events.find((event) => event.id === `msg:${ROOT}:root:b-task`)).toMatchObject({ kind: 'tool_call', time: T0 + 55 });
    });

    it('keeps the fork inferred at the child start when no loaded tool call carries the id', () => {
      const projection = projectJourney(
        sources({
          temporal: rows({ parent_session_id: 'root', parent_tool_use_id: 'toolu_09' }),
          selected: parentPage([message({ message_id: 'root:b-read', tool_name: 'Read', tool_use_id: 'toolu_08', timestamp: T0 + 50 })]),
        }),
      );
      const [fork] = projection.relations.filter((r) => r.kind === 'spawn');
      expect(fork).toMatchObject({ time: T0 + 60, grade: 'inferred' });
      expect(fork?.fromEventId).toBeUndefined();
      expect(fork?.basis).toBe(
        'sessions row · parent_session_id · parent_tool_use_id toolu_09 · fork placed at the child start: no loaded parent tool call carries toolu_09',
      );
      expect(projection.events.find((event) => event.kind === 'spawn')).toMatchObject({ laneId: ROOT, time: T0 + 60, grade: 'inferred' });

      const unrecorded = projectJourney(sources({ temporal: rows({ parent_session_id: 'root' }), selected: parentPage([taskCall]) }));
      expect(unrecorded.relations.filter((r) => r.kind === 'spawn').map((r) => [r.time, r.grade, r.basis])).toEqual([
        [T0 + 60, 'inferred', 'sessions row · parent_session_id · parent_tool_use_id unrecorded · fork placed at the child start: no parent tool-use id recorded'],
      ]);
    });

    it('keeps a matched fork ambiguous when the subagent tree names another tool use', () => {
      const projection = projectJourney(
        sources({
          temporal: rows({ parent_session_id: 'root', parent_tool_use_id: 'toolu_09' }),
          hierarchy: tree([node(), linked('root', 'toolu_77')]),
          selected: parentPage([taskCall]),
        }),
      );
      expect(projection.relations.filter((r) => r.kind === 'spawn').map((r) => [r.time, r.grade, r.fromEventId])).toEqual([
        [T0 + 55, 'ambiguous', `msg:${ROOT}:root:b-task`],
      ]);
      expect(projection.gaps.find((gap) => gap.kind === 'parentage_conflict')?.detail).toBe(
        'sessions row names tool use toolu_09; subagent tree names toolu_77',
      );
    });

    it('draws one fork when the two sources agree', () => {
      const projection = projectJourney(
        sources({ temporal: rows({ parent_session_id: 'root', parent_tool_use_id: 'toolu_09' }), hierarchy: tree([node(), linked('root', 'toolu_09')]) }),
      );
      const spawns = projection.relations.filter((r) => r.kind === 'spawn');
      expect(spawns).toHaveLength(1);
      expect(spawns[0]?.basis.startsWith('sessions row and subagent tree agree · ')).toBe(true);
      expect(projection.gaps.some((gap) => gap.kind === 'parentage_conflict')).toBe(false);
    });

    it('draws both candidates as ambiguous when the sources name different parents', () => {
      const projection = projectJourney(
        sources({ temporal: rows({ parent_session_id: 'root' }), hierarchy: tree([node(), linked('other', null)]) }),
      );
      const spawns = projection.relations.filter((r) => r.kind === 'spawn');
      expect(spawns.map((r) => [r.id, r.fromLaneId, r.grade])).toEqual([
        [`rel:spawn:${CHILD}:sessions row`, ROOT, 'ambiguous'],
        [`rel:spawn:${CHILD}:subagent tree`, laneIdOf('cursor', 'other'), 'ambiguous'],
      ]);
      // Placement follows the session row; the disagreement is a gap, not a merge.
      expect(projection.lanes.find((lane) => lane.id === CHILD)?.parentId).toBe(ROOT);
      expect(projection.gaps.find((gap) => gap.kind === 'parentage_conflict')).toEqual({
        id: `gap:parentage_conflict:${CHILD}`,
        laneId: CHILD,
        kind: 'parentage_conflict',
        grade: 'ambiguous',
        detail: 'sessions row names parent root; subagent tree names other',
      });
    });

    it('grades an agreed parent ambiguous when the sources name different tool uses', () => {
      const projection = projectJourney(
        sources({ temporal: rows({ parent_session_id: 'root', parent_tool_use_id: 'toolu_a' }), hierarchy: tree([node(), linked('root', 'toolu_b')]) }),
      );
      expect(projection.relations.filter((r) => r.kind === 'spawn').map((r) => r.grade)).toEqual(['ambiguous']);
      expect(projection.gaps.find((gap) => gap.kind === 'parentage_conflict')?.detail).toBe(
        'sessions row names tool use toolu_a; subagent tree names toolu_b',
      );
    });

    it('infers a join only where the child ends inside the parent measured extent', () => {
      const ended = projectJourney(sources({ temporal: rows({ parent_session_id: 'root', ended_at: T0 + 900 }) }));
      expect(ended.relations.filter((r) => r.kind === 'rejoin')).toEqual([
        {
          id: `rel:rejoin:${CHILD}`,
          kind: 'rejoin',
          fromLaneId: CHILD,
          toLaneId: ROOT,
          time: T0 + 900,
          grade: 'inferred',
          basis: "child recorded end inside the parent's measured extent · no result or handoff record in this read",
        },
      ]);
      const open = projectJourney(sources({ temporal: rows({ parent_session_id: 'root' }) }));
      expect(open.relations.some((r) => r.kind === 'rejoin')).toBe(false);
      const outlives = projectJourney(sources({ temporal: rows({ parent_session_id: 'root', ended_at: T0 + 7200 }) }));
      expect(outlives.relations.some((r) => r.kind === 'rejoin')).toBe(false);
    });
  });

  describe('edited files', () => {
    it('links a timed edit to the one loaded tool call recorded in its second', () => {
      const editAt = (T0 + 120) * 1_000_000 + 994_000;
      const project = (messages: LcmMessageV1[]) =>
        projectJourney(
          sources({
            temporal: temporal({
              sessions: [session({ edited_files_recorded: true })],
              edited_files: [{ path: 'src/auth/mod.rs', provider: 'cursor', session_id: 'root', change_type: null, hunks: null, edited_at_micros: editAt }],
            }),
            selected: { laneId: ROOT, messages },
          }),
        ).events.find((event) => event.kind === 'file_edit');
      const edit = message({ message_id: 'root:b-edit', ordinal: 1, tool_name: 'edit_file_v2', tool_use_id: 'call_2jug3QnkUS9kwSbiI4oSDy5a', timestamp: T0 + 120 });
      expect(project([message({ message_id: 'root:b-read', ordinal: 0, tool_name: 'read_file_v2', timestamp: T0 + 120 }), edit])).toEqual({
        id: `edit:${ROOT}:src/auth/mod.rs:${editAt}`,
        laneId: ROOT,
        kind: 'file_edit',
        time: editAt / 1_000_000,
        sequence: 1,
        grade: 'exact',
        source: 'file_rollup',
        label: 'mod.rs',
        detail: 'src/auth/mod.rs · tool call edit_file_v2 call_2jug3QnkUS9kwSbiI4oSDy5a',
        ref: 'src/auth/mod.rs',
        linkedEventId: `msg:${ROOT}:root:b-edit`,
      });
      // Two tool calls in that second name no single call.
      const twin = message({ message_id: 'root:b-edit-2', ordinal: 2, tool_name: 'edit_file_v2', tool_use_id: 'call_other', timestamp: T0 + 120 });
      expect(project([edit, twin])?.linkedEventId).toBeUndefined();
      // A different second is not a coincidence.
      expect(project([{ ...edit, timestamp: T0 + 121 }])?.linkedEventId).toBeUndefined();
    });

    it('places a timed edit at its recorded time and keeps untimed edits as a gap', () => {
      const projection = projectJourney(
        sources({
          temporal: temporal({
            sessions: [session({ edited_files_recorded: true })],
            edited_files: [
              { path: 'src/a.ts', provider: 'cursor', session_id: 'root', change_type: 'modified', hunks: 2, edited_at_micros: (T0 + 120) * 1_000_000 },
              { path: 'src/b.ts', provider: 'cursor', session_id: 'root', change_type: null, hunks: null, edited_at_micros: null },
              { path: 'src/c.ts', provider: 'cursor', session_id: 'root', change_type: 'added', hunks: 1 },
            ],
          }),
        }),
      );
      expect(projection.events.filter((event) => event.kind === 'file_edit')).toEqual([
        {
          id: `edit:${ROOT}:src/a.ts:${(T0 + 120) * 1_000_000}`,
          laneId: ROOT,
          kind: 'file_edit',
          time: T0 + 120,
          sequence: 1,
          grade: 'exact',
          source: 'file_rollup',
          label: 'a.ts',
          detail: 'modified · 2 hunks · src/a.ts',
          ref: 'src/a.ts',
        },
      ]);
      expect(projection.gaps.find((gap) => gap.kind === 'edit_time_unrecorded')?.detail).toBe(
        '2 edited files recorded · no edit time in this read',
      );
    });
  });

  it('grades the relation ambiguous when the child starts before its parent', () => {
    const projection = projectJourney(parentChild(T0 - 30));
    expect(projection.relations[0]).toMatchObject({ grade: 'ambiguous' });
    expect(projection.relations[0]?.basis).toContain('child start precedes parent start');
    expect(projection.events.find((event) => event.kind === 'spawn')?.grade).toBe('ambiguous');
  });

  it('does not invent a relation when the parent is outside the page', () => {
    const projection = projectJourney(
      sources({
        hierarchy: tree([
          node({ link: 'linked', parent_session_id: 'elsewhere', parent_tool_use_id: 't' }),
        ]),
      }),
    );
    expect(projection.relations).toEqual([]);
    expect(projection.lanes[0]?.parentId).toBeNull();
    expect(projection.gaps).toContainEqual(
      expect.objectContaining({
        laneId: ROOT,
        kind: 'parent_outside_page',
        grade: 'unavailable',
        detail: 'subagent tree names parent elsewhere, outside this loaded page',
      }),
    );
  });

  it('turns a missing parent into a gap and a cycle into an ambiguous gap', () => {
    const projection = projectJourney(
      sources({
        temporal: temporal({
          sessions: [session(), session({ session_id: 'loop', started_at: T0 + 1 })],
        }),
        hierarchy: tree([
          node({ link: 'missing_parent', parent_session_id: 'gone' }),
          node({ session_id: 'loop', link: 'cycle', parent_session_id: 'root' }),
        ]),
      }),
    );
    expect(projection.gaps).toContainEqual(
      expect.objectContaining({
        laneId: ROOT,
        kind: 'parent_outside_page',
        detail: 'recorded parent gone was never ingested',
      }),
    );
    expect(projection.gaps).toContainEqual(
      expect.objectContaining({
        laneId: laneIdOf('cursor', 'loop'),
        kind: 'parent_cycle',
        grade: 'ambiguous',
      }),
    );
    expect(projection.relations).toEqual([]);
  });

  it('emits one page-wide gap when the parentage authority is not served', () => {
    for (const hierarchy of [
      null,
      tree([], { available: false }),
      tree([], { error: 'tree store locked' }),
    ]) {
      const projection = projectJourney(sources({ hierarchy, hierarchyState: 'unavailable' }));
      const gaps = projection.gaps.filter((gap) => gap.kind === 'parentage_unavailable');
      expect(gaps).toHaveLength(1);
      expect(gaps[0]).toMatchObject({ laneId: null, grade: 'unavailable' });
      expect(gaps[0]?.detail).toContain('unavailable');
    }
    const errored = projectJourney(
      sources({ hierarchy: tree([], { error: 'tree store locked' }) }),
    );
    expect(errored.gaps.find((gap) => gap.kind === 'parentage_unavailable')?.detail).toContain(
      'tree store locked',
    );
  });

  it('names a truncated parentage read with its recorded counts', () => {
    const projection = projectJourney(
      sources({
        hierarchy: tree([node()], { truncated: true, missing_parent_count: 4, cycle_count: 1 }),
      }),
    );
    expect(projection.gaps).toContainEqual(
      expect.objectContaining({
        laneId: null,
        kind: 'parentage_unavailable',
        detail: 'parentage authority truncated · 4 missing parents · 1 cycles',
      }),
    );
  });

  it('always states that handoff authority is unbound', () => {
    for (const projection of [projectJourney(sources()), projectJourney(sources({ temporal: temporal({ sessions: [] }) }))]) {
      const handoff = projection.gaps.filter((gap) => gap.kind === 'handoff_unavailable');
      expect(handoff).toHaveLength(1);
      expect(handoff[0]).toMatchObject({ laneId: null, grade: 'unavailable' });
    }
  });
});

describe('projectJourney events', () => {
  it('places start and end events with the grade of their source', () => {
    const projection = projectJourney(
      sources({
        temporal: temporal({
          sessions: [
            session({ session_id: 'recorded', ended_at: T0 + 100 }),
            session({ session_id: 'observed', last_message_at: T0 + 100 }),
          ],
        }),
      }),
    );
    const ends = projection.events.filter((event) => event.kind === 'session_end');
    expect(ends.map((event) => event.grade).sort()).toEqual(['exact', 'inferred']);
    const inferred = ends.find((event) => event.grade === 'inferred');
    expect(inferred?.detail).toBe('last message observation, not a recorded session end');
    const starts = projection.events.filter((event) => event.kind === 'session_start');
    expect(starts).toHaveLength(2);
    expect(starts.every((event) => event.grade === 'exact' && event.sequence === 0)).toBe(true);
  });

  it('grades commits by evidence and never prints the confidence number', () => {
    const projection = projectJourney(
      sources({
        temporal: temporal({
          commits: [
            commit({ commit_sha: 'aaaaaaa111', evidence: 'transcript', confidence: 0.83 }),
            commit({
              commit_sha: 'bbbbbbb222',
              evidence: 'time_window',
              confidence: 0.41,
              span_overlap_kind: 'branch_span',
              relation: 'overlapping',
            }),
            commit({ commit_sha: 'ccccccc333', session_id: 'not-loaded' }),
          ],
        }),
      }),
    );
    const commits = projection.events.filter((event) => event.kind === 'commit');
    expect(commits).toHaveLength(2);
    const exact = commits.find((event) => event.label === 'aaaaaaa');
    const inferred = commits.find((event) => event.label === 'bbbbbbb');
    expect(exact).toMatchObject({ grade: 'exact', detail: 'authored · transcript', ref: 'aaaaaaa111' });
    expect(inferred).toMatchObject({
      grade: 'inferred',
      detail: 'overlapping · time_window · branch_span',
    });
    for (const event of commits) {
      expect(event.detail).not.toMatch(/0\.\d/);
      expect(event.label).not.toMatch(/0\.\d/);
    }
  });

  it('sequences recorded events per lane by time then id', () => {
    const projection = projectJourney(
      sources({
        temporal: temporal({
          sessions: [session({ ended_at: T0 + 900 })],
          commits: [
            commit({ commit_sha: 'late', committed_at: T0 + 600 }),
            commit({ commit_sha: 'early', committed_at: T0 + 300 }),
          ],
        }),
      }),
    );
    expect(projection.events.map((event) => [event.kind, event.sequence])).toEqual([
      ['session_start', 0],
      ['commit', 1],
      ['commit', 2],
      ['session_end', 3],
    ]);
    expect(projection.events[1]?.ref).toBe('early');
  });

  it('classifies loaded transcript turns by tool then role and reports undated ones', () => {
    const projection = projectJourney(
      sources({
        selected: {
          laneId: ROOT,
          messages: [
            message({ message_id: 'u', role: 'user', ordinal: 0, content: 'do  the\n thing' }),
            message({ message_id: 't', role: 'assistant', tool_name: 'Read', ordinal: 1 }),
            message({ message_id: 'a', role: 'assistant', ordinal: 2, timestamp: T0 + 30 }),
            message({ message_id: 'o', role: null, ordinal: 3, snippet: 'sys' }),
            message({ message_id: 'ut', role: 'user', tool_name: 'Bash', ordinal: 4 }),
          ],
        },
      }),
    );
    const turns = projection.events.filter((event) => event.source === 'transcript');
    expect(turns.map((event) => event.kind)).toEqual([
      'message_user',
      'tool_call',
      'message_assistant',
      'message_other',
      'tool_call',
    ]);
    expect(turns.map((event) => event.sequence)).toEqual([0, 1, 2, 3, 4]);
    expect(turns.map((event) => event.time)).toEqual([null, null, T0 + 30, null, null]);
    expect(turns.map((event) => event.label)).toEqual(['user', 'Read', 'assistant', 'role unrecorded', 'Bash']);
    expect(turns[0]?.detail).toBe('do the thing');
    expect(turns[3]?.detail).toBe('sys');
    expect(turns[0]?.id).toBe(`msg:${ROOT}:u`);
    expect(turns.every((event) => event.grade === 'exact')).toBe(true);
    expect(projection.gaps).toContainEqual(
      expect.objectContaining({
        laneId: ROOT,
        kind: 'undated_events',
        grade: 'unavailable',
        detail: '4 of 5 loaded turns carry no timestamp; placed in recorded order',
      }),
    );
  });

  it('ignores a selected transcript whose lane is not in the page', () => {
    const projection = projectJourney(
      sources({ selected: { laneId: laneIdOf('cursor', 'absent'), messages: [message()] } }),
    );
    expect(projection.events.some((event) => event.source === 'transcript')).toBe(false);
    expect(projection.gaps.some((gap) => gap.kind === 'undated_events')).toBe(false);
  });

  it('truncates a long turn to one line of at most 140 characters', () => {
    const projection = projectJourney(
      sources({ selected: { laneId: ROOT, messages: [message({ content: 'x'.repeat(400) })] } }),
    );
    const detail = projection.events.find((event) => event.source === 'transcript')?.detail;
    expect(detail).toHaveLength(140);
    expect(detail?.endsWith('…')).toBe(true);
  });
});

describe('projectJourney intervals and extent', () => {
  it('projects branch spans and one proximity interval per participant in the page', () => {
    const projection = projectJourney(
      sources({
        temporal: temporal({
          sessions: [session(), session({ session_id: 'peer', provider: 'codex', started_at: T0 + 1 })],
          branch_spans: [branchSpan(), branchSpan({ session_id: 'not-loaded' })],
        }),
        encounters: [
          encounter({ relation_kind: 'overlapping_edit', warning_class: 'same_file' }, [
            participant('cursor', 'root'),
            participant('codex', 'peer'),
            participant('codex', 'elsewhere'),
          ]),
          encounter(
            { relation_kind: 'code_neighborhood_candidate', warning_class: 'neighborhood' },
            [participant('cursor', 'root')],
            'enc-2',
          ),
        ],
      }),
    );
    const spans = projection.intervals.filter((interval) => interval.kind === 'git_span');
    expect(spans).toHaveLength(1);
    expect(spans[0]).toMatchObject({
      laneId: ROOT,
      start: T0 + 100,
      end: T0 + 400,
      label: 'feature/x · /w',
      grade: 'exact',
      tone: null,
      ref: null,
    });
    const proximity = projection.intervals.filter((interval) => interval.kind === 'proximity');
    expect(proximity).toHaveLength(3);
    expect(proximity.filter((interval) => interval.ref === 'enc-1').map((i) => i.laneId).sort()).toEqual(
      [ROOT, laneIdOf('codex', 'peer')].sort(),
    );
    expect(proximity.find((interval) => interval.ref === 'enc-1')).toMatchObject({
      start: T0 + 10,
      end: T0 + 20,
      label: 'overlapping edit',
      tone: 'overlap',
      grade: 'exact',
    });
    expect(proximity.find((interval) => interval.ref === 'enc-2')).toMatchObject({
      tone: 'candidate',
      grade: 'inferred',
      label: 'code neighborhood candidate',
    });
  });

  it('spans the extent over lanes, dated events and intervals, padded to an hour', () => {
    const lone = projectJourney(sources());
    expect(lone.extent).toEqual({ start: T0, end: T0 + 3600 });

    const wide = projectJourney(
      sources({
        temporal: temporal({
          sessions: [session({ ended_at: T0 + 100 })],
          commits: [commit({ committed_at: T0 + 5000 })],
        }),
      }),
    );
    expect(wide.extent).toEqual({ start: T0, end: T0 + 5000 });

    const spanned = projectJourney(
      sources({ temporal: temporal({ branch_spans: [branchSpan({ last_at: T0 + 7200 })] }) }),
    );
    expect(spanned.extent).toEqual({ start: T0, end: T0 + 7200 });

    expect(projectJourney(sources({ temporal: temporal({ sessions: [] }) })).extent).toBeNull();
  });

  it('counts hollow, open-ended and message totals from the readings', () => {
    const projection = projectJourney(
      sources({
        temporal: temporal({
          sessions: [
            session({ session_id: 'a', messages: 0 }),
            session({ session_id: 'b', messages: 7, ended_at: T0 + 10 }),
          ],
        }),
      }),
    );
    expect(projection.stats).toMatchObject({
      lanes: 2,
      hollow: 1,
      openEnded: 1,
      messages: 7,
      undated: 0,
    });
  });
});

describe('the Loom fixture', () => {
  const payload = <T,>(path: string, search = ''): T => (resolveFixture(path, search) as { payload: T }).payload;
  const temporalPage = payload<LoomTemporalPayloadV1>('/api/loom/temporal', 'limit=200');
  const subagents = payload<AnalyticsSubagentTreePayloadV1>('/api/plugins/analytics/subagent-tree');
  const forksWith = (provider: string, sessionId: string) =>
    projectJourney({
      temporal: temporalPage,
      hierarchy: subagents,
      hierarchyState: 'loaded',
      selected: {
        laneId: laneIdOf(provider, sessionId),
        messages: payload<{ messages: LcmMessageV1[] }>(`/api/plugins/hermes-lcm/session/${sessionId}`).messages,
      },
      encounters: [],
    }).relations.filter((relation) => relation.kind === 'spawn');

  it('forks exactly on the spawning Task call of the loaded parent transcript', () => {
    const root = laneIdOf('codex', 'session.codex.root');
    const child = laneIdOf('codex', 'session.codex.child');
    const forks = new Map(forksWith('codex', 'session.codex.root').map((relation) => [relation.toLaneId, relation]));
    expect(forks.get(child)).toEqual({
      id: `rel:spawn:${child}`,
      kind: 'spawn',
      fromLaneId: root,
      toLaneId: child,
      time: null,
      grade: 'exact',
      basis: 'sessions row and subagent tree agree · parent_session_id · parent_tool_use_id toolu_codex_01 · fork placed on the spawning tool call Task',
      fromEventId: `msg:${root}:session.codex.root:0007`,
    });
    // The grandchild's parent transcript is not the loaded one.
    expect(forks.get(laneIdOf('codex', 'session.codex.grandchild'))).toMatchObject({
      fromLaneId: child,
      grade: 'inferred',
      basis: 'sessions row and subagent tree agree · parent_session_id · parent_tool_use_id toolu_codex_02 · fork placed at the child start: the parent transcript is not loaded',
    });
  });

  it('forks a row-parented child exactly only where the loaded parent carries its call', () => {
    const parent = '02bc8f3c-d4e6-4176-afea-000000770509';
    const exact = forksWith('cursor', parent).find((relation) => relation.fromLaneId === laneIdOf('cursor', parent));
    expect([exact?.grade, exact?.fromEventId, exact?.basis]).toEqual([
      'exact',
      `msg:${laneIdOf('cursor', parent)}:${parent}:0019`,
      'sessions row · parent_session_id · parent_tool_use_id toolu_loom_5 · fork placed on the spawning tool call Task',
    ]);
    const other = '037c8f3c-d4e6-4176-afea-000000770521';
    const inferred = forksWith('cursor', other).find((relation) => relation.fromLaneId === laneIdOf('cursor', other));
    expect([inferred?.grade, inferred?.basis]).toEqual([
      'inferred',
      'sessions row · parent_session_id · parent_tool_use_id toolu_loom_17 · fork placed at the child start: no loaded parent tool call carries toolu_loom_17',
    ]);
  });
});
