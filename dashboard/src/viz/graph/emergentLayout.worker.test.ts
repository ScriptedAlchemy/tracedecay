import { afterEach, expect, it, vi } from 'vitest';
import { prepareField } from './layout.ts';
import { loadForceAtlas2, settleEmergentField } from './emergentField.ts';
import type { EmergentLayoutJob } from './emergentLayout.ts';

afterEach(() => vi.unstubAllGlobals());

it('returns the same exact positions as the existing bounded synchronous settle', async () => {
  const prepared = prepareField({
    nodes: Array.from({ length: 8 }, (_, id) => ({ id: String(id), label: String(id), kind: 'function' })),
    edges: Array.from({ length: 6 }, (_, id) => ({ source: String(id), target: String(id + 1) })),
    viewport: { width: 640, height: 320 },
    kindRgb: () => [100, 100, 100],
  });
  const job: EmergentLayoutJob = {
    graph: prepared.graph.export(), nodeCount: prepared.nodeCount, edgeDensity: prepared.edgeDensity,
  };
  const worker = {
    onmessage: null as ((event: { data: EmergentLayoutJob }) => Promise<void>) | null,
    postMessage: vi.fn(),
  };
  vi.stubGlobal('self', worker);
  await import('./emergentLayout.worker.ts');
  await worker.onmessage!({ data: job });
  settleEmergentField(prepared, await loadForceAtlas2());
  expect(worker.postMessage).toHaveBeenCalledExactlyOnceWith(
    prepared.graph.mapNodes((id, attributes) => ({ id, x: attributes.x, y: attributes.y })),
  );
});
