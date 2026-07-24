import { render, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphCanvas } from './GraphCanvas.tsx';

type NodeAttributes = Record<string, unknown>;
type NodeReducer = (node: string, data: NodeAttributes) => NodeAttributes;

const sigmaState = vi.hoisted(() => ({
  nodeReducer: undefined as NodeReducer | undefined,
}));

vi.mock('./activation.ts', () => ({
  ActivationField: class MockActivationField {
    heatOf() {
      return 0;
    }

    subscribe() {
      return () => {};
    }

    get warm() {
      return false;
    }
  },
  cssColorToRgb: () => [128, 128, 128],
  lerpRgb: () => 'rgb(128, 128, 128)',
  lerpRgbTuple: () => [128, 128, 128],
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
      _graph: unknown,
      _container: unknown,
      settings: { nodeReducer?: NodeReducer },
    ) {
      sigmaState.nodeReducer = settings.nodeReducer;
    }

    setCustomBBox() {}
    on() {}
    refresh() {}
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
    sigmaState.nodeReducer = undefined;
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
