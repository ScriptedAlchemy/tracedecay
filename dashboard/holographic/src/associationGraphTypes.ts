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

export interface EdgeStyle {
  /** Token-driven stroke that re-resolves per theme. */
  color: string;
  /** SVG dash pattern — a redundant, colorblind-safe channel on top of hue. */
  dash?: string;
}

/**
 * Per-kind edge encoding. Colors ride the shared categorical chart tokens
 * (`--hm-cat-*`, which carry explicit light-theme overrides), picked so the
 * hues stay separable under deuteranopia: blue vs amber vs pink/purple, with
 * dash patterns distinguishing kinds even in grayscale. Hue mnemonics follow
 * the edge's anchor node: contains starts at an (amber) category, bundles at
 * a (pink) bank.
 */
export const EDGE_STYLES: Record<string, EdgeStyle> = {
  contains: { color: "var(--hm-cat-2, #f7c76a)", dash: "5 3" },
  mentions: { color: "var(--hm-cat-1, #7aa7ff)", dash: "1.5 2.5" },
  bundles: { color: "var(--hm-cat-3, #ff7ab6)" },
  bank: { color: "var(--hm-cat-4, #c09bff)", dash: "7 3 1.5 3" },
};

export const EDGE_FALLBACK_STYLE: EdgeStyle = {
  color: "var(--hm-line-strong)",
};

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
