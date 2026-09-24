/**
 * The renderer-independent core of the Cortex field: the drawn slice as a
 * scene, the one degree scale and kind marks, the camera, and the label
 * collision pass. The relief painter and the canvas wiring both build on it.
 *
 * Everything the field draws is derived here from the served slice and
 * nothing else. A node the wire gave no degree keeps `degree: null` all the
 * way to the paint, where it is drawn at the minimum size with a dashed
 * outline, so an absent measurement never reads as a small one.
 */
import type { GraphEdgeV1, GraphNodeV1 } from '../../contracts/generated.ts';
import { kindColor } from '../../viz/graph/kindColor.ts';
import { directoryOf } from './cortexRelief.ts';

/* ---- scene --------------------------------------------------------------- */

/** Module label for a symbol with no served file path. Printed, never blank. */
export const NO_PATH_MODULE = '(path absent)';

export interface SceneNode {
  readonly id: string;
  readonly label: string;
  readonly kind: string;
  readonly degree: number | null;
  readonly filePath: string | null;
  /** Directory of the file, or {@link NO_PATH_MODULE}. */
  readonly module: string;
}

export interface SceneEdge {
  readonly source: string;
  readonly target: string;
  readonly kind: string;
}

export interface CortexScene {
  readonly nodes: readonly SceneNode[];
  /** Edges whose both endpoints are drawn. */
  readonly edges: readonly SceneEdge[];
  /** Served edges with an endpoint outside the slice; counted, not drawn. */
  readonly danglingEdges: number;
  readonly byId: ReadonlyMap<string, SceneNode>;
  readonly neighbors: ReadonlyMap<string, ReadonlySet<string>>;
  /** Largest served degree on the slice, at least 1. */
  readonly maxDegree: number;
  /** Symbols the wire gave no degree. */
  readonly unknownDegree: number;
}

export function sceneFromSlice(
  nodes: readonly GraphNodeV1[],
  edges: readonly GraphEdgeV1[],
): CortexScene {
  const sceneNodes: SceneNode[] = nodes.map((node) => ({
    id: node.id,
    label: node.name ?? node.qualified_name ?? node.id,
    kind: node.kind,
    degree: node.degree ?? null,
    filePath: node.file_path ?? null,
    module: node.file_path ? directoryOf(node.file_path) : NO_PATH_MODULE,
  }));
  const byId = new Map(sceneNodes.map((node) => [node.id, node]));
  const neighbors = new Map<string, Set<string>>(sceneNodes.map((node) => [node.id, new Set()]));
  const drawn: SceneEdge[] = [];
  let dangling = 0;
  for (const edge of edges) {
    if (!byId.has(edge.source) || !byId.has(edge.target)) {
      dangling += 1;
      continue;
    }
    drawn.push({ source: edge.source, target: edge.target, kind: edge.kind });
    if (edge.source !== edge.target) {
      neighbors.get(edge.source)!.add(edge.target);
      neighbors.get(edge.target)!.add(edge.source);
    }
  }
  let maxDegree = 1;
  let unknownDegree = 0;
  for (const node of sceneNodes) {
    if (node.degree == null) unknownDegree += 1;
    else maxDegree = Math.max(maxDegree, node.degree);
  }
  return {
    nodes: sceneNodes,
    edges: drawn,
    danglingEdges: dangling,
    byId,
    neighbors,
    maxDegree,
    unknownDegree,
  };
}

/** The node and everything one drawn edge away from it. */
export function neighbourhood(scene: CortexScene, id: string | null): ReadonlySet<string> | null {
  if (id === null || !scene.byId.has(id)) return null;
  return new Set([id, ...(scene.neighbors.get(id) ?? [])]);
}

/** Symbols grouped by module, most populous first, then by name. */
export function modulesOf(scene: CortexScene): { module: string; members: SceneNode[] }[] {
  const groups = new Map<string, SceneNode[]>();
  for (const node of scene.nodes) {
    const members = groups.get(node.module) ?? [];
    members.push(node);
    groups.set(node.module, members);
  }
  return [...groups]
    .map(([module, members]) => ({ module, members: sortByDegree(members) }))
    .sort((a, b) => b.members.length - a.members.length || a.module.localeCompare(b.module));
}

/** Highest served degree first; absent degree after every measured one. */
export function sortByDegree<T extends { degree: number | null; label: string; id: string }>(
  nodes: readonly T[],
): T[] {
  return [...nodes].sort(
    (a, b) =>
      (b.degree ?? -1) - (a.degree ?? -1) ||
      a.label.localeCompare(b.label) ||
      a.id.localeCompare(b.id),
  );
}

/** The keyboard cursor's order over the field: the ledger's order, degree first. */
export function keyboardOrder(scene: CortexScene): string[] {
  return sortByDegree(scene.nodes).map((node) => node.id);
}

/**
 * One degree scale for the whole slice: radius grows with the square root of
 * degree, so AREA is proportional to degree. Absent degree takes the minimum
 * and the renderer marks it; it is not degree zero.
 */
export function degreeRadius(
  degree: number | null,
  maxDegree: number,
  range: { readonly min: number; readonly max: number },
): number {
  if (degree == null || degree <= 0) return range.min;
  return range.min + (range.max - range.min) * Math.sqrt(Math.min(1, degree / maxDegree));
}

/** The served degree at a percentile of the slice: the "hub" cut for labels. */
export function hubDegree(scene: CortexScene, percentile = 0.75): number {
  const degrees = scene.nodes
    .flatMap((node) => (node.degree == null ? [] : [node.degree]))
    .sort((a, b) => a - b);
  if (degrees.length === 0) return Infinity;
  return degrees[Math.floor((degrees.length - 1) * percentile)]!;
}

/* ---- kind marks ---------------------------------------------------------- */

export type KindShape = 'circle' | 'square' | 'diamond' | 'ring';

/**
 * Kind is carried by hue AND shape. The hue is the app's shared kind ramp
 * (`kindColor`), so the field and the ledger's spine agree; that ramp shares
 * the cool band with the signal cyan and runs up to near-white, so the shape
 * is what keeps two kinds apart at small sizes and selection never relies on
 * hue at all.
 */
const KIND_SHAPE: Readonly<Record<string, KindShape>> = {
  function: 'circle',
  method: 'circle',
  field: 'circle',
  constant: 'circle',
  variable: 'circle',
  struct: 'square',
  class: 'square',
  enum: 'square',
  type: 'square',
  trait: 'diamond',
  interface: 'diamond',
  impl: 'diamond',
  module: 'ring',
  file: 'ring',
};

export function kindShape(kind: string): KindShape {
  return KIND_SHAPE[kind.toLowerCase()] ?? 'circle';
}

/* ---- camera -------------------------------------------------------------- */

export interface Bounds {
  readonly x0: number;
  readonly y0: number;
  readonly x1: number;
  readonly y1: number;
}

/** screen = world * k + t */
export interface Camera {
  readonly k: number;
  readonly tx: number;
  readonly ty: number;
}

export function fitCamera(bounds: Bounds, width: number, height: number, pad: number): Camera {
  const bw = Math.max(1e-6, bounds.x1 - bounds.x0);
  const bh = Math.max(1e-6, bounds.y1 - bounds.y0);
  const k = Math.max(1e-6, Math.min((width - 2 * pad) / bw, (height - 2 * pad) / bh));
  return {
    k,
    tx: width / 2 - ((bounds.x0 + bounds.x1) / 2) * k,
    ty: height / 2 - ((bounds.y0 + bounds.y1) / 2) * k,
  };
}

/** Zoom by `factor` about a screen point, which stays under the pointer. */
export function zoomAt(
  camera: Camera,
  px: number,
  py: number,
  factor: number,
  limits: { readonly min: number; readonly max: number },
): Camera {
  const k = Math.min(limits.max, Math.max(limits.min, camera.k * factor));
  const ratio = k / camera.k;
  return { k, tx: px - (px - camera.tx) * ratio, ty: py - (py - camera.ty) * ratio };
}

export function project(camera: Camera, x: number, y: number): { x: number; y: number } {
  return { x: x * camera.k + camera.tx, y: y * camera.k + camera.ty };
}

export function unproject(camera: Camera, x: number, y: number): { x: number; y: number } {
  return { x: (x - camera.tx) / camera.k, y: (y - camera.ty) / camera.k };
}

/* ---- painter contract ---------------------------------------------------- */

/** Token colours sampled from the night-window block; a canvas cannot read CSS. */
export interface ScenePalette {
  readonly substrate: string;
  readonly dim: string;
  readonly text: string;
  readonly muted: string;
  readonly edge: string;
  readonly accent: string;
  readonly unknown: string;
  readonly danger: string;
}

export interface PaintFrame {
  readonly ctx: CanvasRenderingContext2D;
  readonly width: number;
  readonly height: number;
  readonly camera: Camera;
  /** Camera scale at Fit; `camera.k / fitK` is the semantic-zoom level. */
  readonly fitK: number;
  readonly palette: ScenePalette;
  readonly hovered: string | null;
  readonly selected: string | null;
  /** The keyboard cursor, shown only while the field holds focus. */
  readonly cursor: string | null;
  /** Neighbourhood of the hovered, else cursor, else selected symbol. */
  readonly emphasis: ReadonlySet<string> | null;
  readonly heat: (id: string) => number;
}

/**
 * A renderer is a pure layout plus a paint. The layout is in world units and
 * is recomputed only when the slice changes; the paint reads the frame and
 * decides nothing.
 */
export interface CortexPainter<L> {
  readonly name: string;
  /** Padding the Fit camera leaves, in screen px. */
  readonly fitPad: number;
  layout(scene: CortexScene, box: { width: number; height: number }): L | Promise<L>;
  bounds(layout: L): Bounds;
  /** World position of a drawn symbol. */
  position(layout: L, id: string): { x: number; y: number } | null;
  /** Screen-space radius a pointer must land within to hit the symbol. */
  hitRadius(layout: L, scene: CortexScene, id: string, frame: { camera: Camera; fitK: number }): number;
  draw(layout: L, scene: CortexScene, frame: PaintFrame): void;
}

/* ---- shared marks -------------------------------------------------------- */

export function traceKindMark(
  ctx: CanvasRenderingContext2D,
  shape: KindShape,
  x: number,
  y: number,
  r: number,
): void {
  ctx.beginPath();
  switch (shape) {
    case 'circle':
    case 'ring':
      ctx.arc(x, y, r, 0, Math.PI * 2);
      break;
    case 'square': {
      const s = r * 0.9;
      ctx.rect(x - s, y - s, s * 2, s * 2);
      break;
    }
    case 'diamond': {
      const s = r * 1.2;
      ctx.moveTo(x, y - s);
      ctx.lineTo(x + s, y);
      ctx.lineTo(x, y + s);
      ctx.lineTo(x - s, y);
      ctx.closePath();
      break;
    }
    default: {
      const unhandled: never = shape;
      return unhandled;
    }
  }
}

/**
 * A symbol body: kind hue and shape over a substrate keyline, so the ramp's
 * near-white kinds still separate from each other and from the relief.
 * Absent degree is a dashed outline, never a filled small body.
 */
export function drawSymbolBody(
  ctx: CanvasRenderingContext2D,
  node: SceneNode,
  x: number,
  y: number,
  r: number,
  options: { alpha: number; palette: ScenePalette },
): void {
  const shape = kindShape(node.kind);
  ctx.save();
  ctx.globalAlpha = options.alpha;
  traceKindMark(ctx, shape, x, y, r);
  if (node.degree == null) {
    ctx.setLineDash([2, 2]);
    ctx.strokeStyle = options.palette.unknown;
    ctx.lineWidth = 1;
    ctx.stroke();
  } else if (shape === 'ring') {
    ctx.strokeStyle = options.palette.substrate;
    ctx.lineWidth = Math.max(1, r * 0.35) + 2;
    ctx.stroke();
    ctx.strokeStyle = kindColor(node.kind, false);
    ctx.lineWidth = Math.max(1, r * 0.35);
    ctx.stroke();
  } else {
    ctx.strokeStyle = options.palette.substrate;
    ctx.lineWidth = 1.5;
    ctx.stroke();
    ctx.fillStyle = kindColor(node.kind, false);
    ctx.fill();
  }
  ctx.restore();
}

/**
 * Keyboard cursor and selection. Selection is a 2px cyan ring and the
 * inspector's gutter, never a hue change, because kinds share the cyan band;
 * hover draws no mark of its own, it only dims what is unrelated. The
 * keyboard cursor is a 2px cyan bracket. A search strike is a cyan hairline
 * whose opacity is the decaying heat.
 */
export function drawStateMarks(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  r: number,
  state: { selected: boolean; cursor: boolean; heat: number },
  palette: ScenePalette,
): void {
  ctx.save();
  if (state.heat > 0.02) {
    ctx.globalAlpha = Math.min(0.9, state.heat);
    ctx.strokeStyle = palette.accent;
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.arc(x, y, r + 7, 0, Math.PI * 2);
    ctx.stroke();
    ctx.globalAlpha = 1;
  }
  if (state.selected) {
    ctx.strokeStyle = palette.substrate;
    ctx.lineWidth = 4;
    ctx.beginPath();
    ctx.arc(x, y, r + 4, 0, Math.PI * 2);
    ctx.stroke();
    ctx.strokeStyle = palette.accent;
    ctx.lineWidth = 2;
    ctx.stroke();
  }
  if (state.cursor) {
    const s = r + 8;
    const arm = Math.max(4, s * 0.45);
    ctx.strokeStyle = palette.accent;
    ctx.lineWidth = 2;
    ctx.beginPath();
    for (const [sx, sy] of [
      [-1, -1],
      [1, -1],
      [1, 1],
      [-1, 1],
    ] as const) {
      ctx.moveTo(x + sx * s, y + sy * (s - arm));
      ctx.lineTo(x + sx * s, y + sy * s);
      ctx.lineTo(x + sx * (s - arm), y + sy * s);
    }
    ctx.stroke();
  }
  ctx.restore();
}

/** Engraved display face for hub labels. */
export function displayFont(size: number, weight = 600): string {
  return `${weight} ${size}px "Archivo Variable", "Archivo", "IBM Plex Sans Variable", sans-serif`;
}

export function monoFont(size: number, weight = 400): string {
  return `${weight} ${size}px "IBM Plex Mono", ui-monospace, SFMono-Regular, monospace`;
}

/** A label with a substrate halo so it reads over lines and relief. */
export function drawHaloLabel(
  ctx: CanvasRenderingContext2D,
  text: string,
  x: number,
  y: number,
  options: { font: string; color: string; halo: string; align?: CanvasTextAlign; alpha?: number },
): void {
  ctx.save();
  ctx.globalAlpha = options.alpha ?? 1;
  ctx.font = options.font;
  ctx.textAlign = options.align ?? 'left';
  ctx.textBaseline = 'middle';
  ctx.lineJoin = 'round';
  ctx.lineWidth = 3;
  ctx.strokeStyle = options.halo;
  ctx.strokeText(text, x, y);
  ctx.fillStyle = options.color;
  ctx.fillText(text, x, y);
  ctx.restore();
}

/* ---- labels -------------------------------------------------------------- */

export interface LabelCandidate {
  readonly id: string;
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

/**
 * Greedy collision pass: labels arrive in priority order, each with its
 * anchor positions in preference order, and a label takes the first position
 * that clears every kept label and every obstacle (the node discs). A label
 * with no clear position is dropped rather than overprinted; the list beside
 * the field names every symbol.
 */
export function placeLabels(
  candidates: readonly (readonly LabelCandidate[])[],
  obstacles: readonly LabelCandidate[] = [],
  gap = 2,
): LabelCandidate[] {
  const kept: LabelCandidate[] = [];
  const overlaps = (a: LabelCandidate, b: LabelCandidate) =>
    a.x < b.x + b.width + gap &&
    b.x < a.x + a.width + gap &&
    a.y < b.y + b.height + gap &&
    b.y < a.y + a.height + gap;
  for (const positions of candidates) {
    const clear = positions.find(
      (position) =>
        !kept.some((box) => overlaps(position, box)) &&
        !obstacles.some((box) => box.id !== position.id && overlaps(position, box)),
    );
    if (clear) kept.push(clear);
  }
  return kept;
}

/** Right, left, above, below a disc of radius `r` at (x, y). */
export function anchorsAround(
  id: string,
  x: number,
  y: number,
  r: number,
  width: number,
  height: number,
): LabelCandidate[] {
  return [
    { id, x: x + r + 5, y: y - height / 2, width, height },
    { id, x: x - r - 5 - width, y: y - height / 2, width, height },
    { id, x: x - width / 2, y: y - r - 3 - height, width, height },
    { id, x: x - width / 2, y: y + r + 3, width, height },
  ];
}
