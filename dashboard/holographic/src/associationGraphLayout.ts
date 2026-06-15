import { useEffect, useMemo, useRef, useState } from "react";
import {
  forceSimulation,
  forceManyBody,
  forceLink,
  forceCenter,
  forceCollide,
  forceX,
  forceY,
  type Simulation,
} from "d3-force";
import type { HolographicGraphNode } from "./types";
import type {
  SimLink,
  SimNode,
  ViewBox,
  WeightedGraphEdge,
} from "./associationGraphTypes";
import {
  GRAPH_HEIGHT,
  GRAPH_WIDTH,
  SETTLE_ALPHA_MIN,
  SETTLE_MAX_MS,
  SETTLE_TICK_BUDGET_MS,
  settleTicks,
} from "./associationGraphTypes";
import { radiusOf } from "./associationGraphUtils";

export interface SimGraph {
  simNodes: SimNode[];
  simLinks: SimLink[];
}

/**
 * Deterministic phyllotaxis seed: the first paint shows a pleasing spread
 * (instead of every node stacked at the center) and the layout settles
 * identically across reloads.
 */
function seedPositions(simNodes: SimNode[]): void {
  const golden = Math.PI * (3 - Math.sqrt(5));
  const cx = GRAPH_WIDTH / 2;
  const cy = GRAPH_HEIGHT / 2;
  const spread =
    Math.max(GRAPH_WIDTH, GRAPH_HEIGHT) / (2 * Math.sqrt(simNodes.length + 1));
  simNodes.forEach((node, i) => {
    const radius = spread * Math.sqrt(i + 0.5);
    const angle = i * golden;
    node.x = cx + radius * Math.cos(angle);
    node.y = cy + radius * Math.sin(angle);
    node.vx = 0;
    node.vy = 0;
  });
}

export function buildSimGraph(
  nodes: HolographicGraphNode[],
  edges: WeightedGraphEdge[],
  degreeMap: Map<string, number>,
): SimGraph {
  const present = new Set(nodes.map((node) => node.id));
  const simNodes: SimNode[] = nodes.map((node) => ({
    id: node.id,
    kind: node.kind,
    label: node.label,
    degree: degreeMap.get(node.id) ?? 0,
    source: node,
  }));
  seedPositions(simNodes);
  const simLinks: SimLink[] = edges
    .filter((edge) => present.has(edge.source) && present.has(edge.target))
    .map((edge) => ({
      source: edge.source,
      target: edge.target,
      kind: edge.kind,
      weight: typeof edge.weight === "number" ? edge.weight : undefined,
    }));
  return { simNodes, simLinks };
}

function createSimulation(graph: SimGraph): Simulation<SimNode, SimLink> {
  const target = settleTicks(graph.simNodes.length);
  const alphaDecay = 1 - Math.pow(SETTLE_ALPHA_MIN, 1 / target);
  return forceSimulation<SimNode>(graph.simNodes)
    .force(
      "link",
      forceLink<SimNode, SimLink>(graph.simLinks)
        .id((node) => node.id)
        .distance(70)
        .strength(0.4),
    )
    .force("charge", forceManyBody<SimNode>().strength(-110))
    .force("center", forceCenter(GRAPH_WIDTH / 2, GRAPH_HEIGHT / 2))
    .force("x", forceX<SimNode>(GRAPH_WIDTH / 2).strength(0.05))
    .force("y", forceY<SimNode>(GRAPH_HEIGHT / 2).strength(0.12))
    .force(
      "collide",
      forceCollide<SimNode>().radius((node) => radiusOf(node.kind, node.degree) + 6),
    )
    .alpha(1)
    .alphaMin(SETTLE_ALPHA_MIN)
    .alphaDecay(alphaDecay)
    .stop();
}

/** Padded view box (matched to the canvas aspect ratio) around a node subset. */
export function computeBounds(nodes: SimNode[], extraPad = 28): ViewBox {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const node of nodes) {
    const r = radiusOf(node.kind, node.degree) + extraPad;
    const x = node.x ?? GRAPH_WIDTH / 2;
    const y = node.y ?? GRAPH_HEIGHT / 2;
    minX = Math.min(minX, x - r);
    minY = Math.min(minY, y - r);
    maxX = Math.max(maxX, x + r);
    maxY = Math.max(maxY, y + r);
  }
  if (!Number.isFinite(minX)) {
    minX = 0;
    minY = 0;
    maxX = GRAPH_WIDTH;
    maxY = GRAPH_HEIGHT;
  }
  let bx = minX;
  let by = minY;
  let bw = Math.max(maxX - minX, 1);
  let bh = Math.max(maxY - minY, 1);
  const targetAR = GRAPH_WIDTH / GRAPH_HEIGHT;
  if (bw / bh < targetAR) {
    const newW = bh * targetAR;
    bx -= (newW - bw) / 2;
    bw = newW;
  } else {
    const newH = bw / targetAR;
    by -= (newH - bh) / 2;
    bh = newH;
  }
  return { x: bx, y: by, w: bw, h: bh };
}

export interface LayoutState {
  simNodes: SimNode[];
  simLinks: SimLink[];
  /** Bumps every settle frame (and once on completion) to drive re-render. */
  frame: number;
  /** 0..1 while the layout is settling; `null` once it has settled. */
  progress: number | null;
}

/**
 * Owns a live d3-force simulation that settles incrementally inside
 * `requestAnimationFrame` instead of a single blocking tick loop. Nodes paint
 * immediately at their seeded positions and animate into place; the main thread
 * is never blocked for more than one frame's tick budget. The simulation holds
 * the *entire* graph — degree/kind filtering happens at render time — so the
 * degree slider re-filters with zero relayout.
 */
export function useGraphLayout(
  nodes: HolographicGraphNode[],
  edges: WeightedGraphEdge[],
  degreeMap: Map<string, number>,
): LayoutState {
  const graph = useMemo(
    () => buildSimGraph(nodes, edges, degreeMap),
    [nodes, edges, degreeMap],
  );
  const [frame, setFrame] = useState(0);
  const [progress, setProgress] = useState<number | null>(
    graph.simNodes.length ? 0 : null,
  );
  const rafRef = useRef<number | null>(null);

  useEffect(() => {
    if (graph.simNodes.length === 0) {
      setProgress(null);
      return;
    }
    const sim = createSimulation(graph);
    const target = settleTicks(graph.simNodes.length);
    let ticks = 0;
    const startedAt = performance.now();
    setProgress(0);
    setFrame((f) => f + 1);

    const step = () => {
      const frameStart = performance.now();
      let settled = false;
      do {
        sim.tick();
        ticks += 1;
        if (sim.alpha() < SETTLE_ALPHA_MIN) {
          settled = true;
          break;
        }
      } while (performance.now() - frameStart < SETTLE_TICK_BUDGET_MS);

      if (settled || performance.now() - startedAt > SETTLE_MAX_MS) {
        sim.stop();
        rafRef.current = null;
        setProgress(null);
        setFrame((f) => f + 1);
        return;
      }
      setProgress(Math.min(0.99, ticks / target));
      setFrame((f) => f + 1);
      rafRef.current = requestAnimationFrame(step);
    };

    rafRef.current = requestAnimationFrame(step);
    return () => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
      sim.stop();
    };
  }, [graph]);

  return { simNodes: graph.simNodes, simLinks: graph.simLinks, frame, progress };
}
