import { useCallback, useRef, useState } from "react";
import { makeSequence } from "../../lib/sequence";
import type { GraphNeighborsResponse, GraphNode, GraphNodeResponse } from "./types";

export function useGraphInspection({
  loadNode,
  loadNeighbors,
  onError,
}: {
  loadNode: (id: string) => Promise<GraphNodeResponse>;
  loadNeighbors: (id: string) => Promise<GraphNeighborsResponse>;
  onError: (message: string) => void;
}) {
  const [selected, setSelected] = useState<GraphNode | null>(null);
  const [neighbors, setNeighbors] = useState<GraphNeighborsResponse | null>(null);
  const inspectSeq = useRef(makeSequence()).current;

  const inspect = useCallback(async (id: string) => {
    const ticket = inspectSeq.next();
    try {
      const [detail, nextNeighbors] = await Promise.all([
        loadNode(id),
        loadNeighbors(id),
      ]);
      if (!inspectSeq.isCurrent(ticket)) return;
      setSelected(detail.node);
      setNeighbors(nextNeighbors);
    } catch (err) {
      if (inspectSeq.isCurrent(ticket)) {
        onError(err instanceof Error ? err.message : String(err));
      }
    }
  }, [inspectSeq, loadNeighbors, loadNode, onError]);

  return {
    selected,
    neighbors,
    setSelected,
    setNeighbors,
    inspect,
  };
}
