/**
 * Cortex renderer B, the luminous point field.
 *
 * The positions are the shipped emergent layout, the same ForceAtlas2 settle
 * and constellation composure the Sigma field runs off-thread, so this
 * renderer changes how the slice is lit and nothing about where it sits.
 *
 * Light is additive and every unit of it is a measurement: a symbol's point
 * grows AND brightens with its served degree, and a relation is a faint line
 * of its source's kind hue, so where many relations overlap the field is
 * brighter because there are more of them. Hover or the keyboard cursor lifts
 * one neighbourhood and lets the rest recede; labels are reserved for the six
 * highest-degree symbols and whatever is hovered, focused or pinned.
 */
import { settleEmergentOffThread } from '../../viz/graph/emergentLayout.ts';
import { prepareField } from '../../viz/graph/layout.ts';
import {
  anchorsAround,
  degreeRadius,
  drawHaloLabel,
  drawStateMarks,
  kindColorAt,
  monoFont,
  placeLabels,
  project,
  sortByDegree,
  type Bounds,
  type CortexPainter,
  type CortexScene,
  type LabelCandidate,
  type PaintFrame,
} from './cortexScene.ts';

/** Symbols labelled without being hovered, focused or pinned. */
export const LUMINOUS_HUB_LABELS = 6;

export interface LuminousLayout {
  readonly positions: ReadonlyMap<string, { x: number; y: number }>;
  readonly bounds: Bounds;
}

export function boundsOf(points: Iterable<{ x: number; y: number }>): Bounds {
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const p of points) {
    x0 = Math.min(x0, p.x);
    y0 = Math.min(y0, p.y);
    x1 = Math.max(x1, p.x);
    y1 = Math.max(y1, p.y);
  }
  if (!Number.isFinite(x0)) return { x0: -1, y0: -1, x1: 1, y1: 1 };
  const padX = Math.max((x1 - x0) * 0.04, 1e-3);
  const padY = Math.max((y1 - y0) * 0.04, 1e-3);
  return { x0: x0 - padX, y0: y0 - padY, x1: x1 + padX, y1: y1 + padY };
}

async function luminousLayout(
  scene: CortexScene,
  box: { width: number; height: number },
): Promise<LuminousLayout> {
  if (typeof Worker === 'undefined') throw new Error('this browser runs no layout worker');
  const prepared = prepareField({
    nodes: scene.nodes.map((node) => ({
      id: node.id,
      label: node.label,
      kind: node.kind,
      ...(node.degree == null ? {} : { degree: node.degree }),
    })),
    edges: scene.edges,
    viewport: box,
    kindRgb: () => [0, 0, 0],
  });
  const settled = await settleEmergentOffThread(prepared, new AbortController().signal);
  if (!settled) throw new Error('the layout was cancelled');
  const positions = new Map<string, { x: number; y: number }>();
  for (const id of prepared.realNodes) {
    // Graphology's y grows upward; the canvas's grows downward.
    positions.set(id, {
      x: prepared.graph.getNodeAttribute(id, 'x') as number,
      y: -(prepared.graph.getNodeAttribute(id, 'y') as number),
    });
  }
  return { positions, bounds: boundsOf(positions.values()) };
}

/** Degree → point radius and luminance, on the slice's one shared scale. */
export function luminance(degree: number | null, maxDegree: number): number {
  if (degree == null || degree <= 0) return 0.28;
  return 0.28 + 0.72 * Math.sqrt(Math.min(1, degree / maxDegree));
}

function zoomScale(frame: { camera: { k: number }; fitK: number }): number {
  return Math.min(2, Math.max(0.85, Math.sqrt(frame.camera.k / frame.fitK)));
}

function pointRadius(scene: CortexScene, id: string, frame: { camera: { k: number }; fitK: number }) {
  const node = scene.byId.get(id);
  return degreeRadius(node?.degree ?? null, scene.maxDegree, { min: 2, max: 7 }) * zoomScale(frame);
}

const spriteCache = new Map<string, HTMLCanvasElement>();

/** One radial falloff per kind, drawn once and stamped additively. */
function sprite(kind: string): HTMLCanvasElement | null {
  const hit = spriteCache.get(kind);
  if (hit) return hit;
  if (typeof document === 'undefined') return null;
  const size = 96;
  const canvas = document.createElement('canvas');
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext('2d');
  if (!ctx) return null;
  const gradient = ctx.createRadialGradient(size / 2, size / 2, 0, size / 2, size / 2, size / 2);
  gradient.addColorStop(0, kindColorAt(kind, 0.86, 0.9));
  gradient.addColorStop(0.18, kindColorAt(kind, 0.78, 0.55));
  gradient.addColorStop(0.45, kindColorAt(kind, 0.7, 0.14));
  gradient.addColorStop(1, kindColorAt(kind, 0.6, 0));
  ctx.fillStyle = gradient;
  ctx.fillRect(0, 0, size, size);
  spriteCache.set(kind, canvas);
  return canvas;
}

function draw(layout: LuminousLayout, scene: CortexScene, frame: PaintFrame): void {
  const { ctx, camera, palette, emphasis } = frame;
  const screen = new Map<string, { x: number; y: number }>();
  for (const [id, p] of layout.positions) screen.set(id, project(camera, p.x, p.y));

  ctx.save();
  ctx.globalCompositeOperation = 'lighter';
  ctx.lineCap = 'round';
  for (const edge of scene.edges) {
    const a = screen.get(edge.source)!;
    const b = screen.get(edge.target)!;
    const lit = emphasis !== null && emphasis.has(edge.source) && emphasis.has(edge.target);
    const dimmed = emphasis !== null && !lit;
    const source = scene.byId.get(edge.source)!;
    ctx.strokeStyle = kindColorAt(source.kind, 0.72, dimmed ? 0.03 : lit ? 0.65 : 0.22);
    ctx.lineWidth = lit ? 1.2 : 0.8;
    ctx.beginPath();
    ctx.moveTo(a.x, a.y);
    ctx.lineTo(b.x, b.y);
    ctx.stroke();
  }
  for (const node of scene.nodes) {
    const p = screen.get(node.id)!;
    const r = pointRadius(scene, node.id, frame);
    const dimmed = emphasis !== null && !emphasis.has(node.id);
    const image = sprite(node.kind);
    const extent = r * 5.5;
    ctx.globalAlpha = luminance(node.degree, scene.maxDegree) * (dimmed ? 0.14 : 1);
    if (image) ctx.drawImage(image, p.x - extent, p.y - extent, extent * 2, extent * 2);
  }
  ctx.restore();

  ctx.save();
  for (const node of scene.nodes) {
    const p = screen.get(node.id)!;
    const r = pointRadius(scene, node.id, frame);
    const dimmed = emphasis !== null && !emphasis.has(node.id);
    ctx.globalAlpha = dimmed ? 0.3 : 1;
    ctx.beginPath();
    ctx.arc(p.x, p.y, Math.max(1.1, r * 0.55), 0, Math.PI * 2);
    if (node.degree == null) {
      ctx.setLineDash([2, 2]);
      ctx.strokeStyle = palette.unknown;
      ctx.lineWidth = 1;
      ctx.stroke();
      ctx.setLineDash([]);
    } else {
      ctx.fillStyle = kindColorAt(node.kind, 0.93);
      ctx.fill();
    }
    ctx.globalAlpha = 1;
    drawStateMarks(
      ctx,
      p.x,
      p.y,
      r,
      {
        selected: frame.selected === node.id,
        hovered: frame.hovered === node.id,
        cursor: frame.cursor === node.id,
        heat: frame.heat(node.id),
      },
      palette,
    );
  }
  ctx.restore();

  const hubs = new Set(
    sortByDegree(scene.nodes)
      .slice(0, LUMINOUS_HUB_LABELS)
      .map((node) => node.id),
  );
  const marked = (id: string) => frame.selected === id || frame.hovered === id || frame.cursor === id;
  const labelled = scene.nodes
    .filter((node) => marked(node.id) || (emphasis === null ? hubs.has(node.id) : emphasis.has(node.id)))
    .sort((a, b) => Number(marked(b.id)) - Number(marked(a.id)) || (b.degree ?? 0) - (a.degree ?? 0));
  ctx.font = monoFont(11);
  const candidates: LabelCandidate[][] = labelled.map((node) => {
    const p = screen.get(node.id)!;
    const r = pointRadius(scene, node.id, frame);
    return anchorsAround(node.id, p.x, p.y, r, ctx.measureText(node.label).width, 14);
  });
  for (const box of placeLabels(candidates)) {
    const node = scene.byId.get(box.id)!;
    drawHaloLabel(ctx, node.label, box.x, box.y + 7, {
      font: monoFont(11, marked(node.id) ? 600 : 400),
      color: palette.text,
      halo: palette.substrate,
      alpha: marked(node.id) ? 1 : 0.78,
    });
  }
}

export const luminousPainter: CortexPainter<LuminousLayout> = {
  name: 'luminous point field',
  relayoutOnResize: false,
  fitPad: 36,
  layout: (scene, box) => luminousLayout(scene, box),
  bounds: (layout) => layout.bounds,
  position: (layout, id) => layout.positions.get(id) ?? null,
  hitRadius: (_layout, scene, id, frame) => pointRadius(scene, id, frame),
  draw,
};
