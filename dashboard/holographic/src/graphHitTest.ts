import type { SimNode } from "./associationGraphTypes";
import { radiusOf } from "./associationGraphUtils";
import { truncate } from "./ui";

/** Focus ring radius around the node circle. */
export function focusRingRadius(nodeRadius: number, isSelected: boolean): number {
  return (isSelected ? nodeRadius + 2 : nodeRadius) + 4.5;
}

/**
 * Nearest node to `from` in the screen direction (dx, dy), within a ~60° cone.
 * Powers spatial arrow-key navigation: pressing → moves focus to the closest
 * node to the right, etc. Returns null when nothing lies that way.
 */
export function nearestInDirection(
  nodes: SimNode[],
  from: SimNode,
  dx: number,
  dy: number,
): SimNode | null {
  const fx = from.x ?? 0;
  const fy = from.y ?? 0;
  let best: SimNode | null = null;
  let bestScore = Infinity;
  for (const node of nodes) {
    if (node.id === from.id) continue;
    const vx = (node.x ?? 0) - fx;
    const vy = (node.y ?? 0) - fy;
    const dist = Math.hypot(vx, vy);
    if (dist === 0) continue;
    const cos = (vx * dx + vy * dy) / dist;
    if (cos < 0.5) continue; // outside the directional cone
    const score = dist / (cos * cos); // closer + better-aligned wins
    if (score < bestScore) {
      bestScore = score;
      best = node;
    }
  }
  return best;
}

const LABEL_MAX_CHARS = 22;

/** Structural anchors first, then most-connected nodes. */
function labelPriority(node: SimNode): number {
  const kindRank =
    node.kind === "category" ? 3 : node.kind === "bank" ? 2 : node.kind === "fact" ? 1 : 0;
  return kindRank * 1000 + node.degree;
}

interface LabelBox {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

function labelBox(node: SimNode, worldFont: number): LabelBox {
  const r = radiusOf(node.kind, node.degree);
  const text = truncate(node.label, LABEL_MAX_CHARS);
  const halfW = (text.length * worldFont * 0.58) / 2 + worldFont * 0.3;
  const top = (node.y ?? 0) + r + 2;
  return {
    x0: (node.x ?? 0) - halfW,
    y0: top,
    x1: (node.x ?? 0) + halfW,
    y1: top + worldFont * 1.15,
  };
}

function overlaps(a: LabelBox, b: LabelBox): boolean {
  return a.x0 < b.x1 && a.x1 > b.x0 && a.y0 < b.y1 && a.y1 > b.y0;
}

/**
 * Collision-aware label culling. Labels are constant screen size, so their
 * world-space boxes shrink as you zoom in (`worldFont = 10 / zoom`): at low
 * zoom only a few high-priority labels survive the greedy placement; zooming
 * in reveals more without overlap. When a neighborhood is highlighted we label
 * exactly that neighborhood (the user is reading it) and skip culling.
 */
export function resolveVisibleLabels(
  nodes: SimNode[],
  highlightIds: Set<string> | null,
  worldFont: number,
  maxLabels = 120,
): Set<string> {
  if (highlightIds) {
    const out = new Set<string>();
    for (const node of nodes) if (highlightIds.has(node.id)) out.add(node.id);
    return out;
  }

  const candidates = nodes
    .slice()
    .sort((a, b) => labelPriority(b) - labelPriority(a))
    .slice(0, 400);

  const placed: LabelBox[] = [];
  const out = new Set<string>();
  for (const node of candidates) {
    if (out.size >= maxLabels) break;
    const box = labelBox(node, worldFont);
    let collides = false;
    for (let i = placed.length - 1; i >= 0; i -= 1) {
      if (overlaps(box, placed[i])) {
        collides = true;
        break;
      }
    }
    if (collides) continue;
    placed.push(box);
    out.add(node.id);
  }
  return out;
}
