import { act, render, screen, waitFor } from '@testing-library/react';
import type Graph from 'graphology';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GraphCanvas } from './GraphCanvas.tsx';
import { ActivationField } from './activation.ts';

/**
 * The failure that arrives AFTER a successful draw.
 *
 * Every other way this canvas can fail is decided before a pixel exists — no
 * WebGL at all, a graph past the tier, a layout engine that never loaded — and
 * each of those is stated instead of drawn. A context the GPU takes back is the
 * exception: the field was real, and then the canvas either freezes on its last
 * frame or clears to nothing while the caption goes on describing a live graph.
 * A frozen picture reads as a live one and a cleared canvas reads as an empty
 * graph, so both are falsified readings.
 *
 * `activation.ts` is deliberately NOT mocked: "the loop stopped" has to be
 * observed against a field that is genuinely still warm, or a loop that merely
 * ran out of heat would pass the same assertion.
 */

const sigmaState = vi.hoisted(() => ({
  graph: undefined as Graph | undefined,
  layers: {} as Record<string, HTMLCanvasElement>,
  constructCount: 0,
  killCount: 0,
}));

vi.mock('graphology-layout-forceatlas2', () => ({
  default: { inferSettings: () => ({ gravity: 1 }), assign: () => undefined },
}));

vi.mock('sigma', () => {
  /** A layer that answers honestly about the context it holds, the way a real
   * canvas does: one that already has a 2d context returns null for every
   * WebGL id, which is the rule the renderer's probe rests on. */
  const layer = (context: 'webgl' | '2d'): HTMLCanvasElement => {
    const canvas = document.createElement('canvas');
    canvas.dataset['layerContext'] = context;
    return canvas;
  };
  return {
    default: class MockSigma {
      constructor(graph: Graph, container: HTMLElement) {
        // Sigma's real stack, in its real order: the bodies and relations are
        // drawn in WebGL, while labels, hover decoration and pointer capture
        // ride 2d layers that no context loss can reach.
        sigmaState.layers = {
          edges: layer('webgl'),
          edgeLabels: layer('2d'),
          nodes: layer('webgl'),
          labels: layer('2d'),
          hovers: layer('2d'),
          hoverNodes: layer('webgl'),
          mouse: layer('2d'),
        };
        for (const canvas of Object.values(sigmaState.layers)) {
          container.appendChild(canvas);
        }
        sigmaState.graph = graph;
        sigmaState.constructCount += 1;
      }

      setCustomBBox() {}

      getCanvases() {
        return sigmaState.layers;
      }

      resize() {}
      on() {}
      refresh() {}
      setSetting() {}

      /** Real `kill` detaches every layer and forgets the map, which is why the
       * watch has to hold the canvases it captured: the restore is dispatched
       * at the canvas of the renderer that died. */
      kill() {
        for (const canvas of Object.values(sigmaState.layers)) canvas.remove();
        sigmaState.killCount += 1;
      }
    },
  };
});

/** A manually pumped animation clock, so "the loop is asleep" is observed
 * rather than inferred from timing. */
const frames: { id: number; run: FrameRequestCallback }[] = [];
let nextFrameId = 1;

const NODES = [
  { id: 'repo:r', label: 'r', kind: 'repository', degree: 2 },
  { id: 'p1', label: 'p1', kind: 'checkout', degree: 1 },
];
const EDGES = [{ source: 'repo:r', target: 'p1', kind: 'checkout' }];

/** The WebGL layers a real loss would reach, named so a regression that watches
 * only the layer the bodies sit on is visible. */
const WEBGL_LAYERS = ['edges', 'nodes', 'hoverNodes'];

function loseContext(canvas: HTMLCanvasElement): Event {
  const event = new Event('webglcontextlost', { cancelable: true });
  canvas.dispatchEvent(event);
  return event;
}

describe('GraphCanvas WebGL context loss', () => {
  beforeEach(() => {
    sigmaState.graph = undefined;
    sigmaState.layers = {};
    sigmaState.constructCount = 0;
    sigmaState.killCount = 0;
    frames.length = 0;
    nextFrameId = 1;
    Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
      configurable: true,
      value: function getContext(this: HTMLCanvasElement, kind: string) {
        const held = this.dataset['layerContext'] ?? 'webgl';
        if (held === '2d') return kind === '2d' ? ({} as RenderingContext) : null;
        return kind.startsWith('webgl') ? ({} as RenderingContext) : null;
      },
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
  });

  it('states the lost renderer instead of leaving a dead canvas on screen', async () => {
    const field = new ActivationField({ halfLifeMs: 4200 });
    render(<GraphCanvas nodes={NODES} edges={EDGES} activation={field} />);
    await waitFor(() => expect(sigmaState.graph).toBeDefined());
    // The field really was drawn: the canvas region is on screen and the
    // activation loop is running against live heat.
    expect(screen.queryByRole('img')).not.toBeNull();
    field.strike(['p1'], 0.9);
    expect(frames).toHaveLength(1);

    const lost = await act(async () => loseContext(sigmaState.layers['nodes']!));

    // Without this the browser abandons the context for good and no restore is
    // ever attempted, so it is the first thing the handler does.
    expect(lost.defaultPrevented).toBe(true);
    // The reader is told the renderer was lost — not that the neighbourhood is
    // empty, which is the sentence an undrawn field would otherwise imply — and
    // is pointed at the same equivalent the no-context path names.
    const stated = screen.getByText(/lost its WebGL context/i);
    expect(stated.textContent).toMatch(/no longer being drawn/i);
    expect(stated.textContent).toMatch(/read the field description below/i);
    expect(document.querySelector('[role="status"][aria-live="polite"]')).not.toBeNull();
    expect(screen.queryByText(/no graph neighborhood to draw/i)).toBeNull();
    expect(document.querySelector('[data-state="unavailable"]')).not.toBeNull();
    // Nothing is drawn in its place: no canvas region, so no blank field that
    // could be read as a graph with nothing in it.
    expect(screen.queryByRole('img')).toBeNull();
    expect(document.querySelector('canvas')).toBeNull();

    // The loop stopped, and the field is still warm — so it stopped because the
    // renderer died, not because there was nothing left to animate.
    expect(frames).toHaveLength(0);
    expect(field.warm).toBe(true);
    expect(sigmaState.killCount).toBe(1);
  });

  it('brings the field back when the browser restores the context', async () => {
    render(<GraphCanvas nodes={NODES} edges={EDGES} />);
    await waitFor(() => expect(sigmaState.constructCount).toBe(1));
    // Captured before the loss: killing the renderer detaches every layer, and
    // the restore arrives at the canvas that lost the context.
    const canvas = sigmaState.layers['nodes']!;

    await act(async () => loseContext(canvas));
    expect(screen.getByText(/lost its WebGL context/i)).toBeTruthy();

    await act(async () => {
      canvas.dispatchEvent(new Event('webglcontextrestored'));
    });

    // Composed again from the same nodes and edges the caller last handed over,
    // with no action of its own.
    await waitFor(() => expect(sigmaState.constructCount).toBe(2));
    expect(screen.queryByText(/lost its WebGL context/i)).toBeNull();
    expect(screen.queryByRole('img')).not.toBeNull();

    // ...and the field that came back is watched as well, on its own layers: a
    // renderer rebuilt after one loss is exactly as exposed to the next.
    await act(async () => loseContext(sigmaState.layers['nodes']!));
    expect(screen.getByText(/lost its WebGL context/i)).toBeTruthy();
  });

  it.each(WEBGL_LAYERS)('reports a context lost on the %s layer', async (id) => {
    render(<GraphCanvas nodes={NODES} edges={EDGES} />);
    await waitFor(() => expect(sigmaState.constructCount).toBe(1));

    const lost = await act(async () => loseContext(sigmaState.layers[id]!));

    expect(lost.defaultPrevented).toBe(true);
    expect(screen.getByText(/lost its WebGL context/i)).toBeTruthy();
  });

  it('leaves the 2d layers alone, because their context cannot be lost', async () => {
    render(<GraphCanvas nodes={NODES} edges={EDGES} />);
    await waitFor(() => expect(sigmaState.constructCount).toBe(1));

    const lost = await act(async () => loseContext(sigmaState.layers['mouse']!));

    expect(lost.defaultPrevented).toBe(false);
    expect(screen.queryByText(/lost its WebGL context/i)).toBeNull();
    expect(screen.queryByRole('img')).not.toBeNull();
  });

  it('releases the watch with the canvas, so a dead layer cannot rebuild it', async () => {
    const { unmount } = render(<GraphCanvas nodes={NODES} edges={EDGES} />);
    await waitFor(() => expect(sigmaState.constructCount).toBe(1));
    const canvas = sigmaState.layers['nodes']!;

    unmount();

    const lost = await act(async () => loseContext(canvas));
    await act(async () => {
      canvas.dispatchEvent(new Event('webglcontextrestored'));
    });

    expect(lost.defaultPrevented).toBe(false);
    expect(sigmaState.constructCount).toBe(1);
  });
});
