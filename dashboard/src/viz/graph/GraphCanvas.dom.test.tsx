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

vi.mock('sigma', () => ({
  default: class MockSigma {
    constructor(
      graph: Graph,
      _container: unknown,
      settings: { nodeReducer?: NodeReducer },
    ) {
      sigmaState.graph = graph;
      sigmaState.nodeReducer = settings.nodeReducer;
    }

    setCustomBBox() {}
    on() {}
    refresh() {
      sigmaState.refreshCount += 1;
    }
    setSetting() {}
    kill() {}
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

describe('GraphCanvas', () => {
  beforeEach(() => {
    sigmaState.graph = undefined;
    sigmaState.nodeReducer = undefined;
    sigmaState.refreshCount = 0;
    sigmaState.strikeListeners.clear();
    stubWebGl(true);
    Object.defineProperties(HTMLElement.prototype, {
      clientWidth: { configurable: true, get: () => 640 },
      clientHeight: { configurable: true, get: () => 320 },
    });
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue({ matches: false }),
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

  it('states the missing WebGL context instead of constructing a renderer that throws', () => {
    stubWebGl(false);
    const { getByText } = render(
      <GraphCanvas
        nodes={[{ id: 'node', label: 'Node', kind: 'function', degree: 1 }]}
        edges={[]}
      />,
    );
    expect(getByText(/no WebGL context/i)).toBeTruthy();
    // Never constructed: Sigma throws without a context, and that exception
    // would take the whole workspace route down through the error boundary.
    expect(sigmaState.nodeReducer).toBeUndefined();
  });
});
