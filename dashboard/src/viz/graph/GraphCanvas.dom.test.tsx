import { render, waitFor } from '@testing-library/react';
import type Graph from 'graphology';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphCanvas } from './GraphCanvas.tsx';
import { ActivationField } from './activation.ts';

type NodeAttributes = Record<string, unknown>;
type NodeReducer = (node: string, data: NodeAttributes) => NodeAttributes;

const sigmaState = vi.hoisted(() => ({
  graph: undefined as Graph | undefined,
  nodeReducer: undefined as NodeReducer | undefined,
  refreshCount: 0,
  constructCount: 0,
  killCount: 0,
  resizeCount: 0,
  strikeListeners: new Set<() => void>(),
}));

vi.mock('./activation.ts', () => ({
  ActivationField: class MockActivationField {
    heatOf() {
      return 0;
    }

    get warm() {
      return false;
    }

    tick() {
      return false;
    }

    subscribe(listener: () => void) {
      sigmaState.strikeListeners.add(listener);
      return () => sigmaState.strikeListeners.delete(listener);
    }

    strike() {
      for (const listener of sigmaState.strikeListeners) listener();
    }
  },
  cssColorToRgb: () => [128, 128, 128],
  lerpRgb: () => 'rgb(128, 128, 128)',
  lerpRgbTuple: () => [128, 128, 128],
  restingNodeTint: () => [128, 128, 128],
  approach: (_current: number, target: number) => target,
  settled: () => true,
}));

vi.mock('graphology-layout-forceatlas2', () => ({
  default: {
    inferSettings: () => ({ gravity: 1 }),
    assign: () => undefined,
  },
}));

/** Mirrors the one behaviour of the real renderer this file is about: Sigma
 * measures `container.offsetWidth` on construction AND on every render, and
 * throws rather than drawing when the answer is zero. A mock that quietly
 * tolerated a collapsed container could not have caught the bug. */
vi.mock('sigma', () => ({
  default: class MockSigma {
    private readonly container: HTMLElement;
    /** Sigma stacks several canvas layers over the container and hands them
     * back by id; the renderer asks for that map to find the ones whose WebGL
     * context can be lost. `GraphCanvas.context.dom.test.tsx` is where losing
     * one is exercised. */
    private readonly layers: Record<string, HTMLCanvasElement> = {
      edges: document.createElement('canvas'),
      nodes: document.createElement('canvas'),
      hoverNodes: document.createElement('canvas'),
    };

    constructor(
      graph: Graph,
      container: HTMLElement,
      settings: { nodeReducer?: NodeReducer },
    ) {
      this.container = container;
      this.measure();
      sigmaState.graph = graph;
      sigmaState.nodeReducer = settings.nodeReducer;
      sigmaState.constructCount += 1;
    }

    private measure() {
      if (this.container.offsetWidth === 0) {
        throw new Error('Sigma: Container has no width.');
      }
    }

    setCustomBBox() {}

    getCanvases() {
      return this.layers;
    }

    /** Real Sigma exposes `resize(force?: boolean): this`; a live renderer
     * absorbs a container resize through it rather than being rebuilt. */
    resize() {
      sigmaState.resizeCount += 1;
      return this;
    }
    on() {}
    refresh() {
      this.measure();
      sigmaState.refreshCount += 1;
    }
    setSetting() {}
    kill() {
      sigmaState.killCount += 1;
    }
  },
}));

/** jsdom has no WebGL, so the canvas would take its no-context fallback.
 * Simulate a WebGL-capable browser for the rendering tests, and the absence of
 * one where that is the case under test. */
function stubWebGl(available: boolean) {
  Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
    configurable: true,
    value: (kind: string) =>
      available && kind.startsWith('webgl') ? ({} as unknown as RenderingContext) : null,
  });
}

/** Give anything the canvas started asynchronously — an emergent field loads
 * its layout engine on demand — its turn to run, so an assertion that nothing
 * was built means never rather than not yet. */
async function flushPendingWork(): Promise<void> {
  for (let tick = 0; tick < 3; tick += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

/** The measured box every element reports, mutable so a test can take it away
 * the way navigating off a workspace does. */
const box = { width: 640, height: 320 };
/** Every live ResizeObserver callback, so a test can deliver a measurement. */
const observerCallbacks = new Set<() => void>();

describe('GraphCanvas', () => {
  beforeEach(() => {
    sigmaState.graph = undefined;
    sigmaState.nodeReducer = undefined;
    sigmaState.refreshCount = 0;
    sigmaState.constructCount = 0;
    sigmaState.killCount = 0;
    sigmaState.resizeCount = 0;
    sigmaState.strikeListeners.clear();
    box.width = 640;
    box.height = 320;
    observerCallbacks.clear();
    stubWebGl(true);
    vi.stubGlobal(
      'ResizeObserver',
      class MockResizeObserver {
        private readonly callback: () => void;
        constructor(callback: () => void) {
          this.callback = callback;
          observerCallbacks.add(callback);
        }
        observe() {}
        disconnect() {
          observerCallbacks.delete(this.callback);
        }
        unobserve() {}
      },
    );
    Object.defineProperties(HTMLElement.prototype, {
      clientWidth: { configurable: true, get: () => box.width },
      clientHeight: { configurable: true, get: () => box.height },
      // Sigma measures `offsetWidth`, and so does the guard that decides
      // whether a renderer may exist, so the fixture has to answer that name.
      offsetWidth: { configurable: true, get: () => box.width },
      offsetHeight: { configurable: true, get: () => box.height },
    });
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue({ matches: false }),
    });
  });

  describe('renderer lifetime against the container box', () => {
    const NODES = [{ id: 'node', label: 'Node', kind: 'function', degree: 1 }];

    /** Deliver a measurement the way the browser does after a layout change. */
    function deliverMeasurement() {
      for (const callback of [...observerCallbacks]) callback();
    }

    it('does not build a renderer for a container that has no box', async () => {
      box.width = 0;
      box.height = 0;

      expect(() => render(<GraphCanvas nodes={NODES} edges={[]} />)).not.toThrow();
      expect(sigmaState.constructCount).toBe(0);
      // An emergent field now fetches its layout engine before composing, so
      // "no renderer" has to outlast that boundary as well: the guard is on
      // whether a box exists, never on whether the engine has answered yet.
      await flushPendingWork();
      expect(sigmaState.constructCount).toBe(0);
    });

    it('builds one once the container is measured, without a mount retry', async () => {
      box.width = 0;
      box.height = 0;
      render(<GraphCanvas nodes={NODES} edges={[]} />);
      expect(sigmaState.constructCount).toBe(0);

      box.width = 640;
      box.height = 320;
      deliverMeasurement();

      await waitFor(() => expect(sigmaState.constructCount).toBe(1));
    });

    // The regression: leaving a workspace collapses the container while the
    // renderer is still alive, and Sigma's next frame -- including ones it
    // schedules itself from a window resize -- measures zero and throws. The
    // renderer has to be gone by then, not merely told to skip a frame.
    it('kills the renderer when the container loses its box', async () => {
      const { rerender } = render(
        <GraphCanvas nodes={NODES} edges={[]} selectedId={null} />,
      );
      await waitFor(() => expect(sigmaState.constructCount).toBe(1));
      expect(sigmaState.killCount).toBe(0);

      box.width = 0;
      box.height = 0;
      deliverMeasurement();

      expect(sigmaState.killCount).toBe(1);
      // A repaint request arriving after the collapse must find nothing to
      // repaint rather than reaching a renderer that would measure zero.
      expect(() =>
        rerender(<GraphCanvas nodes={NODES} edges={[]} selectedId="node" />),
      ).not.toThrow();
    });

    it('rebuilds when the box comes back', async () => {
      render(<GraphCanvas nodes={NODES} edges={[]} />);
      await waitFor(() => expect(sigmaState.constructCount).toBe(1));

      box.width = 0;
      box.height = 0;
      deliverMeasurement();
      expect(sigmaState.killCount).toBe(1);

      box.width = 800;
      box.height = 400;
      deliverMeasurement();

      await waitFor(() => expect(sigmaState.constructCount).toBe(2));
    });

    // The counterpart to the three tests above, and the reason the renderer's
    // lifetime is keyed on whether a box exists rather than on how big it is.
    // Keying it on the dimensions made every drag of a window edge or opening
    // of a side panel kill the renderer, rebuild the graphology graph and re-run
    // the 200-iteration ForceAtlas2 settle -- a full layout per resize frame.
    it('resizes the live renderer instead of rebuilding it', async () => {
      render(<GraphCanvas nodes={NODES} edges={[]} />);
      await waitFor(() => expect(sigmaState.constructCount).toBe(1));

      box.width = 900;
      box.height = 500;
      deliverMeasurement();
      box.width = 1200;
      box.height = 640;
      deliverMeasurement();

      await waitFor(() => expect(sigmaState.resizeCount).toBeGreaterThan(0));
      expect(sigmaState.constructCount).toBe(1);
      expect(sigmaState.killCount).toBe(0);
    });
  });

  it('prints the visual grammar instead of leaving circles unexplained', () => {
    const { getByLabelText } = render(
      <GraphCanvas
        nodes={[{ id: 'node', label: 'Node', kind: 'function', degree: 1 }]}
        edges={[]}
        encoding={{
          body: 'one symbol',
          size: 'connectedness',
          hue: 'symbol kind',
          signal: 'activation or supplied vitality',
          relation: 'one real graph edge',
        }}
      />,
    );

    const key = getByLabelText('Graph visual key').textContent ?? '';
    expect(key).toMatch(/disc\s*·\s*one symbol/i);
    expect(key).toMatch(/size\s*·\s*connectedness/i);
    expect(key).toMatch(/hue\s*·\s*symbol kind/i);
    expect(key).toMatch(/glow\s*·\s*activation or supplied vitality/i);
    expect(key).toMatch(/line\s*·\s*one real graph edge/i);
  });

  it('clamps dense connected fields before their bodies can fuse', async () => {
    const nodes = Array.from({ length: 40 }, (_, index) => ({
      id: `node-${index}`,
      label: `Node ${index}`,
      kind: index % 2 === 0 ? 'function' : 'struct',
      degree: index === 0 ? 39 : 1,
    }));
    const edges = nodes.slice(1).map((node) => ({
      source: nodes[0]!.id,
      target: node.id,
    }));

    render(<GraphCanvas nodes={nodes} edges={edges} />);
    await waitFor(() => expect(sigmaState.graph).toBeDefined());

    const realSizes = sigmaState
      .graph!.nodes()
      .filter((id) => !id.startsWith('__'))
      .map((id) => sigmaState.graph!.getNodeAttribute(id, 'size') as number);
    expect(Math.max(...realSizes)).toBeLessThanOrEqual(7.5);
  });

  it('keeps absent connectedness unknown instead of coercing it to a healthy value', async () => {
    const { getByText } = render(
      <GraphCanvas
        nodes={[{ id: 'unknown', label: 'Unknown degree', kind: 'function' }]}
        edges={[]}
      />,
    );
    await waitFor(() => expect(sigmaState.graph).toBeDefined());

    expect(sigmaState.graph!.getNodeAttribute('unknown', 'degree')).toBeUndefined();
    expect(sigmaState.graph!.getNodeAttribute('unknown', 'size')).toBeGreaterThan(0);
    expect(getByText(/connectedness is absent for 1 symbol/i).textContent).toMatch(
      /minimum marker, not zero/i,
    );
  });

  it('preserves low-alpha rendering attributes for companion nodes', async () => {
    render(
      <GraphCanvas
        nodes={[{ id: 'node', label: 'Node', kind: 'function', degree: 1 }]}
        edges={[]}
      />,
    );
    await waitFor(() => expect(sigmaState.nodeReducer).toBeDefined());
    const companion = {
      x: 1,
      y: 2,
      size: 16,
      color: 'rgba(122, 162, 247, 0.050)',
      label: '',
      zIndex: 0,
    };

    for (const managed of [
      '__halo__node',
      '__bloom__node',
      '__ring__node',
      '__pulse__0',
      '__way__0:1',
    ]) {
      expect(sigmaState.nodeReducer?.(managed, companion)).toEqual(companion);
    }
  });

  it('wakes the sleeping render loop when a caller-owned field strikes from outside', async () => {
    const field = new ActivationField();
    render(
      <GraphCanvas
        nodes={[
          { id: 'hub', label: 'Hub', kind: 'repository', degree: 2 },
          { id: 'leaf', label: 'Leaf', kind: 'project', degree: 1 },
        ]}
        edges={[{ source: 'hub', target: 'leaf' }]}
        activation={field}
      />,
    );
    await waitFor(() => expect(sigmaState.nodeReducer).toBeDefined());
    // The canvas subscribed to the field it was handed — this is the seam a
    // live SSE strike (struck entirely outside this component) uses to wake
    // the loop instead of leaving heat on the field that nothing draws.
    expect(sigmaState.strikeListeners.size).toBeGreaterThan(0);
    const before = sigmaState.refreshCount;
    field.strike(['leaf'], 0.8);
    await waitFor(() => expect(sigmaState.refreshCount).toBeGreaterThan(before));
  });

  it('states the missing WebGL context and the caller-supplied text alternative', async () => {
    stubWebGl(false);
    const { getByText } = render(
      <GraphCanvas
        nodes={[{ id: 'node', label: 'Node', kind: 'function', degree: 1 }]}
        edges={[]}
        fallbackDescription="the project registry remains available as a text alternative"
      />,
    );
    expect(getByText(/no WebGL context/i)).toBeTruthy();
    expect(getByText(/project registry remains available/i)).toBeTruthy();
    expect(document.querySelector('[role="status"][aria-live="polite"]')).not.toBeNull();
    // Never constructed: Sigma throws without a context, and that exception
    // would take the whole workspace route down through the error boundary.
    // Held across the layout engine's async boundary too — a renderer that
    // merely arrives late is still a renderer that must not exist.
    expect(sigmaState.nodeReducer).toBeUndefined();
    await flushPendingWork();
    expect(sigmaState.nodeReducer).toBeUndefined();
  });
});
