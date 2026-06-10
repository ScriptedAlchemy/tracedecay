/**
 * Minimal incremental force simulation (hand-rolled; no d3 dependency).
 *
 * Designed for a few hundred visible nodes: link springs, pairwise repulsion
 * with a cheap spatial grid, light centering, and collision separation.
 * Runs one `tick()` per animation frame and cools via alpha decay, so newly
 * expanded nodes settle smoothly instead of re-laying-out the whole graph.
 */

import type { GraphEdge, GraphNode } from "./types";

export interface SimNode extends GraphNode {
  x: number;
  y: number;
  vx: number;
  vy: number;
  /** Fixed position while the user drags the node. */
  fx: number | null;
  fy: number | null;
  radius: number;
  /** Edges currently visible in the canvas (vs `degree` = full-graph count). */
  visibleDegree: number;
}

export interface SimEdge extends GraphEdge {
  sourceNode: SimNode;
  targetNode: SimNode;
}

export interface Simulation {
  nodes: SimNode[];
  edges: SimEdge[];
  /** Cooling factor in [0, 1]; the sim is asleep below ALPHA_MIN. */
  alpha: number;
  tick(): void;
  /** Reheats the simulation (e.g. after expansion or drag release). */
  reheat(target?: number): void;
  isActive(): boolean;
}

const ALPHA_MIN = 0.004;
const ALPHA_DECAY = 0.025;
const VELOCITY_DECAY = 0.65;
const LINK_DISTANCE = 110;
const LINK_STRENGTH = 0.32;
const REPULSION = 2600;
const CENTER_STRENGTH = 0.012;
const GRID_CELL = 180;
/** Per-tick speed cap; prevents dense clusters from exploding off-camera. */
const MAX_SPEED = 26;

export function nodeRadius(degree: number): number {
  return 7 + Math.min(13, Math.sqrt(Math.max(0, degree)) * 2.2);
}

/**
 * Builds (or rebuilds) a simulation, preserving positions of nodes that
 * already exist in `previous` so progressive expansion feels stable.
 */
export function createSimulation(
  nodes: GraphNode[],
  edges: GraphEdge[],
  previous?: Simulation | null,
  anchorId?: string | null,
): Simulation {
  const prevById = new Map<string, SimNode>();
  if (previous) {
    for (const node of previous.nodes) prevById.set(node.id, node);
  }
  const anchor = anchorId ? prevById.get(anchorId) : undefined;
  const cx = anchor ? anchor.x : 0;
  const cy = anchor ? anchor.y : 0;

  const visibleDegree = new Map<string, number>();
  for (const edge of edges) {
    visibleDegree.set(edge.source, (visibleDegree.get(edge.source) || 0) + 1);
    visibleDegree.set(edge.target, (visibleDegree.get(edge.target) || 0) + 1);
  }

  const simNodes: SimNode[] = nodes.map((node, index) => {
    const prev = prevById.get(node.id);
    const angle = (index / Math.max(1, nodes.length)) * Math.PI * 2;
    const jitter = 60 + (index % 7) * 26;
    return {
      ...node,
      x: prev ? prev.x : cx + Math.cos(angle) * jitter,
      y: prev ? prev.y : cy + Math.sin(angle) * jitter,
      vx: prev ? prev.vx : 0,
      vy: prev ? prev.vy : 0,
      fx: prev ? prev.fx : null,
      fy: prev ? prev.fy : null,
      radius: nodeRadius(node.degree || 0),
      visibleDegree: visibleDegree.get(node.id) || 0,
    };
  });

  const byId = new Map(simNodes.map((node) => [node.id, node]));
  const simEdges: SimEdge[] = [];
  for (const edge of edges) {
    const sourceNode = byId.get(edge.source);
    const targetNode = byId.get(edge.target);
    if (sourceNode && targetNode) simEdges.push({ ...edge, sourceNode, targetNode });
  }

  let alpha = 1;

  function applyRepulsion() {
    // Spatial grid keeps repulsion ~O(n * neighbors) instead of O(n²).
    const grid = new Map<string, SimNode[]>();
    for (const node of simNodes) {
      const key = `${Math.floor(node.x / GRID_CELL)}:${Math.floor(node.y / GRID_CELL)}`;
      const cell = grid.get(key);
      if (cell) cell.push(node);
      else grid.set(key, [node]);
    }
    for (const node of simNodes) {
      const gx = Math.floor(node.x / GRID_CELL);
      const gy = Math.floor(node.y / GRID_CELL);
      for (let dx = -1; dx <= 1; dx++) {
        for (let dy = -1; dy <= 1; dy++) {
          const cell = grid.get(`${gx + dx}:${gy + dy}`);
          if (!cell) continue;
          for (const other of cell) {
            if (other === node) continue;
            let ddx = node.x - other.x;
            let ddy = node.y - other.y;
            let distSq = ddx * ddx + ddy * ddy;
            if (distSq < 1) {
              ddx = (Math.random() - 0.5) * 2;
              ddy = (Math.random() - 0.5) * 2;
              distSq = ddx * ddx + ddy * ddy;
            }
            const force = (REPULSION * alpha) / distSq;
            const dist = Math.sqrt(distSq);
            node.vx += (ddx / dist) * force;
            node.vy += (ddy / dist) * force;
          }
        }
      }
    }
  }

  function applyLinks() {
    for (const edge of simEdges) {
      const { sourceNode: a, targetNode: b } = edge;
      let dx = b.x - a.x;
      let dy = b.y - a.y;
      let dist = Math.sqrt(dx * dx + dy * dy);
      if (dist < 1) {
        dx = 1;
        dy = 0;
        dist = 1;
      }
      const stretch = ((dist - LINK_DISTANCE) / dist) * LINK_STRENGTH * alpha;
      const fx = dx * stretch;
      const fy = dy * stretch;
      a.vx += fx;
      a.vy += fy;
      b.vx -= fx;
      b.vy -= fy;
    }
  }

  function applyCollision() {
    for (const edge of simEdges) {
      const { sourceNode: a, targetNode: b } = edge;
      const minDist = a.radius + b.radius + 6;
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const dist = Math.sqrt(dx * dx + dy * dy) || 1;
      if (dist < minDist) {
        const push = ((minDist - dist) / dist) * 0.5;
        a.x -= dx * push;
        a.y -= dy * push;
        b.x += dx * push;
        b.y += dy * push;
      }
    }
  }

  function tick() {
    if (alpha < ALPHA_MIN) return;
    applyRepulsion();
    applyLinks();
    for (const node of simNodes) {
      node.vx += -node.x * CENTER_STRENGTH * alpha;
      node.vy += -node.y * CENTER_STRENGTH * alpha;
      node.vx *= VELOCITY_DECAY;
      node.vy *= VELOCITY_DECAY;
      const speed = Math.sqrt(node.vx * node.vx + node.vy * node.vy);
      if (speed > MAX_SPEED) {
        node.vx = (node.vx / speed) * MAX_SPEED;
        node.vy = (node.vy / speed) * MAX_SPEED;
      }
      if (node.fx !== null && node.fy !== null) {
        node.x = node.fx;
        node.y = node.fy;
        node.vx = 0;
        node.vy = 0;
      } else {
        node.x += node.vx;
        node.y += node.vy;
      }
    }
    applyCollision();
    alpha *= 1 - ALPHA_DECAY;
  }

  return {
    nodes: simNodes,
    edges: simEdges,
    get alpha() {
      return alpha;
    },
    set alpha(value: number) {
      alpha = value;
    },
    tick,
    reheat(target = 0.6) {
      alpha = Math.max(alpha, target);
    },
    isActive() {
      return alpha >= ALPHA_MIN;
    },
  };
}
