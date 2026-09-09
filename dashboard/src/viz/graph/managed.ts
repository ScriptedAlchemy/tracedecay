import type Graph from 'graphology';

/** Managed companion prefixes. These are renderer-owned nodes and edges that
 * carry glow, dendrite geometry and travelling light; topology queries filter
 * them out, while glow companions inherit their owner's focus treatment. */
export const HALO = '__halo__';
export const BLOOM = '__bloom__';
export const RING = '__ring__';
export const PULSE = '__pulse__';
export const WAY = '__way__';

export function isManaged(id: string): boolean {
  return (
    id.startsWith(HALO) ||
    id.startsWith(BLOOM) ||
    id.startsWith(RING) ||
    id.startsWith(PULSE) ||
    id.startsWith(WAY)
  );
}

/** One logical relation, rendered as a dendrite: a chain of short segments
 * tracing a quadratic curve between two real nodes. Keeping the polyline lets
 * travelling activation run along the curve rather than cutting the chord. */
export interface Strand {
  from: string;
  to: string;
  points: Array<[number, number]>;
}

/** Add or update a managed companion node in one call. */
export function upsert(
  graph: Graph,
  id: string,
  attributes: Record<string, unknown>,
): void {
  // Empty strings still enter Sigma's label collision grid: a larger halo
  // would win its body's cell and suppress the actual identity label.
  const decoration = { ...attributes, label: null };
  if (graph.hasNode(id)) graph.mergeNodeAttributes(id, decoration);
  else graph.addNode(id, decoration);
}
