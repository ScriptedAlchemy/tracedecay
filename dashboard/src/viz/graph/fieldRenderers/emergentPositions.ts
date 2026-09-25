import { useEffect, useState } from 'react';
import { prepareField } from '../layout.ts';
import { settleEmergentOffThread } from '../emergentLayout.ts';
import type { GraphCanvasEdge, GraphCanvasNode } from '../types.ts';

export type EmergentPositions =
  | { state: 'pending' }
  | { state: 'failed'; reason: string }
  | { state: 'ready'; positions: ReadonlyMap<string, [number, number]> };

/**
 * Force-settled coordinates for a returned graph, computed by the bounded
 * emergent-layout worker. `null` input skips layout for renderers that do not
 * place symbols spatially.
 */
export function useEmergentPositions(
  nodes: readonly GraphCanvasNode[] | null,
  edges: readonly GraphCanvasEdge[],
): EmergentPositions {
  const [result, setResult] = useState<EmergentPositions>({ state: 'pending' });
  useEffect(() => {
    if (!nodes) return;
    setResult({ state: 'pending' });
    const abort = new AbortController();
    const prepared = prepareField({
      nodes,
      edges,
      viewport: { width: 1200, height: 800 },
      kindRgb: () => [0, 0, 0],
    });
    settleEmergentOffThread(prepared, abort.signal).then(
      (done) => {
        if (!done || abort.signal.aborted) return;
        const positions = new Map<string, [number, number]>();
        for (const id of prepared.realNodes) {
          positions.set(id, [
            prepared.graph.getNodeAttribute(id, 'x') as number,
            prepared.graph.getNodeAttribute(id, 'y') as number,
          ]);
        }
        setResult({ state: 'ready', positions });
      },
      (error: unknown) => {
        if (!abort.signal.aborted) {
          setResult({ state: 'failed', reason: error instanceof Error ? error.message : 'layout failed' });
        }
      },
    );
    return () => abort.abort();
  }, [nodes, edges]);
  return result;
}
