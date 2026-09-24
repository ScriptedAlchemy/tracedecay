import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { TemporalScene, type TemporalSceneProps } from './TemporalScene.tsx';
import type { SceneNode, SceneWindow, TemporalSceneModel } from './types.ts';

/**
 * The scene renderer is handed a laid-out model and draws it. Under test: every
 * mark the layout emitted reaches the DOM as a selectable, labelled element;
 * the canvas substrate is painted when a 2D context exists and honestly
 * declared missing when it does not; the toolbar, wheel and minimap turn into
 * window changes with the arithmetic the reader expects; hover only inspects.
 */

const WINDOW: SceneWindow = { start: 1_784_700_000, end: 1_784_707_200 };
const ROOT = JSON.stringify(['cursor', 'root']);
const CHILD = JSON.stringify(['cursor', 'child']);
const BUNDLE = JSON.stringify(['codex', 'bundle']);
const ROOT_LABEL = 'root session';
const BUNDLE_LABEL = 'codex fan-out';

function node(overrides: Partial<SceneNode> & Pick<SceneNode, 'id' | 'kind' | 'x'>): SceneNode {
  return {
    laneId: ROOT,
    y: 80,
    xBasis: 'time',
    grade: 'exact',
    source: 'transcript',
    label: overrides.id,
    detail: null,
    ref: overrides.id,
    selected: false,
    focus: 'neutral',
    halfHit: 12,
    ...overrides,
  };
}

function fixture(): TemporalSceneModel {
  return {
    viewport: { width: 960, left: 200, right: 28, window: WINDOW },
    zoom: 'event',
    height: 240,
    lanes: [
      { id: ROOT, kind: 'session', label: ROOT_LABEL, provider: 'cursor', depth: 0, y: 80, height: 48, x0: 200, x1: 810, endSource: 'session_end', focus: 'neutral', expanded: true, offscreen: false, revealed: true, collapsedDescendants: 0, row: 0 },
      { id: CHILD, kind: 'session', label: 'child agent', provider: 'cursor', depth: 1, y: 130, height: 48, x0: 444, x1: 688, endSource: null, focus: 'neutral', expanded: true, offscreen: false, revealed: true, collapsedDescendants: 0, row: 1 },
      { id: BUNDLE, kind: 'bundle', label: BUNDLE_LABEL, provider: 'codex', depth: 0, y: 190, height: 48, x0: 300, x1: 700, endSource: 'last_message', focus: 'neutral', expanded: false, offscreen: false, revealed: true, collapsedDescendants: 3, row: 2 },
    ],
    nodes: [
      node({ id: 'n-start', kind: 'session_start', x: 200, source: 'session', label: ROOT_LABEL, ref: 'root' }),
      node({ id: 'n-tool', kind: 'tool_call', x: 320, label: 'Read', detail: 'tool_use toolu_01' }),
      node({ id: 'n-spawn', kind: 'spawn', x: 444, source: 'parentage', label: 'child agent', ref: CHILD }),
      node({ id: 'n-commit', kind: 'commit', x: 500, grade: 'inferred', source: 'commit', label: '9f3c2ab', detail: 'commit time within session extent' }),
      node({ id: 'n-msg', kind: 'message_other', x: 600, xBasis: 'sequence', label: 'system' }),
      node({ id: 'n-end', kind: 'session_end', x: 688, y: 130, laneId: CHILD, grade: 'explicit', source: 'session', label: 'child agent' }),
    ],
    paths: [
      { id: 'p-root', kind: 'lane', fromId: ROOT, toId: ROOT, grade: 'exact', basis: null, focus: 'neutral', controls: [200, 80, 810, 80], weight: 0.7 },
      { id: 'p-child', kind: 'lane', fromId: CHILD, toId: CHILD, grade: 'explicit', basis: null, focus: 'neutral', controls: [444, 130, 688, 130], weight: 0.3 },
      { id: 'p-bundle', kind: 'lane', fromId: BUNDLE, toId: BUNDLE, grade: 'inferred', basis: null, focus: 'neutral', controls: [300, 190, 700, 190], weight: 0.5 },
      { id: 'p-spawn', kind: 'spawn', fromId: 'n-spawn', toId: CHILD, grade: 'exact', basis: 'parent_session_id · parent_tool_use_id toolu_01', focus: 'neutral', controls: [444, 80, 470, 80, 418, 130, 444, 130], weight: null },
      { id: 'p-seq', kind: 'sequence', fromId: 'n-tool', toId: 'n-msg', grade: 'unavailable', basis: 'recorded order', focus: 'neutral', controls: [320, 80, 600, 80], weight: null },
    ],
    clusters: [
      { id: 'c-bundle', laneId: BUNDLE, memberLaneIds: ['a', 'b', 'c', 'd'], x0: 300, x1: 700, y: 190, height: 28, counts: { sessions: 4, subagents: 3, messages: 57, commits: 2, openEnded: 1 }, grades: { exact: 3, inferred: 1 }, focus: 'neutral' },
    ],
    intervals: [
      { id: 'iv-git', laneId: ROOT, kind: 'git_span', x0: 350, x1: 560, y: 94, label: 'main · 3 commits', grade: 'explicit', tone: null, ref: null },
      { id: 'iv-prox', laneId: CHILD, kind: 'proximity', x0: 500, x1: 640, y: 144, label: 'overlap with codex', grade: 'ambiguous', tone: 'overlap', ref: 'enc-1' },
    ],
    gaps: [
      { id: 'gap-extent', laneId: CHILD, kind: 'extent_unknown', grade: 'unavailable', detail: 'no session_end or last_message recorded', x: 688, y: 130 },
      { id: 'gap-handoff', laneId: null, kind: 'handoff_unavailable', grade: 'unavailable', detail: 'no loaded provider records handoffs', x: null, y: null },
    ],
    rails: [{ id: 'rail-cursor', kind: 'provider', label: 'cursor', y0: 56, y1: 160, lanes: 2 }],
    ticks: [0, 1, 2, 3].map((step) => ({ x: 200 + step * 183, time: WINDOW.start + step * 1800, label: `09:${40 + step * 5}` })),
    labels: [
      { id: 'lbl-tool', text: 'Read', x: 320, y: 62, anchor: 'middle', priority: 1, group: 'event' },
      { id: 'lbl-lane-root', text: ROOT_LABEL, x: 4, y: 80, anchor: 'start', priority: 0, group: 'lane' },
    ],
    minimap: {
      bins: [3, 0, 2, 5, 1, 0, 4, 2].map((events, index) => ({ x0: index * 120, x1: (index + 1) * 120, events, lanes: events > 0 ? 1 : 0 })),
      lanes: [
        { id: ROOT, y: 12, x0: 0, x1: 600, endSource: 'session_end' },
        { id: CHILD, y: 24, x0: 200, x1: 450, endSource: null },
        { id: BUNDLE, y: 36, x0: 100, x1: 500, endSource: 'last_message' },
      ],
      window: { x0: 0, x1: 960 },
      width: 960,
      height: 48,
    },
    cursor: { x: 320, laneId: ROOT, xBasis: 'time' },
    counts: { lanesTotal: 7, lanesVisible: 3, lanesCollapsed: 4, eventsTotal: 12, eventsDrawn: 6, eventsCulled: 0, eventsWithheld: 4, eventsFiltered: 0, eventsFolded: 2, relationsTotal: 2, relationsDrawn: 1, relationsWithheld: 1 },
    denseDefault: false,
  };
}

const originalGetContext = HTMLCanvasElement.prototype.getContext;

/** A 2D context that records every method it is asked for and draws nothing. */
function stubCanvas(available: boolean): string[] {
  const calls: string[] = [];
  const gradient = { addColorStop(): void {} };
  const context = new Proxy({} as Record<string, unknown>, {
    get(_target, property) {
      if (typeof property !== 'string') return undefined;
      return (): unknown => {
        calls.push(property);
        return property.startsWith('create') ? gradient : undefined;
      };
    },
    set() {
      return true;
    },
  });
  Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
    configurable: true,
    value: () => (available ? context : null),
  });
  return calls;
}

function renderScene(overrides: Partial<TemporalSceneProps> = {}) {
  const handlers = {
    onSelectLane: vi.fn(),
    onSelectEvent: vi.fn(),
    onToggleBranch: vi.fn(),
    onSelectEncounter: vi.fn(),
    onWindowChange: vi.fn(),
    onInspect: vi.fn(),
  };
  const model = overrides.model ?? fixture();
  const view = render(
    <TemporalScene
      model={model}
      ariaLabel="Temporal execution field"
      reducedMotion={true}
      {...handlers}
      {...overrides}
    />,
  );
  return { ...view, ...handlers, model };
}

function lastWindow(spy: ReturnType<typeof vi.fn>): SceneWindow {
  const call = spy.mock.calls.at(-1);
  if (!call) throw new Error('onWindowChange was not called');
  return call[0] as SceneWindow;
}

describe('TemporalScene', () => {
  beforeEach(() => {
    stubCanvas(true);
  });
  afterEach(() => {
    Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
      configurable: true,
      value: originalGetContext,
    });
  });

  describe('layers', () => {
    it('paints the canvas substrate once a 2D context exists', () => {
      const calls = stubCanvas(true);
      const { container } = renderScene();
      expect(container.querySelector('[data-scene-layer="canvas"]')).toBeTruthy();
      expect(container.querySelector('[data-scene-layer="unavailable"]')).toBeNull();
      expect(calls).toContain('stroke');
      expect(calls).toContain('arcTo');
      expect(screen.queryByRole('status')).toBeNull();
    });

    it('declares the canvas unavailable and keeps every event in the overlay', () => {
      stubCanvas(false);
      const { container, model } = renderScene();
      expect(container.querySelector('[data-scene-layer="unavailable"]')).toBeTruthy();
      expect(screen.getByRole('status').textContent).toContain('scene layer unavailable');
      expect(container.querySelectorAll('[data-event]').length).toBe(model.nodes.length);
    });
  });

  describe('cursor and tail', () => {
    it('draws the reveal cursor at the model x', () => {
      const { container } = renderScene();
      const cursor = container.querySelector('[data-cursor]')!;
      expect(cursor.getAttribute('x1')).toBe('320');
      expect(container.querySelector('[data-cursor-mark]')?.getAttribute('data-cursor-basis')).toBe('time');
    });
  });

  describe('events', () => {
    it('renders one button per node with a Select label', () => {
      const { container, model } = renderScene();
      const events = container.querySelectorAll('[data-event]');
      expect(events.length).toBe(model.nodes.length);
      for (const element of events) {
        expect(element.getAttribute('role')).toBe('button');
        expect(element.getAttribute('aria-label')?.startsWith('Select ')).toBe(true);
      }
    });

    it('selects on click and on Enter', () => {
      const { container, onSelectEvent } = renderScene();
      fireEvent.click(container.querySelector('[data-event="n-commit"]')!);
      expect(onSelectEvent).toHaveBeenCalledWith('n-commit');
      fireEvent.keyDown(container.querySelector('[data-event="n-spawn"]')!, { key: 'Enter' });
      expect(onSelectEvent).toHaveBeenCalledWith('n-spawn');
      expect(onSelectEvent).toHaveBeenCalledTimes(2);
    });

    it('declares a sequence-placed node as recorded order', () => {
      const { container } = renderScene();
      const sequenced = container.querySelector('[data-event="n-msg"]')!;
      expect(sequenced.getAttribute('data-x-basis')).toBe('sequence');
      expect(sequenced.querySelector('title')?.textContent).toContain('recorded order');
      expect(container.querySelector('[data-event="n-commit"]')?.getAttribute('data-grade')).toBe('inferred');
    });

    it('hover inspects without selecting', () => {
      const { container, model, onInspect, onSelectEvent } = renderScene();
      const tool = container.querySelector('[data-event="n-tool"]')!;
      fireEvent.mouseOver(tool);
      expect(onInspect).toHaveBeenLastCalledWith(model.nodes.find((entry) => entry.id === 'n-tool'));
      const otherLane = container.querySelector(`[data-lane-group='${CHILD}']`) as SVGGElement;
      expect(otherLane.style.opacity).toBe('0.55');
      fireEvent.mouseOut(tool);
      expect(onInspect).toHaveBeenLastCalledWith(null);
      expect(otherLane.style.opacity).toBe('1');
      expect(onSelectEvent).not.toHaveBeenCalled();
    });
  });

  describe('lanes and branches', () => {
    it('opens a lane from its label row', () => {
      const { onSelectLane } = renderScene();
      fireEvent.click(screen.getByRole('button', { name: `Open session ${ROOT_LABEL}` }));
      expect(onSelectLane).toHaveBeenCalledWith(ROOT);
    });

    it('toggles a branch from the lane column and from the cluster body', () => {
      const { onToggleBranch } = renderScene();
      fireEvent.click(screen.getByRole('button', { name: `Collapse branch ${ROOT_LABEL}` }));
      expect(onToggleBranch).toHaveBeenCalledWith(ROOT);
      fireEvent.click(
        screen.getByRole('button', { name: `Expand branch ${BUNDLE_LABEL} · 4 sessions · 3 subagents · 57 messages` }),
      );
      expect(onToggleBranch).toHaveBeenCalledWith(BUNDLE);
    });
  });

  describe('intervals and gaps', () => {
    it('exposes a proximity encounter as a button that pivots selection', () => {
      const { container, onSelectEncounter } = renderScene();
      const proximity = container.querySelector('[data-proximity-encounter="enc-1"]')!;
      expect(proximity).toBeTruthy();
      fireEvent.click(proximity.closest('[role="button"]')!);
      expect(onSelectEncounter).toHaveBeenCalledWith('enc-1');
      expect(container.querySelector('[data-interval-kind="git_span"]')).toBeTruthy();
    });

    it('marks lane gaps in the field and lists page-wide gaps in the legend', () => {
      const { container } = renderScene();
      const gap = container.querySelector('[data-gap-kind="extent_unknown"]')!;
      expect(gap.getAttribute('role')).toBe('img');
      expect(screen.getByRole('list', { name: 'Evidence gaps' }).textContent).toContain('handoff unavailable');
      for (const grade of ['EXACT', 'EXPLICIT', 'INFERRED', 'AMBIGUOUS', 'STALE', 'UNAVAILABLE']) {
        expect(screen.getByText(grade)).toBeTruthy();
      }
    });
  });

  describe('time window', () => {
    it('zooms in to the middle half around the centre', () => {
      const { onWindowChange } = renderScene();
      fireEvent.click(screen.getByRole('button', { name: 'Zoom in' }));
      const next = lastWindow(onWindowChange);
      expect(next.end - next.start).toBeCloseTo(3600, 6);
      expect((next.start + next.end) / 2).toBeCloseTo((WINDOW.start + WINDOW.end) / 2, 6);
    });

    it('ctrl-wheel on the overlay narrows the window', () => {
      const { container, onWindowChange } = renderScene();
      fireEvent.wheel(container.querySelector('[data-scene-layer="overlay"]')!, { ctrlKey: true, deltaY: -100 });
      const next = lastWindow(onWindowChange);
      expect(next.end - next.start).toBeLessThan(7200);
    });

    it('pans later by a quarter of the span', () => {
      const { onWindowChange } = renderScene();
      fireEvent.click(screen.getByRole('button', { name: 'Pan later' }));
      const next = lastWindow(onWindowChange);
      expect(next.start).toBeCloseTo(WINDOW.start + 1800, 6);
      expect(next.end - next.start).toBeCloseTo(7200, 6);
    });

    it('offers Fit only when the window differs from the full extent', () => {
      const fitted = renderScene({ fullWindow: WINDOW });
      expect(screen.getByRole('button', { name: 'Fit' })).toHaveProperty('disabled', true);
      fitted.unmount();
      const full = { start: WINDOW.start - 3600, end: WINDOW.end + 3600 };
      const { onWindowChange } = renderScene({ fullWindow: full });
      const fit = screen.getByRole('button', { name: 'Fit' });
      expect(fit).toHaveProperty('disabled', false);
      fireEvent.click(fit);
      expect(onWindowChange).toHaveBeenCalledWith(full);
    });

    it('treats a click on the empty field as clearing the lane selection', () => {
      const { container, onSelectLane, onWindowChange } = renderScene();
      const background = container.querySelector('[data-field-background]')!;
      fireEvent.pointerDown(background, { pointerId: 1, button: 0, clientX: 400 });
      fireEvent.pointerUp(background, { pointerId: 1, clientX: 401 });
      expect(onSelectLane).toHaveBeenCalledWith(null);
      expect(onWindowChange).not.toHaveBeenCalled();
    });
  });

  describe('minimap', () => {
    it('draws bins, lanes and the viewport rect', () => {
      const { container } = renderScene();
      expect(container.querySelectorAll('[data-minimap-bin]').length).toBe(8);
      expect(container.querySelectorAll('[data-minimap-lane]').length).toBe(3);
      expect(container.querySelector('[data-scene-viewport]')).toBeTruthy();
    });

    it('pans with the arrow keys while keeping the span', () => {
      const { onWindowChange } = renderScene();
      fireEvent.keyDown(screen.getByRole('group', { name: 'Temporal minimap' }), { key: 'ArrowRight' });
      const next = lastWindow(onWindowChange);
      expect(next.end - next.start).toBeCloseTo(7200, 6);
      expect(next.start).toBeGreaterThan(WINDOW.start);
    });
  });
});
