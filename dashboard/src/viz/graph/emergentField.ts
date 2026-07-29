import type Graph from 'graphology';
import { nodeHullFrame, type FieldFrame, type PreparedField } from './layout.ts';

/**
 * The emergent path: a field whose SHAPE is the finding, settled by
 * ForceAtlas2 and then composed so its components read as one constellation.
 *
 * Everything here is pure: the engine arrives as an argument, so the settle
 * and the composition can be exercised without a renderer, a document, or the
 * real library.
 */

/** The synchronous ForceAtlas2 entry point, exactly as the package types it.
 * Written as a type-only `import()` so naming it costs nothing at runtime. */
export type ForceAtlas2Module = typeof import('graphology-layout-forceatlas2').default;

/**
 * Settle the field, then compose it. The two are one step: the composure pass
 * below only makes sense on FA2's output, and the camera frame is computed
 * afterwards so it always frames the composed result.
 */
export function settleEmergentField(
  prepared: PreparedField,
  forceAtlas2: ForceAtlas2Module,
): void {
  const { graph, nodeCount, edgeDensity } = prepared;
  const fa2 = forceAtlas2.inferSettings(graph);
  forceAtlas2.assign(graph, {
    // A dense component needs more settling time to actually reach the
    // wider spread the settings below ask for; capped so a huge sparse
    // graph doesn't pay for iterations it does not need.
    iterations: Math.round(Math.min(400, 200 + edgeDensity * 40)),
    settings: {
      ...fa2,
      // Small graphs over-spread with inferred gravity; pull clusters in so
      // the tissue reads dense, not lost in the void. A DENSE graph needs
      // the opposite correction on top of that: less pull-to-centre and
      // more repulsion, or its own edges collapse it into a fused hairball
      // no matter how few nodes it has -- node count alone (the previous
      // basis for both knobs) says nothing about how much mutual pull a
      // component's edges apply.
      gravity:
        ((fa2.gravity ?? 1) *
          (nodeCount < 60 ? Math.max(8, 200 / nodeCount) : 2)) /
        (1 + edgeDensity * 0.5),
      scalingRatio: 4 + edgeDensity * 3,
    },
  });
  composeConstellation(graph);
}

/**
 * Constellation composure: FA2 lets disconnected components drift apart
 * under nothing but mutual repulsion and per-node gravity, which pulls
 * each node toward the origin independently of which component it is
 * in -- it does not pull components toward EACH OTHER. On a small graph
 * (a handful of repos, each an isolated hub-and-spokes component) that
 * reliably produces two or three tight clumps separated by a gap several
 * times their own size: exactly the "vast dead field" the camera then
 * faithfully frames, because there is nothing wrong with the frame, only
 * with how far apart the content drifted before it was fit. A single
 * connected component is a no-op here.
 */
export function composeConstellation(graph: Graph): void {
  const componentOf = new Map<string, number>();
  const membersOf: string[][] = [];
  for (const start of graph.nodes()) {
    if (componentOf.has(start)) continue;
    const index = membersOf.length;
    const members: string[] = [];
    membersOf.push(members);
    const queue = [start];
    componentOf.set(start, index);
    while (queue.length) {
      const current = queue.pop()!;
      members.push(current);
      for (const neighbor of graph.neighbors(current)) {
        if (!componentOf.has(neighbor)) {
          componentOf.set(neighbor, index);
          queue.push(neighbor);
        }
      }
    }
  }
  const componentCount = membersOf.length;
  if (componentCount <= 1) return;
  const centroids = membersOf.map((members) => {
    let x = 0, y = 0;
    for (const node of members) {
      x += graph.getNodeAttribute(node, 'x') as number;
      y += graph.getNodeAttribute(node, 'y') as number;
    }
    return { x: x / members.length, y: y / members.length };
  });
  const extents = membersOf.map((members, index) => {
    const c = centroids[index]!;
    let extent = 0.15; // floor: an orphan has zero spread of its own.
    for (const node of members) {
      const ex = Math.abs((graph.getNodeAttribute(node, 'x') as number) - c.x);
      const ey = Math.abs((graph.getNodeAttribute(node, 'y') as number) - c.y);
      extent = Math.max(extent, ex, ey);
    }
    return extent;
  });
  let dominant = 0;
  for (let index = 1; index < componentCount; index += 1) {
    if (membersOf[index]!.length > membersOf[dominant]!.length) dominant = index;
  }
  const totalRealNodes = membersOf.reduce((sum, m) => sum + m.length, 0);
  const secondLargest = Math.max(
    0,
    ...membersOf.filter((_, index) => index !== dominant).map((m) => m.length),
  );
  // Two genuinely different shapes share this code path. Brain-style
  // graphs are a constellation of many roughly EQUAL-sized components
  // (one hub-and-spokes cluster per repo) -- there is no "main" body,
  // so the original flat ring (every component equally spaced, sized
  // from the single largest extent) is exactly right and reads as a
  // deliberate necklace. Code-style graphs are the opposite: ONE
  // component holds most of the real nodes and everything else is a
  // stray orphan or two. Forcing the orphan case through the flat-ring
  // formula was the actual defect -- a near-zero-extent orphan was
  // pushed out to the SAME radius the dominant cluster needed, which
  // inflates the camera's fitted bbox far more than it helps legibility
  // ("orphans set the extent," shrinking the real structure to a
  // sliver of the canvas). Detect that shape (one component holding a
  // clear majority, with no other component anywhere close to its
  // size) and anchor everything else to it in a tight margin band
  // instead; otherwise fall back to the flat ring both shapes used to
  // share.
  const hasDominantComponent =
    membersOf[dominant]!.length >= totalRealNodes * 0.5 &&
    membersOf[dominant]!.length >= secondLargest * 3;
  if (hasDominantComponent) {
    const dominantExtent = extents[dominant]!;
    const domCentroid = centroids[dominant]!;
    const others = Array.from({ length: componentCount }, (_, index) => index).filter(
      (index) => index !== dominant,
    );
    others.forEach((component, spokeIndex) => {
      const c = centroids[component]!;
      const spoke = Math.min(
        dominantExtent + extents[component]! * 1.2 + 0.3,
        dominantExtent * 2,
      );
      const angle = (spokeIndex / others.length) * Math.PI * 2;
      const dx = -domCentroid.x + Math.cos(angle) * spoke - c.x;
      const dy = -domCentroid.y + Math.sin(angle) * spoke - c.y;
      for (const node of membersOf[component]!) {
        graph.updateNodeAttribute(node, 'x', (x) => (x as number) + dx);
        graph.updateNodeAttribute(node, 'y', (y) => (y as number) + dy);
      }
    });
    // Re-centre the dominant component itself onto the origin, so the
    // spokes above (measured from its pre-move centroid) and the frame
    // both anchor to where the graph's real mass actually is.
    for (const node of membersOf[dominant]!) {
      graph.updateNodeAttribute(node, 'x', (x) => (x as number) - domCentroid.x);
      graph.updateNodeAttribute(node, 'y', (y) => (y as number) - domCentroid.y);
    }
    return;
  }
  // The ring only needs to be as large as geometry actually
  // requires: adjacent components sit `2π/N` apart in angle, so the
  // chord between two neighbouring centroids is `2·ring·sin(π/N)`,
  // and that chord must clear twice each component's own extent for
  // their (roughly circular) footprints not to overlap. Solving for
  // ring gives exactly the radius non-overlap needs.
  const maxExtent = Math.max(...extents);
  const ring = (maxExtent / Math.sin(Math.PI / componentCount)) * 1.15;
  membersOf.forEach((members, component) => {
    const c = centroids[component]!;
    const angle = (component / componentCount) * Math.PI * 2;
    const dx = Math.cos(angle) * ring - c.x;
    const dy = Math.sin(angle) * ring - c.y;
    for (const node of members) {
      graph.updateNodeAttribute(node, 'x', (x) => (x as number) + dx);
      graph.updateNodeAttribute(node, 'y', (y) => (y as number) + dy);
    }
  });
}

/** The frozen bbox is computed after the settle, so the camera always frames
 * the composed result, not the raw FA2 scatter. */
export function frameEmergentField(prepared: PreparedField): FieldFrame {
  return nodeHullFrame(prepared.graph, prepared.realNodes);
}
