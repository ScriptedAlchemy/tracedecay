import Graph from 'graphology';
import type { GraphCanvasEdge, GraphCanvasNode } from './types.ts';

/**
 * Layout preparation for the graph canvas: turn the caller's nodes and edges
 * into a graphology graph with coordinates and body sizes.
 *
 * Deliberately free of both the DOM and Sigma. Everything the arithmetic needs
 * about the medium, the canvas box it has to fit into, and which side of the
 * substrate the kind hues are lit against, arrives as a plain value, so the
 * whole of this file can be exercised without a renderer or a document.
 */

export interface PreparedField {
  graph: Graph;
  /** Whether every node arrived with coordinates that MEASURE something. */
  placed: boolean;
  /** The caller's own nodes that were drawn. */
  realNodes: string[];
  nodeCount: number;
  edgeDensity: number;
  denseField: boolean;
  roominess: number;
}

export interface PrepareFieldInput {
  nodes: readonly GraphCanvasNode[];
  edges: readonly GraphCanvasEdge[];
  /** The canvas box the field has to fit into, in device-independent pixels. */
  viewport: { width: number; height: number };
  /** Resolves a node kind to its rgb in the current medium. Injected rather
   * than imported so this module never has to reach for a canvas to parse a
   * colour: the caller already knows which side of the substrate it is on. */
  kindRgb: (kind: string) => [number, number, number];
}

/**
 * A field is "placed" only if EVERY node was measured into it. One node
 * without coordinates would otherwise be dropped at the seed circle beside
 * nodes whose position is a claim about their data, and the reader has no
 * way to tell the two apart.
 */
export function isMeasuredField(nodes: readonly GraphCanvasNode[]): boolean {
  return nodes.every((node) => Number.isFinite(node.x) && Number.isFinite(node.y));
}

export function prepareField({
  nodes,
  edges,
  viewport,
  kindRgb,
}: PrepareFieldInput): PreparedField {
  const graph = new Graph({ multi: true, type: 'directed' });
  const knownDegrees = nodes.flatMap((node) =>
    node.degree == null ? [] : [Math.max(0, node.degree)],
  );
  const maxDegree = Math.max(...knownDegrees, 1);
  const placed = isMeasuredField(nodes);
  // Deterministic circular seed (sorted order) so layouts are stable
  // across reloads of the same subgraph.
  const sorted = [...nodes].sort((a, b) => a.id.localeCompare(b.id));
  // Sigma fits the camera to the graph's extent, so an identical base radius
  // renders far larger on a sparse graph than on a dense one: forty symbols
  // read correctly on Code while seven filled the Brain canvas with discs.
  // Normalising the radius against node count keeps a node the same apparent
  // object however much of the graph is on screen -- degree still sets the
  // relative sizes within a graph, which is the only comparison that means
  // anything. The dense case is left exactly where it was.
  const density = Math.min(1, Math.max(0.45, Math.sqrt(nodes.length / 40)));
  // The second half of the same problem: node radius is in screen pixels but
  // the SPACING between nodes is not, so the identical graph that reads
  // correctly in a 1060px canvas becomes a solid mass of overlapping discs in
  // a 270px one. Measuring the canvas area each node actually gets lets a
  // narrow viewport shrink the bodies instead of fusing them. It only ever
  // reduces -- a roomy canvas is already correct.
  const perNode = (viewport.width * viewport.height) / Math.max(nodes.length, 1);
  const roominess = Math.min(1, Math.sqrt(perNode / 8000));
  // A dense connected component (many edges per node) settles into a
  // tighter FA2 packing than a sparse one of the same node count even
  // after the repulsion tuning below, so its bodies need to shrink
  // further or they fuse into an undifferentiated mass regardless of how
  // roomy the canvas is. Average degree is a cheap, whole-graph proxy for
  // that packing pressure -- exact per-component density is not worth the
  // extra pass here.
  const edgeDensity = nodes.length > 0 ? (2 * edges.length) / nodes.length : 0;
  const densityShrink = Math.min(1, 1.8 / (1 + edgeDensity * 0.55));
  const bodyScale = Math.max(0.32, density * roominess * densityShrink);
  // Dense Code fields need a hard screen-space ceiling in addition to the
  // relative scaling above. Relative scaling preserves rank but cannot stop
  // one high-degree hub from becoming the bright disc every neighbour
  // disappears behind. The ceiling relaxes with actual room, but never
  // beyond 7.5 px in the dense tier.
  const denseField = nodes.length >= 32 || edgeDensity >= 2;
  const bodyCeiling = denseField ? 7.5 * Math.max(0.62, roominess) : Infinity;
  sorted.forEach((node, index) => {
    const angle = (index / sorted.length) * Math.PI * 2;
    const [kr, kg, kb] = kindRgb(node.kind);
    const degreeFraction =
      node.degree == null ? 0 : Math.max(0, node.degree) / maxDegree;
    // A repository is a categorical junction, not a larger holding. Its
    // checkout count still determines edges and labels, but never body mass.
    const baseSize =
      node.kind === 'repository' ? 6 : 5 + 9 * Math.sqrt(degreeFraction);
    graph.addNode(node.id, {
      label: node.label,
      kind: node.kind,
      degree: node.degree,
      x: placed ? node.x! : Math.cos(angle),
      y: placed ? node.y! : Math.sin(angle),
      size: Math.min(baseSize * bodyScale, bodyCeiling),
      isHub: node.degree != null && node.degree >= maxDegree * 0.75,
      // A graph with no vitality measurement rests mid-scale, so an absent
      // signal never masquerades as a dead network.
      vitality:
        node.vitality == null ? 0.6 : Math.max(0, Math.min(1, node.vitality)),
      kindRgb: [kr, kg, kb] as [number, number, number],
    });
  });
  for (const edge of edges) {
    if (graph.hasNode(edge.source) && graph.hasNode(edge.target)) {
      graph.addEdge(edge.source, edge.target, { kind: edge.kind });
    }
  }

  return {
    graph,
    placed,
    realNodes: graph.nodes(),
    nodeCount: nodes.length,
    edgeDensity,
    denseField,
    roominess,
  };
}
