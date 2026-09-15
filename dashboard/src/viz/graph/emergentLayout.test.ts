import { afterEach, expect, it, vi } from 'vitest';
import { prepareField } from './layout.ts';
import { settleEmergentOffThread, type EmergentLayoutJob } from './emergentLayout.ts';

class LayoutWorker {
  static instances: LayoutWorker[] = [];
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessageerror: (() => void) | null = null;
  terminate = vi.fn();
  postMessage = vi.fn<(job: EmergentLayoutJob) => void>();
  constructor() { LayoutWorker.instances.push(this); }
}

function prepare() {
  return prepareField({
    nodes: [{ id: 'a', label: 'A', kind: 'function' }, { id: 'b', label: 'B', kind: 'function' }],
    edges: [{ source: 'a', target: 'b' }],
    viewport: { width: 640, height: 320 },
    kindRgb: () => [100, 100, 100],
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  LayoutWorker.instances = [];
});

it('transfers the real graph and installs all exact coordinates before terminating', async () => {
  vi.stubGlobal('Worker', LayoutWorker);
  const prepared = prepare();
  const pending = settleEmergentOffThread(prepared, new AbortController().signal);
  const worker = LayoutWorker.instances[0]!;
  expect(worker.postMessage).toHaveBeenCalledWith({
    graph: prepared.graph.export(), nodeCount: prepared.nodeCount, edgeDensity: prepared.edgeDensity,
  });
  worker.onmessage!({ data: [{ id: 'b', x: 8.125, y: -4 }, { id: 'a', x: -1, y: 2 }] });
  expect(await pending).toBe(true);
  expect(prepared.graph.getNodeAttribute('b', 'x')).toBe(8.125);
  expect(prepared.graph.getNodeAttribute('a', 'y')).toBe(2);
  expect(worker.terminate).toHaveBeenCalledOnce();
});

it('terminates on cancellation and ignores a late result without changing coordinates', async () => {
  vi.stubGlobal('Worker', LayoutWorker);
  const prepared = prepare();
  const before = prepared.graph.export();
  const abort = new AbortController();
  const pending = settleEmergentOffThread(prepared, abort.signal);
  const worker = LayoutWorker.instances[0]!;
  abort.abort();
  worker.onmessage!({ data: [{ id: 'a', x: 100, y: 100 }, { id: 'b', x: 100, y: 100 }] });
  expect(await pending).toBe(false);
  expect(worker.terminate).toHaveBeenCalledOnce();
  expect(prepared.graph.export()).toEqual(before);
});

it.each([
  [{ id: 'a', x: 1, y: 2 }, { id: 'foreign', x: 3, y: 4 }],
  [{ id: 'a', x: 1, y: 2 }, { id: 'a', x: 3, y: 4 }],
  [{ id: 'a', x: 1, y: 2 }, { id: 'b', x: NaN, y: 4 }],
  [{ id: 'a', x: 1, y: 2 }],
])('rejects incomplete or invalid coordinates atomically (%j)', async (...positions) => {
  vi.stubGlobal('Worker', LayoutWorker);
  const prepared = prepare();
  const before = prepared.graph.export();
  const pending = settleEmergentOffThread(prepared, new AbortController().signal);
  const worker = LayoutWorker.instances[0]!;
  worker.onmessage!({ data: positions });
  await expect(pending).rejects.toThrow('invalid positions');
  expect(prepared.graph.export()).toEqual(before);
  expect(worker.terminate).toHaveBeenCalledOnce();
});

it('rejects worker failure and unavailable workers without running a main-thread fallback', async () => {
  vi.stubGlobal('Worker', LayoutWorker);
  const pending = settleEmergentOffThread(prepare(), new AbortController().signal);
  const worker = LayoutWorker.instances[0]!;
  worker.onerror!();
  await expect(pending).rejects.toThrow('worker failed');
  expect(worker.terminate).toHaveBeenCalledOnce();
  vi.stubGlobal('Worker', undefined);
  await expect(settleEmergentOffThread(prepare(), new AbortController().signal)).rejects.toThrow();
});

it('never starts a worker for measured or already cancelled fields', async () => {
  vi.stubGlobal('Worker', LayoutWorker);
  const measured = { ...prepare(), placed: true };
  await expect(settleEmergentOffThread(measured, new AbortController().signal)).rejects.toThrow('Measured');
  const abort = new AbortController();
  abort.abort();
  expect(await settleEmergentOffThread(prepare(), abort.signal)).toBe(false);
  expect(LayoutWorker.instances).toHaveLength(0);
});
