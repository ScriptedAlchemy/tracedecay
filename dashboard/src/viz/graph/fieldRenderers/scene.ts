import { approach, cssColorToRgb, settled, type ActivationField } from '../activation.ts';
import { palette, type GraphPalette } from '../palette.ts';

/**
 * The renderer-neutral field the Brain draws.
 *
 * A renderer receives positioned bodies and drawn relations whose
 * geometry was already decided by a measured or emergent layout, plus an
 * activation field it may only sample. It never decides what a position,
 * size or relation means; the scene builders do, and the host's legend
 * states it.
 */

export interface SceneBody {
  id: string;
  label: string;
  /** `body` carries a holdings measure; `hub` is a massless relation junction
   * drawn at a fixed categorical size. */
  role: 'body' | 'hub';
  kind: string;
  x: number;
  y: number;
  /** Radius in field units on the one shared mass scale; hubs are fixed. */
  radius: number;
  /** The named holdings measure, or null when the source did not measure it. */
  mass: number | null;
  /** Indexed units by class, for renderers that draw one mark per unit. */
  units: { stores: number; artifacts: number } | null;
  /** Recency 0..1 relative to the field's horizon, or null when unmeasured. */
  vitality: number | null;
  /** Exact printed readings, in order. `absent` stays printed. */
  detail: readonly string[];
  /** The repository identity this body belongs to, when recorded. */
  group: string | null;
  /** The packed cell this body sits in, when it was crowded into one. */
  cluster: string | null;
}

/** A packed cell of bodies. Drawn as one counted frame until the camera is
 * close enough for its members to stop overlapping. */
export interface SceneCluster {
  id: string;
  members: readonly string[];
  x: number;
  y: number;
  width: number;
  height: number;
  /** Zoom (camera scale over fit scale) at which members separate. */
  resolveZoom: number;
  /** Summed member holdings, printed on the frame with the exact count. */
  mass: number;
  /** Packed distance between neighbouring members, in field units. */
  spacing: number;
}

export type RelationGrade = 'EXACT' | 'INFERRED';

export interface ScenePath {
  source: string;
  target: string;
  relation: string;
  grade: RelationGrade;
}

export interface SceneColumn {
  label: string;
  bound: string;
  count: number;
}

export interface FieldScene {
  bodies: readonly SceneBody[];
  paths: readonly ScenePath[];
  clusters: readonly SceneCluster[];
  /** Camera frame in field units, larger y is up. */
  extent: { x: [number, number]; y: [number, number] };
  /** Recency columns centred on x = 0..n-1, when the field is measured. */
  columns: readonly SceneColumn[] | null;
  /** Drawn adjacency; activation may travel only along it. */
  neighbors: ReadonlyMap<string, readonly string[]>;
}

/** Reader state: inspection and camera focus. None of it is activity. */
export interface FieldView {
  inspected: string | null;
  /** Camera focus (repository zoom or focused plate). Bodies outside recede. */
  focus: ReadonlySet<string> | null;
}

/** One admitted event that reached a drawn body, with its evidenced hop. */
export interface Synapse {
  from: string;
  to: string | null;
  /** `performance.now()` at admission, the clock the frame loop runs on. */
  at: number;
  label: string;
  /** Wall-clock receipt time, printed beside the heat. */
  time: string;
}

export interface FieldPalette extends GraphPalette {
  /** Stores and other identity-neutral holdings. */
  ice: [number, number, number];
  grid: [number, number, number];
  gridMajor: [number, number, number];
  ink: [number, number, number];
  inkMuted: [number, number, number];
  face: [number, number, number];
  edgeStrong: [number, number, number];
  displayFont: string;
}

export function sampleFieldPalette(element: HTMLElement): FieldPalette {
  const style = getComputedStyle(element);
  const token = (name: string, fallback: string): [number, number, number] =>
    cssColorToRgb(style.getPropertyValue(name).trim() || fallback);
  return {
    ...palette(element),
    ice: token('--raw-graph-ice', '#d4ecf7'),
    grid: token('--raw-grid-minor', '#23262c'),
    gridMajor: token('--raw-grid', '#2c3036'),
    ink: token('--ink-primary', '#eef0f3'),
    inkMuted: token('--ink-muted', '#9ea3aa'),
    face: token('--night-face', '#16181b'),
    edgeStrong: token('--edge-strong', '#5c6168'),
    displayFont: style.getPropertyValue('--font-display').trim() || 'sans-serif',
  };
}

export interface FieldRendererOptions {
  container: HTMLElement;
  scene: FieldScene;
  field: ActivationField;
  palette: FieldPalette;
  isReduced: () => boolean;
  onHover: (id: string | null) => void;
  onSelect: (id: string) => void;
}

export interface FieldRenderer {
  setView(view: FieldView): void;
  synapse(synapse: Synapse): void;
  resize(): void;
  zoom(factor: number): void;
  fit(): void;
  retheme(palette: FieldPalette): void;
  destroy(): void;
}

export type FieldRendererFactory = (options: FieldRendererOptions) => FieldRenderer;

/**
 * How a body's drawn radius grows with zoom: as zoom^0.25 rather than
 * linearly, so zooming separates positions faster than bodies swell and a
 * packed cell resolves into its members.
 */
export const BODY_ZOOM_GROWTH = 0.25;

export function bodyScreenRadius(radius: number, scale: number, fitScale: number): number {
  return radius * fitScale * (scale / fitScale) ** BODY_ZOOM_GROWTH;
}

/** The zoom at which bodies of `maxRadius` packed `spacing` apart stop
 * overlapping under {@link bodyScreenRadius}. Never below 1. */
export function resolveZoom(maxRadius: number, spacing: number): number {
  return Math.max(1, ((2 * maxRadius) / Math.max(spacing, 1e-9)) ** (1 / (1 - BODY_ZOOM_GROWTH)));
}

/**
 * A uniform grid over static world positions, so pointer picking touches the
 * handful of bodies near the pointer instead of every body on the field.
 */
export function createSpatialIndex<T extends { x: number; y: number }>(items: readonly T[], cell: number) {
  const buckets = new Map<string, T[]>();
  const key = (cx: number, cy: number): string => `${cx}:${cy}`;
  for (const item of items) {
    const k = key(Math.floor(item.x / cell), Math.floor(item.y / cell));
    const bucket = buckets.get(k);
    if (bucket) bucket.push(item);
    else buckets.set(k, [item]);
  }
  return {
    /** Items whose position lies within `radius` of (x, y), plus bucket slack. */
    near(x: number, y: number, radius: number): T[] {
      const found: T[] = [];
      const x0 = Math.floor((x - radius) / cell);
      const x1 = Math.floor((x + radius) / cell);
      const y0 = Math.floor((y - radius) / cell);
      const y1 = Math.floor((y + radius) / cell);
      for (let cx = x0; cx <= x1; cx += 1) {
        for (let cy = y0; cy <= y1; cy += 1) {
          const bucket = buckets.get(key(cx, cy));
          if (bucket) found.push(...bucket);
        }
      }
      return found;
    },
  };
}

/** How long a travelling synapse light takes to cross its one hop. */
export const SYNAPSE_TRAVEL_MS = 700;
/** Reduced motion repaints decaying heat in discrete steps, never per frame. */
export const REDUCED_REPAINT_MS = 1000;

/** World (field units, y up) to screen (CSS px, y down), fit with padding and
 * a pointer-centred zoom on top. Pure arithmetic so it can be tested and so a
 * capture harness can locate a body without a renderer. */
export interface CameraState {
  scale: number;
  tx: number;
  ty: number;
}

export function fitCamera(
  extent: FieldScene['extent'],
  width: number,
  height: number,
  pad: { top: number; right: number; bottom: number; left: number },
): CameraState {
  const spanX = Math.max(extent.x[1] - extent.x[0], 1e-6);
  const spanY = Math.max(extent.y[1] - extent.y[0], 1e-6);
  const innerW = Math.max(width - pad.left - pad.right, 1);
  const innerH = Math.max(height - pad.top - pad.bottom, 1);
  const scale = Math.min(innerW / spanX, innerH / spanY);
  const tx = pad.left + (innerW - spanX * scale) / 2 - extent.x[0] * scale;
  const ty = pad.top + (innerH - spanY * scale) / 2 + extent.y[1] * scale;
  return { scale, tx, ty };
}

export function toScreen(camera: CameraState, x: number, y: number): [number, number] {
  return [camera.tx + x * camera.scale, camera.ty - y * camera.scale];
}

export function toWorld(camera: CameraState, sx: number, sy: number): [number, number] {
  return [(sx - camera.tx) / camera.scale, (camera.ty - sy) / camera.scale];
}

/** Zoom by `factor` keeping the world point under (sx, sy) fixed. */
export function zoomAt(camera: CameraState, sx: number, sy: number, factor: number): CameraState {
  const [wx, wy] = toWorld(camera, sx, sy);
  const scale = camera.scale * factor;
  return { scale, tx: sx - wx * scale, ty: sy + wy * scale };
}

/** Frame a set of bodies, used by repository zoom and plate focus. */
export function focusCamera(
  bodies: readonly SceneBody[],
  width: number,
  height: number,
  margin: number,
): CameraState | null {
  if (bodies.length === 0) return null;
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (const body of bodies) {
    minX = Math.min(minX, body.x - body.radius);
    maxX = Math.max(maxX, body.x + body.radius);
    minY = Math.min(minY, body.y - body.radius);
    maxY = Math.max(maxY, body.y + body.radius);
  }
  const padX = Math.max((maxX - minX) * 0.35, 0.35);
  const padY = Math.max((maxY - minY) * 0.35, 0.35);
  return fitCamera(
    { x: [minX - padX, maxX + padX], y: [minY - padY, maxY + padY] },
    width,
    height,
    { top: margin, right: margin, bottom: margin, left: margin },
  );
}

/** A camera that eases toward its target unless motion is reduced. */
export class EasedCamera {
  current: CameraState;
  target: CameraState;

  constructor(initial: CameraState) {
    this.current = { ...initial };
    this.target = { ...initial };
  }

  set(next: CameraState, reduced: boolean): void {
    this.target = { ...next };
    if (reduced) this.current = { ...next };
  }

  /** Advance toward the target; returns true while still moving. */
  step(deltaMs: number): boolean {
    const c = this.current;
    const t = this.target;
    c.scale = approach(c.scale, t.scale, deltaMs, 90);
    c.tx = approach(c.tx, t.tx, deltaMs, 90);
    c.ty = approach(c.ty, t.ty, deltaMs, 90);
    const done =
      settled(c.scale / t.scale, 1, 0.001) && settled(c.tx, t.tx, 0.4) && settled(c.ty, t.ty, 0.4);
    if (done) this.current = { ...t };
    return !done;
  }
}

/**
 * The single paint scheduler for a field canvas. Frames run only while
 * something real is unresolved (warm heat, a travelling synapse, an easing
 * camera); an idle field schedules nothing. Under reduced motion heat is
 * repainted in one-second steps and nothing travels.
 */
export function createFrameLoop(draw: (now: number, deltaMs: number) => boolean, isReduced: () => boolean) {
  let frame = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let last = 0;
  let alive = true;
  const run = (now: number): void => {
    frame = 0;
    if (!alive) return;
    const delta = last === 0 ? 16 : Math.min(now - last, 100);
    last = now;
    const more = draw(now, delta);
    if (!more) {
      last = 0;
      return;
    }
    if (isReduced()) {
      timer = setTimeout(() => {
        timer = null;
        run(performance.now());
      }, REDUCED_REPAINT_MS);
    } else {
      frame = requestAnimationFrame(run);
    }
  };
  return {
    wake(): void {
      if (!alive || frame !== 0 || timer !== null) return;
      frame = requestAnimationFrame(run);
    },
    stop(): void {
      alive = false;
      if (frame !== 0) cancelAnimationFrame(frame);
      if (timer !== null) clearTimeout(timer);
    },
  };
}

/** A device-pixel-ratio aware 2D canvas filling its container. */
export function mountCanvas(container: HTMLElement): {
  canvas: HTMLCanvasElement;
  context: CanvasRenderingContext2D;
  size: () => { width: number; height: number };
  fitToContainer: () => void;
} | null {
  const canvas = document.createElement('canvas');
  canvas.style.position = 'absolute';
  canvas.style.inset = '0';
  canvas.style.width = '100%';
  canvas.style.height = '100%';
  const context = canvas.getContext('2d');
  if (!context) return null;
  container.appendChild(canvas);
  let width = 0;
  let height = 0;
  const fitToContainer = (): void => {
    width = container.clientWidth;
    height = container.clientHeight;
    const ratio = window.devicePixelRatio || 1;
    canvas.width = Math.max(1, Math.round(width * ratio));
    canvas.height = Math.max(1, Math.round(height * ratio));
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
  };
  fitToContainer();
  return { canvas, context, size: () => ({ width, height }), fitToContainer };
}

/** Wheel zoom around the pointer, bounded by `limit`, and drag pan. */
export function attachCameraGestures(
  canvas: HTMLCanvasElement,
  camera: EasedCamera,
  changed: () => void,
  limit: (scale: number) => number,
): () => void {
  let drag: { x: number; y: number; moved: boolean } | null = null;
  const wheel = (event: WheelEvent): void => {
    event.preventDefault();
    const rect = canvas.getBoundingClientRect();
    const t = camera.target;
    const factor = limit(t.scale * Math.exp(-event.deltaY * 0.0015)) / t.scale;
    camera.set(zoomAt(camera.target, event.clientX - rect.left, event.clientY - rect.top, factor), true);
    changed();
  };
  const down = (event: PointerEvent): void => {
    drag = { x: event.clientX, y: event.clientY, moved: false };
  };
  const move = (event: PointerEvent): void => {
    if (!drag || event.buttons === 0) return;
    const dx = event.clientX - drag.x;
    const dy = event.clientY - drag.y;
    if (!drag.moved && Math.hypot(dx, dy) < 4) return;
    drag.moved = true;
    drag.x = event.clientX;
    drag.y = event.clientY;
    const t = camera.target;
    camera.set({ scale: t.scale, tx: t.tx + dx, ty: t.ty + dy }, true);
    changed();
  };
  const up = (): void => {
    drag = null;
  };
  canvas.addEventListener('wheel', wheel, { passive: false });
  canvas.addEventListener('pointerdown', down);
  window.addEventListener('pointermove', move);
  window.addEventListener('pointerup', up);
  return () => {
    canvas.removeEventListener('wheel', wheel);
    canvas.removeEventListener('pointerdown', down);
    window.removeEventListener('pointermove', move);
    window.removeEventListener('pointerup', up);
  };
}

/** Antialiased point sprites per colour: a solid core with a one-pixel soft
 * edge, so thousands of `drawImage` calls read as points rather than squares. */
export function createSpriteCache(): { get(rgb: [number, number, number]): HTMLCanvasElement } {
  const cache = new Map<string, HTMLCanvasElement>();
  return {
    get(rgb) {
      const quantized = rgb.map((channel) => Math.round(channel / 6) * 6) as [number, number, number];
      const key = quantized.join(',');
      let sprite = cache.get(key);
      if (sprite) return sprite;
      sprite = document.createElement('canvas');
      sprite.width = sprite.height = 16;
      const context = sprite.getContext('2d');
      if (context) {
        const gradient = context.createRadialGradient(8, 8, 0, 8, 8, 8);
        gradient.addColorStop(0, rgbaString(quantized, 1));
        gradient.addColorStop(0.4, rgbaString(quantized, 0.85));
        gradient.addColorStop(0.75, rgbaString(quantized, 0.22));
        gradient.addColorStop(1, rgbaString(quantized, 0));
        context.fillStyle = gradient;
        context.fillRect(0, 0, 16, 16);
      }
      cache.set(key, sprite);
      return sprite;
    },
  };
}

export function rgbaString([r, g, b]: [number, number, number], alpha: number): string {
  return `rgba(${r}, ${g}, ${b}, ${Math.max(0, Math.min(1, alpha)).toFixed(3)})`;
}

/** The substrate with its 32 px minor and 128 px major graticule, anchored
 * to the field origin so it pans with the camera. */
export function drawGraticule(
  context: CanvasRenderingContext2D,
  width: number,
  height: number,
  camera: CameraState,
  colors: FieldPalette,
): void {
  context.fillStyle = rgbaString(colors.substrate, 1);
  context.fillRect(0, 0, width, height);
  context.lineWidth = 1;
  const [ox, oy] = toScreen(camera, 0, 0);
  for (const [step, color, alpha] of [
    [32, colors.grid, 0.55],
    [128, colors.gridMajor, 0.5],
  ] as const) {
    context.strokeStyle = rgbaString(color, alpha);
    context.beginPath();
    for (let x = ((ox % step) + step) % step; x < width; x += step) {
      context.moveTo(Math.round(x) + 0.5, 0);
      context.lineTo(Math.round(x) + 0.5, height);
    }
    for (let y = ((oy % step) + step) % step; y < height; y += step) {
      context.moveTo(0, Math.round(y) + 0.5);
      context.lineTo(width, Math.round(y) + 0.5);
    }
    context.stroke();
  }
}

export function drawColumns(
  context: CanvasRenderingContext2D,
  width: number,
  height: number,
  camera: CameraState,
  columns: readonly SceneColumn[],
  colors: FieldPalette,
): void {
  context.save();
  context.font = `500 10px ${colors.labelFont}`;
  context.textAlign = 'center';
  for (let index = 0; index <= columns.length; index += 1) {
    const [x] = toScreen(camera, index - 0.5, 0);
    if (x < -2 || x > width + 2) continue;
    context.strokeStyle = rgbaString(colors.edgeStrong, index === 0 || index === columns.length ? 0.35 : 0.22);
    context.setLineDash(index === 0 || index === columns.length ? [] : [2, 4]);
    context.beginPath();
    context.moveTo(Math.round(x) + 0.5, 34);
    context.lineTo(Math.round(x) + 0.5, height - 8);
    context.stroke();
  }
  context.setLineDash([]);
  // Narrow columns keep the bound and the count; the column name is the
  // first thing dropped. Columns too narrow for their bounds print no header
  // at all rather than overprinting; the registry readout beside the field
  // prints the same bounds and counts.
  const spacing = camera.scale;
  context.letterSpacing = '0.16em';
  const widest = Math.max(...columns.map((column) => context.measureText(column.bound.toUpperCase()).width));
  context.letterSpacing = '0px';
  if (spacing < widest + 8) {
    context.restore();
    return;
  }
  columns.forEach((column, index) => {
    const [x] = toScreen(camera, index, 0);
    if (x < -80 || x > width + 80) return;
    context.fillStyle = rgbaString(colors.ink, 0.86);
    engraved(context, column.bound.toUpperCase(), x, 16);
    context.fillStyle = rgbaString(colors.inkMuted, 0.9);
    context.fillText(spacing < 120 ? String(column.count) : `${column.label} · ${column.count}`, x, 29);
  });
  context.restore();
}

export function drawMassAxis(
  context: CanvasRenderingContext2D,
  camera: CameraState,
  extent: FieldScene['extent'],
  height: number,
  colors: FieldPalette,
): void {
  const [, top] = toScreen(camera, 0, extent.y[1]);
  const [, bottom] = toScreen(camera, 0, extent.y[0]);
  const x = 22;
  context.strokeStyle = rgbaString(colors.edgeStrong, 0.55);
  context.lineWidth = 1;
  context.beginPath();
  context.moveTo(x + 0.5, Math.max(48, top + 12));
  context.lineTo(x + 0.5, Math.min(height - 16, bottom - 12));
  context.stroke();
  context.save();
  context.font = `500 9px ${colors.labelFont}`;
  context.fillStyle = rgbaString(colors.inkMuted, 0.9);
  context.translate(x - 7, (top + bottom) / 2);
  context.rotate(-Math.PI / 2);
  context.textAlign = 'center';
  context.fillText('INDEXED MASS  ·  LOG  ↑', 0, 0);
  context.restore();
}

/** Quadratic control point; curvature is a property of the relation kind. */
export function bow(ax: number, ay: number, bx: number, by: number, relation: string): [number, number] {
  const k = relation === 'checkout' ? 0.16 : relation === 'contains' ? 0 : 0.08;
  return [(ax + bx) / 2 - (by - ay) * k, (ay + by) / 2 + (bx - ax) * k];
}

/** An engraved legend: uppercase at the design system's 0.16em tracking. */
export function engraved(context: CanvasRenderingContext2D, text: string, x: number, y: number): void {
  context.letterSpacing = '0.16em';
  context.fillText(text, x, y);
  context.letterSpacing = '0px';
}
