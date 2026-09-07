import { render, waitFor } from '@testing-library/react';
import type Graph from 'graphology';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphCanvas } from './GraphCanvas.tsx';
import { ActivationField } from './activation.ts';
import { setMotionPreference } from '../trace/reducedMotion.ts';

/**
 * End-to-end evidence for travelling activation.
 *
 * Unlike `GraphCanvas.dom.test.tsx`, this file deliberately does NOT mock
 * `activation.ts`: the whole question is whether a strike delivered from
 * outside the renderer — the way the Brain's SSE effect delivers one — reaches
 * the drawn graph and travels the real edge. Sigma is mocked only far enough
 * to hand back the graphology instance it was given, so every assertion below
 * is made against the geometry the renderer actually composed.
 */

type Reducer = (id: string, data: Record<string, unknown>) => Record<string, unknown>;

const sigmaState = vi.hoisted(() => ({
  graph: undefined as Graph | undefined,
  edgeReducer: undefined as Reducer | undefined,
  refreshes: 0,
}));

vi.mock('graphology-layout-forceatlas2', () => ({
  default: { inferSettings: () => ({ gravity: 1 }), assign: () => undefined },
}));

vi.mock('sigma', () => ({
  default: class MockSigma {
    /** The layer map the renderer reads to find the canvases whose WebGL
     * context it must watch. */
    private readonly layers = { nodes: document.createElement('canvas') };

    constructor(
      graph: Graph,
      _container: unknown,
      settings: { edgeReducer?: Reducer },
    ) {
      sigmaState.graph = graph;
      sigmaState.edgeReducer = settings.edgeReducer;
    }

    setCustomBBox() {}

    getCanvases() {
      return this.layers;
    }

    resize() {}
    on() {}
    refresh() {
      sigmaState.refreshes += 1;
    }
    setSetting() {}
    kill() {}
  },
}));

/** A manually pumped animation clock, so "the loop is asleep" and "the loop
 * ran a frame" are both directly observable rather than inferred from timing.
 * Handles are stable and cancellation genuinely dequeues, because "stopped" is
 * one of the states under test and a no-op cancel cannot distinguish it. */
const frames: { id: number; run: FrameRequestCallback }[] = [];
let nextFrameId = 1;

function pump(now: number) {
  const due = frames.splice(0, frames.length);
  for (const frame of due) frame.run(now);
}

/** The Brain's own shape: one repository hub wired to one checkout. */
const NODES = [
  { id: 'repo:r', label: 'r', kind: 'repository', degree: 2 },
  { id: 'p1', label: 'p1', kind: 'checkout', degree: 1 },
];
const EDGES = [{ source: 'repo:r', target: 'p1', kind: 'checkout' }];

function pulseNodes(graph: Graph): string[] {
  return graph.nodes().filter((node) => node.startsWith('__pulse__'));
}

function positionOf(graph: Graph, id: string): [number, number] {
  return [
    graph.getNodeAttribute(id, 'x') as number,
    graph.getNodeAttribute(id, 'y') as number,
  ];
}

describe('GraphCanvas travelling activation', () => {
  beforeEach(() => {
    sigmaState.graph = undefined;
    sigmaState.edgeReducer = undefined;
    sigmaState.refreshes = 0;
    frames.length = 0;
    nextFrameId = 1;
    Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
      configurable: true,
      value: (kind: string) => (kind.startsWith('webgl') ? ({} as RenderingContext) : null),
    });
    Object.defineProperties(HTMLElement.prototype, {
      clientWidth: { configurable: true, get: () => 640 },
      clientHeight: { configurable: true, get: () => 320 },
      // Sigma measures `offsetWidth`, and so does the guard that decides
      // whether a renderer may exist, so the fixture has to answer that name.
      offsetWidth: { configurable: true, get: () => 640 },
      offsetHeight: { configurable: true, get: () => 320 },
    });
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue({ matches: false }),
    });
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
      const id = nextFrameId++;
      frames.push({ id, run: callback });
      return id;
    });
    vi.stubGlobal('cancelAnimationFrame', (id: number) => {
      const at = frames.findIndex((frame) => frame.id === id);
      if (at >= 0) frames.splice(at, 1);
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    localStorage.removeItem('td.motion-preference');
  });

  it('carries an externally delivered strike along the real edge and then sleeps', async () => {
    const field = new ActivationField({ halfLifeMs: 4200 });
    render(<GraphCanvas nodes={NODES} edges={EDGES} activation={field} />);
    await waitFor(() => expect(sigmaState.graph).toBeDefined());
    const graph = sigmaState.graph!;

    // A cold field leaves the loop asleep: the canvas composed one static
    // frame and requested nothing.
    expect(frames).toHaveLength(0);
    expect(pulseNodes(graph)).toEqual([]);

    // Exactly what BrainPage does when one SSE event lands: fire the neuron
    // its scope names, then one hop along the drawn edge at a third the
    // energy. Neither call knows the render loop exists.
    field.strike(['p1'], 0.9);
    field.strike(['repo:r'], 0.3);

    // The strike alone woke the loop — nothing else could have, the field has
    // no clock of its own.
    expect(frames).toHaveLength(1);

    pump(0);
    const travellers = pulseNodes(graph);
    expect(travellers).toEqual(['__pulse__0']);

    // The edge between the two struck neurons conducts, because both of its
    // ends are warm.
    const lit = sigmaState.edgeReducer?.('e', {
      srcReal: 'repo:r',
      dstReal: 'p1',
      size: 1,
    });
    const cold = sigmaState.edgeReducer?.('e', {
      srcReal: 'repo:r',
      dstReal: 'absent',
      size: 1,
    });
    expect(lit?.['color']).not.toEqual(cold?.['color']);
    expect(lit?.['size']).toBeGreaterThan(1);

    // The light leaves from the end where the event actually happened. `p1` is
    // the hotter node because it is the one the event's own scope named; the
    // hub was only reached by the hop. So the traveller starts on `p1` and
    // runs toward the hub, not the other way round.
    const hub = positionOf(graph, 'repo:r');
    const origin = positionOf(graph, 'p1');
    expect(positionOf(graph, '__pulse__0')).toEqual(origin);

    const distanceTo = ([x, y]: [number, number], [tx, ty]: [number, number]) =>
      Math.hypot(x - tx, y - ty);
    pump(500);
    const midway = positionOf(graph, '__pulse__0');
    expect(midway).not.toEqual(origin);
    expect(distanceTo(midway, hub)).toBeLessThan(distanceTo(origin, hub));
    expect(distanceTo(midway, origin)).toBeGreaterThan(0);

    // ...and when the field finally goes cold the traveller is removed and the
    // loop stops asking for frames. An idle dashboard costs nothing.
    // `pump` drains the queue, so an empty queue afterwards means this frame
    // declined to request a successor rather than that one was discarded.
    pump(400_000);
    expect(field.warm).toBe(false);
    expect(pulseNodes(graph)).toEqual([]);
    expect(frames).toHaveLength(0);
  });

  it('never starts the loop or draws a traveller under reduced motion', async () => {
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue({ matches: true }),
    });
    const field = new ActivationField({ halfLifeMs: 4200 });
    render(<GraphCanvas nodes={NODES} edges={EDGES} activation={field} />);
    await waitFor(() => expect(sigmaState.graph).toBeDefined());
    const graph = sigmaState.graph!;

    field.strike(['p1'], 0.9);
    field.strike(['repo:r'], 0.3);

    // The state still lands — the glow is repainted statically — but nothing
    // is ever animated toward it.
    expect(frames).toHaveLength(0);
    expect(pulseNodes(graph)).toEqual([]);
    expect(graph.hasNode('__halo__p1')).toBe(true);
  });

  // The canvas used to read `prefers-reduced-motion` directly, which meant the
  // app's own persisted control — the one a reader actually sets, and the only
  // way to ask for stillness on an OS that reports no preference — had no effect
  // on the single most motion-heavy surface in the product. These two cover both
  // directions of that pin, because a control that can only agree with the OS is
  // not a control.
  it('honours a pinned "reduced" preference on an OS that reports no preference', async () => {
    localStorage.setItem('td.motion-preference', 'reduced');
    const field = new ActivationField({ halfLifeMs: 4200 });
    render(<GraphCanvas nodes={NODES} edges={EDGES} activation={field} />);
    await waitFor(() => expect(sigmaState.graph).toBeDefined());
    const graph = sigmaState.graph!;

    field.strike(['p1'], 0.9);
    field.strike(['repo:r'], 0.3);

    expect(frames).toHaveLength(0);
    expect(pulseNodes(graph)).toEqual([]);
    // The reading still arrives, statically.
    expect(graph.hasNode('__halo__p1')).toBe(true);
  });

  it('honours a pinned "full" preference on an OS that asks for reduced motion', async () => {
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue({ matches: true }),
    });
    localStorage.setItem('td.motion-preference', 'full');
    const field = new ActivationField({ halfLifeMs: 4200 });
    render(<GraphCanvas nodes={NODES} edges={EDGES} activation={field} />);
    await waitFor(() => expect(sigmaState.graph).toBeDefined());
    const graph = sigmaState.graph!;

    field.strike(['p1'], 0.9);
    field.strike(['repo:r'], 0.3);

    expect(frames).toHaveLength(1);
    pump(0);
    expect(pulseNodes(graph)).toEqual(['__pulse__0']);
  });

  it('stops a running loop the moment motion is turned off mid-flight', async () => {
    const field = new ActivationField({ halfLifeMs: 4200 });
    const { rerender } = render(
      <GraphCanvas nodes={NODES} edges={EDGES} activation={field} />,
    );
    await waitFor(() => expect(sigmaState.graph).toBeDefined());
    const graph = sigmaState.graph!;

    field.strike(['p1'], 0.9);
    field.strike(['repo:r'], 0.3);
    pump(0);
    expect(pulseNodes(graph)).toEqual(['__pulse__0']);

    // Setting the preference notifies every `useReducedMotion` subscriber; React
    // re-renders and the canvas settles. A shortened animation would leave the
    // traveller parked somewhere on the curve — a genuine no-motion path has
    // nowhere for it to be, so it is gone.
    setMotionPreference('reduced');
    rerender(<GraphCanvas nodes={NODES} edges={EDGES} activation={field} />);
    await waitFor(() => expect(pulseNodes(graph)).toEqual([]));
    expect(frames).toHaveLength(0);
    expect(field.warm).toBe(true);
  });

  it('does not rebuild the renderer when only the select handler identity changes', async () => {
    const field = new ActivationField();
    const { rerender } = render(
      <GraphCanvas nodes={NODES} edges={EDGES} activation={field} onSelect={() => {}} />,
    );
    await waitFor(() => expect(sigmaState.graph).toBeDefined());
    const first = sigmaState.graph;

    rerender(
      <GraphCanvas nodes={NODES} edges={EDGES} activation={field} onSelect={() => {}} />,
    );
    rerender(
      <GraphCanvas
        nodes={NODES}
        edges={EDGES}
        activation={field}
        selectedId="p1"
        onSelect={() => {}}
      />,
    );

    // Same renderer, same laid-out graph: a live event arriving every second
    // must not cost a teardown and a fresh force layout.
    expect(sigmaState.graph).toBe(first);
  });
});
