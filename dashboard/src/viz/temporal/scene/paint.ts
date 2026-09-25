/**
 * The Canvas2D substrate of the temporal field.
 *
 * A measuring instrument: 32px graticule hairlines with the labelled ticks as
 * majors, one banded rail per session, and causal links routed orthogonally
 * in their grade's line style. Luminance is spent only on real quantities:
 * recency within the loaded page (dimmest at the oldest start, full at NOW)
 * and the selected causal chain, which lifts over the recency veil with a
 * restrained cyan halo. Additive blending applies to rails and links so that
 * only where two real marks cross does the field brighten.
 *
 * Semantic zoom is one rule: a lane that cannot hold legible glyphs draws its
 * density summary instead. A collapsed branch is always a summary, a
 * shared-scale histogram of member sessions with a measured extent, with
 * begun-but-unmeasured sessions dotted above it; a session lane whose marks
 * collide draws its extent band and a floor rug of dated events. Zooming the
 * window or expanding a branch resolves the same lanes to rails and glyphs.
 */
import type { DensityBin, LaneDensity } from '../density.ts';
import { gradeStroke, type TemporalPalette } from '../palette.ts';
import type { SceneLane, ScenePath } from '../types.ts';
import { focusAlpha, hatch, isLifted, pathEnds, recencySpan, resolvedLanes, type SceneFrame } from './frame.ts';

const GRATICULE_PX = 32;
const CORNER_PX = 4;
const HALO_PX = 7;
const HALO_ALPHA = 0.16;
const BAND_INSET = 3;

function isLink(path: ScenePath): boolean {
  return path.kind === 'spawn' || path.kind === 'handoff' || path.kind === 'rejoin' || path.kind === 'result';
}

/** Down the relation instant, a small corner, then along the child rail. */
function traceRoute(ctx: CanvasRenderingContext2D, path: ScenePath): void {
  const { x0, y0, x1, y1 } = pathEnds(path.controls);
  const x = (x0 + x1) / 2;
  const dir = y1 >= y0 ? 1 : -1;
  const r = Math.min(CORNER_PX, Math.abs(y1 - y0) / 2);
  ctx.moveTo(x, y0);
  ctx.lineTo(x, y1 - dir * r);
  ctx.arcTo(x, y1, x + r, y1, r);
  ctx.lineTo(x1, y1);
}

function traceRail(ctx: CanvasRenderingContext2D, lane: SceneLane): void {
  const y = Math.round(lane.y) + 0.5;
  ctx.moveTo(lane.x0, y);
  ctx.lineTo(Math.max(lane.x1, lane.x0 + 1), y);
}

/** One path per layer: a dense page is hundreds of bins per lane. */
function fillBins(
  ctx: CanvasRenderingContext2D,
  bins: readonly DensityBin[],
  color: string,
  alpha: number,
  rect: (bin: DensityBin, w: number) => readonly [number, number, number, number] | null,
): void {
  ctx.beginPath();
  for (const bin of bins) {
    const r = rect(bin, Math.max(1, bin.x1 - bin.x0 - 1));
    if (r) ctx.rect(r[0], r[1], r[2], r[3]);
  }
  ctx.globalAlpha = alpha;
  ctx.fillStyle = color;
  ctx.fill();
}

/** Dots through every rect in one clip: begun, extent unknown. */
function dotRects(ctx: CanvasRenderingContext2D, rects: readonly (readonly [number, number, number, number])[], color: string, alpha: number): void {
  if (rects.length === 0) return;
  ctx.save();
  ctx.beginPath();
  let x0 = Infinity;
  let x1 = -Infinity;
  let y0 = Infinity;
  let y1 = -Infinity;
  for (const [x, y, w, h] of rects) {
    ctx.rect(x, y, w, h);
    x0 = Math.min(x0, x);
    x1 = Math.max(x1, x + w);
    y0 = Math.min(y0, y);
    y1 = Math.max(y1, y + h);
  }
  ctx.clip();
  ctx.beginPath();
  for (let x = x0 + 1; x < x1; x += 3) for (let y = y0 + 1; y < y1; y += 3) ctx.rect(x, y, 1, 1);
  ctx.globalAlpha = alpha;
  ctx.fillStyle = color;
  ctx.fill();
  ctx.restore();
}

function paintBundle(ctx: CanvasRenderingContext2D, lane: SceneLane, density: LaneDensity, peak: number, palette: TemporalPalette, alpha: number): void {
  const floor = lane.y + lane.height / 2 - BAND_INSET;
  const room = Math.max(4, lane.height - BAND_INSET * 2 - 2);
  const h = (count: number): number => Math.sqrt(count / peak) * room;
  fillBins(ctx, density.bins, palette.signal, 0.55 * alpha, (bin, w) => (bin.active > 0 ? [bin.x0, floor - h(bin.active), w, h(bin.active)] : null));
  const open = density.bins.flatMap((bin) => {
    const top = h(bin.active + bin.open);
    const base = h(bin.active);
    return top - base > 0.5 ? [[bin.x0, floor - top, Math.max(1, bin.x1 - bin.x0 - 1), top - base] as const] : [];
  });
  dotRects(ctx, open, palette.text, 0.55 * alpha);
}

function paintRug(ctx: CanvasRenderingContext2D, lane: SceneLane, density: LaneDensity, palette: TemporalPalette, alpha: number): void {
  const floor = lane.y + lane.height / 2 - 1;
  fillBins(ctx, density.bins, palette.text, 0.75 * alpha, (bin, w) => {
    if (bin.events === 0) return null;
    const tick = Math.min(BAND_INSET + 2, 1 + Math.log2(1 + bin.events));
    return [bin.x0 + w / 2 - 0.5, floor - tick, 1, tick];
  });
}

export function paintScene(ctx: CanvasRenderingContext2D, frame: SceneFrame, palette: TemporalPalette): void {
  const { model, fieldX0, fieldX1, top, height, density } = frame;
  const width = Math.max(1, fieldX1 - fieldX0);
  const resolved = resolvedLanes(frame);
  const additive = palette.light ? 'source-over' : 'lighter';
  ctx.save();
  ctx.beginPath();
  ctx.rect(fieldX0, top, width, height - top);
  ctx.clip();
  ctx.lineWidth = 1;
  ctx.setLineDash([]);

  // Graticule: 32px minor hairlines as texture, the labelled ticks as majors.
  ctx.strokeStyle = palette.grid;
  ctx.globalAlpha = palette.light ? 0.35 : 0.3;
  ctx.beginPath();
  for (let x = fieldX0 + GRATICULE_PX; x < fieldX1; x += GRATICULE_PX) {
    ctx.moveTo(Math.round(x) + 0.5, top);
    ctx.lineTo(Math.round(x) + 0.5, height);
  }
  ctx.stroke();
  ctx.strokeStyle = palette.edge;
  ctx.globalAlpha = 0.35;
  ctx.beginPath();
  for (const tick of model.ticks) {
    ctx.moveTo(Math.round(tick.x) + 0.5, top);
    ctx.lineTo(Math.round(tick.x) + 0.5, height);
  }
  ctx.stroke();

  // Lane bands with hairline separators; the selected band carries the tint.
  for (const lane of model.lanes) {
    const y0 = lane.y - lane.height / 2;
    if (isLifted(lane.focus)) {
      ctx.globalAlpha = lane.focus === 'selected' ? 0.08 : 0.035;
      ctx.fillStyle = palette.signal;
      ctx.fillRect(fieldX0, y0, width, lane.height);
    } else if (lane.row % 2 === 1) {
      ctx.globalAlpha = palette.light ? 0.03 : 0.022;
      ctx.fillStyle = palette.text;
      ctx.fillRect(fieldX0, y0, width, lane.height);
    }
    ctx.globalAlpha = 0.7;
    ctx.fillStyle = palette.grid;
    ctx.fillRect(fieldX0, Math.round(y0 + lane.height) - 1, width, 1);
  }
  for (const rail of model.rails) {
    ctx.globalAlpha = 0.9;
    ctx.fillStyle = palette.edge;
    ctx.fillRect(fieldX0, Math.round(rail.y0), width, 1);
  }

  // Summaries where lanes cannot hold glyphs, on one scale for every bundle.
  let bundlePeak = 1;
  for (const lane of model.lanes) {
    const peak = lane.kind === 'bundle' ? density?.lanes.get(lane.id)?.peak : undefined;
    if (peak) bundlePeak = Math.max(bundlePeak, peak.active + peak.open);
  }
  for (const lane of model.lanes) {
    const laneDensity = density?.lanes.get(lane.id);
    if (!laneDensity || !lane.revealed) continue;
    const alpha = focusAlpha(lane.focus);
    if (lane.kind === 'bundle') paintBundle(ctx, lane, laneDensity, bundlePeak, palette, alpha);
    if (!resolved.has(lane.id)) paintRug(ctx, lane, laneDensity, palette, alpha);
  }

  ctx.globalCompositeOperation = additive;
  const laneById = new Map(model.lanes.map((lane) => [lane.id, lane] as const));
  for (const path of model.paths) {
    if (path.kind !== 'lane') continue;
    const lane = laneById.get(path.fromId);
    if (!lane || lane.kind === 'bundle') continue;
    const stroke = gradeStroke(path.grade, palette);
    const resolves = resolved.has(lane.id);
    if (!resolves) {
      // An aggregated session is its extent band; the grade rides on the
      // hairline through it, since a dashed band reads as a barcode.
      ctx.globalAlpha = 0.28 * focusAlpha(path.focus);
      ctx.strokeStyle = stroke.color;
      ctx.lineWidth = Math.max(2, lane.height - BAND_INSET * 4);
      ctx.setLineDash([]);
      ctx.beginPath();
      traceRail(ctx, lane);
      ctx.stroke();
    }
    ctx.globalAlpha = focusAlpha(path.focus);
    ctx.strokeStyle = stroke.color;
    ctx.lineWidth = 1 + (path.weight ?? 0) * 1.5;
    ctx.setLineDash([...stroke.dash]);
    ctx.beginPath();
    traceRail(ctx, lane);
    ctx.stroke();
  }
  const links = model.paths.filter(isLink);
  for (const path of links) {
    const stroke = gradeStroke(path.grade, palette);
    // A join is an inference from two extents; it recedes behind the forks.
    ctx.globalAlpha = focusAlpha(path.focus) * (path.kind === 'rejoin' ? 0.6 : 1);
    ctx.strokeStyle = stroke.color;
    ctx.lineWidth = 1;
    ctx.setLineDash([...stroke.dash]);
    ctx.beginPath();
    traceRoute(ctx, path);
    ctx.stroke();
  }
  ctx.globalCompositeOperation = 'source-over';
  ctx.setLineDash([1, 3]);
  ctx.strokeStyle = palette.text;
  for (const path of model.paths) {
    if (path.kind !== 'sequence' && path.kind !== 'edit_link') continue;
    ctx.globalAlpha = 0.45 * focusAlpha(path.focus);
    ctx.beginPath();
    ctx.moveTo(path.controls[0]!, path.controls[1]!);
    ctx.lineTo(path.controls[2]!, path.controls[3]!);
    ctx.stroke();
  }
  ctx.setLineDash([]);

  // A lane with no measured extent ends in a hatched stub.
  ctx.strokeStyle = palette.dim;
  for (const lane of model.lanes) {
    if (lane.endSource !== null || lane.offscreen || !lane.revealed || lane.kind === 'bundle') continue;
    ctx.save();
    ctx.beginPath();
    ctx.rect(lane.x0, lane.y - 3, 18, 6);
    ctx.clip();
    ctx.globalAlpha = 0.8 * focusAlpha(lane.focus);
    hatch(ctx, lane.x0, lane.y - 3, lane.x0 + 18, lane.y + 3, 3);
    ctx.restore();
  }

  // Recency: a veil from the oldest start in the page to NOW.
  const span = recencySpan(frame);
  if (span) {
    const veil = ctx.createLinearGradient(span.headX, 0, span.tailX, 0);
    veil.addColorStop(0, palette.substrate);
    veil.addColorStop(1, 'transparent');
    ctx.globalAlpha = 0.5;
    ctx.fillStyle = veil;
    ctx.fillRect(fieldX0, top, width, height - top);
  }

  // The selected causal chain lifts over the veil: a restrained halo, then
  // the crisp stroke in its own grade style.
  ctx.lineCap = 'round';
  const lifted: [ScenePath, (c: CanvasRenderingContext2D) => void][] = [];
  for (const path of model.paths) {
    if (!isLifted(path.focus)) continue;
    if (path.kind === 'lane') {
      const lane = laneById.get(path.fromId);
      if (lane && lane.kind !== 'bundle' && resolved.has(lane.id)) lifted.push([path, (c) => traceRail(c, lane)]);
    } else if (isLink(path)) {
      lifted.push([path, (c) => traceRoute(c, path)]);
    }
  }
  for (const [, trace] of lifted) {
    ctx.globalAlpha = HALO_ALPHA;
    ctx.strokeStyle = palette.signal;
    ctx.lineWidth = HALO_PX;
    ctx.setLineDash([]);
    ctx.beginPath();
    trace(ctx);
    ctx.stroke();
  }
  ctx.lineCap = 'butt';
  for (const [path, trace] of lifted) {
    const stroke = gradeStroke(path.grade, palette);
    ctx.globalAlpha = 1;
    ctx.strokeStyle = path.focus === 'selected' && path.kind === 'lane' ? palette.signalHot : stroke.color;
    ctx.lineWidth = path.kind === 'lane' ? 1.5 + (path.weight ?? 0) * 1.5 : 1.5;
    ctx.setLineDash([...stroke.dash]);
    ctx.beginPath();
    trace(ctx);
    ctx.stroke();
  }
  ctx.setLineDash([]);
  ctx.restore();
  paintUnrevealed(ctx, frame, palette);
}

/** Veils and hatches the band after a dated cursor: those records are
 * withheld, so the band reads as not yet revealed rather than empty. */
function paintUnrevealed(ctx: CanvasRenderingContext2D, frame: SceneFrame, palette: TemporalPalette): void {
  const { model, fieldX1, top, height } = frame;
  const cursor = model.cursor;
  if (!cursor || cursor.xBasis !== 'time' || cursor.x >= fieldX1) return;
  const x0 = Math.max(frame.fieldX0, cursor.x);
  ctx.save();
  ctx.beginPath();
  ctx.rect(x0, top, fieldX1 - x0, height - top);
  ctx.clip();
  ctx.globalAlpha = 0.5;
  ctx.fillStyle = palette.substrate;
  ctx.fillRect(x0, top, fieldX1 - x0, height - top);
  ctx.globalAlpha = 0.06;
  ctx.strokeStyle = palette.text;
  hatch(ctx, x0, top, fieldX1, height, 9);
  ctx.restore();
}
