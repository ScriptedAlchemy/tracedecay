import type { SimulationLinkDatum, SimulationNodeDatum } from "d3-force";
import type { HolographicGraphEdge, HolographicGraphNode } from "./types";

export interface SimNode extends SimulationNodeDatum {
  id: string;
  kind: string;
  label: string;
  degree: number;
  source: HolographicGraphNode;
}

export interface SimLink extends SimulationLinkDatum<SimNode> {
  kind: string;
  /**
   * Co-occurrence weight. Populated from the payload when the backend supplies
   * one (`HolographicGraphEdge.weight`); otherwise the renderer falls back to a
   * structural proxy (endpoint degree) so thickness still encodes a real signal.
   */
  weight?: number;
}

/** Edge payload, widened with the optional weight the wire contract may carry. */
export type WeightedGraphEdge = HolographicGraphEdge & { weight?: number };

export interface Neighbor {
  id: string;
  kind: string;
  label: string;
  edgeKind: string;
  content?: string;
  entityType?: string;
}

export interface ViewBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

export const KIND_COLORS: Record<string, string> = {
  fact: "#818cf8",
  entity: "#34d399",
  category: "#fbbf24",
  bank: "#f472b6",
};
export const FALLBACK_COLOR = "#94a3b8";
export const KIND_ORDER = ["fact", "entity", "category", "bank"];

/** `bold` matches the heavier stroke "bundles" edges get in the graph below. */
export const EDGE_LEGEND: { kind: string; relation: string; bold?: boolean }[] = [
  { kind: "contains", relation: "category → fact" },
  { kind: "mentions", relation: "fact → entity" },
  { kind: "bundles", relation: "bank → fact", bold: true },
  { kind: "bank", relation: "category → bank" },
];

export const GRAPH_WIDTH = 1180;
export const GRAPH_HEIGHT = 860;

/**
 * Incremental-settle tuning. The simulation no longer runs to completion in a
 * single blocking loop; instead it ticks inside `requestAnimationFrame` until
 * `alpha` decays past `SETTLE_ALPHA_MIN`. `SETTLE_TICK_BUDGET_MS` caps how long
 * we tick per frame so we never block past one frame, and `SETTLE_MAX_MS` is a
 * hard wall-clock safety stop. `settleTicks()` derives a node-count-adaptive
 * tick target (the audit's "~50 + nodes/10" guidance, clamped) that we convert
 * into an `alphaDecay`.
 */
export const SETTLE_ALPHA_MIN = 0.01;
export const SETTLE_TICK_BUDGET_MS = 8;
export const SETTLE_MAX_MS = 4500;

export function settleTicks(nodeCount: number): number {
  return Math.max(110, Math.min(240, Math.round(70 + nodeCount / 6)));
}
