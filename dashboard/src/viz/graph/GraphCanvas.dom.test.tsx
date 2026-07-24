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

    get warm() {
      return false;
    }
  },
  cssColorToRgb: () => [128, 128, 128],
  lerpRgb: () => 'rgb(128, 128, 128)',
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

describe('GraphCanvas', () => {
  beforeEach(() => {
    sigmaState.nodeReducer = undefined;
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

    expect(sigmaState.nodeReducer?.('__halo__node', companion)).toEqual(companion);
    expect(sigmaState.nodeReducer?.('__pulse__edge', companion)).toEqual(companion);
  });
});
