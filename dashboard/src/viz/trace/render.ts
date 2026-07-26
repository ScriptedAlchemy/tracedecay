/**
 * Canvas2D renderer for the TRACE surface.
 *
 * Lifted from the round-two prototype's `render.js` and reduced to what the
 * neighbors endpoint can actually feed. This module draws and does nothing
 * else: it is handed positions, per-channel stretch, per-node bloom and a
 * resolved palette, and it paints one frame. It never integrates, never decides
 * how a body responds to a gesture, and never reads the clock — if a mark
 * moves, it is because the simulation moved it.
 *
 * Dropped from the prototype, deliberately: the dimmed cortex relief underlay.
 * Those region blobs are module rollups from a separate aggregation the
 * neighbors endpoint does not serve, and a shoreline drawn from guessed
 * membership would make cross-module calls look measured when they are not.
 * The field carries no underlay until the cortex endpoint exists.
 *
 * Canvas cannot read CSS custom properties, so the composing component resolves
 * the token block once per theme flip and hands the resolved strings in. That
 * keeps `tokens.css` the single source of the instrument's colour without a
 * `getComputedStyle` call inside the draw loop.
 */
import { kindColor } from '../graph/kindColor.ts';
import type { TraceFrame, TraceModel, TracePalette } from './types.ts';

/* ---- measurement → mark ------------------------------------------------- */

/** Channel width in px: the call-site count on that one edge. */
export function channelWidth(calls: number): number {
  return 2.2 + Math.sqrt(Math.max(0, calls)) * 1.15;
}

/** Node sill width in px: the symbol's degree, straight off the payload. */
export function sillWidth(degree: number | null): number {
  // An unmeasured degree gets the floor width and a hollow sill (see
  // `drawNodes`) — absence is drawn, never rendered as a measured zero.
  return 16 + Math.max(0, degree ?? 0) * 0.62;
}

/**
 * How wide a channel is at its head, as a fraction of its width at the mouth.
 *
 * The approved sheet tapers 0.78 → 1.0 and calls it hydrological; at that
 * depth, over a run this short, the two edges are within a pixel of parallel
 * and the ribbon reads as a machined bar with a hue on it. Direction is
 * supposed to be said twice — by hue and by taper — and only one of them was
 * audible. 0.55 makes the second one legible without letting the head fall
 * under the 2.2 px floor `channelWidth` sets for a single call site.
 */
export const CHANNEL_HEAD_FRACTION = 0.55;

/**
 * Width along a channel as a fraction of its width at the mouth, at `t` = 0
 * (head) through 1 (mouth).
 *
 * A straight line between two widths is a wedge, and a wedge is a machined
 * shape. Water is not: a watercourse gains width as the square root of the
 * flow it has accumulated, which is the standing exponent in hydraulic
 * geometry, and it is also — not coincidentally — the same square root
 * `channelWidth` above already puts between a call-site count and a width, and
 * that `markDiameter` puts between a symbol count and a mark on the Code
 * spine. So the taper is not a new law invented for this curve. It is the law
 * the field already uses for magnitude, applied along the run instead of
 * across it: accumulate flow linearly down the channel, then take its root.
 *
 * The consequence is a slightly convex edge — fuller at mid-run than a line
 * would be, easing as it nears the mouth — which is the profile of a
 * watercourse rather than a funnel.
 *
 * Both endpoints stay exact: `t = 0` returns `headFraction` and `t = 1`
 * returns 1, so the measured width at the mouth is drawn at the mouth and the
 * shaping happens strictly between two measurements, never at one.
 */
export function taperAt(t: number, headFraction: number = CHANNEL_HEAD_FRACTION): number {
  const clamped = t < 0 ? 0 : t > 1 ? 1 : t;
  // Flow at the head, back-derived so that √(flow) lands exactly on the head
  // fraction — the inverse of the width law, so the two cannot disagree.
  const headFlow = headFraction * headFraction;
  return Math.sqrt(headFlow + (1 - headFlow) * clamped);
}

/** Below this stretch a channel is at rest and gets no tension rail. */
export const TENSION_FLOOR_PX = 5;
/**
 * Stretch at which the reduced-motion tension rail reaches full thickness.
 * The rail saturates because a 172 px stretch drawn at true scale is a 23 px
 * slab that swamps the field; the figure is printed alongside so two different
 * stretches drawing the same thickness can still be told apart.
 */
export const TENSION_SATURATION_PX = 60;

/** Ring label. Rings are hop DISTANCE from the focus, never elevation. */
export function ringLabel(ring: number): string {
  if (ring === 0) return 'focus';
  const hops = Math.abs(ring);
  const unit = hops === 1 ? 'hop' : 'hops';
  return `${hops} ${unit} ${ring < 0 ? 'up' : 'down'}`;
}

/* ---- geometry ----------------------------------------------------------- */

type Point = readonly [number, number];

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return function next(): number {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A closed irregular outline, used for the focus basin. */
function blob(
  ctx: CanvasRenderingContext2D,
  cx: number,
  cy: number,
  radiusX: number,
  radiusY: number,
  seed: number,
  rough = 0.14,
  samples = 72,
): void {
  const random = mulberry32(seed);
  const harmonics = [
    { k: 2 + Math.floor(random() * 2), a: rough * (0.55 + random() * 0.5), p: random() * 6.283 },
    { k: 3 + Math.floor(random() * 3), a: rough * (0.34 + random() * 0.4), p: random() * 6.283 },
    { k: 6 + Math.floor(random() * 4), a: rough * (0.14 + random() * 0.2), p: random() * 6.283 },
  ];
  ctx.beginPath();
  for (let i = 0; i <= samples; i += 1) {
    const t = (i / samples) * Math.PI * 2;
    let m = 1;
    for (const h of harmonics) m += h.a * Math.sin(h.k * t + h.p);
    const px = cx + Math.cos(t) * radiusX * m;
    const py = cy + Math.sin(t) * radiusY * m;
    if (i === 0) ctx.moveTo(px, py);
    else ctx.lineTo(px, py);
  }
  ctx.closePath();
}

/**
 * Catmull–Rom through the waypoints, emitted as cubics. This is what makes
 * several channels sharing a trunk read as one braided river rather than as
 * parallel arrows.
 */
function traceCurve(ctx: CanvasRenderingContext2D, points: readonly Point[], move: boolean): void {
  if (points.length === 0) return;
  const first = points[0]!;
  const last = points[points.length - 1]!;
  const p: Point[] = [first, ...points, last];
  const start = p[1]!;
  if (move) ctx.moveTo(start[0], start[1]);
  else ctx.lineTo(start[0], start[1]);
  for (let i = 1; i < p.length - 2; i += 1) {
    const [x0, y0] = p[i - 1]!;
    const [x1, y1] = p[i]!;
    const [x2, y2] = p[i + 1]!;
    const [x3, y3] = p[i + 2]!;
    ctx.bezierCurveTo(
      x1 + (x2 - x0) / 6,
      y1 + (y2 - y0) / 6,
      x2 - (x3 - x1) / 6,
      y2 - (y3 - y1) / 6,
      x2,
      y2,
    );
  }
}

/**
 * A tapered ribbon: width is a measured quantity at each end, and the run
 * between them follows the hydrological profile in `taperAt`.
 *
 * The wider end is the mouth whichever end it is on, so an upstream tributary
 * converging on the focus and a downstream distributary fanning away from it
 * are the same curve read in opposite directions rather than two shapes.
 */
function ribbon(
  ctx: CanvasRenderingContext2D,
  points: readonly Point[],
  widthStart: number,
  widthEnd: number,
): void {
  const n = points.length;
  if (n < 2) return;
  const normals = points.map((_, i) => {
    const a = points[Math.max(0, i - 1)]!;
    const b = points[Math.min(n - 1, i + 1)]!;
    const dx = b[0] - a[0];
    const dy = b[1] - a[1];
    const len = Math.hypot(dx, dy) || 1;
    return [-dy / len, dx / len] as const;
  });
  const mouthWidth = Math.max(widthStart, widthEnd);
  const headWidth = Math.min(widthStart, widthEnd);
  // Which end the mouth is on. `t` always runs head → mouth so the profile is
  // written once and read forwards or backwards.
  const mouthAtEnd = widthEnd >= widthStart;
  const headFraction = mouthWidth === 0 ? 1 : headWidth / mouthWidth;
  const half = (i: number): number => {
    const along = i / (n - 1);
    return (mouthWidth * taperAt(mouthAtEnd ? along : 1 - along, headFraction)) / 2;
  };
  const upper: Point[] = points.map((pt, i) => [
    pt[0] + normals[i]![0] * half(i),
    pt[1] + normals[i]![1] * half(i),
  ]);
  const lower: Point[] = points
    .map(
      (pt, i): Point => [pt[0] - normals[i]![0] * half(i), pt[1] - normals[i]![1] * half(i)],
    )
    .reverse();
  ctx.beginPath();
  traceCurve(ctx, upper, true);
  traceCurve(ctx, lower, false);
  ctx.closePath();
}

function roundRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
): void {
  const radius = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + radius, y);
  ctx.arcTo(x + w, y, x + w, y + h, radius);
  ctx.arcTo(x + w, y + h, x, y + h, radius);
  ctx.arcTo(x, y + h, x, y, radius);
  ctx.arcTo(x, y, x + w, y, radius);
  ctx.closePath();
}

/* ---- the renderer ------------------------------------------------------- */

export interface TraceViewport {
  width: number;
  height: number;
  dpr: number;
  scale: number;
  offsetX: number;
  offsetY: number;
}

export interface TraceRenderer {
  setPalette(palette: TracePalette): void;
  setViewport(box: { width: number; height: number; dpr: number }): TraceViewport;
  readonly viewport: TraceViewport;
  toScreen(worldX: number, worldY: number): { x: number; y: number };
  toWorld(screenX: number, screenY: number): { x: number; y: number };
  /** Nearest node to a world point within `radius`, or null. */
  hitTest(positions: Float64Array, worldX: number, worldY: number, radius?: number): string | null;
  draw(frame: TraceFrame): void;
}

export function createRenderer(
  canvas: HTMLCanvasElement,
  model: TraceModel,
): TraceRenderer {
  const context = canvas.getContext('2d', { alpha: true });
  if (!context) throw new Error('Canvas2D unavailable');
  // Bound to a fresh, explicitly typed const: the guard above narrows
  // `context`, but the draw helpers below are hoisted function declarations and
  // TypeScript will not carry a narrowing into them.
  const ctx: CanvasRenderingContext2D = context;

  const nodes = model.nodes;
  const indexOfId = new Map(nodes.map((node, i) => [node.id, i]));

  /**
   * Label rank within a ring: 0 for the nearest row of names, 1 for the row
   * pushed further out. Computed once from the LAYOUT anchors, not from live
   * positions, so a drag does not make the labels reshuffle underneath the
   * reader's hand.
   */
  const staggerIndex = new Map<string, number>();
  for (const ring of new Set(nodes.map((node) => node.ring))) {
    [...nodes]
      .filter((node) => node.ring === ring)
      .sort((a, b) => a.x0 - b.x0)
      .forEach((node, i) => staggerIndex.set(node.id, i % 2));
  }

  let palette: TracePalette | null = null;
  let view: TraceViewport = {
    width: 0,
    height: 0,
    dpr: 1,
    scale: 1,
    offsetX: 0,
    offsetY: 0,
  };

  /** Cheap per-frame scratch so a 60 Hz loop allocates nothing. */
  const point = { x: 0, y: 0 };
  function readPosition(positions: Float64Array, i: number): { x: number; y: number } {
    point.x = positions[i * 2] ?? 0;
    point.y = positions[i * 2 + 1] ?? 0;
    return point;
  }

  interface LabelOptions {
    size?: number;
    color?: string;
    align?: CanvasTextAlign;
    tracking?: number;
    halo?: boolean;
    upper?: boolean;
  }

  /**
   * Type is specified in CSS pixels and divided back out of the world
   * transform, so a label is the same physical size whether the field is
   * fitted into a 1440 px page or a 700 px column.
   *
   * Without this the world scale silently shrank every label with the picture:
   * at the widths this drill-in actually gets, a 9 px world label rendered at
   * roughly 4.5 device pixels, which is not small type — it is no type. The
   * compensation is capped so a very narrow column blows the labels up until
   * they swamp the marks instead.
   */
  function typeScale(): number {
    return Math.min(2.2, 1 / Math.max(0.05, view.scale));
  }

  function label(text: string, x: number, y: number, options: LabelOptions = {}): void {
    const pal = palette!;
    const { size = 9, color, align = 'left', tracking = 0, halo = true, upper = false } = options;
    const body = upper ? text.toUpperCase() : text;
    ctx.font = `${(size * typeScale()).toFixed(2)}px ui-monospace, "SFMono-Regular", "JetBrains Mono", monospace`;
    ctx.textAlign = tracking > 0 ? 'left' : align;
    ctx.textBaseline = 'alphabetic';
    // A label on a moving field has to survive every crossing, so it is set
    // with a substrate-coloured halo exactly as the static sheets do it.
    const haloWidth = 3 * typeScale();
    if (tracking > 0) {
      const track = tracking * typeScale();
      const glyphs = [...body];
      const total = glyphs.reduce((sum, g) => sum + ctx.measureText(g).width + track, -track);
      let cursor = align === 'right' ? x - total : align === 'center' ? x - total / 2 : x;
      for (const glyph of glyphs) {
        if (halo) {
          ctx.strokeStyle = pal.surface0;
          ctx.lineWidth = haloWidth;
          ctx.strokeText(glyph, cursor, y);
        }
        ctx.fillStyle = color ?? pal.textMuted;
        ctx.fillText(glyph, cursor, y);
        cursor += ctx.measureText(glyph).width + track;
      }
      return;
    }
    if (halo) {
      ctx.strokeStyle = pal.surface0;
      ctx.lineWidth = haloWidth;
      ctx.lineJoin = 'round';
      ctx.strokeText(body, x, y);
    }
    ctx.fillStyle = color ?? pal.textMuted;
    ctx.fillText(body, x, y);
  }

  /* ---- 1. hop rings ------------------------------------------------------ */
  function drawRings(): void {
    const pal = palette!;
    const t = typeScale();
    ctx.save();
    for (const [ring, y] of model.rows) {
      ctx.beginPath();
      ctx.moveTo(6, y);
      ctx.lineTo(model.world.width - 8, y);
      ctx.strokeStyle = pal.grid;
      ctx.lineWidth = 1;
      ctx.setLineDash(ring === 0 ? [] : [1, 6]);
      ctx.stroke();
      ctx.setLineDash([]);
      // Set flush left and ABOVE its own rule. Right-aligning these into the
      // margin clipped every one of them the moment the labels started
      // compensating for the world scale — the margin is a fixed number of
      // world units and the type no longer is.
      label(ringLabel(ring), 6, y - 5 * t, {
        align: 'left',
        color: ring === 0 ? pal.accent : pal.textMuted,
        tracking: 1.1,
        upper: true,
      });
    }
    ctx.restore();
    label('hop ring — distance from the focus, not elevation', 6, 24 * t, {
      align: 'left',
      tracking: 1.1,
      upper: true,
    });
  }

  /* ---- 2. membranes: the types the flow passes through ------------------- */
  interface Box {
    x0: number;
    y0: number;
    x1: number;
    y1: number;
  }
  const boxes = new Map<string, Box>();

  function drawMembranes(positions: Float64Array): void {
    const pal = palette!;
    const t = typeScale();
    boxes.clear();
    model.membranes.forEach((membrane, m) => {
      let x0 = Infinity;
      let x1 = -Infinity;
      let y0 = Infinity;
      let y1 = -Infinity;
      for (const id of membrane.of) {
        const i = indexOfId.get(id);
        if (i === undefined) continue;
        const p = readPosition(positions, i);
        x0 = Math.min(x0, p.x);
        x1 = Math.max(x1, p.x);
        y0 = Math.min(y0, p.y);
        y1 = Math.max(y1, p.y);
      }
      if (!Number.isFinite(x0)) return;
      x0 -= 44;
      x1 += 44;
      y0 -= 34;
      y1 += 34;
      ctx.save();
      roundRect(ctx, x0, y0, x1 - x0, y1 - y0, Math.min((y1 - y0) / 2, 60));
      ctx.fillStyle = pal.membraneFill;
      ctx.fill();
      ctx.strokeStyle = pal.edgeStrong;
      ctx.lineWidth = 1;
      ctx.globalAlpha = 0.7;
      ctx.stroke();
      ctx.restore();
      // Enclosures nest and overlap freely — two types can each hold symbols on
      // the same ring — so their names are stacked rather than all set on the
      // box's own top edge, where they overprinted each other into mush.
      label(membrane.label, x0 + 10, y0 - (7 + (m % 3) * 15) * t, {
        color: pal.textMuted,
        tracking: 1.1,
        upper: true,
      });
      boxes.set(membrane.id, { x0, y0, x1, y1 });
    });
  }

  /** Weakest-first paint order, computed once from the immutable model. */
  const drawOrder = model.channels
    .map((channel, e) => ({ channel, e }))
    .sort((a, b) => a.channel.calls - b.channel.calls);

  /* ---- 3. channels: width is call sites, brightness is live tension ------ */
  interface Reading {
    x: number;
    y: number;
    text: string;
    hue: string;
  }

  function channelPath(
    ax: number,
    ay: number,
    bx: number,
    by: number,
    dir: string,
    focusX: number,
  ): Point[] {
    if (dir === 'in') {
      // A call that entered a type and moves between its methods before
      // leaving. Drawn, not implied — it is the sheet's whole argument.
      return [
        [ax, ay],
        [(ax + bx) / 2, Math.min(ay, by) - 46],
        [bx, by],
      ];
    }
    const mid = (ay + by) / 2;
    const pull = dir === 'up' ? 0.42 : 0.34;
    return [
      [ax, ay],
      [ax + (focusX - ax) * pull * 0.5, mid - (by - ay) * 0.18],
      [bx + (focusX - bx) * pull * 0.12, mid + (by - ay) * 0.2],
      [bx, by],
    ];
  }

  function drawChannels(
    positions: Float64Array,
    stretches: Float64Array,
    reducedMotion: boolean,
  ): Reading[] {
    const pal = palette!;
    const focusIndex = indexOfId.get(model.focusId) ?? 0;
    const focusX = positions[focusIndex * 2] ?? model.world.width / 2;
    const readings: Reading[] = [];
    // Weakest first, so a one-call-site hairline can never be laid over the
    // 40-call-site trunk it crosses. Painted order is the only depth cue a 2D
    // field has, and it should agree with the measurement everything else here
    // encodes.
    for (const { channel, e } of drawOrder) {
      const ai = indexOfId.get(channel.a);
      const bi = indexOfId.get(channel.b);
      if (ai === undefined || bi === undefined) continue;
      const ax = positions[ai * 2] ?? 0;
      const ay = positions[ai * 2 + 1] ?? 0;
      const bx = positions[bi * 2] ?? 0;
      const by = positions[bi * 2 + 1] ?? 0;
      const w = channelWidth(channel.calls);
      const points = channelPath(ax, ay, bx, by, channel.dir, focusX);
      const upstream = channel.dir === 'up' || channel.dir === 'in';
      const hue = upstream ? pal.upstream : pal.downstream;
      // A channel that leaves the graph keeps FULL width to its dashed mouth.
      // The design note's absence beat is "full width, then it stops" — the
      // flow it carried was measured, and narrowing it toward the end would
      // draw the lost traffic as dwindling when what is unknown is only where
      // it went.
      const lost = channel.dir === 'lost';
      const head = lost ? w : w * CHANNEL_HEAD_FRACTION;
      const widthStart = channel.dir === 'up' ? head : w;
      const widthEnd = channel.dir === 'up' ? w : head;
      const stretch = Math.abs(stretches[e] ?? 0);
      // Tension is the SAME measurement as width (call sites); under load the
      // channel reports how far it has been pulled off its rest length, so the
      // felt channel and the drawn channel cannot disagree.
      const load = Math.min(1, stretch / 26);
      ctx.save();
      ribbon(ctx, points, widthStart, widthEnd);
      ctx.fillStyle = hue;
      // Base fill is deliberately low: a dense neighbourhood stacks dozens of
      // translucent ribbons, and at 0.42 they accumulated into one solid slab
      // in which no individual channel — and therefore no call-site width —
      // could be read at all.
      ctx.globalAlpha = 0.26 + load * 0.34;
      ctx.fill();
      ctx.globalAlpha = 0.62 + load * 0.38;
      ctx.strokeStyle = hue;
      ctx.lineWidth = 0.8 + load * 1.4;
      ctx.stroke();
      ctx.restore();

      if (reducedMotion && stretch > TENSION_FLOOR_PX) {
        // Reduced motion: tension becomes literal thickness on a core rail,
        // because a reader who cannot see the deformation must still be able
        // to measure it. It wears the PARTIAL state hue rather than the accent,
        // because an accent rail is indistinguishable from the upstream
        // channel it sits inside.
        const railWidth = 1.5 + Math.min(1, stretch / TENSION_SATURATION_PX) * 6.5;
        ctx.save();
        ribbon(ctx, points, railWidth, railWidth);
        ctx.fillStyle = pal.statePartial;
        ctx.globalAlpha = 0.72;
        ctx.fill();
        ctx.restore();
        readings.push({
          x: (ax + bx) / 2,
          y: (ay + by) / 2,
          text: `${stretch.toFixed(0)} px`,
          hue: pal.statePartial,
        });
      } else if (channel.calls >= 4) {
        readings.push({
          x: (ax + bx) / 2,
          y: (ay + by) / 2 + (channel.dir === 'in' ? -30 : 0),
          text: String(channel.calls),
          hue,
        });
      }
    }
    return readings;
  }

  /* ---- 4. membrane ports: where flow crosses a type boundary ------------- */
  function drawPorts(positions: Float64Array): void {
    const pal = palette!;
    ctx.save();
    ctx.strokeStyle = pal.edgeStrong;
    ctx.lineWidth = 2.4;
    for (const membrane of model.membranes) {
      const box = boxes.get(membrane.id);
      if (!box) continue;
      for (const id of membrane.of) {
        const i = indexOfId.get(id);
        if (i === undefined) continue;
        const p = readPosition(positions, i);
        for (const edgeY of [box.y0, box.y1]) {
          ctx.beginPath();
          ctx.moveTo(p.x - 9, edgeY);
          ctx.lineTo(p.x + 9, edgeY);
          ctx.stroke();
        }
      }
    }
    ctx.restore();
  }

  /* ---- 5. dashed mouths: the edges this frame does NOT draw -------------- */
  function drawMouths(positions: Float64Array): void {
    const pal = palette!;
    ctx.save();
    ctx.setLineDash([4, 4]);
    ctx.lineWidth = 1.4;
    ctx.strokeStyle = pal.stateUnknown;
    ctx.globalAlpha = 0.75;
    nodes.forEach((node, i) => {
      if (!node.undrawnEdges) return;
      const p = readPosition(positions, i);
      // The mouth points away from the focus, so it reads as flow leaving the
      // frame rather than as another channel inside it.
      const away = node.ring < 0 ? -1 : 1;
      const reach = 14 + Math.min(34, Math.sqrt(node.undrawnEdges) * 6);
      ctx.beginPath();
      ctx.moveTo(p.x, p.y + away * 7);
      ctx.lineTo(p.x, p.y + away * (7 + reach));
      ctx.stroke();
    });
    ctx.restore();
  }

  /* ---- 6. hover bloom: depth and latency are the node's degree ----------- */
  function drawBloom(positions: Float64Array, bloom: Float64Array, light: boolean): void {
    const pal = palette!;
    nodes.forEach((node, i) => {
      const value = bloom[i] ?? 0;
      if (value < 0.004) return;
      const p = readPosition(positions, i);
      const hue = node.degree == null ? pal.stateUnknown : kindColor(node.kind, light);
      // Bloom DEPTH scales with degree: a hub opens a wide slow well, a leaf a
      // tight flick. Same number that sets its inertia.
      const reach = (14 + Math.max(3, node.degree ?? 0) * 0.9) * value;
      ctx.save();
      for (let ring = 3; ring >= 1; ring -= 1) {
        ctx.beginPath();
        ctx.ellipse(
          p.x,
          p.y,
          sillWidth(node.degree) / 2 + reach * ring * 0.55,
          7 + reach * ring * 0.34,
          0,
          0,
          Math.PI * 2,
        );
        ctx.strokeStyle = hue;
        ctx.globalAlpha = value * (0.42 - ring * 0.09);
        ctx.lineWidth = ring === 1 ? 1.6 : 0.9;
        ctx.stroke();
      }
      ctx.restore();
    });
  }

  /* ---- 7. nodes: a sill whose WIDTH is the symbol's degree --------------- */
  function drawNodes(positions: Float64Array, light: boolean, draggingId: string | null): void {
    const pal = palette!;
    nodes.forEach((node, i) => {
      if (node.id === model.focusId) return;
      const p = readPosition(positions, i);
      const w = sillWidth(node.degree);
      if (node.degree == null) {
        // Unmeasured degree: a hollow dashed sill at the floor width. Absence
        // is drawn as absence, never as a measured zero.
        ctx.save();
        roundRect(ctx, p.x - w / 2, p.y - 5, w, 10, 5);
        ctx.strokeStyle = pal.textMuted;
        ctx.lineWidth = 1;
        ctx.globalAlpha = 0.55;
        ctx.setLineDash([3, 4]);
        ctx.stroke();
        ctx.restore();
        return;
      }
      roundRect(ctx, p.x - w / 2, p.y - 5, w, 10, 5);
      ctx.save();
      ctx.fillStyle = kindColor(node.kind, light);
      ctx.globalAlpha = 0.9;
      ctx.fill();
      ctx.restore();
      ctx.strokeStyle = node.id === draggingId ? pal.accent : pal.surface0;
      ctx.lineWidth = node.id === draggingId ? 1.8 : 1;
      ctx.stroke();
    });
  }

  /* ---- 8. the focus basin ------------------------------------------------ */
  function drawFocus(positions: Float64Array): void {
    const pal = palette!;
    const i = indexOfId.get(model.focusId);
    if (i === undefined) return;
    const p = readPosition(positions, i);
    ctx.save();
    for (let r = 4; r >= 1; r -= 1) {
      blob(ctx, p.x, p.y, 20 + r * 15, 12 + r * 8, 77 + r * 13, 0.07);
      if (r === 1) {
        ctx.fillStyle = pal.accent;
        ctx.globalAlpha = 0.85;
        ctx.fill();
      }
      ctx.strokeStyle = pal.accent;
      ctx.globalAlpha = 0.25 + (5 - r) * 0.16;
      ctx.lineWidth = r === 1 ? 1.6 : 0.9;
      ctx.stroke();
    }
    ctx.restore();
  }

  /* ---- 9. labels, set last ---------------------------------------------- */
  function drawLabels(positions: Float64Array, readings: Reading[]): void {
    const pal = palette!;
    const t = typeScale();
    nodes.forEach((node, i) => {
      if (node.id === model.focusId) return;
      const p = readPosition(positions, i);
      const above = node.ring < 0;
      // Every offset scales with the type, or a compensated label lands on top
      // of the sill it is naming. The stagger is the second half of that: a row
      // of seven names at one height collides at any column width the workspace
      // offers, and alternating rows is cheaper to read than truncating.
      const stagger = staggerIndex.get(node.id) ?? 0;
      const dy = (above ? -14 : 22) * t + (above ? -1 : 1) * stagger * 26 * t;
      label(node.name, p.x, p.y + dy, {
        size: 11,
        color: pal.textPrimary,
        align: 'center',
      });
      const degreeText = node.degree == null ? 'degree absent' : `deg ${node.degree}`;
      label(degreeText, p.x, p.y + dy + (above ? -11 : 11) * t, {
        color: node.degree == null ? pal.stateUnknown : pal.textMuted,
        align: 'center',
      });
      const notes: string[] = [];
      if (node.selfCalls) notes.push(`↻ ${node.selfCalls} self`);
      if (node.undrawnEdges) notes.push(`+${node.undrawnEdges} not drawn`);
      if (notes.length) {
        label(notes.join(' · '), p.x, p.y + dy + (above ? -22 : 22) * t, {
          color: pal.stateUnknown,
          align: 'center',
        });
      }
    });

    const fi = indexOfId.get(model.focusId);
    if (fi !== undefined) {
      const f = readPosition(positions, fi);
      const focusNode = nodes[fi]!;
      label(focusNode.name, f.x, f.y + 4 * t, {
        size: 15,
        color: pal.textPrimary,
        align: 'center',
      });
      const stats =
        focusNode.degree == null
          ? 'degree absent from payload'
          : `degree ${focusNode.degree}`;
      label(stats, f.x, f.y + 26 * t, { color: pal.textMuted, align: 'center' });
      if (focusNode.filePath) {
        label(
          focusNode.startLine == null
            ? focusNode.filePath
            : `${focusNode.filePath}:${focusNode.startLine}`,
          f.x,
          f.y + 40 * t,
          { color: pal.accent, align: 'center', tracking: 1.1, upper: true },
        );
      }
    }

    for (const reading of readings) {
      label(reading.text, reading.x, reading.y + 3 * t, { color: reading.hue, align: 'center' });
    }
  }

  return {
    setPalette(next: TracePalette) {
      palette = next;
    },

    /**
     * Fit the world into the canvas box. The composing component uses the same
     * transform to map a pointer back into world space, so a gesture lands on
     * the mark the reader aimed at.
     */
    setViewport({ width, height, dpr }) {
      const scale = Math.min(width / model.world.width, height / model.world.height);
      view = {
        width,
        height,
        dpr,
        scale,
        offsetX: (width - model.world.width * scale) / 2,
        offsetY: (height - model.world.height * scale) / 2,
      };
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
      return { ...view };
    },

    get viewport() {
      return { ...view };
    },

    toScreen(worldX, worldY) {
      return { x: view.offsetX + worldX * view.scale, y: view.offsetY + worldY * view.scale };
    },
    toWorld(screenX, screenY) {
      return { x: (screenX - view.offsetX) / view.scale, y: (screenY - view.offsetY) / view.scale };
    },

    hitTest(positions, worldX, worldY, radius = 46) {
      let best: string | null = null;
      let bestDistance = radius;
      nodes.forEach((node, i) => {
        const dx = (positions[i * 2] ?? 0) - worldX;
        const dy = (positions[i * 2 + 1] ?? 0) - worldY;
        // Sills are wide and short, so the hit region is scaled to match the
        // mark rather than being a circle over a lozenge.
        const distance = Math.hypot(dx / Math.max(1, sillWidth(node.degree) / 26), dy);
        if (distance < bestDistance) {
          bestDistance = distance;
          best = node.id;
        }
      });
      return best;
    },

    draw(frame: TraceFrame) {
      if (!palette) throw new Error('setPalette must be called before draw');
      const { positions, stretches, bloom, draggingId, reducedMotion } = frame;
      ctx.setTransform(view.dpr, 0, 0, view.dpr, 0, 0);
      ctx.clearRect(0, 0, view.width, view.height);
      ctx.translate(view.offsetX, view.offsetY);
      ctx.scale(view.scale, view.scale);
      ctx.lineJoin = 'round';
      ctx.lineCap = 'round';

      drawRings();
      drawMembranes(positions);
      const readings = drawChannels(positions, stretches, reducedMotion);
      drawPorts(positions);
      drawMouths(positions);
      drawBloom(positions, bloom, palette.light);
      drawNodes(positions, palette.light, draggingId);
      drawFocus(positions);
      drawLabels(positions, readings);
    },
  };
}
