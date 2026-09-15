import Graph from 'graphology';
import { loadForceAtlas2, settleEmergentField } from './emergentField.ts';
import type { EmergentLayoutJob } from './emergentLayout.ts';

// One bounded settle per worker. Termination cancels the synchronous engine.
self.onmessage = async (event: MessageEvent<EmergentLayoutJob>) => {
  try {
    const graph = Graph.from(event.data.graph);
    settleEmergentField({
      graph,
      nodeCount: event.data.nodeCount,
      edgeDensity: event.data.edgeDensity,
    }, await loadForceAtlas2());
    self.postMessage(graph.mapNodes((id, attributes) => ({
      id, x: attributes.x, y: attributes.y,
    })));
  } catch {
    self.postMessage({ error: 'Force layout worker failed' });
  }
};
