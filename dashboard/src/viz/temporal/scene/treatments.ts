import type { LaneDensity } from '../density.ts';
import { gradeStroke, type TemporalPalette } from '../palette.ts';
import type { EvidenceGrade, SceneLane, ScenePath } from '../types.ts';
import { hatch } from './frame.ts';

/** Draw the layout's actual cubic, preserving both endpoint coordinates. */
export function traceCurve(ctx: CanvasRenderingContext2D, path: ScenePath): void {
  const c = path.controls;
  if (c.length < 4) return;
  ctx.moveTo(c[0]!, c[1]!);
  if (c.length >= 8) ctx.bezierCurveTo(c[2]!, c[3]!, c[4]!, c[5]!, c[6]!, c[7]!);
  else ctx.lineTo(c[2]!, c[3]!);
}

/** The original woven bundle: thickness is measured concurrent membership,
 * while begun sessions without an extent remain a separate dotted strand. */
export function paintRibbon(ctx: CanvasRenderingContext2D, lane: SceneLane, density: LaneDensity, palette: TemporalPalette, alpha: number): void {
  const maxHalf = Math.max(1, lane.height / 2 - 2);
  const half = (count: number): number => count > 0 ? Math.min(maxHalf, 0.8 + Math.sqrt(count) * 3.2) : 0;
  const { bins } = density;
  ctx.fillStyle = palette.signal;
  ctx.strokeStyle = palette.signal;
  ctx.lineWidth = 1;
  ctx.setLineDash([]);
  // Separate occupied runs so an empty interval never acquires a ribbon.
  for (let first = 0; first < bins.length;) {
    if (bins[first]!.active === 0) {
      first += 1;
      continue;
    }
    let end = first + 1;
    while (end < bins.length && bins[end]!.active > 0) end += 1;
    ctx.beginPath();
    ctx.moveTo(bins[first]!.x0, lane.y - half(bins[first]!.active));
    for (let index = first; index < end; index += 1) {
      const bin = bins[index]!;
      ctx.lineTo((bin.x0 + bin.x1) / 2, lane.y - half(bin.active));
    }
    const last = bins[end - 1]!;
    ctx.lineTo(last.x1, lane.y - half(last.active));
    ctx.lineTo(last.x1, lane.y + half(last.active));
    for (let index = end - 1; index >= first; index -= 1) {
      const bin = bins[index]!;
      ctx.lineTo((bin.x0 + bin.x1) / 2, lane.y + half(bin.active));
    }
    ctx.lineTo(bins[first]!.x0, lane.y + half(bins[first]!.active));
    ctx.closePath();
    ctx.globalAlpha = 0.28 * alpha;
    ctx.fill();
    ctx.globalAlpha = 0.7 * alpha;
    ctx.stroke();
    first = end;
  }
  ctx.globalAlpha = 0.7 * alpha;
  ctx.strokeStyle = palette.dim;
  ctx.setLineDash([1, 3]);
  ctx.beginPath();
  for (const bin of bins) {
    if (bin.open <= 0) continue;
    const y = lane.y - half(bin.active) - (bin.active > 0 ? 3 : 0);
    ctx.moveTo(bin.x0, y);
    ctx.lineTo(bin.x1, y);
  }
  ctx.stroke();
  ctx.setLineDash([]);
}

/** Fill patterns add a second grade channel to strata. The shared dashed
 * centre line and explicit grade labels remain visible in every treatment. */
export function fillGrade(ctx: CanvasRenderingContext2D, grade: EvidenceGrade, x: number, y: number, width: number, height: number, palette: TemporalPalette, alpha: number): void {
  if (width <= 0 || height <= 0) return;
  const color = gradeStroke(grade, palette).color;
  ctx.save();
  ctx.beginPath();
  ctx.rect(x, y, width, height);
  ctx.clip();
  ctx.fillStyle = color;
  ctx.strokeStyle = color;
  ctx.lineWidth = 1;
  ctx.setLineDash([]);
  switch (grade) {
    case 'exact':
    case 'explicit':
      ctx.globalAlpha = (grade === 'exact' ? 0.5 : 0.28) * alpha;
      ctx.fillRect(x, y, width, height);
      break;
    case 'inferred':
    case 'ambiguous':
    case 'stale':
      ctx.globalAlpha = 0.6 * alpha;
      hatch(ctx, x, y, x + width, y + height, grade === 'stale' ? 8 : 4);
      if (grade === 'ambiguous') {
        ctx.beginPath();
        for (let dx = x - height; dx < x + width + height; dx += 4) {
          ctx.moveTo(dx, y);
          ctx.lineTo(dx + height, y + height);
        }
        ctx.stroke();
      }
      break;
    case 'unavailable':
      ctx.globalAlpha = 0.85 * alpha;
      ctx.beginPath();
      for (let dx = x + 1; dx < x + width; dx += 3) {
        for (let dy = y + 1; dy < y + height; dy += 3) ctx.rect(dx, dy, 1, 1);
      }
      ctx.fill();
      break;
    default: {
      const exhaustive: never = grade;
      throw new Error(`unknown grade fill: ${String(exhaustive)}`);
    }
  }
  ctx.restore();
}
