import { render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DensityBin, LaneDensity, SceneDensity } from '../density.ts';
import { TemporalScene } from '../TemporalScene.tsx';
import type { SceneNode, SceneWindow, TemporalSceneModel } from '../types.ts';

/**
 * What the field paints, observed through the canvas calls it makes and the
 * overlay marks it emits: semantic zoom between density summaries and glyphs,
 * recency luminance, the lifted causal chain, grade tags and loaded-page
 * markers. The shared overlay contract is in `TemporalScene.dom.test`.
 */

const WINDOW: SceneWindow = { start: 1_784_700_000, end: 1_784_707_200 };
const ROOT = JSON.stringify(['cursor', 'root']);
const CHILD = JSON.stringify(['cursor', 'child']);
const BUNDLE = JSON.stringify(['codex', 'bundle']);

function node(overrides: Partial<SceneNode> & Pick<SceneNode, 'id' | 'kind' | 'x'>): SceneNode {
  return { laneId: ROOT, y: 80, xBasis: 'time', grade: 'exact', source: 'session', label: overrides.id, detail: null, ref: overrides.id, selected: false, focus: 'neutral', halfHit: 12, ...overrides };
}

function model(over: Partial<TemporalSceneModel> = {}): TemporalSceneModel {
  return {
    viewport: { width: 960, left: 200, right: 28, window: WINDOW },
    zoom: 'agent',
    height: 240,
    lanes: [
      { id: ROOT, kind: 'session', label: 'root session', provider: 'cursor', depth: 0, y: 80, height: 48, x0: 200, x1: 810, endSource: 'session_end', focus: 'neutral', expanded: false, offscreen: false, revealed: true, collapsedDescendants: 0, row: 0 },
      { id: CHILD, kind: 'session', label: 'child agent', provider: 'cursor', depth: 1, y: 130, height: 48, x0: 444, x1: 688, endSource: null, focus: 'neutral', expanded: false, offscreen: false, revealed: true, collapsedDescendants: 0, row: 1 },
      { id: BUNDLE, kind: 'bundle', label: 'codex fan-out', provider: 'codex', depth: 0, y: 190, height: 48, x0: 300, x1: 700, endSource: 'last_message', focus: 'neutral', expanded: false, offscreen: false, revealed: true, collapsedDescendants: 3, row: 2 },
    ],
    nodes: [
      node({ id: 'n-start', kind: 'session_start', x: 200 }),
      node({ id: 'n-spawn', kind: 'spawn', x: 444, source: 'parentage', ref: CHILD }),
      node({ id: 'n-commit', kind: 'commit', x: 450, grade: 'inferred', source: 'commit', label: '9f3c2ab' }),
    ],
    paths: [
      { id: 'p-root', kind: 'lane', fromId: ROOT, toId: ROOT, grade: 'exact', basis: null, focus: 'neutral', controls: [200, 80, 810, 80], weight: 0.7 },
      { id: 'p-child', kind: 'lane', fromId: CHILD, toId: CHILD, grade: 'unavailable', basis: null, focus: 'neutral', controls: [444, 130, 462, 130], weight: 0.2 },
      { id: 'p-bundle', kind: 'lane', fromId: BUNDLE, toId: BUNDLE, grade: 'exact', basis: null, focus: 'neutral', controls: [300, 190, 700, 190], weight: 0.5 },
      { id: 'p-spawn', kind: 'spawn', fromId: ROOT, toId: CHILD, grade: 'ambiguous', basis: 'child start precedes parent start', focus: 'neutral', controls: [428, 80, 447, 80, 441, 130, 460, 130], weight: null },
      { id: 'p-spawn-exact', kind: 'spawn', fromId: ROOT, toId: BUNDLE, grade: 'exact', basis: 'parent_session_id', focus: 'neutral', controls: [572, 80, 591, 80, 585, 190, 604, 190], weight: null },
    ],
    clusters: [
      { id: 'c-bundle', laneId: BUNDLE, memberLaneIds: ['a', 'b', 'c'], x0: 300, x1: 700, y: 190, height: 48, counts: { sessions: 3, subagents: 3, messages: 57, commits: 0, openEnded: 1 }, grades: { exact: 3 }, focus: 'neutral' },
    ],
    intervals: [],
    gaps: [],
    rails: [{ id: 'rail-cursor', kind: 'provider', label: 'cursor', y0: 56, y1: 154, lanes: 2 }],
    ticks: [0, 1, 2, 3].map((step) => ({ x: 200 + step * 183, time: WINDOW.start + step * 1800, label: `09:${40 + step * 5}` })),
    labels: [],
    minimap: { bins: [], lanes: [], window: { x0: 0, x1: 960 }, width: 960, height: 48 },
    cursor: null,
    counts: { lanesTotal: 6, lanesVisible: 3, lanesCollapsed: 1, eventsTotal: 3, eventsDrawn: 3, eventsCulled: 0, eventsWithheld: 0, eventsFiltered: 0, eventsFolded: 0, relationsTotal: 2, relationsDrawn: 2, relationsWithheld: 0 },
    denseDepth: null,
    ...over,
  };
}

function laneDensity(laneId: string, minGap: number, bins: DensityBin[] = []): LaneDensity {
  return {
    laneId,
    bins,
    peak: { active: Math.max(0, ...bins.map((bin) => bin.active)), open: Math.max(0, ...bins.map((bin) => bin.open)), events: Math.max(0, ...bins.map((bin) => bin.events)) },
    totals: { sessions: laneId === BUNDLE ? 4 : 1, messages: laneId === BUNDLE ? 91 : 12, commits: laneId === ROOT ? 1 : 0, events: 3, undated: 0, openEnded: 1 },
    minGap,
  };
}

function density(over: Partial<SceneDensity> = {}, rootGap = 244): SceneDensity {
  const bundleBins: DensityBin[] = [
    { x0: 300, x1: 306, active: 2, open: 1, starts: 1, events: 2 },
    { x0: 306, x1: 312, active: 1, open: 1, starts: 0, events: 0 },
  ];
  return {
    binPx: 6,
    lanes: new Map([
      [ROOT, laneDensity(ROOT, rootGap)],
      [CHILD, laneDensity(CHILD, Infinity)],
      [BUNDLE, laneDensity(BUNDLE, Infinity, bundleBins)],
    ]),
    headTime: WINDOW.start,
    tailTime: WINDOW.start + 5400,
    tailX: 749,
    ...over,
  };
}

interface Recorded {
  calls: string[];
  rects: number[][];
  lineWidths: number[];
  composites: string[];
}

const originalGetContext = HTMLCanvasElement.prototype.getContext;

function stubCanvas(): Recorded {
  const recorded: Recorded = { calls: [], rects: [], lineWidths: [], composites: [] };
  const context = new Proxy({} as Record<string, unknown>, {
    get(_target, property) {
      if (typeof property !== 'string') return undefined;
      return (...args: number[]): unknown => {
        recorded.calls.push(property);
        if (property === 'rect') recorded.rects.push(args);
        if (property === 'stroke') recorded.lineWidths.push(currentLineWidth);
        return property.startsWith('create') ? { addColorStop(): void {} } : undefined;
      };
    },
    set(_target, property, value) {
      if (property === 'lineWidth') currentLineWidth = Number(value);
      if (property === 'globalCompositeOperation') recorded.composites.push(String(value));
      return true;
    },
  });
  let currentLineWidth = 1;
  Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', { configurable: true, value: () => context });
  return recorded;
}

function renderWith(over: { model?: TemporalSceneModel; density?: SceneDensity | null } = {}) {
  return render(
    <TemporalScene
      model={over.model ?? model()}
      density={over.density === undefined ? density() : over.density}
      ariaLabel="Temporal execution field"
      reducedMotion={true}
      onSelectLane={vi.fn()}
      onSelectEvent={vi.fn()}
      onToggleBranch={vi.fn()}
      onWindowChange={vi.fn()}
    />,
  );
}

const withFocus = (laneIds: Record<string, 'selected' | 'path'>, pathIds: Record<string, 'selected' | 'path'> = {}) => {
  const base = model();
  return model({
    lanes: base.lanes.map((lane) => ({ ...lane, focus: laneIds[lane.id] ?? 'context' })),
    paths: base.paths.map((path) => ({ ...path, focus: pathIds[path.id] ?? 'context' })),
  });
};

describe('temporal field paint', () => {
  let recorded: Recorded;
  beforeEach(() => {
    recorded = stubCanvas();
  });
  afterEach(() => {
    Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', { configurable: true, value: originalGetContext });
  });

  describe('semantic zoom', () => {
    const glyphs = (container: HTMLElement) => container.querySelectorAll('[data-event] [data-glyph]').length;

    it('draws glyphs where marks sit apart and every event keeps its button', () => {
      const { container } = renderWith();
      expect(glyphs(container)).toBe(3);
      expect(container.querySelectorAll('[data-event]').length).toBe(3);
    });

    it('aggregates a lane whose marks collide into its event rug', () => {
      const { container } = renderWith({ density: density({}, 8) });
      expect(glyphs(container)).toBe(0);
      expect(container.querySelectorAll('[data-event]').length).toBe(3);
    });

    it('aggregates every lane at workstream zoom and below the legible pitch', () => {
      expect(glyphs(renderWith({ model: model({ zoom: 'workstream' }) }).container)).toBe(0);
      const short = model();
      const { container } = renderWith({ model: model({ lanes: short.lanes.map((lane) => ({ ...lane, height: 16 })) }) });
      expect(glyphs(container)).toBe(0);
    });

    it('resolves the expanded session even when its marks are crowded', () => {
      const base = model();
      const expanded = model({ lanes: base.lanes.map((lane) => (lane.id === ROOT ? { ...lane, expanded: true } : lane)) });
      expect(glyphs(renderWith({ model: expanded, density: density({}, 4) }).container)).toBe(3);
    });

    it('bars a bundle by measured member sessions on a square-root scale', () => {
      renderWith();
      // Peak active+open is 3; two active of a 40px room is sqrt(2/3)*40 tall,
      // standing on the floor at 190 + 24 - 3.
      const bar = recorded.rects.find((rect) => rect[0] === 300 && rect[2] === 5 && rect[1]! < 211)!;
      expect(bar[1]).toBeCloseTo(211 - Math.sqrt(2 / 3) * 40, 6);
      expect(bar[3]).toBeCloseTo(Math.sqrt(2 / 3) * 40, 6);
      expect(recorded.calls).toContain('clip');
    });
  });

  describe('luminance', () => {
    it('dims an older mark toward half luminance and keeps NOW at full', () => {
      const { container } = renderWith();
      // Head at x=200, NOW at 200 + 5400/7200 * 732 = 749.
      const commit = container.querySelector('[data-event="n-commit"] g[opacity]')!;
      expect(Number(commit.getAttribute('opacity'))).toBeCloseTo(0.5 + 0.5 * (250 / 549), 6);
      const start = container.querySelector('[data-event="n-start"] g[opacity]')!;
      expect(Number(start.getAttribute('opacity'))).toBe(0.5);
    });

    it('keeps luminance flat when the page has no dated extent', () => {
      const { container } = renderWith({ density: density({ headTime: null, tailTime: null, tailX: null }) });
      expect(container.querySelector('[data-event="n-commit"] g[opacity]')?.getAttribute('opacity')).toBe('1');
    });

    it('lifts the selected chain with a 7px halo, and paints no halo without a selection', () => {
      renderWith();
      expect(recorded.lineWidths).not.toContain(7);
      recorded.lineWidths.length = 0;
      renderWith({ model: withFocus({ [CHILD]: 'selected', [ROOT]: 'path' }, { 'p-root': 'path', 'p-spawn': 'selected' }) });
      expect(recorded.lineWidths.filter((width) => width === 7)).toHaveLength(2);
    });

    it('holds a lifted mark at full luminance and halos the selected event', () => {
      const base = model();
      const selected = model({ nodes: base.nodes.map((entry) => (entry.id === 'n-start' ? { ...entry, selected: true, focus: 'selected' } : entry)) });
      const { container } = renderWith({ model: selected });
      expect(container.querySelector('[data-event="n-start"] g[opacity]')?.getAttribute('opacity')).toBe('1');
      expect(container.querySelector('[data-event="n-start"] circle[opacity="0.14"]')).toBeTruthy();
    });

    it('blends rails and links additively in the dark theme and restores normal paint', () => {
      renderWith();
      expect(recorded.composites).toEqual(['lighter', 'source-over']);
    });
  });

  describe('causal links', () => {
    it('routes links orthogonally', () => {
      renderWith();
      expect(recorded.calls).toContain('arcTo');
      expect(recorded.calls).not.toContain('bezierCurveTo');
    });

    it('tags every non-EXACT link with its grade', () => {
      const { container } = renderWith();
      expect([...container.querySelectorAll('[data-link-grade]')].map((tag) => tag.textContent)).toEqual(['AMBIGUOUS']);
    });

    it('tallies every drawn link grade in the ruler, joins apart from forks', () => {
      const base = model();
      const withJoin = model({
        paths: [...base.paths, { id: 'p-join', kind: 'rejoin', fromId: CHILD, toId: ROOT, grade: 'inferred', basis: 'child recorded end inside the parent', focus: 'neutral', controls: [672, 130, 691, 130, 685, 80, 704, 80], weight: null }],
      });
      const { container } = renderWith({ model: withJoin });
      expect(container.querySelector('[data-link-tally]')?.textContent).toBe('1 FORKS AMBIGUOUS · 1 FORKS EXACT · 1 JOINS INFERRED');
    });

    it('tags an EXACT link once it is on the lifted chain', () => {
      const { container } = renderWith({ model: withFocus({ [BUNDLE]: 'selected' }, { 'p-spawn-exact': 'selected' }) });
      expect([...container.querySelectorAll('[data-link-grade]')].map((tag) => tag.textContent)).toEqual(['AMBIGUOUS', 'EXACT']);
    });
  });

  describe('loaded-page markers', () => {
    it('marks NOW at the newest loaded record, not at the window edge', () => {
      const { container } = renderWith();
      const tail = container.querySelector('[data-tail-marker]')!;
      expect(tail.getAttribute('data-tail-x')).toBe('749');
      expect(tail.querySelector('title')?.textContent).toContain('NOW = newest record in this loaded page');
      expect(tail.querySelector('title')?.textContent).toContain('not a live stream');
    });

    it('points past the window when NOW lies after it', () => {
      const { container } = renderWith({ density: density({ tailX: null, tailTime: WINDOW.end + 600 }) });
      expect(container.querySelector('[data-tail-marker]')?.getAttribute('data-tail-x')).toBe('later');
      expect(screen.getByText('NOW →')).toBeTruthy();
    });

    it('keeps a recorded-order cursor inside its own lane', () => {
      const { container } = renderWith({ model: model({ cursor: { x: 520, laneId: CHILD, xBasis: 'sequence' } }) });
      const cursor = container.querySelector('[data-cursor]')!;
      expect(cursor.getAttribute('y1')).toBe('106');
      expect(cursor.getAttribute('y2')).toBe('154');
      expect(screen.getByText('CURSOR · RECORDED ORDER')).toBeTruthy();
    });

    it('spans the whole field with a dated cursor', () => {
      const { container } = renderWith({ model: model({ cursor: { x: 520, laneId: ROOT, xBasis: 'time' } }) });
      expect(container.querySelector('[data-cursor]')?.getAttribute('y2')).toBe('240');
    });

    it('engraves the provider rail legend in the gutter', () => {
      const { container } = renderWith();
      expect(container.querySelector('[data-rail-legend="cursor"] text')?.textContent).toBe('CURSOR · 2');
    });
  });

  describe('lane column', () => {
    const laneTitle = (container: HTMLElement, laneId: string) =>
      container.querySelector(`[data-lane-row='${laneId}'] title`)?.textContent;

    it('prints reconciled totals: root plus delegated sessions, and what was not measured', () => {
      const { container } = renderWith();
      expect(laneTitle(container, BUNDLE)).toBe('codex fan-out · 1+3 sess · 91 msg · peak 2 · 1 open');
      expect(laneTitle(container, ROOT)).toBe('root session · cursor · 12 msg · 1 commit');
    });

    it('marks persistent selection with a 2px cyan gutter', () => {
      const { container } = renderWith({ model: withFocus({ [CHILD]: 'selected' }) });
      const gutter = container.querySelector('[data-selected-gutter]')!;
      expect(gutter.getAttribute('y')).toBe('106');
      expect(gutter.getAttribute('width')).toBe('2');
    });

    it('names its encodings in the legend', () => {
      const { container } = renderWith();
      expect(container.querySelector('[data-legend-encodings]')?.textContent).toContain('brightness = recency within the loaded page');
    });
  });
});
