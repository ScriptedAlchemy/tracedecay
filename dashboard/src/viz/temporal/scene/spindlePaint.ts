/**
 * The recovered Loom's fine luminous fibers, painted from the shared scene.
 * Each fiber is one recorded session; the layout owns every sample and clips
 * it to the measured extent and replay boundary before it reaches Canvas.
 */
import { gradeStroke, type TemporalPalette } from '../palette.ts';
import type { EvidenceGrade, FocusTreatment, SceneFiber, SceneNode, ScenePath } from '../types.ts';
import { focusAlpha, isLifted, isLink, recencyAlpha, recencySpan, type SceneFrame } from './frame.ts';
import { traceCurve } from './treatments.ts';

function evidenceColor(grade: EvidenceGrade, color: string | undefined, palette: TemporalPalette): string {
  if (palette.highContrast) return palette.text;
  if (grade === 'ambiguous' || grade === 'stale' || grade === 'unavailable') return gradeStroke(grade, palette).color;
  return color ?? gradeStroke(grade, palette).color;
}

function traceFiber(ctx: CanvasRenderingContext2D, fiber: SceneFiber): void {
  const first = fiber.points[0];
  if (!first) return;
  ctx.moveTo(first.x, first.y);
  for (let index = 1; index < fiber.points.length; index += 1) {
    const point = fiber.points[index]!;
    ctx.lineTo(point.x, point.y);
  }
}

/** Reuse the current path across the recovered diffuse, soft and fine passes.
 * Dashes stay on every pass, so a glow cannot turn an unsupported span solid. */
function strokeFiber(
  ctx: CanvasRenderingContext2D,
  grade: EvidenceGrade,
  focus: FocusTreatment,
  color: string | undefined,
  palette: TemporalPalette,
  relation = false,
): void {
  const stroke = gradeStroke(grade, palette);
  const ink = evidenceColor(grade, color, palette);
  const lifted = isLifted(focus);
  const quietRelation = relation && !lifted && grade === 'inferred';
  const alpha = focusAlpha(focus) * (palette.highContrast || lifted ? 1 : quietRelation ? 0.22 : relation ? 0.55 : 0.95);
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  ctx.setLineDash([...stroke.dash]);
  ctx.strokeStyle = ink;
  ctx.shadowColor = ink;

  if (!palette.highContrast && grade !== 'unavailable' && !quietRelation) {
    ctx.globalCompositeOperation = 'lighter';
    ctx.globalAlpha = alpha * (relation ? 0.065 : 0.14);
    ctx.lineWidth = lifted ? 5 : relation ? 3 : 3.5;
    ctx.shadowBlur = 16;
    ctx.stroke();
  }

  ctx.globalCompositeOperation = 'source-over';
  ctx.globalAlpha = alpha * (relation ? 0.2 : 0.32);
  ctx.lineWidth = lifted ? 2.4 : relation ? 1 : 1.6;
  ctx.shadowBlur = palette.highContrast || grade === 'unavailable' || quietRelation ? 0 : 4;
  ctx.stroke();

  ctx.shadowBlur = 0;
  ctx.globalAlpha = alpha;
  const coreWidth = focus === 'selected' ? 1.35 : focus === 'path' ? 1 : relation ? 0.65 : 0.85;
  ctx.lineWidth = palette.highContrast ? Math.max(1.2, coreWidth) : coreWidth;
  ctx.stroke();
}

function paintFiber(ctx: CanvasRenderingContext2D, fiber: SceneFiber, palette: TemporalPalette): void {
  if (fiber.points.length === 0) return;
  if (fiber.points.length === 1) {
    const point = fiber.points[0]!;
    ctx.globalAlpha = focusAlpha(fiber.focus) * (palette.highContrast ? 1 : 0.7);
    ctx.fillStyle = evidenceColor(fiber.grade, fiber.color, palette);
    ctx.beginPath();
    ctx.arc(point.x, point.y, 1.25, 0, Math.PI * 2);
    ctx.fill();
    return;
  }
  ctx.beginPath();
  traceFiber(ctx, fiber);
  strokeFiber(ctx, fiber.grade, fiber.focus, fiber.color, palette);
}

function paintRelation(ctx: CanvasRenderingContext2D, path: ScenePath, palette: TemporalPalette): void {
  ctx.beginPath();
  traceCurve(ctx, path);
  strokeFiber(ctx, path.grade, path.focus, path.color, palette, true);
}

/** These are the already projected event records, never decorative particles. */
function paintNode(ctx: CanvasRenderingContext2D, node: SceneNode, frame: SceneFrame, palette: TemporalPalette): void {
  const alpha = focusAlpha(node.focus) * (palette.highContrast || isLifted(node.focus) ? 1 : recencyAlpha(frame, node.x));
  const color = evidenceColor(node.grade, node.color, palette);
  ctx.globalCompositeOperation = 'source-over';
  ctx.globalAlpha = alpha * 0.85;
  ctx.fillStyle = color;
  ctx.shadowColor = color;
  ctx.shadowBlur = palette.highContrast ? 0 : node.selected ? 10 : 5;
  ctx.beginPath();
  ctx.arc(node.x, node.y, node.selected ? 2.6 : 1.25, 0, Math.PI * 2);
  ctx.fill();
  ctx.shadowBlur = 0;
  if (node.grade === 'unavailable') return;
  ctx.fillStyle = palette.text;
  ctx.globalAlpha = alpha * 0.75;
  ctx.beginPath();
  ctx.arc(node.x, node.y, 0.45, 0, Math.PI * 2);
  ctx.fill();
}

export function paintSpindle(ctx: CanvasRenderingContext2D, frame: SceneFrame, palette: TemporalPalette): void {
  const { model, fieldX0, fieldX1, top, height } = frame;
  const spindle = model.spindle;
  if (!spindle) return;
  const width = Math.max(1, fieldX1 - fieldX0);
  ctx.save();
  ctx.globalCompositeOperation = 'source-over';
  ctx.shadowBlur = 0;
  ctx.beginPath();
  ctx.rect(fieldX0, top, width, height - top);
  ctx.clip();
  ctx.setLineDash([]);

  // The time ruler stays legible behind the fibers without reintroducing rows.
  ctx.strokeStyle = palette.grid;
  ctx.lineWidth = 1;
  ctx.globalAlpha = 0.16;
  ctx.beginPath();
  for (const tick of model.ticks) {
    ctx.moveTo(Math.round(tick.x) + 0.5, top);
    ctx.lineTo(Math.round(tick.x) + 0.5, height);
  }
  ctx.stroke();

  for (const fiber of spindle.fibers) paintFiber(ctx, fiber, palette);
  for (const path of model.paths) {
    if (isLink(path)) paintRelation(ctx, path, palette);
    else if (path.kind === 'sequence' || path.kind === 'edit_link') {
      ctx.globalCompositeOperation = 'source-over';
      ctx.globalAlpha = 0.45 * focusAlpha(path.focus);
      ctx.strokeStyle = evidenceColor(path.grade, path.color, palette);
      ctx.lineWidth = 0.8;
      ctx.setLineDash([1, 3]);
      ctx.beginPath();
      traceCurve(ctx, path);
      ctx.stroke();
    }
  }
  ctx.setLineDash([]);

  // Retain the loaded-page recency channel; a selected family lifts above it.
  const span = recencySpan(frame);
  if (span && !palette.highContrast) {
    const veil = ctx.createLinearGradient(span.headX, 0, span.tailX, 0);
    veil.addColorStop(0, palette.substrate);
    veil.addColorStop(1, 'transparent');
    ctx.globalCompositeOperation = 'source-over';
    ctx.globalAlpha = 0.5;
    ctx.fillStyle = veil;
    ctx.fillRect(fieldX0, top, width, height - top);
  }
  for (const fiber of spindle.fibers) if (isLifted(fiber.focus)) paintFiber(ctx, fiber, palette);
  for (const path of model.paths) if (isLink(path) && isLifted(path.focus)) paintRelation(ctx, path, palette);
  for (const node of model.nodes) paintNode(ctx, node, frame, palette);
  ctx.setLineDash([]);
  ctx.restore();
}
