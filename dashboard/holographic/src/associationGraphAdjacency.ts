import type { HolographicGraphEdge, HolographicGraphNode } from "./types";
import type { Neighbor } from "./associationGraphTypes";

export interface AdjacencyData {
  degreeMap: Map<string, number>;
  maxEntityDegree: number;
  adjacency: Map<string, Neighbor[]>;
}

export function buildAdjacency(
  nodes: HolographicGraphNode[],
  edges: HolographicGraphEdge[],
): AdjacencyData {
  const nodeById = new Map(nodes.map((node) => [node.id, node]));
  const map = new Map<string, number>();
  const adj = new Map<string, Neighbor[]>();
  const link = (a: string, b: string, edgeKind: string) => {
    const other = nodeById.get(b);
    if (!other) return;
    if (!adj.has(a)) adj.set(a, []);
    adj.get(a)!.push({
      id: b,
      kind: other.kind,
      label: other.label,
      edgeKind,
      content: other.content,
      entityType: other.entity_type,
    });
  };
  for (const edge of edges) {
    map.set(edge.source, (map.get(edge.source) ?? 0) + 1);
    map.set(edge.target, (map.get(edge.target) ?? 0) + 1);
    link(edge.source, edge.target, edge.kind);
    link(edge.target, edge.source, edge.kind);
  }
  let maxEntity = 1;
  for (const node of nodes) {
    if (node.kind === "entity") {
      maxEntity = Math.max(maxEntity, map.get(node.id) ?? 0);
    }
  }
  return { degreeMap: map, maxEntityDegree: maxEntity, adjacency: adj };
}
