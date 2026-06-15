import { KIND_FAMILY_TOKENS } from "./types";

export interface Camera {
  x: number;
  y: number;
  k: number;
}

export type EdgeAccent = "amber" | "blue" | "pink" | "green" | "muted";

export interface ThemeColors {
  label: string;
  halo: string;
  ring: string;
  path: string;
  accents: Record<EdgeAccent, string>;
  family: Record<string, string>;
}

export interface CanvasSize {
  width: number;
  height: number;
}

export interface FramedNode {
  x: number;
  y: number;
  radius: number;
}

export interface FramedSimulation {
  nodes: FramedNode[];
}

/** Edge styling by kind: theme accent (resolved at draw time) + alpha + dash. */
const EDGE_STYLE_DEFS: Record<
  string,
  { accent: EdgeAccent; alpha: number; dash: number[]; width: number }
> = {
  calls: { accent: "amber", alpha: 0.55, dash: [], width: 1.4 },
  uses: { accent: "blue", alpha: 0.45, dash: [6, 4], width: 1.1 },
  implements: { accent: "pink", alpha: 0.55, dash: [3, 3], width: 1.3 },
  extends: { accent: "pink", alpha: 0.45, dash: [8, 3], width: 1.3 },
  contains: { accent: "muted", alpha: 0.28, dash: [2, 4], width: 1 },
  type_of: { accent: "blue", alpha: 0.36, dash: [4, 4], width: 1 },
  returns: { accent: "green", alpha: 0.42, dash: [5, 3], width: 1.1 },
  receives: { accent: "green", alpha: 0.34, dash: [2, 3], width: 1 },
  derives_macro: { accent: "pink", alpha: 0.3, dash: [1, 3], width: 1 },
  annotates: { accent: "muted", alpha: 0.3, dash: [1, 4], width: 1 },
};
const DEFAULT_EDGE_STYLE_DEF = {
  accent: "muted" as EdgeAccent,
  alpha: 0.3,
  dash: [] as number[],
  width: 1,
};

/** `#rrggbb` → `rgba()`; non-hex values pass through untouched. */
export function withAlpha(color: string, alpha: number): string {
  const match = /^#([0-9a-f]{6})$/i.exec(color);
  if (!match) return color;
  const n = parseInt(match[1], 16);
  return `rgba(${(n >> 16) & 0xff}, ${(n >> 8) & 0xff}, ${n & 0xff}, ${alpha})`;
}

/**
 * Samples the shell design tokens so the canvas follows light/dark themes —
 * canvas 2D can't resolve `var()`, so node/edge accents are read here too.
 */
export function readTheme(el: HTMLElement): ThemeColors {
  const styles = getComputedStyle(el);
  const read = (name: string, fallback: string) => styles.getPropertyValue(name).trim() || fallback;
  const family: Record<string, string> = {};
  for (const [key, [token, fallback]] of Object.entries(KIND_FAMILY_TOKENS)) {
    family[key] = read(token, fallback);
  }
  return {
    label: read("--ts-text", "#e7fff9"),
    halo: read("--ts-void", "#030607"),
    ring: read("--ts-text", "#e7fff9"),
    path: read("--ts-cyan", "#75f4d2"),
    family,
    accents: {
      amber: read("--ts-amber", "#f7c76a"),
      blue: read("--ts-blue", "#7aa7ff"),
      pink: read("--ts-pink", "#ff7ab6"),
      green: read("--ts-green", "#67e8a9"),
      muted: read("--ts-text-2", "#a8c8c0"),
    },
  };
}

export function edgeStyle(kind: string, theme: Pick<ThemeColors, "accents">) {
  const def = EDGE_STYLE_DEFS[kind] || DEFAULT_EDGE_STYLE_DEF;
  return {
    color: withAlpha(theme.accents[def.accent], def.alpha),
    dash: def.dash,
    width: def.width,
  };
}

/**
 * Frames the bounding box of every simulated node. `smooth` lerps toward the
 * target instead of snapping, for per-frame tracking while the layout settles.
 */
export function fitCameraToNodes(
  size: CanvasSize,
  sim: FramedSimulation,
  camera: Camera,
  smooth = false,
) {
  if (sim.nodes.length === 0) return;
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const node of sim.nodes) {
    minX = Math.min(minX, node.x - node.radius);
    minY = Math.min(minY, node.y - node.radius);
    maxX = Math.max(maxX, node.x + node.radius);
    maxY = Math.max(maxY, node.y + node.radius);
  }
  const spanX = Math.max(1, maxX - minX);
  const spanY = Math.max(1, maxY - minY);
  const fitK = Math.min(size.width / spanX, size.height / spanY) * 0.88;
  const targetX = (minX + maxX) / 2;
  const targetY = (minY + maxY) / 2;
  const targetK = Math.min(5, Math.max(0.12, fitK));
  if (smooth) {
    camera.x += (targetX - camera.x) * 0.2;
    camera.y += (targetY - camera.y) * 0.2;
    camera.k += (targetK - camera.k) * 0.2;
  } else {
    camera.x = targetX;
    camera.y = targetY;
    camera.k = targetK;
  }
}

export function toWorldPoint(
  camera: Camera,
  rect: { left: number; top: number; width: number; height: number },
  point: { x: number; y: number },
) {
  return {
    x: (point.x - rect.left - rect.width / 2) / camera.k + camera.x,
    y: (point.y - rect.top - rect.height / 2) / camera.k + camera.y,
  };
}

export function zoomCameraAtPoint(
  camera: Camera,
  rect: { left: number; top: number; width: number; height: number },
  point: { x: number; y: number },
  nextK: number,
) {
  const before = toWorldPoint(camera, rect, point);
  camera.k = Math.min(5, Math.max(0.12, nextK));
  const after = toWorldPoint(camera, rect, point);
  camera.x += before.x - after.x;
  camera.y += before.y - after.y;
}

export function hitTestNode<T extends { id: string; x: number; y: number; radius: number }>(
  nodes: T[],
  camera: Camera,
  rect: { left: number; top: number; width: number; height: number },
  point: { x: number; y: number },
): T | null {
  const world = toWorldPoint(camera, rect, point);
  let best: T | null = null;
  let bestDist = Infinity;
  for (const node of nodes) {
    const dx = node.x - world.x;
    const dy = node.y - world.y;
    const dist = Math.hypot(dx, dy);
    const reach = node.radius + 6 / camera.k;
    if (dist < reach && dist < bestDist) {
      best = node;
      bestDist = dist;
    }
  }
  return best;
}

export function neighborhoodIds(
  nodeId: string,
  edges: Array<{ source: string; target: string }>,
) {
  const ids = new Set<string>([nodeId]);
  for (const edge of edges) {
    if (edge.source === nodeId) ids.add(edge.target);
    if (edge.target === nodeId) ids.add(edge.source);
  }
  return ids;
}
