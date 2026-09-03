import Graph from 'graphology';
import { WAY, type Strand } from './managed.ts';
import type { FieldExtent, GraphCanvasEdge, GraphCanvasNode } from './types.ts';

/**
 * Layout preparation for the graph canvas: turn the caller's nodes and edges
 * into a graphology graph with coordinates, body sizes and dendrite geometry.
 *
 * Deliberately free of both the DOM and Sigma. Everything the arithmetic needs
 * about the medium — the canvas box it has to fit into, and which side of the
 * substrate the kind hues are lit against — arrives as a plain value, so the
 * whole of this file can be exercised without a renderer or a document.
 */

/** The camera's frame, in layout coordinates. */
export interface FieldFrame {
  x: [number, number];
  y: [number, number];
}

export interface PreparedField {
  graph: Graph;
  /** Whether every node arrived with coordinates that MEASURE something. */
  placed: boolean;
  /** The caller's own nodes, captured before the dendrite pass adds waypoints. */
  realNodes: string[];
  /** Real topology, captured before the dendrite pass rewrites the edge set.
   * Strike propagation and hover isolation answer from this map, so the
   * rendering geometry can never be mistaken for the graph's structure. */
  neighborsOf: Map<string, string[]>;
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
    graph.addNode(node.id, {
      label: node.label,
      kind: node.kind,
      degree: node.degree,
      x: placed ? node.x! : Math.cos(angle),
      y: placed ? node.y! : Math.sin(angle),
      size: Math.min((5 + 9 * Math.sqrt(degreeFraction)) * bodyScale, bodyCeiling),
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

  // Captured here, while the graph still holds nothing but the caller's own
  // topology: the emergent settle only moves coordinates, and the dendrite
  // pass that follows replaces every edge with a chain of waypoints.
  const realNodes = graph.nodes();
  const neighborsOf = new Map<string, string[]>();
  for (const node of realNodes) neighborsOf.set(node, graph.neighbors(node));

  return {
    graph,
    placed,
    realNodes,
    neighborsOf,
    nodeCount: nodes.length,
    edgeDensity,
    denseField,
    roominess,
  };
}

/**
 * A relation is connective tissue, not a ruled line. Each edge becomes a
 * quadratic curve sampled into short segments; the bow direction is hashed
 * from the endpoint pair so parallel relations separate instead of
 * overprinting, and re-renders of the same subgraph curve identically.
 * Above the segment budget the graph keeps straight edges: legibility at
 * density beats flourish.
 *
 * Runs on final coordinates, so it is the last thing layout preparation does.
 *
 * @param edgeCount the caller's own relation count, which is the budget this
 * is measured against — not the graph's edge count, which has already dropped
 * relations whose endpoints were not drawn.
 */
export function buildDendrites(graph: Graph, edgeCount: number): Strand[] {
  const segments = edgeCount <= 120 ? 7 : edgeCount <= 400 ? 4 : 0;
  const strands: Strand[] = [];
  if (segments <= 1) {
    graph.forEachEdge((edge, _attrs, from, to) => {
      graph.setEdgeAttribute(edge, 'srcReal', from);
      graph.setEdgeAttribute(edge, 'dstReal', to);
    });
    return strands;
  }
  for (const edge of [...graph.edges()]) {
    const [from, to] = graph.extremities(edge);
    if (!from || !to) continue;
    const a = graph.getNodeAttributes(from);
    const b = graph.getNodeAttributes(to);
    const ax = a['x'] as number;
    const ay = a['y'] as number;
    const bx = b['x'] as number;
    const by = b['y'] as number;
    let hash = 0;
    for (const character of `${from}\u0000${to}`) {
      hash = (hash * 31 + character.charCodeAt(0)) >>> 0;
    }
    const bow = (hash % 2 === 0 ? 1 : -1) * (0.1 + ((hash >>> 3) % 7) * 0.014);
    // Perpendicular offset of the Bézier control point, in layout units.
    const cx = (ax + bx) / 2 - (by - ay) * bow;
    const cy = (ay + by) / 2 + (bx - ax) * bow;
    const points: Array<[number, number]> = [];
    for (let step = 0; step <= segments; step += 1) {
      const t = step / segments;
      const u = 1 - t;
      points.push([
        u * u * ax + 2 * u * t * cx + t * t * bx,
        u * u * ay + 2 * u * t * cy + t * t * by,
      ]);
    }
    const strandIndex = strands.length;
    strands.push({ from, to, points });
    graph.dropEdge(edge);
    let previous = from;
    for (let step = 1; step <= segments; step += 1) {
      const isLast = step === segments;
      const point = points[step]!;
      let node = to;
      if (!isLast) {
        node = `${WAY}${strandIndex}:${step}`;
        graph.addNode(node, {
          x: point[0],
          y: point[1],
          // Geometry only: a waypoint is a joint, never a visible body.
          size: 0.01,
          color: 'rgba(0, 0, 0, 0)',
          label: '',
          zIndex: 0,
        });
      }
      graph.addEdge(previous, node, { srcReal: from, dstReal: to });
      previous = node;
    }
  }
  return strands;
}

/**
 * Frame the real network only: dendrite waypoints bow outside the node hull
 * and glow companions scale with heat, and neither may be allowed to move the
 * camera.
 */
export function nodeHullFrame(graph: Graph, realNodes: readonly string[]): FieldFrame {
  let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
  for (const node of realNodes) {
    const x = graph.getNodeAttribute(node, 'x') as number;
    const y = graph.getNodeAttribute(node, 'y') as number;
    if (x < minX) minX = x;
    if (x > maxX) maxX = x;
    if (y < minY) minY = y;
    if (y > maxY) maxY = y;
  }
  // ~15% margin so the constellation fills the viewport instead of
  // floating in an over-large frame, with enough slack left over that a
  // label overhanging its node (rendered outside the node's own radius)
  // never clips at the canvas edge.
  const padX = (maxX - minX || 1) * 0.15;
  const padY = (maxY - minY || 1) * 0.15;
  return { x: [minX - padX, maxX + padX], y: [minY - padY, maxY + padY] };
}

/** The caller's own axis, used verbatim. */
export function axisFrame(extent: FieldExtent): FieldFrame {
  return { x: extent.x, y: extent.y };
}
