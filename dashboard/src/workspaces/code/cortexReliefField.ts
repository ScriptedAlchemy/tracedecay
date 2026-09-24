/**
 * The Cortex relief field.
 *
 * Symbols sit inside the hull of the directory that holds their file;
 * directories are packed so that those sharing drawn relations sit together.
 * Under each hull lies a relief whose height is the density of drawn relation
 * endpoints among THAT directory's own symbols, a Gaussian sum weighted by the
 * relations each carries, sampled and drawn only inside its hull. Two packed
 * directories therefore never share a ridge: every rise is coupling measured
 * inside one directory. Contours sit at one printed interval shared by every
 * hull, so heights compare across directories; the hillshade is the same
 * surface lit from the upper left.
 *
 * Relations inside a directory are straight hairlines. Relations between
 * directories are aggregated per directory pair into one trunk between gates
 * on the facing hull boundaries; a trunk's width and brightness are its
 * measured relation count, which is also printed on it.
 *
 * Labels are a hierarchy: directory names and hub symbols (served degree at
 * or above the slice's 90th percentile at Fit, 75th from 1.4x) in the display
 * face; every other symbol only on hover, focus, selection or past 2x zoom. A label that
 * collides is dropped, never overprinted; the ledger names every symbol.
 */
import {
  anchorsAround,
  degreeRadius,
  displayFont,
  drawHaloLabel,
  drawStateMarks,
  drawSymbolBody,
  hubDegree,
  modulesOf,
  monoFont,
  placeLabels,
  project,
  type Bounds,
  type CortexPainter,
  type CortexScene,
  type LabelCandidate,
  type PaintFrame,
} from './cortexScene.ts';

/** World spacing between neighbouring symbols. */
const S = 24;
const GOLDEN_ANGLE = Math.PI * (3 - Math.sqrt(5));
/** Gaussian kernel of the relief, in world units. */
export const RELIEF_SIGMA = S * 1.1;
const CELL = S / 3;

export interface ReliefModule {
  readonly module: string;
  readonly count: number;
  readonly cx: number;
  readonly cy: number;
  readonly radius: number;
  /** Closed boundary: the members' padded support hull. */
  readonly hull: readonly { x: number; y: number }[];
}

export interface ContourLevel {
  readonly level: number;
  readonly index: boolean;
  /** Flat x0,y0,x1,y1 segments in world units. */
  readonly segments: Float32Array;
}

export interface ReliefSurface {
  readonly cols: number;
  readonly rows: number;
  readonly x0: number;
  readonly y0: number;
  readonly cell: number;
  readonly values: Float32Array;
  readonly max: number;
  readonly contours: readonly ContourLevel[];
}

/** Relations between one pair of directories, drawn as one trunk. */
export interface ReliefTrunk {
  readonly a: ReliefModule;
  readonly b: ReliefModule;
  readonly count: number;
}

export interface ReliefLayout {
  readonly positions: ReadonlyMap<string, { x: number; y: number }>;
  readonly moduleOf: ReadonlyMap<string, ReliefModule>;
  readonly modules: readonly ReliefModule[];
  /** One relief per directory, from its own members only. */
  readonly surfaces: ReadonlyMap<ReliefModule, ReliefSurface>;
  /** The contour interval every hull shares. */
  readonly interval: number;
  readonly trunks: readonly ReliefTrunk[];
  readonly bounds: Bounds;
}

/* ---- layout -------------------------------------------------------------- */

function moduleRadius(count: number): number {
  return S * 0.95 * Math.sqrt(count) + S * 0.9;
}

/** Module centres: packed without overlap, drawn together by shared relations. */
function packModules(
  groups: readonly { module: string; members: readonly { id: string }[] }[],
  scene: CortexScene,
  aspect: number,
): { cx: number; cy: number; radius: number }[] {
  const index = new Map<string, number>();
  groups.forEach((group, i) => group.members.forEach((member) => index.set(member.id, i)));
  const n = groups.length;
  const weight = Array.from({ length: n }, () => new Float64Array(n));
  for (const edge of scene.edges) {
    const a = index.get(edge.source)!;
    const b = index.get(edge.target)!;
    if (a !== b) {
      weight[a]![b]! += 1;
      weight[b]![a]! += 1;
    }
  }
  const radius = groups.map((group) => moduleRadius(group.members.length));
  const x = new Float64Array(n);
  const y = new Float64Array(n);
  // The packing leans to the box's aspect: a wide field spreads modules
  // sideways. Only the pull to the origin is anisotropic; distances are not.
  const stretch = Math.sqrt(Math.max(0.5, Math.min(3, aspect)));
  for (let i = 0; i < n; i += 1) {
    const r = S * 3.2 * Math.sqrt(i);
    x[i] = Math.cos(i * GOLDEN_ANGLE) * r * stretch;
    y[i] = (Math.sin(i * GOLDEN_ANGLE) * r) / stretch;
  }
  const gx = 1 - 0.008 / stretch;
  const gy = 1 - 0.008 * stretch;
  const gap = S * 1.1;
  for (let iteration = 0; iteration < 500; iteration += 1) {
    for (let i = 0; i < n; i += 1) {
      for (let j = i + 1; j < n; j += 1) {
        const dx = x[j]! - x[i]!;
        const dy = y[j]! - y[i]!;
        const d = Math.max(1e-3, Math.hypot(dx, dy));
        const rest = radius[i]! + radius[j]! + gap;
        let move = 0;
        if (d < rest) move = -(rest - d) / 2;
        else if (weight[i]![j]! > 0) move = (d - rest) * 0.03 * Math.min(1, weight[i]![j]! / 3);
        const ux = (dx / d) * move;
        const uy = (dy / d) * move;
        x[i] = x[i]! + ux;
        y[i] = y[i]! + uy;
        x[j] = x[j]! - ux;
        y[j] = y[j]! - uy;
      }
      // A weak pull to the origin keeps unrelated modules from drifting off.
      x[i] = x[i]! * gx;
      y[i] = y[i]! * gy;
    }
  }
  // Stretching centres sideways only ever increases separation, so it can
  // bring the packing to the box's aspect without creating an overlap.
  let left = Infinity;
  let right = -Infinity;
  let top = Infinity;
  let bottom = -Infinity;
  for (let i = 0; i < n; i += 1) {
    left = Math.min(left, x[i]! - radius[i]!);
    right = Math.max(right, x[i]! + radius[i]!);
    top = Math.min(top, y[i]! - radius[i]!);
    bottom = Math.max(bottom, y[i]! + radius[i]!);
  }
  const current = (right - left) / Math.max(1, bottom - top);
  const widen = Math.min(2.2, Math.max(1, (aspect * 0.9) / current));
  return groups.map((_, i) => ({ cx: x[i]! * widen, cy: y[i]!, radius: radius[i]! }));
}

/**
 * Members start on a sunflower, degree-ranked from the centre, then relax:
 * they repel each other, drawn relations inside the module pull their ends
 * together, and a relation leaving the module leans its end toward the other
 * module, so bundles leave from the side they are headed.
 */
function placeMembers(
  scene: CortexScene,
  groups: readonly { module: string; members: readonly { id: string }[] }[],
  centres: readonly { cx: number; cy: number; radius: number }[],
): Map<string, { x: number; y: number }> {
  const positions = new Map<string, { x: number; y: number }>();
  const groupOf = new Map<string, number>();
  groups.forEach((group, g) => {
    const { cx, cy, radius } = centres[g]!;
    const inner = radius - S * 0.75;
    const spread = inner / Math.sqrt(Math.max(1, group.members.length));
    group.members.forEach((member, i) => {
      groupOf.set(member.id, g);
      const r = group.members.length === 1 ? 0 : spread * Math.sqrt(i + 0.5);
      positions.set(member.id, {
        x: cx + Math.cos(i * GOLDEN_ANGLE) * r,
        y: cy + Math.sin(i * GOLDEN_ANGLE) * r,
      });
    });
  });
  for (let iteration = 0; iteration < 160; iteration += 1) {
    for (const group of groups) {
      const members = group.members;
      for (let i = 0; i < members.length; i += 1) {
        for (let j = i + 1; j < members.length; j += 1) {
          const a = positions.get(members[i]!.id)!;
          const b = positions.get(members[j]!.id)!;
          const dx = b.x - a.x;
          const dy = b.y - a.y;
          const d = Math.max(1e-3, Math.hypot(dx, dy));
          if (d < S) {
            const push = (S - d) / 2;
            a.x -= (dx / d) * push;
            a.y -= (dy / d) * push;
            b.x += (dx / d) * push;
            b.y += (dy / d) * push;
          }
        }
      }
    }
    for (const edge of scene.edges) {
      const gs = groupOf.get(edge.source)!;
      const gt = groupOf.get(edge.target)!;
      const a = positions.get(edge.source)!;
      const b = positions.get(edge.target)!;
      if (gs === gt) {
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const d = Math.max(1e-3, Math.hypot(dx, dy));
        const pull = (d - S * 1.25) * 0.02;
        a.x += (dx / d) * pull;
        a.y += (dy / d) * pull;
        b.x -= (dx / d) * pull;
        b.y -= (dy / d) * pull;
      } else {
        for (const [p, other] of [
          [a, centres[gt]!],
          [b, centres[gs]!],
        ] as const) {
          const dx = other.cx - p.x;
          const dy = other.cy - p.y;
          const d = Math.max(1e-3, Math.hypot(dx, dy));
          p.x += (dx / d) * 0.35;
          p.y += (dy / d) * 0.35;
        }
      }
    }
    groups.forEach((group, g) => {
      const { cx, cy, radius } = centres[g]!;
      const inner = radius - S * 0.75;
      for (const member of group.members) {
        const p = positions.get(member.id)!;
        const d = Math.hypot(p.x - cx, p.y - cy);
        if (d > inner) {
          p.x = cx + ((p.x - cx) / d) * inner;
          p.y = cy + ((p.y - cy) / d) * inner;
        }
      }
    });
  }
  return positions;
}

/** Padded support hull: at each bearing, the farthest member plus padding. */
function supportHull(
  points: readonly { x: number; y: number }[],
  cx: number,
  cy: number,
  pad: number,
): { x: number; y: number }[] {
  const steps = 48;
  const radii: number[] = [];
  for (let step = 0; step < steps; step += 1) {
    const angle = (step / steps) * Math.PI * 2;
    const ux = Math.cos(angle);
    const uy = Math.sin(angle);
    let reach = 0;
    for (const p of points) reach = Math.max(reach, (p.x - cx) * ux + (p.y - cy) * uy);
    radii.push(reach + pad);
  }
  // One smoothing pass rounds the corners the support function leaves.
  const smooth = radii.map(
    (r, i) => (radii[(i + steps - 1) % steps]! + 2 * r + radii[(i + 1) % steps]!) / 4,
  );
  return smooth.map((r, step) => {
    const angle = (step / steps) * Math.PI * 2;
    return { x: cx + Math.cos(angle) * r, y: cy + Math.sin(angle) * r };
  });
}

/** A readable contour interval: 1, 2 or 5 times a power of ten, ~10 levels. */
export function contourInterval(max: number): number {
  const raw = Math.max(max / 10, 1);
  const power = 10 ** Math.floor(Math.log10(raw));
  for (const step of [1, 2, 5, 10]) if (raw <= step * power) return step * power;
  return 10 * power;
}

/**
 * The relief: at each grid cell, the sum over symbols of (drawn edges at that
 * symbol) × a unit-peak Gaussian. One isolated endpoint peaks at exactly 1.
 */
export function reliefSurface(
  weighted: readonly { x: number; y: number; weight: number }[],
  bounds: Bounds,
  sigma: number = RELIEF_SIGMA,
  cell: number = CELL,
): Omit<ReliefSurface, 'contours'> {
  const cols = Math.max(2, Math.ceil((bounds.x1 - bounds.x0) / cell) + 1);
  const rows = Math.max(2, Math.ceil((bounds.y1 - bounds.y0) / cell) + 1);
  const values = new Float32Array(cols * rows);
  const reach = Math.ceil((3 * sigma) / cell);
  const inv = 1 / (2 * sigma * sigma);
  for (const point of weighted) {
    if (point.weight <= 0) continue;
    const ci = Math.round((point.x - bounds.x0) / cell);
    const cj = Math.round((point.y - bounds.y0) / cell);
    for (let j = Math.max(0, cj - reach); j <= Math.min(rows - 1, cj + reach); j += 1) {
      const wy = bounds.y0 + j * cell - point.y;
      for (let i = Math.max(0, ci - reach); i <= Math.min(cols - 1, ci + reach); i += 1) {
        const wx = bounds.x0 + i * cell - point.x;
        values[j * cols + i]! += point.weight * Math.exp(-(wx * wx + wy * wy) * inv);
      }
    }
  }
  let max = 0;
  for (const value of values) max = Math.max(max, value);
  return { cols, rows, x0: bounds.x0, y0: bounds.y0, cell, values, max };
}

/** Marching squares at one level; segments in world units, linearly interpolated. */
export function contourSegments(
  surface: Pick<ReliefSurface, 'cols' | 'rows' | 'x0' | 'y0' | 'cell' | 'values'>,
  level: number,
): Float32Array {
  const { cols, rows, x0, y0, cell, values } = surface;
  const out: number[] = [];
  const at = (i: number, j: number) => values[j * cols + i]!;
  const lerp = (a: number, b: number) => (a === b ? 0.5 : (level - a) / (b - a));
  for (let j = 0; j < rows - 1; j += 1) {
    for (let i = 0; i < cols - 1; i += 1) {
      const tl = at(i, j);
      const tr = at(i + 1, j);
      const br = at(i + 1, j + 1);
      const bl = at(i, j + 1);
      const code =
        (tl >= level ? 8 : 0) | (tr >= level ? 4 : 0) | (br >= level ? 2 : 0) | (bl >= level ? 1 : 0);
      if (code === 0 || code === 15) continue;
      const x = x0 + i * cell;
      const y = y0 + j * cell;
      const top = [x + lerp(tl, tr) * cell, y] as const;
      const right = [x + cell, y + lerp(tr, br) * cell] as const;
      const bottom = [x + lerp(bl, br) * cell, y + cell] as const;
      const left = [x, y + lerp(tl, bl) * cell] as const;
      const push = (a: readonly [number, number], b: readonly [number, number]) =>
        out.push(a[0], a[1], b[0], b[1]);
      switch (code) {
        case 1:
        case 14:
          push(left, bottom);
          break;
        case 2:
        case 13:
          push(bottom, right);
          break;
        case 3:
        case 12:
          push(left, right);
          break;
        case 4:
        case 11:
          push(top, right);
          break;
        case 6:
        case 9:
          push(top, bottom);
          break;
        case 7:
        case 8:
          push(left, top);
          break;
        case 5:
          push(left, top);
          push(bottom, right);
          break;
        case 10:
          push(top, right);
          push(left, bottom);
          break;
        default:
          break;
      }
    }
  }
  return new Float32Array(out);
}

export function reliefLayout(scene: CortexScene, aspect = 2): ReliefLayout {
  const groups = modulesOf(scene);
  const centres = packModules(groups, scene, aspect);
  const positions = placeMembers(scene, groups, centres);
  const modules: ReliefModule[] = groups.map((group, g) => {
    const { cx, cy, radius } = centres[g]!;
    const points = group.members.map((member) => positions.get(member.id)!);
    const mx = points.reduce((sum, p) => sum + p.x, 0) / points.length;
    const my = points.reduce((sum, p) => sum + p.y, 0) / points.length;
    return {
      module: group.module,
      count: group.members.length,
      cx,
      cy,
      radius,
      hull: supportHull(points, mx, my, S * 0.85),
    };
  });
  const moduleOf = new Map<string, ReliefModule>();
  groups.forEach((group, g) => group.members.forEach((m) => moduleOf.set(m.id, modules[g]!)));

  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const module of modules) {
    for (const p of module.hull) {
      x0 = Math.min(x0, p.x);
      y0 = Math.min(y0, p.y);
      x1 = Math.max(x1, p.x);
      y1 = Math.max(y1, p.y);
    }
  }
  // The frame is the hulls plus room for the directory label above each.
  const bounds = { x0: x0 - S * 0.3, y0: y0 - S * 0.9, x1: x1 + S * 0.3, y1: y1 + S * 0.3 };

  const incident = new Map<string, number>();
  const pairs = new Map<string, ReliefTrunk & { count: number }>();
  for (const edge of scene.edges) {
    incident.set(edge.source, (incident.get(edge.source) ?? 0) + 1);
    incident.set(edge.target, (incident.get(edge.target) ?? 0) + 1);
    const a = moduleOf.get(edge.source)!;
    const b = moduleOf.get(edge.target)!;
    if (a === b) continue;
    const [first, second] = a.module < b.module ? [a, b] : [b, a];
    const key = `${first.module}\u0000${second.module}`;
    const trunk = pairs.get(key) ?? { a: first, b: second, count: 0 };
    trunk.count += 1;
    pairs.set(key, trunk);
  }

  const bases = new Map<ReliefModule, Omit<ReliefSurface, 'contours'>>();
  let max = 0;
  groups.forEach((group, g) => {
    const module = modules[g]!;
    let hx0 = Infinity;
    let hy0 = Infinity;
    let hx1 = -Infinity;
    let hy1 = -Infinity;
    for (const p of module.hull) {
      hx0 = Math.min(hx0, p.x);
      hy0 = Math.min(hy0, p.y);
      hx1 = Math.max(hx1, p.x);
      hy1 = Math.max(hy1, p.y);
    }
    const base = reliefSurface(
      group.members.map((member) => ({
        ...positions.get(member.id)!,
        weight: incident.get(member.id) ?? 0,
      })),
      { x0: hx0, y0: hy0, x1: hx1, y1: hy1 },
    );
    bases.set(module, base);
    max = Math.max(max, base.max);
  });
  const interval = contourInterval(max);
  const surfaces = new Map<ReliefModule, ReliefSurface>();
  for (const [module, base] of bases) {
    const contours: ContourLevel[] = [];
    for (let n = 1; n * interval < base.max; n += 1) {
      contours.push({ level: n * interval, index: n % 5 === 0, segments: contourSegments(base, n * interval) });
    }
    surfaces.set(module, { ...base, contours });
  }
  return {
    positions,
    moduleOf,
    modules,
    surfaces,
    interval,
    trunks: [...pairs.values()].sort((p, q) => q.count - p.count),
    bounds,
  };
}

/* ---- paint --------------------------------------------------------------- */

interface ShadeCache {
  readonly tint: HTMLCanvasElement;
  readonly light: HTMLCanvasElement;
  readonly shadow: HTMLCanvasElement;
}

const shadeCache = new WeakMap<ReliefSurface, ShadeCache>();

/** Relief tint and hillshade as three alpha masks at grid resolution. */
function shading(surface: ReliefSurface): ShadeCache | null {
  const cached = shadeCache.get(surface);
  if (cached) return cached;
  if (typeof document === 'undefined') return null;
  const { cols, rows, values, max } = surface;
  const make = () => {
    const canvas = document.createElement('canvas');
    canvas.width = cols;
    canvas.height = rows;
    return canvas;
  };
  const tint = make();
  const light = make();
  const shadow = make();
  const tctx = tint.getContext('2d');
  const lctx = light.getContext('2d');
  const sctx = shadow.getContext('2d');
  if (!tctx || !lctx || !sctx || max <= 0) return null;
  const timg = tctx.createImageData(cols, rows);
  const limg = lctx.createImageData(cols, rows);
  const simg = sctx.createImageData(cols, rows);
  // Light from the upper left, 45 degrees up; relief exaggerated to read.
  const lx = -0.5;
  const ly = -0.5;
  const lz = 0.707;
  const exaggeration = 26 / max;
  for (let j = 0; j < rows; j += 1) {
    for (let i = 0; i < cols; i += 1) {
      const v = values[j * cols + i]!;
      const dzdx =
        (values[j * cols + Math.min(cols - 1, i + 1)]! - values[j * cols + Math.max(0, i - 1)]!) *
        exaggeration;
      const dzdy =
        (values[Math.min(rows - 1, j + 1) * cols + i]! - values[Math.max(0, j - 1) * cols + i]!) *
        exaggeration;
      const norm = Math.hypot(dzdx, dzdy, 1);
      const lambert = (-dzdx * lx - dzdy * ly + lz) / norm - lz;
      const o = (j * cols + i) * 4;
      timg.data[o + 3] = Math.round(255 * Math.pow(v / max, 0.75));
      limg.data[o + 3] = Math.round(255 * Math.max(0, lambert) * 2.2);
      simg.data[o + 3] = Math.round(255 * Math.max(0, -lambert) * 2.2);
      for (const img of [timg, limg, simg]) {
        img.data[o] = 255;
        img.data[o + 1] = 255;
        img.data[o + 2] = 255;
      }
    }
  }
  tctx.putImageData(timg, 0, 0);
  lctx.putImageData(limg, 0, 0);
  sctx.putImageData(simg, 0, 0);
  const result = { tint, light, shadow };
  shadeCache.set(surface, result);
  return result;
}

/** Colour an alpha mask in place with one token colour. */
function tintMask(mask: HTMLCanvasElement, color: string): HTMLCanvasElement | null {
  const out = document.createElement('canvas');
  out.width = mask.width;
  out.height = mask.height;
  const ctx = out.getContext('2d');
  if (!ctx) return null;
  ctx.drawImage(mask, 0, 0);
  ctx.globalCompositeOperation = 'source-in';
  ctx.fillStyle = color;
  ctx.fillRect(0, 0, out.width, out.height);
  return out;
}

const coloured = new WeakMap<HTMLCanvasElement, Map<string, HTMLCanvasElement>>();
function colouredMask(mask: HTMLCanvasElement, color: string): HTMLCanvasElement | null {
  let byColor = coloured.get(mask);
  if (!byColor) {
    byColor = new Map();
    coloured.set(mask, byColor);
  }
  const hit = byColor.get(color);
  if (hit) return hit;
  const made = tintMask(mask, color);
  if (made) byColor.set(color, made);
  return made;
}

const EDGE_DASH: Record<string, number[]> = {
  calls: [],
  references: [5, 4],
  contains: [1.5, 3],
};

function zoomOf(frame: { camera: { k: number }; fitK: number }): number {
  return frame.camera.k / frame.fitK;
}

function screenRadius(scene: CortexScene, id: string, frame: { camera: { k: number }; fitK: number }) {
  const node = scene.byId.get(id);
  const z = Math.min(1.8, Math.max(0.8, Math.sqrt(zoomOf(frame))));
  // The ceiling shrinks as the slice grows, so 250 bodies do not fuse.
  const ceiling = scene.nodes.length > 120 ? 7 : 9.5;
  return degreeRadius(node?.degree ?? null, scene.maxDegree, { min: 2.4, max: ceiling }) * z;
}

function hullPath(ctx: CanvasRenderingContext2D, module: ReliefModule, frame: PaintFrame): void {
  ctx.beginPath();
  module.hull.forEach((p, i) => {
    const q = project(frame.camera, p.x, p.y);
    if (i === 0) ctx.moveTo(q.x, q.y);
    else ctx.lineTo(q.x, q.y);
  });
  ctx.closePath();
}

/** Each directory's own relief, clipped to its hull. */
function drawRelief(layout: ReliefLayout, frame: PaintFrame): void {
  const { ctx, camera, palette } = frame;
  for (const module of layout.modules) {
    const surface = layout.surfaces.get(module)!;
    ctx.save();
    hullPath(ctx, module, frame);
    ctx.clip();
    const shade = shading(surface);
    if (shade) {
      const origin = project(camera, surface.x0 - surface.cell / 2, surface.y0 - surface.cell / 2);
      const w = surface.cols * surface.cell * camera.k;
      const h = surface.rows * surface.cell * camera.k;
      ctx.imageSmoothingEnabled = true;
      ctx.imageSmoothingQuality = 'high';
      const tint = colouredMask(shade.tint, palette.edge);
      const light = colouredMask(shade.light, palette.text);
      const shadow = colouredMask(shade.shadow, '#000');
      // The tint is scaled to the shared interval's range, so a low hull
      // reads low next to a high one.
      ctx.globalAlpha = 0.36 * Math.min(1, surface.max / Math.max(layout.interval * 3, 1e-6));
      if (tint) ctx.drawImage(tint, origin.x, origin.y, w, h);
      ctx.globalAlpha = 0.2;
      if (light) ctx.drawImage(light, origin.x, origin.y, w, h);
      ctx.globalAlpha = 0.45;
      if (shadow) ctx.drawImage(shadow, origin.x, origin.y, w, h);
    }
    ctx.lineCap = 'round';
    ctx.strokeStyle = palette.edge;
    for (const contour of surface.contours) {
      ctx.globalAlpha = contour.index ? 0.8 : 0.42;
      ctx.lineWidth = contour.index ? 1.2 : 0.7;
      ctx.beginPath();
      const s = contour.segments;
      for (let i = 0; i < s.length; i += 4) {
        ctx.moveTo(s[i]! * camera.k + camera.tx, s[i + 1]! * camera.k + camera.ty);
        ctx.lineTo(s[i + 2]! * camera.k + camera.tx, s[i + 3]! * camera.k + camera.ty);
      }
      ctx.stroke();
    }
    ctx.restore();
    ctx.save();
    hullPath(ctx, module, frame);
    ctx.globalAlpha = 0.95;
    ctx.strokeStyle = palette.dim;
    ctx.lineWidth = 1;
    ctx.stroke();
    ctx.restore();
  }
}

function gate(from: ReliefModule, to: ReliefModule): { x: number; y: number } {
  const dx = to.cx - from.cx;
  const dy = to.cy - from.cy;
  const d = Math.max(1e-3, Math.hypot(dx, dy));
  return { x: from.cx + (dx / d) * from.radius * 0.85, y: from.cy + (dy / d) * from.radius * 0.85 };
}

/**
 * Relations. Inside a directory: a hairline per relation. Across: one trunk
 * per directory pair whose width and brightness are its relation count, fed
 * by a faint fibre from each symbol to its gate.
 */
function drawRelations(layout: ReliefLayout, scene: CortexScene, frame: PaintFrame): LabelCandidate[] {
  const { ctx, camera, palette, emphasis } = frame;
  const z = zoomOf(frame);
  const lifted = liftedBySelection(frame);
  const quiet = Math.min(1, Math.sqrt(120 / Math.max(120, scene.edges.length)));
  const counts: LabelCandidate[] = [];
  ctx.save();
  ctx.lineCap = 'round';
  for (const edge of scene.edges) {
    const a = layout.positions.get(edge.source)!;
    const b = layout.positions.get(edge.target)!;
    const ma = layout.moduleOf.get(edge.source)!;
    const mb = layout.moduleOf.get(edge.target)!;
    const lit = emphasis !== null && emphasis.has(edge.source) && emphasis.has(edge.target);
    const dimmed = emphasis !== null && !lit;
    ctx.strokeStyle = lit ? (lifted ? palette.accent : palette.text) : palette.edge;
    ctx.globalAlpha = dimmed ? 0.05 : lit ? 0.75 : (z >= 2.5 ? 0.55 : 0.3) * quiet;
    ctx.lineWidth = lit ? 1.3 : 0.8;
    ctx.setLineDash(z >= 2.5 || lit ? (EDGE_DASH[edge.kind] ?? []) : []);
    const p0 = project(camera, a.x, a.y);
    const p3 = project(camera, b.x, b.y);
    ctx.beginPath();
    ctx.moveTo(p0.x, p0.y);
    if (ma === mb) {
      ctx.lineTo(p3.x, p3.y);
    } else {
      const g1 = project(camera, gate(ma, mb).x, gate(ma, mb).y);
      const g2 = project(camera, gate(mb, ma).x, gate(mb, ma).y);
      if (lit) {
        ctx.bezierCurveTo(g1.x, g1.y, g2.x, g2.y, p3.x, p3.y);
      } else {
        // A fibre to each gate; the trunk between gates is drawn once below.
        ctx.lineTo(g1.x, g1.y);
        ctx.moveTo(p3.x, p3.y);
        ctx.lineTo(g2.x, g2.y);
      }
    }
    ctx.stroke();
  }
  ctx.setLineDash([]);
  const busiest = layout.trunks[0]?.count ?? 1;
  for (const trunk of layout.trunks) {
    const g1 = project(camera, gate(trunk.a, trunk.b).x, gate(trunk.a, trunk.b).y);
    const g2 = project(camera, gate(trunk.b, trunk.a).x, gate(trunk.b, trunk.a).y);
    const share = trunk.count / busiest;
    ctx.strokeStyle = palette.edge;
    ctx.globalAlpha = (emphasis !== null ? 0.25 : 1) * (0.3 + 0.6 * Math.sqrt(share));
    ctx.lineWidth = 1 + 4 * Math.sqrt(share);
    ctx.beginPath();
    ctx.moveTo(g1.x, g1.y);
    ctx.lineTo(g2.x, g2.y);
    ctx.stroke();
    ctx.strokeStyle = palette.text;
    ctx.globalAlpha *= 0.35 * share;
    ctx.lineWidth = Math.max(0.6, ctx.lineWidth * 0.35);
    ctx.stroke();
    const text = `${trunk.count}`;
    ctx.font = monoFont(10, 500);
    const width = ctx.measureText(text).width + 6;
    counts.push({ id: `trunk:${trunk.a.module}>${trunk.b.module}:${text}`, x: (g1.x + g2.x) / 2 - width / 2, y: (g1.y + g2.y) / 2 - 7, width, height: 14 });
  }
  ctx.restore();
  return counts;
}

/** Selection lifts its neighbourhood only while nothing is hovered or focused. */
function liftedBySelection(frame: PaintFrame): boolean {
  return frame.selected !== null && frame.hovered === null && frame.cursor === null;
}

function drawSymbols(layout: ReliefLayout, scene: CortexScene, frame: PaintFrame, trunkCounts: LabelCandidate[]): void {
  const { ctx, camera, palette, emphasis } = frame;
  const z = zoomOf(frame);
  // At Fit only the top tenth by degree; from 1.4x the top quarter.
  const hub = hubDegree(scene, z >= 1.4 ? 0.75 : 0.9);
  const lifted = liftedBySelection(frame);
  const obstacles: LabelCandidate[] = [];

  if (lifted && frame.selected !== null) {
    const world = layout.positions.get(frame.selected);
    if (world) {
      // One restrained halo: the selection's neighbourhood is lit, not glowing.
      const p = project(camera, world.x, world.y);
      const r = screenRadius(scene, frame.selected, frame) * 5;
      const halo = ctx.createRadialGradient(p.x, p.y, 0, p.x, p.y, r);
      halo.addColorStop(0, palette.accent);
      halo.addColorStop(1, 'transparent');
      ctx.save();
      ctx.globalAlpha = 0.16;
      ctx.fillStyle = halo;
      ctx.fillRect(p.x - r, p.y - r, 2 * r, 2 * r);
      ctx.restore();
    }
  }

  const labels: {
    positions: LabelCandidate[];
    priority: number;
    strong: boolean;
    text: string;
    font: string;
    alpha: number;
    color: string;
  }[] = [];
  for (const module of layout.modules) {
    let top = module.hull[0]!;
    for (const p of module.hull) if (p.y < top.y) top = p;
    const cx = module.hull.reduce((sum, p) => sum + p.x, 0) / module.hull.length;
    const at = project(camera, cx, top.y);
    const text = `${module.module} · ${module.count}`;
    ctx.font = monoFont(10, 500);
    const width = ctx.measureText(text).width;
    labels.push({
      positions: [{ id: `module:${module.module}`, x: at.x - width / 2, y: at.y - 16, width, height: 14 }],
      priority: 2e6 + module.count,
      strong: true,
      text,
      font: monoFont(10, 500),
      alpha: emphasis !== null ? 0.45 : 0.9,
      color: palette.muted,
    });
  }
  for (const node of scene.nodes) {
    const world = layout.positions.get(node.id)!;
    const p = project(camera, world.x, world.y);
    const r = screenRadius(scene, node.id, frame);
    const dimmed = emphasis !== null && !emphasis.has(node.id);
    drawSymbolBody(ctx, node, p.x, p.y, r, { alpha: dimmed ? 0.18 : 1, palette });
    drawStateMarks(
      ctx,
      p.x,
      p.y,
      r,
      {
        selected: frame.selected === node.id,
        cursor: frame.cursor === node.id,
        heat: frame.heat(node.id),
      },
      palette,
    );
    obstacles.push({ id: node.id, x: p.x - r, y: p.y - r, width: 2 * r, height: 2 * r });
    const marked = frame.selected === node.id || frame.hovered === node.id || frame.cursor === node.id;
    const isHub = (node.degree ?? -1) >= hub;
    const inFocus = emphasis?.has(node.id) ?? false;
    if (!marked && !isHub && !inFocus && z < 2) continue;
    const display = marked || (isHub && !dimmed);
    const font = display ? displayFont(marked ? 13 : 12, marked ? 700 : 600) : monoFont(10);
    ctx.font = font;
    const width = ctx.measureText(node.label).width;
    labels.push({
      positions: anchorsAround(node.id, p.x, p.y, r, width, display ? 15 : 12),
      priority: (marked ? 1e6 : 0) + (inFocus ? 1e4 : 0) + (isHub ? 1e3 : 0) + (node.degree ?? 0),
      strong: display,
      text: node.label,
      font,
      alpha: dimmed ? 0.3 : display ? 1 : 0.78,
      color: display ? palette.text : palette.muted,
    });
  }
  labels.push(
    ...trunkCounts.map((box) => ({
      positions: [box],
      priority: -1,
      strong: false,
      text: box.id.slice(box.id.lastIndexOf(':') + 1),
      font: monoFont(10, 500),
      alpha: emphasis !== null ? 0.35 : 0.85,
      color: palette.text,
    })),
  );
  labels.sort((a, b) => b.priority - a.priority);
  // Directory names, hubs and marked symbols are culled only against each
  // other; leaf labels must also clear every body, so they never cover one.
  const strong = labels.filter((label) => label.strong);
  const leaves = labels.filter((label) => !label.strong);
  const keptStrong = placeLabels(strong.map((label) => label.positions));
  const keptLeaves = placeLabels(
    leaves.map((label) => label.positions),
    [...obstacles, ...keptStrong],
  );
  const kept = new Map([...keptStrong, ...keptLeaves].map((box) => [box.id, box]));
  for (const label of labels) {
    const box = kept.get(label.positions[0]!.id);
    if (!box) continue;
    drawHaloLabel(ctx, label.text, box.x, box.y + box.height / 2, {
      font: label.font,
      color: label.color,
      halo: palette.substrate,
      alpha: label.alpha,
    });
  }
}

export const reliefPainter: CortexPainter<ReliefLayout> = {
  name: 'relief field',
  fitPad: 20,
  layout: (scene, box) => reliefLayout(scene, box.width / Math.max(1, box.height)),
  bounds: (layout) => layout.bounds,
  position: (layout, id) => layout.positions.get(id) ?? null,
  hitRadius: (_layout, scene, id, frame) => screenRadius(scene, id, frame),
  draw(layout, scene, frame) {
    drawRelief(layout, frame);
    const trunkCounts = drawRelations(layout, scene, frame);
    drawSymbols(layout, scene, frame, trunkCounts);
    drawHaloLabel(
      frame.ctx,
      `contour interval ${layout.interval} endpoint${layout.interval === 1 ? '' : 's'} · index every 5th · per directory · σ ${Math.round(RELIEF_SIGMA)} u`,
      frame.width / 2,
      frame.height - 12,
      { font: monoFont(10), color: frame.palette.muted, halo: frame.palette.substrate, align: 'center' },
    );
  },
};
