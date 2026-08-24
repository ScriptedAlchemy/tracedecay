import type { EdgeStyle, SimLink } from "./associationGraphTypes";
import {
  EDGE_FALLBACK_STYLE,
  EDGE_STYLES,
  FALLBACK_COLOR,
  KIND_COLORS,
} from "./associationGraphTypes";
import type { SimNode } from "./associationGraphTypes";

export function displayEntityType(entityType?: string | null) {
  const value = (entityType || "").trim();
  if (!value || value.toLowerCase() === "unknown") return "";
  return value;
}

export function colorOf(kind: string): string {
  return KIND_COLORS[kind] ?? FALLBACK_COLOR;
}

export function edgeStyle(kind: string): EdgeStyle {
  return EDGE_STYLES[kind] ?? EDGE_FALLBACK_STYLE;
}

export function radiusOf(kind: string, degree: number): number {
  const base = kind === "fact" ? 7 : kind === "entity" ? 5 : 10;
  return base + Math.min(16, Math.sqrt(degree) * 2.4);
}

export function linkEndpoint(v: SimLink["source"]): SimNode | null {
  return v && typeof v === "object" ? (v as SimNode) : null;
}

/**
 * Co-occurrence weight for an edge. Uses the payload `weight` when the backend
 * supplies one; otherwise falls back to a structural proxy: a `mentions` edge
 * is weighted by the entity endpoint's degree (how many facts mention it — a
 * real corpus co-occurrence signal), `bundles` by the bank's fan-out.
 */
export function coOccurrenceWeight(
  link: SimLink,
  source: SimNode,
  target: SimNode,
): number {
  if (typeof link.weight === "number" && Number.isFinite(link.weight)) {
    return Math.max(0, link.weight);
  }
  if (link.kind === "mentions") {
    const entity =
      source.kind === "entity"
        ? source
        : target.kind === "entity"
          ? target
          : null;
    return entity ? Math.max(1, entity.degree) : 1;
  }
  if (link.kind === "bundles") {
    const bank =
      source.kind === "bank" ? source : target.kind === "bank" ? target : null;
    return bank ? Math.max(2, bank.degree) : 2;
  }
  return 1;
}

/** Map a co-occurrence weight to a stroke width (world units). */
export function edgeStrokeWidth(
  weight: number,
  maxWeight: number,
  emphasized: boolean,
): number {
  const norm = maxWeight > 1 ? Math.sqrt(Math.min(weight, maxWeight) / maxWeight) : 0;
  const base = 0.5 + norm * 2.1;
  return emphasized ? Math.max(base, 1.5) + 0.8 : base;
}
