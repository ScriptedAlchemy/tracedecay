import { describe, expect, it } from 'vitest';
import {
  DEFAULT_DENSE_LANE_THRESHOLD,
  RULER_HEIGHT,
  fittedWindowFor,
  layoutTemporalScene,
  timeToX,
  xToTime,
} from './layout.ts';
import type {
  JourneyEvent,
  JourneyExtent,
  JourneyGap,
  JourneyInterval,
  JourneyLane,
  JourneyProjection,
  JourneyRelation,
  LayoutOptions,
  SceneViewport,
  TemporalSceneModel,
} from './types.ts';

/**
 * The layout runs in the plain `node` project: pixels are decided from the
 * projection and the options alone, so every placement rule is checked here
 * without a renderer.
 */

const T0 = 1_784_700_000;

function lane(over: Partial<JourneyLane> & { id: string }): JourneyLane {
  return {
    sessionId: over.id,
    provider: 'cursor',
    label: over.id,
    agent: null,
    start: T0,
    end: null,
    endSource: null,
    parentId: null,
    depth: 0,
    isSubagent: false,
    messages: 10,
    editedFilesRecorded: false,
    editedFileCount: 0,
    models: [],
    ...over,
  };
}

function event(over: Partial<JourneyEvent> & { id: string; laneId: string }): JourneyEvent {
  return {
    kind: 'session_start',
    time: T0,
    sequence: 0,
    grade: 'exact',
    source: 'session',
    label: 'start',
    detail: null,
    ref: over.id,
    ...over,
  };
}

/** Start/end events for a lane, plus any extra recorded events, sequenced
 * the way the projection does: time then id. */
function laneEvents(l: JourneyLane, extra: JourneyEvent[] = []): JourneyEvent[] {
  const events: JourneyEvent[] = [
    event({ id: `start:${l.id}`, laneId: l.id, kind: 'session_start', time: l.start }),
    ...extra,
  ];
  if (l.end !== null) {
    events.push(
      event({
        id: `end:${l.id}`,
        laneId: l.id,
        kind: 'session_end',
        time: l.end,
        label: 'end',
        grade: l.endSource === 'session_end' ? 'exact' : 'inferred',
      }),
    );
  }
  return events
    .sort((a, b) => (a.time ?? 0) - (b.time ?? 0) || (a.id < b.id ? -1 : 1))
    .map((e, sequence) => ({ ...e, sequence }));
}

function spawn(parent: JourneyLane, child: JourneyLane): JourneyRelation {
  return {
    id: `rel:spawn:${child.id}`,
    kind: 'spawn',
    fromLaneId: parent.id,
    toLaneId: child.id,
    time: child.start,
    grade: 'exact',
    basis: 'parent_session_id · parent_tool_use_id toolu_01',
  };
}

function spawnEvent(parent: JourneyLane, child: JourneyLane): JourneyEvent {
  return event({
    id: `spawn:${child.id}`,
    laneId: parent.id,
    kind: 'spawn',
    time: child.start,
    source: 'parentage',
    label: child.label,
    ref: child.id,
  });
}

function projection(parts: {
  lanes: JourneyLane[];
  events?: JourneyEvent[];
  relations?: JourneyRelation[];
  gaps?: JourneyGap[];
  intervals?: JourneyInterval[];
}): JourneyProjection {
  const lanes = parts.lanes;
  const events = parts.events ?? lanes.flatMap((l) => laneEvents(l));
  let extent: JourneyExtent | null = null;
  if (lanes.length > 0) {
    let start = Infinity;
    let end = -Infinity;
    for (const l of lanes) {
      start = Math.min(start, l.start);
      end = Math.max(end, l.start, l.end ?? l.start);
    }
    for (const e of events) if (e.time !== null) end = Math.max(end, e.time);
    if (end - start < 3600) end = start + 3600;
    extent = { start, end };
  }
  const providers = new Map<string, { id: string; lanes: number; messages: number }>();
  for (const l of lanes) {
    const entry = providers.get(l.provider) ?? { id: l.provider, lanes: 0, messages: 0 };
    entry.lanes += 1;
    entry.messages += l.messages;
    providers.set(l.provider, entry);
  }
  return {
    lanes,
    events,
    relations: parts.relations ?? [],
    gaps: parts.gaps ?? [],
    intervals: parts.intervals ?? [],
    extent,
    stats: {
      lanes: lanes.length,
      roots: lanes.filter((l) => l.parentId === null).length,
      subagents: lanes.filter((l) => l.isSubagent).length,
      messages: lanes.reduce((sum, l) => sum + l.messages, 0),
      openEnded: lanes.filter((l) => l.end === null).length,
      hollow: lanes.filter((l) => l.messages === 0).length,
      undated: 0,
      providers: [...providers.values()],
    },
  };
}

function viewportFor(extent: JourneyExtent | null, window = fittedWindowFor(extent)): SceneViewport {
  return { width: 1000, left: 200, right: 20, window };
}

function optionsFor(
  proj: JourneyProjection,
  over: Partial<LayoutOptions> = {},
): LayoutOptions {
  return {
    viewport: viewportFor(proj.extent),
    zoom: 'agent',
    branches: { collapsed: new Set(), expanded: new Set() },
    selectedLaneId: null,
    selectedEventId: null,
    reveal: null,
    hiddenKinds: new Set(),
    denseLaneThreshold: DEFAULT_DENSE_LANE_THRESHOLD,
    ...over,
  };
}

/* A small delegation tree: A spawns B, B spawns C; D is a codex root. */
const A = lane({ id: 'A', start: T0, end: T0 + 3000, endSource: 'session_end', messages: 100 });
const B = lane({
  id: 'B',
  parentId: 'A',
  depth: 1,
  start: T0 + 600,
  end: T0 + 1200,
  endSource: 'last_message',
  isSubagent: true,
  messages: 20,
});
const C = lane({ id: 'C', parentId: 'B', depth: 2, start: T0 + 700, isSubagent: true, messages: 0 });
const D = lane({ id: 'D', provider: 'codex', start: T0 + 100, end: T0 + 2000, endSource: 'session_end' });

const COMMIT_B = event({
  id: 'commit:B:abc',
  laneId: 'B',
  kind: 'commit',
  time: T0 + 900,
  source: 'commit',
  label: 'abcdef1',
  ref: 'abcdef1234',
});

function treeProjection(): JourneyProjection {
  return projection({
    lanes: [A, B, C, D],
    events: [
      ...laneEvents(A, [spawnEvent(A, B)]),
      ...laneEvents(B, [spawnEvent(B, C), COMMIT_B]),
      ...laneEvents(C),
      ...laneEvents(D),
    ],
    relations: [spawn(A, B), spawn(B, C)],
    gaps: [
      { id: 'gap:extent_unknown:C', laneId: 'C', kind: 'extent_unknown', grade: 'unavailable', detail: 'no recorded end' },
      { id: 'gap:handoff_unavailable', laneId: null, kind: 'handoff_unavailable', grade: 'unavailable', detail: 'unbound' },
    ],
    intervals: [
      {
        id: 'span:A',
        laneId: 'A',
        kind: 'git_span',
        start: T0 + 100,
        end: T0 + 500,
        label: 'master · /w',
        grade: 'exact',
        tone: null,
        ref: null,
      },
      {
        id: 'prox:e1:D',
        laneId: 'D',
        kind: 'proximity',
        start: T0 + 200,
        end: T0 + 300,
        label: 'overlapping edit',
        grade: 'exact',
        tone: 'overlap',
        ref: 'e1',
      },
    ],
  });
}

function sceneLane(model: TemporalSceneModel, id: string) {
  const found = model.lanes.find((l) => l.id === id);
  if (!found) throw new Error(`lane ${id} not in scene`);
  return found;
}

describe('time mapping', () => {
  const viewport = viewportFor({ start: T0, end: T0 + 3600 });

  it('maps time to x strictly increasing and inverts exactly', () => {
    const times = [T0 - 60, T0, T0 + 900, T0 + 1800, T0 + 3660];
    const xs = times.map((t) => timeToX(viewport, t));
    for (let i = 1; i < xs.length; i += 1) expect(xs[i]).toBeGreaterThan(xs[i - 1] ?? Infinity);
    expect(timeToX(viewport, viewport.window.start)).toBe(200);
    expect(timeToX(viewport, viewport.window.end)).toBe(980);
    for (const t of times) expect(xToTime(viewport, timeToX(viewport, t))).toBeCloseTo(t, 6);
  });

  it('fits an hour when there is no extent', () => {
    expect(fittedWindowFor(null)).toEqual({ start: 0, end: 3600 });
    expect(fittedWindowFor({ start: T0, end: T0 + 3600 })).toEqual({ start: T0 - 72, end: T0 + 3672 });
  });
});

describe('layoutTemporalScene', () => {
  it('is deterministic for the same projection and options', () => {
    const proj = treeProjection();
    const options = optionsFor(proj, { selectedLaneId: 'B', zoom: 'event' });
    expect(layoutTemporalScene(proj, options)).toEqual(layoutTemporalScene(proj, options));
  });

  it('stacks rows top to bottom under the ruler with a gap between providers', () => {
    const model = layoutTemporalScene(treeProjection(), optionsFor(treeProjection()));
    expect(model.lanes.map((l) => l.id)).toEqual(['A', 'B', 'C', 'D']);
    for (let i = 1; i < model.lanes.length; i += 1) {
      expect(model.lanes[i]?.y).toBeGreaterThan(model.lanes[i - 1]?.y ?? Infinity);
      expect(model.lanes[i]?.row).toBe(i);
    }
    expect(model.lanes[0]?.y).toBe(RULER_HEIGHT + 15);
    // A, B, C are 30px each; the codex rail starts 10px below them.
    expect(sceneLane(model, 'D').y).toBe(RULER_HEIGHT + 90 + 10 + 15);
    expect(model.rails.map((r) => [r.label, r.lanes])).toEqual([
      ['cursor', 3],
      ['codex', 1],
    ]);
    expect(model.height).toBe(RULER_HEIGHT + 120 + 10 + 16);
    expect(model.lanes.every((l) => l.kind === 'session' && !l.expanded)).toBe(true);
    expect(model.counts).toMatchObject({ lanesTotal: 4, lanesVisible: 4, lanesCollapsed: 0 });
  });

  it('places lane extents on the time axis and gives an open lane a short tail', () => {
    const proj = treeProjection();
    const options = optionsFor(proj);
    const model = layoutTemporalScene(proj, options);
    const a = sceneLane(model, 'A');
    expect(a.x0).toBeCloseTo(timeToX(options.viewport, T0), 6);
    expect(a.x1).toBeCloseTo(timeToX(options.viewport, T0 + 3000), 6);
    const c = sceneLane(model, 'C');
    expect(c.x1 - c.x0).toBeCloseTo(18, 6);
    expect(c.endSource).toBeNull();
    const lanePaths = model.paths.filter((p) => p.kind === 'lane');
    expect(lanePaths.map((p) => p.grade)).toEqual(['exact', 'inferred', 'unavailable', 'exact']);
    expect(lanePaths[0]?.weight).toBe(1);
    expect(lanePaths[2]?.weight).toBe(0);
    expect(lanePaths[0]?.controls).toEqual([a.x0, a.y, a.x1, a.y]);
  });

  it('collapses an ancestor into a bundle whose cluster counts reconcile', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(
      proj,
      optionsFor(proj, { branches: { collapsed: new Set(['A']), expanded: new Set() } }),
    );
    expect(model.lanes.map((l) => l.id)).toEqual(['A', 'D']);
    const a = sceneLane(model, 'A');
    expect(a.kind).toBe('bundle');
    expect(a.collapsedDescendants).toBe(2);
    expect(model.clusters).toHaveLength(1);
    const cluster = model.clusters[0];
    expect(cluster?.memberLaneIds).toEqual(['B', 'C']);
    expect(cluster?.counts).toEqual({
      sessions: cluster?.memberLaneIds.length,
      subagents: 2,
      messages: 20,
      commits: 1,
      openEnded: 1,
    });
    expect(cluster?.grades).toEqual({ inferred: 1, unavailable: 1, exact: 2 });
    expect(cluster?.laneId).toBe('A');
    // Nothing from the hidden lanes is drawn. The bundle draws its own
    // session's marks, but its spawn marks fold into the cluster count.
    expect(model.nodes.every((n) => n.laneId === 'D' || n.laneId === 'A')).toBe(true);
    expect(model.nodes.some((n) => n.laneId === 'A')).toBe(true);
    expect(model.nodes.some((n) => n.laneId === 'A' && n.kind === 'spawn')).toBe(false);
    expect(model.counts.eventsFolded).toBeGreaterThan(0);
    expect(model.counts.lanesCollapsed).toBe(1);
    // The spawn into the bundle is not a self-loop; the spawn B→C is inside it.
    expect(model.paths.filter((p) => p.kind === 'spawn')).toHaveLength(0);
    expect(model.counts.relationsDrawn).toBe(0);
    // The gap on hidden lane C anchors at the bundle.
    const gap = model.gaps.find((g) => g.kind === 'extent_unknown');
    expect(gap).toMatchObject({ x: a.x1, y: a.y });
  });

  it('starts roots collapsed on a dense page and lets one be re-opened', () => {
    const lanes: JourneyLane[] = [];
    for (let i = 0; i < 6; i += 1) {
      const root = lane({ id: `r${i}`, start: T0 + i * 10, end: T0 + 500, endSource: 'session_end' });
      lanes.push(root, lane({ id: `c${i}`, parentId: root.id, depth: 1, start: T0 + i * 10 + 5 }));
    }
    const proj = projection({ lanes });
    const dense = layoutTemporalScene(proj, optionsFor(proj, { denseLaneThreshold: 10 }));
    expect(dense.denseDefault).toBe(true);
    expect(dense.lanes.map((l) => l.id)).toEqual(['r0', 'r1', 'r2', 'r3', 'r4', 'r5']);
    expect(dense.lanes.every((l) => l.kind === 'bundle')).toBe(true);
    expect(dense.counts.lanesCollapsed).toBe(6);

    const reopened = layoutTemporalScene(
      proj,
      optionsFor(proj, {
        denseLaneThreshold: 10,
        branches: { collapsed: new Set(), expanded: new Set(['r2']) },
      }),
    );
    expect(reopened.lanes.map((l) => l.id)).toEqual(['r0', 'r1', 'r2', 'c2', 'r3', 'r4', 'r5']);
    expect(sceneLane(reopened, 'r2').kind).toBe('session');
    expect(reopened.counts.lanesCollapsed).toBe(5);

    const sparse = layoutTemporalScene(proj, optionsFor(proj, { denseLaneThreshold: 12 }));
    expect(sparse.denseDefault).toBe(false);
    expect(sparse.lanes).toHaveLength(12);
  });

  it('collapses every parent at workstream zoom', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(
      proj,
      optionsFor(proj, {
        zoom: 'workstream',
        branches: { collapsed: new Set(), expanded: new Set(['A', 'B']) },
      }),
    );
    expect(model.lanes.map((l) => [l.id, l.kind])).toEqual([
      ['A', 'bundle'],
      ['D', 'session'],
    ]);
    expect(model.lanes.every((l) => l.height === 16)).toBe(true);
    expect(model.clusters).toHaveLength(1);
  });

  it('withholds records after the reveal boundary and cuts the active lane by sequence', () => {
    const proj = treeProjection();
    const withTranscript: JourneyProjection = {
      ...proj,
      events: [
        ...proj.events,
        event({ id: 'msg:A:1', laneId: 'A', kind: 'message_user', source: 'transcript', time: T0 + 100, sequence: 0, label: 'user' }),
        event({ id: 'msg:A:2', laneId: 'A', kind: 'message_assistant', source: 'transcript', time: T0 + 200, sequence: 1, label: 'assistant' }),
        event({ id: 'msg:A:3', laneId: 'A', kind: 'tool_call', source: 'transcript', time: T0 + 300, sequence: 2, label: 'Read' }),
      ],
    };
    const model = layoutTemporalScene(
      withTranscript,
      optionsFor(withTranscript, {
        zoom: 'event',
        selectedLaneId: 'A',
        reveal: { time: T0 + 650, laneId: 'A', sequence: 1 },
      }),
    );
    const drawn = new Set(model.nodes.map((n) => n.id));
    // Dated records after the boundary on every lane are withheld.
    expect(drawn.has('end:A')).toBe(false);
    expect(drawn.has('commit:B:abc')).toBe(false);
    expect(drawn.has('start:C')).toBe(false);
    expect(drawn.has('end:D')).toBe(false);
    // Records at or before it stay.
    expect(drawn.has('start:A')).toBe(true);
    expect(drawn.has('spawn:B')).toBe(true);
    expect(drawn.has('start:B')).toBe(true);
    // The active lane's transcript is cut by sequence even though msg 3 is dated before the boundary.
    expect(drawn.has('msg:A:1')).toBe(true);
    expect(drawn.has('msg:A:2')).toBe(true);
    expect(drawn.has('msg:A:3')).toBe(false);
    expect(model.counts.eventsWithheld).toBe(
      withTranscript.events.length - model.counts.eventsDrawn - model.counts.eventsFolded,
    );
    // Relation B→C at T0+700 is after the boundary; A→B at T0+600 is drawn.
    expect(model.paths.filter((p) => p.kind === 'spawn').map((p) => p.toId)).toEqual(['B']);
    expect(model.counts).toMatchObject({ relationsTotal: 2, relationsDrawn: 1, relationsWithheld: 1 });
    expect(model.cursor).toEqual({
      x: timeToX(model.viewport, T0 + 650),
      laneId: 'A',
      xBasis: 'time',
    });
  });

  it('does not let an undated cursor cut other lanes', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(
      proj,
      optionsFor(proj, { reveal: { time: null, laneId: 'A', sequence: 0 } }),
    );
    expect(model.counts.eventsWithheld).toBe(0);
    expect(model.counts.relationsWithheld).toBe(0);
    expect(model.nodes.some((n) => n.id === 'end:D')).toBe(true);
  });

  it('filters hidden kinds and counts them without touching the projection', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(
      proj,
      optionsFor(proj, { hiddenKinds: new Set(['commit', 'spawn']) }),
    );
    expect(model.nodes.some((n) => n.kind === 'commit' || n.kind === 'spawn')).toBe(false);
    expect(model.counts.eventsFiltered).toBe(3);
    expect(model.counts.eventsDrawn + model.counts.eventsFiltered).toBe(proj.events.length);
    expect(proj.events.filter((e) => e.kind === 'commit')).toHaveLength(1);
  });

  it('culls dated records outside the window and counts them', () => {
    const proj = treeProjection();
    const window = { start: T0 + 550, end: T0 + 1000 };
    const model = layoutTemporalScene(
      proj,
      optionsFor(proj, { viewport: viewportFor(proj.extent, window) }),
    );
    const drawn = model.nodes.map((n) => n.id).sort();
    expect(drawn).toEqual(['commit:B:abc', 'spawn:B', 'spawn:C', 'start:B', 'start:C'].sort());
    expect(model.counts.eventsCulled).toBe(proj.events.length - drawn.length);
    for (const node of model.nodes) {
      expect(node.x).toBeGreaterThanOrEqual(model.viewport.left);
      expect(node.x).toBeLessThanOrEqual(model.viewport.width - model.viewport.right);
    }
    // Lane extents are clamped to the window; A's start lies left of it.
    expect(sceneLane(model, 'A').x0).toBe(200);
    expect(sceneLane(model, 'A').x1).toBe(980);
    expect(sceneLane(model, 'A').offscreen).toBe(false);
    // The git span on A (T0+100..T0+500) is entirely before the window.
    expect(model.intervals.map((i) => i.id)).toEqual([]);
  });

  it('marks a lane offscreen when its whole recorded extent is outside the window', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(
      proj,
      optionsFor(proj, { viewport: viewportFor(proj.extent, { start: T0 + 1500, end: T0 + 1600 }) }),
    );
    expect(sceneLane(model, 'B').offscreen).toBe(true);
    expect(sceneLane(model, 'A').offscreen).toBe(false);
  });

  it('lays undated turns across a sequence gutter joined by recorded-order paths', () => {
    const proj = treeProjection();
    const withTranscript: JourneyProjection = {
      ...proj,
      events: [
        ...proj.events,
        event({ id: 'msg:A:1', laneId: 'A', kind: 'message_user', source: 'transcript', time: null, sequence: 0, label: 'user' }),
        event({ id: 'msg:A:2', laneId: 'A', kind: 'tool_call', source: 'transcript', time: null, sequence: 1, label: 'Read' }),
        event({ id: 'msg:A:3', laneId: 'A', kind: 'message_assistant', source: 'transcript', time: T0 + 1500, sequence: 2, label: 'assistant' }),
        event({ id: 'msg:A:4', laneId: 'A', kind: 'message_other', source: 'transcript', time: null, sequence: 3, label: 'system' }),
      ],
      gaps: [
        ...proj.gaps,
        { id: 'gap:undated_events:A', laneId: 'A', kind: 'undated_events', grade: 'unavailable', detail: '3 of 4 loaded turns carry no timestamp; placed in recorded order' },
      ],
    };
    const model = layoutTemporalScene(
      withTranscript,
      optionsFor(withTranscript, { zoom: 'event', selectedLaneId: 'A' }),
    );
    const a = sceneLane(model, 'A');
    expect(a.expanded).toBe(true);
    expect(a.height).toBe(112 + 22);
    expect(model.lanes.filter((l) => l.id !== 'A').every((l) => l.height === 22)).toBe(true);

    const undated = model.nodes.filter((n) => n.xBasis === 'sequence');
    expect(undated.map((n) => n.id)).toEqual(['msg:A:1', 'msg:A:2', 'msg:A:4']);
    for (let i = 1; i < undated.length; i += 1) {
      expect(undated[i]?.x).toBeGreaterThan(undated[i - 1]?.x ?? Infinity);
    }
    const gutterY = a.y + a.height / 2 - 14;
    expect(undated.every((n) => n.y === gutterY)).toBe(true);
    expect(undated[0]?.x).toBeCloseTo(a.x0 + (1 / 4) * (a.x1 - a.x0), 6);

    const dated = model.nodes.find((n) => n.id === 'msg:A:3');
    expect(dated).toMatchObject({ xBasis: 'time', y: a.y });
    expect(dated?.x).toBeCloseTo(timeToX(model.viewport, T0 + 1500), 6);

    const sequencePaths = model.paths.filter((p) => p.kind === 'sequence');
    expect(sequencePaths.map((p) => [p.fromId, p.toId])).toEqual([
      ['msg:A:1', 'msg:A:2'],
      ['msg:A:2', 'msg:A:4'],
    ]);
    expect(sequencePaths[0]).toMatchObject({ grade: 'exact', basis: 'recorded order' });
    expect(sequencePaths[0]?.controls).toEqual([
      undated[0]?.x,
      gutterY,
      undated[1]?.x,
      gutterY,
    ]);
    expect(model.gaps.find((g) => g.kind === 'undated_events')).toMatchObject({ x: a.x0, y: gutterY });
    expect(model.counts.eventsFolded).toBe(0);

    // The same transcript is folded, not drawn, when the lane is not expanded.
    const folded = layoutTemporalScene(withTranscript, optionsFor(withTranscript, { selectedLaneId: 'A' }));
    expect(folded.counts.eventsFolded).toBe(4);
    expect(folded.nodes.some((n) => n.source === 'transcript')).toBe(false);
    expect(sceneLane(folded, 'A').expanded).toBe(false);

    // A sequence-based cursor sits on the emitted node.
    const cursor = layoutTemporalScene(
      withTranscript,
      optionsFor(withTranscript, {
        zoom: 'event',
        selectedLaneId: 'A',
        reveal: { time: null, laneId: 'A', sequence: 1 },
      }),
    );
    const second = cursor.nodes.find((n) => n.id === 'msg:A:2');
    expect(cursor.cursor).toEqual({ x: second?.x, laneId: 'A', xBasis: 'sequence' });
    expect(cursor.nodes.some((n) => n.id === 'msg:A:4')).toBe(false);
    expect(cursor.nodes.some((n) => n.id === 'msg:A:3')).toBe(false);
  });

  it('keeps hit regions from overlapping on a dense lane', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(proj, optionsFor(proj));
    const a = model.nodes.filter((n) => n.laneId === 'A').sort((l, r) => l.x - r.x);
    expect(a.map((n) => n.id)).toEqual(['start:A', 'spawn:B', 'end:A']);
    const gap = (a[1]?.x ?? 0) - (a[0]?.x ?? 0);
    expect(a[0]?.halfHit).toBeCloseTo(Math.min(22, gap / 2), 6);
    expect(model.nodes.find((n) => n.laneId === 'C')?.halfHit).toBe(22);
  });

  it('applies focus-plus-context around the selected lane', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(proj, optionsFor(proj, { selectedLaneId: 'C' }));
    expect(model.lanes.map((l) => [l.id, l.focus])).toEqual([
      ['A', 'path'],
      ['B', 'path'],
      ['C', 'selected'],
      ['D', 'context'],
    ]);
    expect(model.nodes.find((n) => n.laneId === 'C')?.focus).toBe('selected');
    expect(model.nodes.find((n) => n.laneId === 'D')?.focus).toBe('context');
    const spawns = model.paths.filter((p) => p.kind === 'spawn');
    expect(spawns.map((p) => [p.toId, p.focus])).toEqual([
      ['B', 'path'],
      ['C', 'selected'],
    ]);
    // Collapsing B still marks the bundle standing for the selected lane's ancestor.
    const bundled = layoutTemporalScene(
      proj,
      optionsFor(proj, { selectedLaneId: 'C', branches: { collapsed: new Set(['B']), expanded: new Set() } }),
    );
    expect(sceneLane(bundled, 'B')).toMatchObject({ kind: 'bundle', focus: 'path' });
    expect(bundled.clusters[0]?.focus).toBe('path');

    const neutral = layoutTemporalScene(proj, optionsFor(proj));
    expect(neutral.lanes.every((l) => l.focus === 'neutral')).toBe(true);
    expect(neutral.paths.every((p) => p.focus === 'neutral')).toBe(true);
  });

  it('draws spawn relations as S-curves between the parent and child rows', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(proj, optionsFor(proj));
    const path = model.paths.find((p) => p.kind === 'spawn' && p.toId === 'B');
    const a = sceneLane(model, 'A');
    const b = sceneLane(model, 'B');
    const x = timeToX(model.viewport, T0 + 600);
    const k = Math.min(28, Math.max(8, Math.abs(b.y - a.y) * 0.35));
    expect(path?.controls).toEqual([x - k, a.y, x + k * 0.2, a.y, x - k * 0.2, b.y, x + k, b.y]);
    expect(path).toMatchObject({ fromId: 'A', grade: 'exact', basis: spawn(A, B).basis, weight: null });
  });

  it('places intervals on their lane rows, clamped to the window', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(proj, optionsFor(proj));
    const a = sceneLane(model, 'A');
    const d = sceneLane(model, 'D');
    expect(model.intervals.find((i) => i.kind === 'git_span')).toMatchObject({
      laneId: 'A',
      y: a.y + Math.min(10, a.height * 0.3),
      x0: timeToX(model.viewport, T0 + 100),
      x1: timeToX(model.viewport, T0 + 500),
    });
    expect(model.intervals.find((i) => i.kind === 'proximity')).toMatchObject({
      laneId: 'D',
      y: d.y,
      tone: 'overlap',
      ref: 'e1',
    });
  });

  it('keeps page-wide gaps unanchored and anchors lane gaps', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(proj, optionsFor(proj));
    expect(model.gaps).toHaveLength(2);
    expect(model.gaps.find((g) => g.kind === 'handoff_unavailable')).toMatchObject({ x: null, y: null, laneId: null });
    const c = sceneLane(model, 'C');
    expect(model.gaps.find((g) => g.kind === 'extent_unknown')).toMatchObject({ x: c.x1, y: c.y });
  });

  it('labels lanes and marked nodes, dropping the lower priority in a collision', () => {
    const proj = treeProjection();
    const near = event({
      id: 'commit:A:near',
      laneId: 'A',
      kind: 'commit',
      time: T0 + 601,
      source: 'commit',
      label: 'deadbee',
      ref: 'deadbeef',
      sequence: 2,
    });
    const crowded: JourneyProjection = { ...proj, events: [...proj.events, near] };
    const model = layoutTemporalScene(crowded, optionsFor(crowded, { selectedEventId: 'commit:A:near' }));
    const laneLabels = model.labels.filter((l) => l.group === 'lane');
    expect(laneLabels.map((l) => [l.text, l.priority])).toEqual([
      ['A', 3],
      ['B', 2],
      ['C', 2],
      ['D', 3],
    ]);
    expect(laneLabels.every((l) => l.x === 8 && l.anchor === 'start')).toBe(true);
    const aLabels = model.labels.filter((l) => l.group === 'lane:A');
    // The selected commit (priority 5) is the only node label on an unexpanded
    // lane: the spawn's name is already printed by the child's own lane row.
    expect(aLabels.map((l) => l.text)).toEqual(['deadbee']);
    expect(aLabels[0]?.priority).toBe(5);
    expect(aLabels[0]?.y).toBe(sceneLane(model, 'A').y + 18);
    // Unmarked session start/end and spawn nodes on an unexpanded lane carry no label.
    expect(model.labels.some((l) => l.text === 'start')).toBe(false);
    const spaced = layoutTemporalScene(proj, optionsFor(proj));
    expect(spaced.labels.filter((l) => l.group === 'lane:A').map((l) => l.text)).toEqual([]);
    // Two commits a pixel apart: the higher priority (selected) keeps its label.
    const twin = event({
      id: 'commit:A:twin',
      laneId: 'A',
      kind: 'commit',
      time: T0 + 602,
      source: 'commit',
      label: 'cafef00',
      ref: 'cafef00d',
      sequence: 3,
    });
    const colliding: JourneyProjection = { ...proj, events: [...proj.events, near, twin] };
    const resolved = layoutTemporalScene(colliding, optionsFor(colliding, { selectedEventId: 'commit:A:twin' }));
    expect(resolved.labels.filter((l) => l.group === 'lane:A').map((l) => l.text)).toEqual(['cafef00']);
  });

  it('maps the minimap window onto the full extent', () => {
    const proj = treeProjection();
    const fitted = layoutTemporalScene(proj, optionsFor(proj));
    expect(fitted.minimap.width).toBe(1000);
    expect(fitted.minimap.height).toBe(64);
    expect(fitted.minimap.bins).toHaveLength(96);
    expect(fitted.minimap.window.x0).toBeCloseTo(0, 6);
    expect(fitted.minimap.window.x1).toBeCloseTo(1000, 6);
    expect(fitted.minimap.bins.reduce((sum, bin) => sum + bin.events, 0)).toBe(
      proj.events.filter((e) => e.time !== null).length,
    );
    expect(fitted.minimap.lanes.map((l) => l.id)).toEqual(['A', 'B', 'C', 'D']);
    expect(fitted.minimap.lanes[0]?.y).toBe(6);
    expect(fitted.minimap.lanes[3]?.y).toBe(58);

    const full = fittedWindowFor(proj.extent);
    const half = { start: full.start, end: full.start + (full.end - full.start) / 2 };
    const zoomed = layoutTemporalScene(proj, optionsFor(proj, { viewport: viewportFor(proj.extent, half) }));
    expect(zoomed.minimap.window.x0).toBeCloseTo(0, 6);
    expect(zoomed.minimap.window.x1).toBeCloseTo(500, 6);
    // Bins cover the full extent regardless of the window.
    expect(zoomed.minimap.bins).toEqual(fitted.minimap.bins);
  });

  it('has no cursor without a reveal boundary', () => {
    const proj = treeProjection();
    expect(layoutTemporalScene(proj, optionsFor(proj)).cursor).toBeNull();
    const absent = layoutTemporalScene(
      proj,
      optionsFor(proj, { reveal: { time: T0 + 10, laneId: 'A', sequence: 99 } }),
    );
    expect(absent.cursor).toBeNull();
  });

  it('emits axis ticks in absolute pixels inside the axis', () => {
    const proj = treeProjection();
    const model = layoutTemporalScene(proj, optionsFor(proj));
    expect(model.ticks.length).toBeGreaterThan(1);
    for (const tick of model.ticks) {
      expect(tick.x).toBeGreaterThanOrEqual(200);
      expect(tick.x).toBeLessThanOrEqual(980);
      expect(tick.x).toBeCloseTo(timeToX(model.viewport, tick.time), 6);
    }
  });

  it('lays out an empty projection without throwing', () => {
    const proj = projection({ lanes: [] });
    const model = layoutTemporalScene(proj, optionsFor(proj));
    expect(model.lanes).toEqual([]);
    expect(model.height).toBe(RULER_HEIGHT + 16);
    expect(model.minimap.bins).toHaveLength(96);
    expect(model.counts.lanesTotal).toBe(0);
  });
});
