import { afterEach, describe, expect, it, vi } from 'vitest';

import type { GraphNodeV1 } from '../../contracts/generated.ts';
import { sceneFromSlice } from './cortexScene.ts';
import { boundsOf, luminance, luminousPainter } from './cortexLuminous.ts';

afterEach(() => vi.unstubAllGlobals());

describe('luminance', () => {
  it('lights by degree on the slice scale, absent degree at the floor', () => {
    expect(luminance(null, 16)).toBe(0.28);
    expect(luminance(0, 16)).toBe(0.28);
    expect(luminance(4, 16)).toBeCloseTo(0.64, 9);
    expect(luminance(16, 16)).toBe(1);
  });
});

describe('boundsOf', () => {
  it('pads the drawn extent and never returns an empty frame', () => {
    expect(boundsOf([{ x: 0, y: 0 }, { x: 100, y: 50 }])).toEqual({ x0: -4, y0: -2, x1: 104, y1: 52 });
    expect(boundsOf([])).toEqual({ x0: -1, y0: -1, x1: 1, y1: 1 });
  });
});

describe('luminousPainter.layout', () => {
  it('fails as a typed layout failure where no worker can settle it', async () => {
    const scene = sceneFromSlice([], []);
    await expect(
      Promise.resolve().then(() => luminousPainter.layout(scene, { width: 400, height: 300 })),
    ).rejects.toThrow('this browser runs no layout worker');
  });

  it('terminates its worker when switching away cancels the layout', async () => {
    const terminate = vi.fn();
    const postMessage = vi.fn();
    vi.stubGlobal('Worker', class {
      terminate = terminate;
      postMessage = postMessage;
    });
    const controller = new AbortController();
    const scene = sceneFromSlice([{ id: 'a', name: 'a', kind: 'function', degree: 1 } as GraphNodeV1], []);
    const pending = luminousPainter.layout(scene, { width: 400, height: 300 }, controller.signal);
    expect(postMessage).toHaveBeenCalledOnce();
    controller.abort();
    await expect(pending).rejects.toThrow('the layout was cancelled');
    expect(terminate).toHaveBeenCalledOnce();
  });
});
