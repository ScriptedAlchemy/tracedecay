import { render, waitFor } from '@testing-library/react';
import type Graph from 'graphology';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphCanvas } from './GraphCanvas.tsx';
import { ActivationField } from './activation.ts';

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
    constructor(
      graph: Graph,
      _container: unknown,
      settings: { edgeReducer?: Reducer },
    ) {
      sigmaState.graph = graph;
      sigmaState.edgeReducer = settings.edgeReducer;
    }

    setCustomBBox() {}
    on() {}
    refresh() {
      sigmaState.refreshes += 1;
    }
    setSetting() {}
    kill() {}
  },
}));

/** A manually pumped animation clock, so "the loop is asleep" and "the loop
 * ran a frame" are both directly observable rather than inferred from timing. */
const frames: FrameRequestCallback[] = [];

function pump(now: number) {
  const due = frames.splice(0, frames.length);
  for (const frame of due) frame(now);
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
    Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
      configurable: true,
      value: (kind: string) => (kind.startsWith('webgl') ? ({} as RenderingContext) : null),
    });
    Object.defineProperties(HTMLElement.prototype, {
      clientWidth: { configurable: true, get: () => 640 },
      clientHeight: { configurable: true, get: () => 320 },
    });
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue({ matches: false }),
    });
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
      frames.push(callback);
      return frames.length;
    });
    vi.stubGlobal('cancelAnimationFrame', () => {});
  });

  afterEach(() => {
    vi.unstubAllGlobals();
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
