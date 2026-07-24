import type { GraphCanvasEdge } from './GraphCanvas.tsx';

/**
 * Undirected one-hop adjacency over the SAME edge list the canvas draws.
 *
 * Travelling activation is only honest if it travels along relations that
 * actually exist. Building the map from the rendered edge set — rather than
 * re-deriving "who is next to whom" from whatever data happened to shape the
 * graph — means a strike can never light a neighbour the viewer cannot also
 * see a line to. If an edge is not on screen, it cannot conduct.
 *
 * One hop only, deliberately. Each additional hop is a further claim about
 * what happened, and the claim gets weaker with distance: on the Brain graph
 * one hop from a checkout reaches its repository (true — activity in a
 * checkout is activity in that repository), while two hops would reach its
 * sibling checkouts and assert something that did not happen at all.
 */
export function buildAdjacency(
  edges: readonly GraphCanvasEdge[],
): ReadonlyMap<string, readonly string[]> {
  const adjacency = new Map<string, string[]>();
  const link = (from: string, to: string) => {
    const existing = adjacency.get(from);
    if (existing) {
      if (!existing.includes(to)) existing.push(to);
    } else {
      adjacency.set(from, [to]);
    }
  };
  for (const edge of edges) {
    if (edge.source === edge.target) continue;
    link(edge.source, edge.target);
    link(edge.target, edge.source);
  }
  return adjacency;
}

/** The neighbours of `id`, or an empty list when the node has none. Returning
 * an empty list (never `undefined`) keeps a caller from having to decide what
 * "no adjacency recorded" means: an isolated node simply propagates nowhere. */
export function neighborsOf(
  adjacency: ReadonlyMap<string, readonly string[]>,
  id: string,
): readonly string[] {
  return adjacency.get(id) ?? EMPTY;
}

const EMPTY: readonly string[] = [];
