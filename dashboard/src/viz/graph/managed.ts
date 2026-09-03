import type Graph from 'graphology';

/** Managed companion prefixes. These are renderer-owned nodes and edges that
 * carry glow, dendrite geometry and travelling light; reducers pass them
 * through untouched and every topology query filters them out. */
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
  if (graph.hasNode(id)) graph.mergeNodeAttributes(id, attributes);
  else graph.addNode(id, attributes);
}
