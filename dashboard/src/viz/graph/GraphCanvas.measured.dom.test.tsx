import { act, render, waitFor } from '@testing-library/react';
import type Graph from 'graphology';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphCanvas } from './GraphCanvas.tsx';

/** Measured coordinates bypass the worker; emergent positions are unavailable
 * until a bounded background layout completes. */

const forceState = vi.hoisted(() => ({
  requested: false,
  signal: null as AbortSignal | null,
  pending: null as Promise<boolean> | null,
}));

vi.mock('./emergentLayout.ts', () => ({
  settleEmergentOffThread: async (_prepared: unknown, signal: AbortSignal) => {
    forceState.requested = true;
    forceState.signal = signal;
    return forceState.pending ?? true;
  },
}));

const sigmaState = vi.hoisted(() => ({
  graph: undefined as Graph | undefined,
  bbox: undefined as { x: [number, number]; y: [number, number] } | undefined,
}));

vi.mock('sigma', () => ({
  default: class MockSigma {
    /** The layer map the renderer reads to find the canvases whose WebGL
     * context it must watch. */
    private readonly layers = { nodes: document.createElement('canvas') };

    constructor(graph: Graph) {
      sigmaState.graph = graph;
    }

    setCustomBBox(bbox: { x: [number, number]; y: [number, number] }) {
      sigmaState.bbox = bbox;
    }

    getCanvases() {
      return this.layers;
    }

    resize() {}
    on() {}
    refresh() {}
    setSetting() {}
    kill() {}
  },
}));

/** Anything the canvas started asynchronously has had its turn by the time
 * this resolves, so "never requested" means never rather than not yet. */
async function flushPendingWork(): Promise<void> {
  for (let tick = 0; tick < 3; tick += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

const PLACED = [
  { id: 'a', label: 'A', kind: 'project', degree: 2, x: -1, y: -1 },
  { id: 'b', label: 'B', kind: 'project', degree: 1, x: 1, y: 1 },
];
const EMERGENT = PLACED.map(({ x: _x, y: _y, ...node }) => node);
const EDGES = [{ source: 'a', target: 'b' }];

describe('GraphCanvas layout engine loading', () => {
  beforeEach(() => {
    forceState.requested = false;
    forceState.signal = null;
    forceState.pending = null;
    sigmaState.graph = undefined;
    sigmaState.bbox = undefined;
    Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
      configurable: true,
      value: (kind: string) =>
        kind.startsWith('webgl') ? ({} as unknown as RenderingContext) : null,
    });
    Object.defineProperties(HTMLElement.prototype, {
      clientWidth: { configurable: true, get: () => 640 },
      clientHeight: { configurable: true, get: () => 320 },
      offsetWidth: { configurable: true, get: () => 640 },
      offsetHeight: { configurable: true, get: () => 320 },
    });
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue({ matches: false }),
    });
  });

  it('never asks for the force layout when every node was measured', async () => {
    render(
      <GraphCanvas nodes={PLACED} edges={EDGES} extent={{ x: [-4, 4], y: [-4, 4] }} />,
    );
    await waitFor(() => expect(sigmaState.graph).toBeDefined());
    await flushPendingWork();

    expect(forceState.requested).toBe(false);
    // ...and the field really was drawn, from the caller's own coordinates,
    // framed by the axis it named rather than by the bodies that occupy it.
    expect(sigmaState.graph!.getNodeAttribute('a', 'x')).toBe(-1);
    expect(sigmaState.graph!.getNodeAttribute('b', 'y')).toBe(1);
    expect(sigmaState.bbox).toEqual({ x: [-4, 4], y: [-4, 4] });
  });

  it('asks for it once a field has no measured positions of its own', async () => {
    render(<GraphCanvas nodes={EMERGENT} edges={EDGES} />);

    await waitFor(() => expect(forceState.requested).toBe(true));
    // Nothing is drawn until the engine has answered: a seed circle on screen
    // would be a composition the reader would read meaning into.
    await waitFor(() => expect(sigmaState.graph).toBeDefined());
  });

  it('keeps positions explicitly pending and aborts layout on unmount', async () => {
    let resolve!: (result: boolean) => void;
    forceState.pending = new Promise<boolean>((done) => { resolve = done; });
    const view = render(<GraphCanvas nodes={EMERGENT} edges={EDGES} cameraControls />);
    expect(view.getByRole('status').textContent).toContain('Calculating graph positions');
    expect((view.getByRole('button', { name: 'Zoom in graph' }) as HTMLButtonElement).disabled).toBe(true);
    const signal = forceState.signal;
    view.unmount();
    expect(signal?.aborted).toBe(true);
    await act(async () => resolve(true));
    expect(sigmaState.graph).toBeUndefined();
    forceState.pending = null;
  });

  it('does not install a late layout over a newer measured topology', async () => {
    let resolve!: (result: boolean) => void;
    forceState.pending = new Promise<boolean>((done) => { resolve = done; });
    const view = render(<GraphCanvas nodes={EMERGENT} edges={EDGES} />);
    const signal = forceState.signal;
    view.rerender(<GraphCanvas nodes={PLACED} edges={EDGES} />);
    const measuredGraph = sigmaState.graph;
    expect(signal?.aborted).toBe(true);
    expect(measuredGraph?.getNodeAttribute('a', 'x')).toBe(-1);
    await act(async () => resolve(true));
    expect(sigmaState.graph).toBe(measuredGraph);
  });

  it('reports a failed layout instead of drawing the initial seed', async () => {
    forceState.pending = Promise.reject(new Error('Worker unavailable'));
    const view = render(<GraphCanvas nodes={EMERGENT} edges={EDGES} />);
    await waitFor(() => expect(view.getByRole('status').textContent).toContain('could not be completed'));
    expect(view.container.querySelector('[role="img"]')).toBeNull();
    forceState.pending = null;
  });
});
