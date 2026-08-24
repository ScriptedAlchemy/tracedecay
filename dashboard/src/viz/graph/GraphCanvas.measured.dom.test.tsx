import { render, waitFor } from '@testing-library/react';
import type Graph from 'graphology';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphCanvas } from './GraphCanvas.tsx';

/**
 * The chunk boundary, asserted rather than assumed.
 *
 * Skipping the force pass on a measured field was never the hard part — that
 * guard has always been there. The cost that survived it was the STATIC
 * import: ForceAtlas2 was pulled into the chunk of every field, including ones
 * whose coordinates the engine must never be allowed to touch. The engine is
 * now reached through a dynamic `import()` on the emergent path only, and the
 * probe below is the module factory itself: it runs exactly once, the first
 * time anything asks for the package, so a measured render that leaves it
 * unrun is a measured render that never loaded the library.
 *
 * The two cases are order-dependent on purpose — a module registry has no
 * "unload", so the negative case has to be the one that runs first, and the
 * positive case that follows is what keeps it from passing vacuously.
 */

const forceState = vi.hoisted(() => ({ requested: false }));

vi.mock('graphology-layout-forceatlas2', () => {
  forceState.requested = true;
  return {
    default: {
      inferSettings: () => ({ gravity: 1 }),
      assign: () => undefined,
    },
  };
});

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
});
