import { describe, expect, it } from 'vitest';
import { fittedWindowFor, layoutTemporalScene, timeToX } from './layout.ts';
import type { TemporalPalette } from './palette.ts';
import { paintSpindle } from './scene/spindlePaint.ts';
import { layoutSpindle } from './spindle.ts';
import type {
  JourneyEvent,
  JourneyLane,
  JourneyProjection,
  LayoutOptions,
  SceneNode,
  TemporalSceneModel,
} from './types.ts';

const START = 1_784_700_000;
const ROOT = JSON.stringify(['cursor', 'root']);
const CHILD = JSON.stringify(['cursor', 'child']);
const OPEN = JSON.stringify(['cursor', 'open']);
const OTHER = JSON.stringify(['codex', 'root']);
const FUTURE = JSON.stringify(['codex', 'later']);

function lane(id: string, overrides: Partial<JourneyLane> = {}): JourneyLane {
  const [provider, sessionId] = JSON.parse(id) as [string, string];
  return {
    id, provider, sessionId, label: `${provider} ${sessionId}`, agent: null,
    start: START, end: START + 800, endSource: 'session_end', parentId: null,
    depth: 0, isSubagent: false, messages: 12, editedFilesRecorded: false,
    editedFileCount: 0, models: [], ...overrides,
  };
}

function event(id: string, laneId: string, time: number | null, overrides: Partial<JourneyEvent> = {}): JourneyEvent {
  return {
    id, laneId, time, kind: 'tool_call', source: 'recorded_event', sequence: 1,
    grade: 'exact', label: id, detail: `source record ${id}`, ref: `record:${id}`,
    ...overrides,
  };
}

function projection(lanes: readonly JourneyLane[], extra: Partial<JourneyProjection> = {}): JourneyProjection {
  const events = lanes.flatMap((item) => [
    event(`start:${item.id}`, item.id, item.start, { kind: 'session_start', source: 'session', sequence: 0 }),
    ...(item.end === null ? [] : [event(`end:${item.id}`, item.id, item.end, { kind: 'session_end', source: 'session', sequence: 9 })]),
  ]);
  const providers = [...new Set(lanes.map((item) => item.provider))].map((id) => ({
    id,
    lanes: lanes.filter((item) => item.provider === id).length,
    messages: lanes.filter((item) => item.provider === id).reduce((sum, item) => sum + item.messages, 0),
  }));
  return {
    lanes, events, relations: [], gaps: [], intervals: [],
    extent: { start: START, end: START + 800 },
    stats: {
      lanes: lanes.length, roots: lanes.filter((item) => item.parentId === null).length,
      subagents: lanes.filter((item) => item.isSubagent).length,
      messages: lanes.reduce((sum, item) => sum + item.messages, 0),
      openEnded: lanes.filter((item) => item.end === null).length,
      hollow: lanes.filter((item) => item.messages === 0).length, undated: 0, providers,
    },
    ...extra,
  };
}

function fixture(): JourneyProjection {
  const lanes = [
    lane(ROOT, { end: START + 600 }),
    lane(CHILD, { start: START + 80, end: START + 550, parentId: ROOT, depth: 1, isSubagent: true }),
    lane(OPEN, { start: START + 120, end: null, endSource: null, parentId: ROOT, depth: 1, isSubagent: true }),
    lane(OTHER, { start: START + 40, endSource: 'last_message' }),
    lane(FUTURE, { start: START + 500, end: START + 750, parentId: OTHER, depth: 1, isSubagent: true }),
  ];
  const base = projection(lanes);
  return {
    ...base,
    events: [
      ...base.events,
      event('tool:spawn', ROOT, START + 80),
      event('tool:read', ROOT, START + 180),
      event('commit:child', CHILD, START + 300, { kind: 'commit', source: 'commit', grade: 'inferred', sequence: 2 }),
      event('undated:root', ROOT, null, { source: 'transcript', grade: 'unavailable', sequence: 3 }),
      event('pr:future', OTHER, START + 700, { kind: 'pull_request', sequence: 4 }),
    ],
    relations: [{
      id: 'spawn:child', kind: 'spawn', fromLaneId: ROOT, toLaneId: CHILD,
      time: START + 80, fromEventId: 'tool:spawn', grade: 'exact',
      basis: 'recorded parent_session_id and parent_tool_use_id',
    }],
    intervals: [{
      id: 'span:root', laneId: ROOT, kind: 'git_span', start: START + 140,
      end: START + 560, label: 'recorded branch span', grade: 'explicit', tone: null, ref: null,
    }],
    gaps: [
      { id: 'gap:open', laneId: OPEN, kind: 'extent_unknown', grade: 'unavailable', detail: 'No recorded end or last message.' },
      { id: 'gap:handoff', laneId: null, kind: 'handoff_unavailable', grade: 'unavailable', detail: 'No session-bound handoff authority.' },
    ],
  };
}

function denseFixture(): JourneyProjection {
  const roots = [lane(ROOT), lane(OTHER)];
  const members = Array.from({ length: 101 }, (_, index) => {
    const parent = roots[index % roots.length]!;
    return lane(JSON.stringify([parent.provider, `member-${Math.floor(index / 2)}`]), {
      start: START + 20 + index * 2, end: START + 500 + index,
      parentId: parent.id, depth: 1, isSubagent: true,
    });
  });
  return projection([roots[0]!, ...members.filter((item) => item.parentId === ROOT), roots[1]!, ...members.filter((item) => item.parentId === OTHER)]);
}

function layout(input: JourneyProjection, overrides: Partial<LayoutOptions> = {}) {
  const options: LayoutOptions = {
    viewport: { width: 1000, left: 200, right: 24, window: { start: START, end: START + 800 } },
    zoom: 'event', branches: { collapsed: new Set(), expanded: new Set() },
    selectedLaneId: ROOT, selectedEventId: 'tool:read', reveal: null,
    hiddenKinds: new Set(), denseLaneThreshold: 48, ...overrides,
  };
  const base = layoutTemporalScene(input, options);
  return { base, options, model: layoutSpindle(input, base, options) };
}

function record(node: SceneNode) {
  const { id, laneId, kind, x, xBasis, grade, source, label, detail, ref, selected, focus } = node;
  return { id, laneId, kind, x, xBasis, grade, source, label, detail, ref, selected, focus };
}

function requiredNode(model: TemporalSceneModel, id: string): SceneNode {
  const node = model.nodes.find((item) => item.id === id);
  expect(node, `event ${id} must remain represented`).toBeDefined();
  return node!;
}

describe('spindle projection', () => {
  it('changes presentation without rewriting source identities, time, evidence, counts, or causal edges', () => {
    const input = fixture();
    const inputBefore = JSON.stringify(input);
    const options: LayoutOptions = {
      viewport: { width: 1000, left: 200, right: 24, window: { start: START, end: START + 800 } },
      zoom: 'event', branches: { collapsed: new Set(), expanded: new Set() },
      selectedLaneId: ROOT, selectedEventId: 'tool:read', reveal: null,
      hiddenKinds: new Set(), denseLaneThreshold: 48,
    };
    const base = layoutTemporalScene(input, options);
    const baseBefore = JSON.stringify(base);
    const model = layoutSpindle(input, base, options);

    expect(JSON.stringify(input)).toBe(inputBefore);
    expect(JSON.stringify(base)).toBe(baseBefore);
    expect(model.nodes.map(record).sort((a, b) => a.id.localeCompare(b.id)))
      .toEqual(base.nodes.map(record).sort((a, b) => a.id.localeCompare(b.id)));
    expect(model.counts).toEqual(base.counts);
    expect(model.cursor).toEqual(base.cursor);
    expect(model.paths.map(({ id, kind, fromId, toId, grade, basis }) => ({ id, kind, fromId, toId, grade, basis })))
      .toEqual(base.paths.map(({ id, kind, fromId, toId, grade, basis }) => ({ id, kind, fromId, toId, grade, basis })));
    expect(model.paths.filter((path) => ['handoff', 'rejoin', 'result'].includes(path.kind))).toEqual([]);
    expect(model.nodes.some((node) => node.y !== requiredNode(base, node.id).y)).toBe(true);
    expect(new Set(model.spindle?.fibers.map((fiber) => fiber.id))).toEqual(new Set(input.lanes.map((item) => item.id)));
  });

  it('keeps a retained event on the same strand when panning, zooming time, or seeking replay', () => {
    const input = fixture();
    const full = layout(input);
    const panned = layout(input, {
      viewport: { ...full.options.viewport, window: { start: START + 150, end: START + 650 } },
    });
    const replay = layout(input, { reveal: { time: START + 400, laneId: ROOT, sequence: 3 } });

    for (const id of ['tool:read', 'commit:child']) {
      const original = requiredNode(full.model, id);
      expect(requiredNode(panned.model, id).y).toBeCloseTo(original.y, 8);
      expect(requiredNode(replay.model, id).y).toBeCloseTo(original.y, 8);
      const source = input.events.find((item) => item.id === id)!;
      expect(requiredNode(panned.model, id).x).toBeCloseTo(timeToX(panned.options.viewport, source.time!), 8);
    }
  });

  it('stops each actual session at its recorded extent and keeps an unknown end as a short unavailable stub', () => {
    const input = fixture();
    const { base, model, options } = layout(input);
    expect(model.spindle?.fibers.length).toBe(input.lanes.length);
    for (const fiber of model.spindle!.fibers) {
      const source = input.lanes.find((item) => item.id === fiber.id)!;
      const first = fiber.points[0]!;
      const last = fiber.points.at(-1)!;
      expect(first.x).toBeCloseTo(timeToX(model.viewport, source.start), 8);
      if (source.end !== null) {
        expect(last.x).toBeCloseTo(timeToX(model.viewport, source.end), 8);
      } else {
        const flat = base.lanes.find((item) => item.id === source.id)!;
        expect(last.x - first.x).toBeLessThanOrEqual(flat.x1 - flat.x0 + 1e-8);
        expect(fiber.grade).toBe('unavailable');
        expect(fiber.endSource).toBeNull();
      }
      for (let index = 0; index < fiber.points.length; index += 1) {
        const point = fiber.points[index]!;
        expect(Number.isFinite(point.y)).toBe(true);
        expect(point.x).toBeGreaterThanOrEqual(first.x - 1e-8);
        expect(point.x).toBeLessThanOrEqual(last.x + 1e-8);
        if (index > 0) expect(point.x).toBeGreaterThanOrEqual(fiber.points[index - 1]!.x);
      }
    }

    const branches = { collapsed: new Set([ROOT]), expanded: new Set<string>() };
    const collapsed = layout(input, { branches });
    expect(collapsed.base.lanes.some((item) => item.id === OPEN)).toBe(false);
    const openTail = collapsed.model.spindle!.fibers.find((fiber) => fiber.id === OPEN)!.points.at(-1)!;
    const sourceGap = input.gaps.find((gap) => gap.id === 'gap:open')!;
    expect(collapsed.model.gaps.find((gap) => gap.id === sourceGap.id)).toMatchObject({
      x: openTail.x, y: openTail.y, detail: sourceGap.detail, grade: sourceGap.grade,
    });
    const beforeStart = layout(input, {
      branches, reveal: { time: START + 100, laneId: ROOT, sequence: 1 },
    }).model;
    const outsideWindow = layout(input, {
      branches, viewport: { ...options.viewport, window: { start: START + 200, end: START + 600 } },
    }).model;
    for (const hidden of [beforeStart, outsideWindow]) {
      expect(hidden.gaps.find((gap) => gap.id === sourceGap.id)).toMatchObject({
        x: null, y: null, detail: sourceGap.detail, grade: sourceGap.grade,
      });
    }
  });

  it('withholds future sessions, events, and strand tails at the same replay boundary', () => {
    const input = fixture();
    const { model, base } = layout(input, {
      reveal: { time: START + 400, laneId: ROOT, sequence: 3 },
      hiddenKinds: new Set(['tool_call']),
    });
    const cutoff = timeToX(model.viewport, START + 400);
    expect(model.spindle?.fibers.some((fiber) => fiber.id === FUTURE)).toBe(false);
    expect(model.nodes.map((node) => node.id).sort()).toEqual(base.nodes.map((node) => node.id).sort());
    expect(model.nodes.some((node) => node.id === 'pr:future' || node.kind === 'tool_call')).toBe(false);
    expect(model.counts.eventsWithheld).toBeGreaterThan(0);
    for (const fiber of model.spindle!.fibers) {
      for (const point of fiber.points) expect(point.x).toBeLessThanOrEqual(cutoff + 1e-8);
    }
    for (const interval of model.intervals) expect(interval.x1).toBeLessThanOrEqual(cutoff + 1e-8);
  });

  it('withholds a causal edge beyond an undated replay cursor in both curved and flat views', () => {
    const source = fixture();
    const input: JourneyProjection = {
      ...source,
      events: source.events.map((item) => item.id === 'tool:spawn'
        ? { ...item, source: 'transcript', sequence: 4 }
        : item.id === 'undated:root' ? { ...item, sequence: 1 } : item),
    };
    const { base, model } = layout(input, {
      selectedEventId: 'undated:root', reveal: { time: null, laneId: ROOT, sequence: 1 },
    });
    for (const scene of [base, model]) {
      expect(scene.cursor?.xBasis).toBe('sequence');
      expect(scene.nodes.some((node) => node.id === 'tool:spawn')).toBe(false);
      expect(scene.paths.some((path) => path.id === 'spawn:child')).toBe(false);
      expect(scene.counts.relationsWithheld).toBe(1);
    }
  });

  it('represents every collapsed member once without duplicating provider-qualified session identities', () => {
    const input = denseFixture();
    const { model, base } = layout(input, { zoom: 'workstream', selectedLaneId: null, selectedEventId: null });
    const fibers = model.spindle!.fibers;

    expect(base.lanes).toHaveLength(2);
    expect(fibers).toHaveLength(103);
    expect(new Set(fibers.map((fiber) => fiber.id))).toEqual(new Set(input.lanes.map((item) => item.id)));
    expect(model.counts.lanesTotal).toBe(base.counts.lanesTotal);
    expect(model.counts.eventsTotal).toBe(base.counts.eventsTotal);
    expect(model.counts.eventsDrawn).toBe(model.nodes.length);
    expect(model.counts.relationsTotal).toBe(base.counts.relationsTotal);
    expect(model.nodes.length).toBeGreaterThan(base.nodes.length);
    expect(new Set(model.nodes.map((node) => node.id)).size).toBe(model.nodes.length);
    const sourceEvents = new Map(input.events.map((item) => [item.id, item]));
    for (const node of model.nodes) {
      const source = sourceEvents.get(node.id);
      expect(source).toBeDefined();
      expect(node.laneId).toBe(source!.laneId);
      expect(node.x).toBeCloseTo(timeToX(model.viewport, source!.time!), 8);
      expect(node.grade).toBe(source!.grade);
      expect(node.ref).toBe(source!.ref);
    }
    expect(model.paths.filter((path) => ['spawn', 'handoff', 'rejoin', 'result'].includes(path.kind))).toEqual([]);
    for (const source of input.lanes) {
      const fiber = fibers.find((item) => item.id === source.id)!;
      expect(fiber.ownerLaneId).toBe(source.parentId ?? source.id);
      expect(fiber.points[0]!.x).toBeCloseTo(timeToX(model.viewport, source.start), 8);
      expect(fiber.points.at(-1)!.x).toBeCloseTo(timeToX(model.viewport, source.end!), 8);
    }

    const expanded = layout(input, {
      zoom: 'agent', selectedLaneId: null, selectedEventId: null,
      branches: { collapsed: new Set(), expanded: new Set([ROOT, OTHER]) },
    }).model;
    expect(expanded.spindle!.fibers.map(({ id, points, familyId }) => ({ id, points, familyId })))
      .toEqual(fibers.map(({ id, points, familyId }) => ({ id, points, familyId })));
    expect(expanded.minimap.lanes).toEqual(model.minimap.lanes);
  });

  it('places ordinal replay on the exact undated event of a selected collapsed member', () => {
    const source = denseFixture();
    const selectedLaneId = source.lanes.find((item) => item.isSubagent)!.id;
    const input: JourneyProjection = {
      ...source,
      events: [
        ...source.events,
        event('undated:member', selectedLaneId, null, { kind: 'message_assistant', source: 'transcript', sequence: 1 }),
        event('undated:member-later', selectedLaneId, null, { kind: 'message_assistant', source: 'transcript', sequence: 2 }),
      ],
    };
    const { base, model } = layout(input, {
      selectedLaneId, selectedEventId: 'undated:member',
      reveal: { time: null, laneId: selectedLaneId, sequence: 1 },
    });
    expect(base.lanes.some((item) => item.id === selectedLaneId)).toBe(false);
    expect(base.cursor).toBeNull();
    const selected = requiredNode(model, 'undated:member');
    expect(selected.xBasis).toBe('sequence');
    expect(model.cursor).toMatchObject({
      laneId: selectedLaneId, xBasis: 'sequence', x: selected.x, y: selected.y,
    });
    expect(model.nodes.some((node) => node.id === 'undated:member-later')).toBe(false);
  });

  it('reconciles every source event once across drawn, filtered, withheld, outside, and folded dense records', () => {
    const source = denseFixture();
    const input = {
      ...source,
      events: [...source.events, ...source.lanes.filter((item) => item.isSubagent).map((item) =>
        event(`transcript:${item.id}`, item.id, START + 530, { kind: 'message_assistant', source: 'transcript', sequence: 3 }))],
    };
    const { model } = layout(input, {
      zoom: 'workstream', selectedLaneId: null, selectedEventId: null,
      viewport: { width: 1000, left: 200, right: 24, window: { start: START + 520, end: START + 580 } },
      reveal: { time: START + 550, laneId: ROOT, sequence: 9 },
      hiddenKinds: new Set(['session_start']),
    });
    expect(model.counts).toMatchObject({
      eventsTotal: 307, eventsDrawn: 31, eventsFiltered: 103,
      eventsWithheld: 52, eventsCulled: 20, eventsFolded: 101,
    });
    const counts = model.counts;
    expect(counts.eventsDrawn + counts.eventsFiltered + counts.eventsWithheld + counts.eventsCulled + counts.eventsFolded)
      .toBe(input.events.length);
  });

  it('does not let a converged event hit region cover another distinct event center', () => {
    const input = projection(Array.from({ length: 103 }, (_, index) =>
      lane(JSON.stringify(['cursor', `parallel-${index}`]))));
    const { model } = layout(input, { zoom: 'agent', selectedLaneId: null, selectedEventId: null });
    const ends = model.nodes.filter((node) => node.kind === 'session_end');
    expect(ends).toHaveLength(103);
    for (const node of ends) {
      const distances = ends.filter((other) => other.id !== node.id)
        .map((other) => Math.hypot(other.x - node.x, other.y - node.y)).filter((distance) => distance > 1e-9);
      const nearest = Math.min(...distances);
      expect(node.hitRadius).toBeDefined();
      expect(node.hitRadius!).toBeLessThanOrEqual(nearest / 2 + 1e-8);
    }
  });

  it('keeps the minimap on the same full time axis when the main viewport pans', () => {
    const input = fixture();
    const full = layout(input);
    const panned = layout(input, { viewport: { ...full.options.viewport, window: { start: START + 150, end: START + 650 } } });
    expect(panned.model.minimap.lanes).toEqual(full.model.minimap.lanes);
    expect(panned.model.minimap.window).not.toEqual(full.model.minimap.window);
    const fullWindow = fittedWindowFor(input.extent);
    const duration = fullWindow.end - fullWindow.start;
    for (const item of full.model.minimap.lanes) {
      const source = input.lanes.find((candidate) => candidate.id === item.id)!;
      expect(source).toBeDefined();
      const start = ((source.start - fullWindow.start) / duration) * full.model.minimap.width;
      expect(item.x0).toBeCloseTo(start, 8);
      if (source.end !== null) {
        const end = ((source.end - fullWindow.start) / duration) * full.model.minimap.width;
        expect(item.x1).toBeCloseTo(end, 8);
      }
    }
  });

  it('paints the supplied unavailable strand without extending it or losing its evidence dashes', () => {
    const source = layout(fixture()).model;
    const fiber = source.spindle!.fibers.find((item) => item.id === OPEN)!;
    const model: TemporalSceneModel = {
      ...source, nodes: [], paths: [], ticks: [],
      spindle: { ...source.spindle!, fibers: [fiber] },
    };
    const palette: TemporalPalette = {
      field: '#001', substrate: '#001', text: '#eef', textMuted: '#aac', edge: '#446',
      grid: '#224', signal: '#0df', signalHot: '#7ff', amber: '#fa0', danger: '#f44',
      violet: '#a6f', ready: '#6f9', dim: '#567', light: false, highContrast: false,
    };
    let points: { x: number; y: number }[] = [];
    let dash: number[] = [];
    let arcs = 0;
    const strokes: { points: { x: number; y: number }[]; dash: number[]; color: string | CanvasGradient | CanvasPattern }[] = [];
    const context = {
      strokeStyle: '',
      save(): void {}, restore(): void {}, rect(): void {}, clip(): void {}, fill(): void {},
      beginPath(): void { points = []; },
      moveTo(x: number, y: number): void { points.push({ x, y }); },
      lineTo(x: number, y: number): void { points.push({ x, y }); },
      setLineDash(value: number[]): void { dash = [...value]; },
      arc(): void { arcs += 1; },
      stroke(): void {
        if (points.length > 0) strokes.push({ points: [...points], dash: [...dash], color: context.strokeStyle });
      },
    } as unknown as CanvasRenderingContext2D;

    paintSpindle(context, {
      model, variant: 'weave', density: null, fieldX0: model.viewport.left,
      fieldX1: model.viewport.width - model.viewport.right, top: 36, height: model.height,
    }, palette);

    expect(strokes.length).toBeGreaterThan(0);
    for (const stroke of strokes) {
      expect(stroke.points).toEqual(fiber.points);
      expect(stroke.dash.length).toBeGreaterThan(0);
      expect(stroke.color).toBe(palette.dim);
    }
    expect(arcs).toBe(0);
  });

  it('uses resolved high-contrast ink without glow or recency veils while retaining evidence dashes', () => {
    const source = layout(fixture()).model;
    const model: TemporalSceneModel = { ...source, ticks: [] };
    const unavailableFiber = model.spindle!.fibers.find((fiber) => fiber.id === OPEN)!;
    const palette: TemporalPalette = {
      field: '#001', substrate: '#001', text: '#fff', textMuted: '#aac', edge: '#446',
      grid: '#224', signal: '#0df', signalHot: '#7ff', amber: '#fa0', danger: '#f44',
      violet: '#a6f', ready: '#6f9', dim: '#567', light: false, highContrast: true,
    };
    type Paint = {
      color: string | CanvasGradient | CanvasPattern;
      shadowBlur: number;
      blend: GlobalCompositeOperation;
    };
    let points: { x: number; y: number }[] = [];
    let dash: number[] = [];
    let gradients = 0;
    const strokes: (Paint & { points: { x: number; y: number }[]; dash: number[] })[] = [];
    const fills: Paint[] = [];
    const context = {
      strokeStyle: '', fillStyle: '', shadowBlur: 0, globalCompositeOperation: 'source-over',
      save(): void {}, restore(): void {}, rect(): void {}, clip(): void {}, arc(): void {}, fillRect(): void {},
      beginPath(): void { points = []; },
      moveTo(x: number, y: number): void { points.push({ x, y }); },
      lineTo(x: number, y: number): void { points.push({ x, y }); },
      bezierCurveTo(_x1: number, _y1: number, _x2: number, _y2: number, x: number, y: number): void { points.push({ x, y }); },
      setLineDash(value: number[]): void { dash = [...value]; },
      createLinearGradient(): CanvasGradient {
        gradients += 1;
        return { addColorStop(): void {} } as CanvasGradient;
      },
      stroke(): void {
        if (points.length > 0) strokes.push({
          points: [...points], dash: [...dash], color: context.strokeStyle,
          shadowBlur: context.shadowBlur, blend: context.globalCompositeOperation,
        });
      },
      fill(): void {
        fills.push({ color: context.fillStyle, shadowBlur: context.shadowBlur, blend: context.globalCompositeOperation });
      },
    } as unknown as CanvasRenderingContext2D;

    paintSpindle(context, {
      model, variant: 'weave',
      density: { binPx: 6, lanes: new Map(), headTime: START, tailTime: START + 800, tailX: model.viewport.width - model.viewport.right },
      fieldX0: model.viewport.left, fieldX1: model.viewport.width - model.viewport.right,
      top: 36, height: model.height,
    }, palette);

    expect(strokes.length).toBeGreaterThan(0);
    expect(fills.length).toBeGreaterThan(0);
    for (const mark of [...strokes, ...fills]) {
      expect(mark.color).toBe(palette.text);
      expect(mark.shadowBlur).toBe(0);
      expect(mark.blend).toBe('source-over');
    }
    const unavailableStrokes = strokes.filter((stroke) =>
      stroke.points[0]?.x === unavailableFiber.points[0]!.x
      && stroke.points[0]?.y === unavailableFiber.points[0]!.y);
    expect(unavailableStrokes.length).toBeGreaterThan(0);
    for (const stroke of unavailableStrokes) expect(stroke.dash).toEqual([1, 5]);
    expect(gradients).toBe(0);
  });
});
