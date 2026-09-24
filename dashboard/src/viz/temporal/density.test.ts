import { describe, expect, it } from 'vitest';
import { layoutDensity, newestLoadedTime } from './density.ts';
import { DEFAULT_DENSE_LANE_THRESHOLD, layoutTemporalScene } from './layout.ts';
import type { JourneyEvent, JourneyLane, JourneyProjection, LayoutOptions } from './types.ts';

const T0 = 1_784_700_000;

function lane(over: Partial<JourneyLane> & { id: string }): JourneyLane {
  return {
    sessionId: over.id, provider: 'cursor', label: over.id, agent: null, start: T0, end: null, endSource: null,
    parentId: null, depth: 0, isSubagent: false, messages: 0, editedFilesRecorded: false, editedFileCount: 0, models: [],
    ...over,
  };
}

function event(over: Partial<JourneyEvent> & { id: string; laneId: string; time: number | null }): JourneyEvent {
  return { kind: 'session_start', sequence: 0, grade: 'exact', source: 'session', label: 'start', detail: null, ref: over.id, ...over };
}

const A = lane({ id: 'A', start: T0 + 50, end: T0 + 450, endSource: 'session_end', messages: 30 });
const B = lane({ id: 'B', parentId: 'A', depth: 1, isSubagent: true, start: T0 + 150, messages: 5 });

const PROJECTION: JourneyProjection = {
  lanes: [A, B],
  events: [
    event({ id: 'start:A', laneId: 'A', time: T0 + 50 }),
    event({ id: 'spawn:B', laneId: 'A', kind: 'spawn', time: T0 + 150, source: 'parentage', ref: 'B', sequence: 1 }),
    event({ id: 'commit:A', laneId: 'A', kind: 'commit', time: T0 + 320, source: 'commit', grade: 'inferred', sequence: 2 }),
    event({ id: 'end:A', laneId: 'A', kind: 'session_end', time: T0 + 450, sequence: 3 }),
    event({ id: 'msg:A:0', laneId: 'A', kind: 'message_user', time: null, source: 'transcript', sequence: 0 }),
    event({ id: 'start:B', laneId: 'B', time: T0 + 150 }),
  ],
  relations: [{ id: 'rel:spawn:B', kind: 'spawn', fromLaneId: 'A', toLaneId: 'B', time: T0 + 150, grade: 'exact', basis: 'parent_session_id' }],
  gaps: [],
  intervals: [],
  extent: { start: T0 + 50, end: T0 + 3650 },
  stats: { lanes: 2, roots: 1, subagents: 1, messages: 35, openEnded: 1, hollow: 0, undated: 0, providers: [{ id: 'cursor', lanes: 2, messages: 35 }] },
};

/** One pixel per second: a 1000px field over a 1000s window, 100px bins. */
function options(over: Partial<LayoutOptions> = {}): LayoutOptions {
  return {
    viewport: { width: 1220, left: 200, right: 20, window: { start: T0, end: T0 + 1000 } },
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

function density(over: Partial<LayoutOptions> = {}) {
  const opts = options(over);
  const model = layoutTemporalScene(PROJECTION, opts);
  return layoutDensity(PROJECTION, model, { reveal: opts.reveal, hiddenKinds: opts.hiddenKinds, binPx: 100 });
}

describe('layoutDensity', () => {
  it('bins measured extent as active and an unrecorded end as open, never active', () => {
    const result = density();
    const a = result.lanes.get('A')!;
    const b = result.lanes.get('B')!;
    expect(a.bins.map((bin) => bin.active)).toEqual([1, 1, 1, 1, 1, 0, 0, 0, 0, 0]);
    expect(a.bins.map((bin) => bin.events)).toEqual([1, 1, 0, 1, 1, 0, 0, 0, 0, 0]);
    expect(a.bins.map((bin) => bin.starts)).toEqual([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    expect(b.bins.map((bin) => bin.active)).toEqual([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    expect(b.bins.map((bin) => bin.open)).toEqual([0, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
    expect(a.bins[0]).toMatchObject({ x0: 200, x1: 300 });
  });

  it('counts an undated turn in the totals and in no bin', () => {
    const a = density().lanes.get('A')!;
    expect(a.totals).toEqual({ sessions: 1, messages: 30, commits: 1, events: 5, undated: 1, openEnded: 0 });
    expect(a.bins.reduce((sum, bin) => sum + bin.events, 0)).toBe(4);
  });

  it('measures the tightest gap between drawn marks on a row', () => {
    expect(density().lanes.get('A')!.minGap).toBe(100);
    expect(density().lanes.get('B')!.minGap).toBe(Infinity);
  });

  it('measures a crowded undated gutter as its own row', () => {
    const turns = Array.from({ length: 5 }, (_, sequence) =>
      event({ id: `msg:A:${sequence}`, laneId: 'A', kind: 'message_assistant', time: null, source: 'transcript', sequence }),
    );
    const projection = { ...PROJECTION, events: [...PROJECTION.events.filter((e) => e.source !== 'transcript'), ...turns] };
    const opts = options({ zoom: 'event', selectedLaneId: 'A' });
    const model = layoutTemporalScene(projection, opts);
    const a = layoutDensity(projection, model, { reveal: null, hiddenKinds: new Set(), binPx: 100 }).lanes.get('A')!;
    // Five turns spread over the 400px lane extent sit 400/6 apart, closer than the row's 100px.
    expect(a.minGap).toBeCloseTo(400 / 6, 6);
    expect(a.totals.undated).toBe(5);
  });

  it('folds a collapsed subtree into its bundle with exact member totals', () => {
    const bundle = density({ branches: { collapsed: new Set(['A']), expanded: new Set() } }).lanes.get('A')!;
    expect(bundle.totals).toEqual({ sessions: 2, messages: 35, commits: 1, events: 6, undated: 1, openEnded: 1 });
    expect(bundle.bins.map((bin) => bin.open)).toEqual([0, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
    expect(bundle.bins.map((bin) => bin.starts)).toEqual([1, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
  });

  it('bins nothing past a dated playback cursor', () => {
    const result = density({ reveal: { time: T0 + 300, laneId: 'A', sequence: 0 } });
    const a = result.lanes.get('A')!;
    expect(a.bins.map((bin) => bin.active)).toEqual([1, 1, 1, 1, 0, 0, 0, 0, 0, 0]);
    expect(a.bins.map((bin) => bin.events)).toEqual([1, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    expect(result.lanes.get('B')!.bins.map((bin) => bin.open)).toEqual([0, 1, 1, 1, 0, 0, 0, 0, 0, 0]);
  });

  it('names the newest loaded record as the tail, not the padded extent', () => {
    expect(newestLoadedTime(PROJECTION)).toBe(T0 + 450);
    const result = density();
    expect(result.tailTime).toBe(T0 + 450);
    expect(result.headTime).toBe(T0 + 50);
    expect(result.tailX).toBe(650);
    const outside = density({ viewport: { width: 1220, left: 200, right: 20, window: { start: T0 + 500, end: T0 + 1500 } } });
    expect(outside.tailX).toBeNull();
  });
});
