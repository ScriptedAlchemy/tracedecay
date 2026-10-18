import { afterEach, describe, expect, it, vi } from 'vitest';

import type { GraphEdgeV1, GraphNodeV1 } from '../../contracts/generated.ts';
import { packPasses, reliefLayout } from './cortexReliefField.ts';
import {
  reliefLayoutOffThread,
  type ReliefLayoutJob,
  type ReliefLayoutReply,
} from './cortexReliefOffThread.ts';
import { sceneFromSlice } from './cortexScene.ts';

class LayoutWorker {
  static instances: LayoutWorker[] = [];
  onmessage: ((event: { data: ReliefLayoutReply }) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessageerror: (() => void) | null = null;
  terminate = vi.fn();
  postMessage = vi.fn<(job: ReliefLayoutJob) => void>();
  constructor() {
    LayoutWorker.instances.push(this);
  }
}

function makeScene() {
  const nodes = ['a', 'b', 'c'].map(
    (id) => ({ id, kind: 'function', name: id, file_path: `src/${id}/x.rs`, degree: 1 }) as GraphNodeV1,
  );
  const edges = [{ source: 'a', target: 'b', kind: 'calls', line: 1 } as GraphEdgeV1];
  return sceneFromSlice(nodes, edges);
}

const BOX = { width: 800, height: 400 };

afterEach(() => {
  vi.unstubAllGlobals();
  LayoutWorker.instances = [];
});

describe('reliefLayoutOffThread', () => {
  it('computes in the worker, hands back its layout, and terminates it', async () => {
    vi.stubGlobal('Worker', LayoutWorker);
    const scene = makeScene();
    const pending = reliefLayoutOffThread(scene, BOX);
    const worker = LayoutWorker.instances[0]!;
    expect(worker.postMessage).toHaveBeenCalledExactlyOnceWith({ scene, aspect: 2 });
    const layout = reliefLayout(scene, 2);
    worker.onmessage!({ data: { layout } });
    expect(await pending).toBe(layout);
    expect(worker.terminate).toHaveBeenCalledTimes(1);
  });

  it('terminates the worker and rejects when aborted', async () => {
    vi.stubGlobal('Worker', LayoutWorker);
    const controller = new AbortController();
    const pending = reliefLayoutOffThread(makeScene(), BOX, controller.signal);
    const worker = LayoutWorker.instances[0]!;
    controller.abort();
    await expect(pending).rejects.toThrow('superseded');
    expect(worker.terminate).toHaveBeenCalledTimes(1);
    worker.onmessage!({ data: { layout: reliefLayout(makeScene(), 2) } });
    expect(worker.terminate).toHaveBeenCalledTimes(2);
  });

  it('starts no worker for a request that is already aborted', async () => {
    vi.stubGlobal('Worker', LayoutWorker);
    const controller = new AbortController();
    controller.abort();
    await expect(reliefLayoutOffThread(makeScene(), BOX, controller.signal)).rejects.toThrow(
      'superseded',
    );
    expect(LayoutWorker.instances).toHaveLength(0);
  });

  it('reuses a finished layout for the same scene instead of recomputing', async () => {
    vi.stubGlobal('Worker', LayoutWorker);
    const scene = makeScene();
    const first = reliefLayoutOffThread(scene, BOX);
    const layout = reliefLayout(scene, 2);
    LayoutWorker.instances[0]!.onmessage!({ data: { layout } });
    await first;
    expect(await reliefLayoutOffThread(scene, BOX)).toBe(layout);
    expect(LayoutWorker.instances).toHaveLength(1);
    void reliefLayoutOffThread(scene, { width: 400, height: 800 });
    expect(LayoutWorker.instances).toHaveLength(2);
  });

  it('rejects with the worker error and terminates', async () => {
    vi.stubGlobal('Worker', LayoutWorker);
    const pending = reliefLayoutOffThread(makeScene(), BOX);
    const worker = LayoutWorker.instances[0]!;
    worker.onmessage!({ data: { error: 'boom' } });
    await expect(pending).rejects.toThrow('boom');
    expect(worker.terminate).toHaveBeenCalledTimes(1);
  });

  it('lays out in place where there is no Worker', async () => {
    vi.stubGlobal('Worker', undefined);
    const scene = makeScene();
    expect(await reliefLayoutOffThread(scene, BOX)).toEqual(reliefLayout(scene, 2));
  });
});

describe('the layout worker', () => {
  it('replies with exactly the layout the in-process function computes', async () => {
    const scene = makeScene();
    const worker = {
      onmessage: null as ((event: { data: ReliefLayoutJob }) => void) | null,
      postMessage: vi.fn(),
    };
    vi.stubGlobal('self', worker);
    await import('./cortexReliefLayout.worker.ts');
    worker.onmessage!({ data: { scene, aspect: 2 } });
    expect(worker.postMessage).toHaveBeenCalledExactlyOnceWith({ layout: reliefLayout(scene, 2) });
  });
});

describe('packPasses', () => {
  it('keeps the full 500 passes for small slices and bounds all-pairs work at the limit', () => {
    expect(packPasses(10)).toBe(500);
    expect(packPasses(100)).toBe(500);
    const limit = packPasses(250);
    expect(limit).toBeLessThan(150);
    expect(limit * ((250 * 249) / 2)).toBeLessThanOrEqual(4_000_000);
    expect(packPasses(5000)).toBe(100);
  });
});
