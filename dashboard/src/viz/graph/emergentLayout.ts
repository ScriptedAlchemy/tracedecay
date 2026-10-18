import type Graph from 'graphology';
import type { PreparedField } from './layout.ts';

/** Renderer-local Graphology transfer; no dashboard data contract is copied. */
export interface EmergentLayoutJob {
  graph: ReturnType<Graph['export']>;
  nodeCount: number;
  edgeDensity: number;
}

/** Install a complete layout atomically. Cancelled jobs never touch the graph. */
export function settleEmergentOffThread(
  prepared: PreparedField,
  signal: AbortSignal,
): Promise<boolean> {
  if (signal.aborted) return Promise.resolve(false);
  if (prepared.placed) return Promise.reject(new Error('Measured fields cannot run force layout'));
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./emergentLayout.worker.ts', import.meta.url), {
      type: 'module',
    });
    let finished = false;
    const finish = (error?: Error): void => {
      if (finished) return;
      finished = true;
      worker.terminate();
      signal.removeEventListener('abort', abort);
      if (error) reject(error);
      else resolve(!signal.aborted);
    };
    const abort = (): void => finish();
    signal.addEventListener('abort', abort, { once: true });
    worker.onerror = () => finish(new Error('Force layout worker failed'));
    worker.onmessageerror = () => finish(new Error('Force layout result could not be read'));
    worker.onmessage = (event: MessageEvent<unknown>) => {
      if (finished || signal.aborted) return;
      const positions = event.data;
      const ids = new Set(prepared.realNodes);
      if (!Array.isArray(positions) || positions.length !== ids.size ||
        positions.some((point) => !point || typeof point.id !== 'string' ||
          !ids.delete(point.id) || !Number.isFinite(point.x) || !Number.isFinite(point.y))) {
        finish(new Error('Force layout returned invalid positions'));
        return;
      }
      for (const { id, x, y } of positions) {
        prepared.graph.mergeNodeAttributes(id, { x, y });
      }
      finish();
    };
    try {
      worker.postMessage({
        graph: prepared.graph.export(),
        nodeCount: prepared.nodeCount,
        edgeDensity: prepared.edgeDensity,
      } satisfies EmergentLayoutJob);
    } catch (error) {
      finish(error instanceof Error ? error : new Error('Force layout could not start'));
    }
  });
}
