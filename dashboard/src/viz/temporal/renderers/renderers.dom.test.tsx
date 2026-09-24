import { render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DensityBin, LaneDensity, SceneDensity } from '../density.ts';
import { TemporalScene } from '../TemporalScene.tsx';
import type { SceneNode, SceneWindow, TemporalSceneModel } from '../types.ts';
import { parseSceneRenderer, SCENE_RENDERERS, type SceneRenderer } from './index.ts';

/**
 * What each exploratory renderer paints differently from the others, observed
 * through the canvas calls it makes and the overlay marks it emits. The shared
 * overlay contract is covered for every renderer in `TemporalScene.dom.test`.
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
    denseDefault: false,
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
    tailX: 750,
    ...over,
  };
}

const originalGetContext = HTMLCanvasElement.prototype.getContext;

function stubCanvas(): string[] {
  const calls: string[] = [];
  const context = new Proxy({} as Record<string, unknown>, {
    get(_target, property) {
      if (typeof property !== 'string') return undefined;
      return (): unknown => {
        calls.push(property);
        return property.startsWith('create') ? { addColorStop(): void {} } : undefined;
      };
    },
    set: () => true,
  });
  Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', { configurable: true, value: () => context });
  return calls;
}

function renderWith(renderer: SceneRenderer, over: { model?: TemporalSceneModel; density?: SceneDensity | null } = {}) {
  return render(
    <TemporalScene
      model={over.model ?? model()}
      density={over.density === undefined ? density() : over.density}
      renderer={renderer}
      ariaLabel="Temporal execution field"
      tailLabel="NOW"
      reducedMotion={true}
      onSelectLane={vi.fn()}
      onSelectEvent={vi.fn()}
      onToggleBranch={vi.fn()}
      onWindowChange={vi.fn()}
    />,
  );
}

const VARIANTS = [SCENE_RENDERERS.rail, SCENE_RENDERERS.weave, SCENE_RENDERERS.strata];

describe('exploratory scene renderers', () => {
  let calls: string[] = [];
  beforeEach(() => {
    calls = stubCanvas();
  });
  afterEach(() => {
    Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', { configurable: true, value: originalGetContext });
  });

  it('selects a renderer from the URL and falls back to the shipped weave', () => {
    expect(parseSceneRenderer('rail')).toBe('rail');
    expect(parseSceneRenderer('weave')).toBe('weave');
    expect(parseSceneRenderer('strata')).toBe('strata');
    expect(parseSceneRenderer('webgl')).toBe('current');
    expect(parseSceneRenderer(null)).toBe('current');
  });

  it('routes causal links orthogonally in the rail, as bent threads in the weave and as plates in the strata', () => {
    renderWith(SCENE_RENDERERS.rail);
    expect(calls).toContain('arcTo');
    expect(calls).not.toContain('bezierCurveTo');
    calls.length = 0;
    renderWith(SCENE_RENDERERS.weave);
    expect(calls).toContain('bezierCurveTo');
    expect(calls).not.toContain('arcTo');
    calls.length = 0;
    renderWith(SCENE_RENDERERS.strata);
    expect(calls).toContain('strokeRect');
    expect(calls).not.toContain('bezierCurveTo');
  });

  it.each(VARIANTS)('$id marks NOW at the newest loaded record, not at the window edge', (renderer) => {
    const { container } = renderWith(renderer);
    const tail = container.querySelector('[data-tail-marker]')!;
    expect(tail.getAttribute('data-tail-x')).toBe('750');
    expect(tail.querySelector('title')?.textContent).toContain('NOW = newest record in this loaded page');
    expect(tail.querySelector('title')?.textContent).toContain('not a live stream');
  });

  it.each(VARIANTS)('$id points past the window when NOW lies after it', (renderer) => {
    const { container } = renderWith(renderer, { density: density({ tailX: null, tailTime: WINDOW.end + 600 }) });
    expect(container.querySelector('[data-tail-marker]')?.getAttribute('data-tail-x')).toBe('later');
    expect(screen.getByText('NOW →')).toBeTruthy();
  });

  it.each(VARIANTS)('$id keeps a recorded-order cursor inside its own lane', (renderer) => {
    const { container } = renderWith(renderer, { model: model({ cursor: { x: 520, laneId: CHILD, xBasis: 'sequence' } }) });
    const cursor = container.querySelector('[data-cursor]')!;
    expect(cursor.getAttribute('y1')).toBe('106');
    expect(cursor.getAttribute('y2')).toBe('154');
    expect(screen.getByText('CURSOR · recorded order')).toBeTruthy();
  });

  it.each(VARIANTS)('$id spans the whole field with a dated cursor', (renderer) => {
    const { container } = renderWith(renderer, { model: model({ cursor: { x: 520, laneId: ROOT, xBasis: 'time' } }) });
    const cursor = container.querySelector('[data-cursor]')!;
    expect(cursor.getAttribute('y2')).toBe('240');
    expect(container.querySelector('[data-cursor-mark]')?.getAttribute('data-cursor-basis')).toBe('time');
  });

  it('tags every non-EXACT causal link with its grade in the rail', () => {
    const { container } = renderWith(SCENE_RENDERERS.rail);
    const tags = [...container.querySelectorAll('[data-link-grade]')].map((tag) => tag.textContent);
    expect(tags).toEqual(['AMBIGUOUS']);
  });

  it('tags an EXACT link once it is on the selected chain', () => {
    const base = model();
    const selected = model({ paths: base.paths.map((path) => (path.id === 'p-spawn-exact' ? { ...path, focus: 'selected' } : path)) });
    const { container } = renderWith(SCENE_RENDERERS.rail, { model: selected });
    expect([...container.querySelectorAll('[data-link-grade]')].map((tag) => tag.textContent)).toEqual(['AMBIGUOUS', 'EXACT']);
  });

  it.each([
    [SCENE_RENDERERS.rail, 8],
    [SCENE_RENDERERS.weave, 12],
    [SCENE_RENDERERS.strata, 9],
  ] as const)('$0.id drops glyphs on a crowded lane but keeps every event button', (renderer, gap) => {
    const { container } = renderWith(renderer, { density: density({}, gap) });
    expect(container.querySelectorAll('[data-event]').length).toBe(3);
    for (const id of ['n-start', 'n-spawn', 'n-commit']) {
      expect(container.querySelector(`[data-event="${id}"] [data-glyph]`)).toBeNull();
    }
    const spacious = renderWith(renderer, { density: density({}, 40) });
    expect(spacious.container.querySelector('[data-event="n-commit"] [data-glyph="commit"]')).toBeTruthy();
  });

  const laneTitle = (container: HTMLElement, laneId: string) =>
    container.querySelector(`[data-lane-row='${laneId}'] title`)?.textContent;

  it('prints reconciled bundle totals, root plus delegated sessions, in the lane column', () => {
    const { container } = renderWith(SCENE_RENDERERS.rail);
    expect(laneTitle(container, BUNDLE)).toBe('codex fan-out · 1+3 sess · 91 msg · peak 2');
    expect(laneTitle(container, ROOT)).toBe('root session · cursor · 12 msg · 1 commit');
  });

  it('prints strata bundle totals with the open sessions it could not measure', () => {
    const { container } = renderWith(SCENE_RENDERERS.strata);
    expect(laneTitle(container, BUNDLE)).toBe('codex fan-out · 1+3 sess · 91 msg · peak 2 · 1 open');
  });

  it('counts weave threads as the root plus its delegated sessions', () => {
    const { container } = renderWith(SCENE_RENDERERS.weave);
    expect(laneTitle(container, BUNDLE)).toBe('codex fan-out · codex · 1+3 threads');
    expect(container.querySelector('[data-cluster] text')?.textContent).toBe('4 THREADS · 91 MSG');
  });

  it('shows the strata grade ladder as the fill patterns it paints', () => {
    const { container } = renderWith(SCENE_RENDERERS.strata);
    expect([...container.querySelectorAll('[data-grade-swatch]')].map((swatch) => swatch.getAttribute('data-grade-swatch'))).toEqual([
      'solid',
      'light',
      'diagonal',
      'cross',
      'sparse',
      'dots',
    ]);
  });

  it.each(VARIANTS)('$id names its own encodings in the legend', (renderer) => {
    const { container } = renderWith(renderer);
    expect(container.querySelector(`[data-legend-encodings="${renderer.id}"]`)?.textContent).toContain('extent unknown');
  });

  it.each(VARIANTS)('$id marks persistent selection with a cyan gutter', (renderer) => {
    const base = model();
    const selected = model({ lanes: base.lanes.map((lane) => (lane.id === CHILD ? { ...lane, focus: 'selected' } : lane)) });
    const { container } = renderWith(renderer, { model: selected });
    const gutter = container.querySelector('[data-selected-gutter]')!;
    expect(gutter.getAttribute('y')).toBe('106');
    expect(gutter.getAttribute('width')).toBe('2');
  });
});
