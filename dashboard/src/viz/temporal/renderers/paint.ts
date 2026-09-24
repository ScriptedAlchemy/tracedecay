/**
 * Canvas and SVG helpers every temporal renderer paints with.
 */
import type { FocusTreatment } from '../types.ts';

export function focusAlpha(focus: FocusTreatment): number {
  switch (focus) {
    case 'context':
      return 0.35;
    case 'path':
      return 0.8;
    case 'selected':
    case 'neutral':
      return 1;
    default: {
      const exhaustive: never = focus;
      throw new Error(`unknown focus treatment: ${String(exhaustive)}`);
    }
  }
}

/** Traces `[x0,y0,cx0,cy0,cx1,cy1,x1,y1]` as a cubic or `[x0,y0,x1,y1]` as a segment. */
export function tracePath(ctx: CanvasRenderingContext2D, controls: readonly number[]): void {
  if (controls.length >= 8) {
    ctx.moveTo(controls[0]!, controls[1]!);
    ctx.bezierCurveTo(controls[2]!, controls[3]!, controls[4]!, controls[5]!, controls[6]!, controls[7]!);
  } else if (controls.length >= 4) {
    ctx.moveTo(controls[0]!, controls[1]!);
    ctx.lineTo(controls[2]!, controls[3]!);
  }
}

/** Where a spawn-family path leaves its parent and lands on its child. The
 * layout centres the curve on the relation instant, so that x is the ends'
 * midpoint. */
export function pathEnds(controls: readonly number[]): { x0: number; y0: number; x1: number; y1: number } {
  const n = controls.length;
  return { x0: controls[0] ?? 0, y0: controls[1] ?? 0, x1: controls[n - 2] ?? 0, y1: controls[n - 1] ?? 0 };
}

/** Hatch rows across a band, clipped by the caller. */
export function hatch(ctx: CanvasRenderingContext2D, x0: number, y0: number, x1: number, y1: number, step: number, slope: 1 | -1): void {
  const h = y1 - y0;
  ctx.beginPath();
  for (let x = x0 - h; x < x1 + h; x += step) {
    if (slope > 0) {
      ctx.moveTo(x, y1);
      ctx.lineTo(x + h, y0);
    } else {
      ctx.moveTo(x, y0);
      ctx.lineTo(x + h, y1);
    }
  }
  ctx.stroke();
}
