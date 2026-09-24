/**
 * What the scene paints from, and the encodings every mark shares: focus,
 * recency within the loaded page, and whether a lane resolves to glyphs.
 */
import { timeToX } from '../layout.ts';
import type { SceneDensity } from '../density.ts';
import type { FocusTreatment, SceneLane, TemporalSceneModel } from '../types.ts';

export interface SceneFrame {
  readonly model: TemporalSceneModel;
  readonly density: SceneDensity | null;
  readonly fieldX0: number;
  readonly fieldX1: number;
  /** Top of the field, below the time ruler. */
  readonly top: number;
  readonly height: number;
}

/** Below this row height a lane cannot hold a glyph plate and aggregates. */
export const LEGIBLE_PITCH_PX = 22;
/** Below this gap between neighbouring marks a lane aggregates. */
export const COLLIDE_PX = 9;
/** The oldest record in the page keeps this share of full luminance. */
export const RECENCY_FLOOR = 0.5;

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

export function isLifted(focus: FocusTreatment): boolean {
  return focus === 'selected' || focus === 'path';
}

/** Where a spawn-family path leaves its parent and lands on its child. The
 * layout centres the curve on the relation instant, so that x is the ends'
 * midpoint. */
export function pathEnds(controls: readonly number[]): { x0: number; y0: number; x1: number; y1: number } {
  const n = controls.length;
  return { x0: controls[0] ?? 0, y0: controls[1] ?? 0, x1: controls[n - 2] ?? 0, y1: controls[n - 1] ?? 0 };
}

/** The page's recency axis on screen: the oldest session start and NOW. */
export function recencySpan(frame: SceneFrame): { headX: number; tailX: number } | null {
  const head = frame.density?.headTime;
  const tail = frame.density?.tailTime;
  if (head == null || tail == null || tail <= head) return null;
  return { headX: timeToX(frame.model.viewport, head), tailX: timeToX(frame.model.viewport, tail) };
}

/** Luminance for a mark at `x`: full at NOW, `RECENCY_FLOOR` at the oldest start. */
export function recencyAlpha(frame: SceneFrame, x: number): number {
  const span = recencySpan(frame);
  if (!span) return 1;
  const r = Math.min(1, Math.max(0, (x - span.headX) / (span.tailX - span.headX)));
  return RECENCY_FLOOR + (1 - RECENCY_FLOOR) * r;
}

const resolvedByFrame = new WeakMap<SceneFrame, ReadonlySet<string>>();

/** Lanes drawn as rails and glyphs; every other lane draws its density
 * summary. A lane resolves when its row can hold a plate and its marks sit
 * apart, or when it is the expanded session. */
export function resolvedLanes(frame: SceneFrame): ReadonlySet<string> {
  const cached = resolvedByFrame.get(frame);
  if (cached) return cached;
  const resolves = (lane: SceneLane): boolean => {
    if (lane.expanded) return true;
    if (frame.model.zoom === 'workstream' || lane.height < LEGIBLE_PITCH_PX) return false;
    const gap = frame.density?.lanes.get(lane.id)?.minGap ?? Infinity;
    return gap >= COLLIDE_PX;
  };
  const ids = new Set(frame.model.lanes.filter(resolves).map((lane) => lane.id));
  resolvedByFrame.set(frame, ids);
  return ids;
}

/** Hatch rows across a band, clipped by the caller. */
export function hatch(ctx: CanvasRenderingContext2D, x0: number, y0: number, x1: number, y1: number, step: number): void {
  const h = y1 - y0;
  ctx.beginPath();
  for (let x = x0 - h; x < x1 + h; x += step) {
    ctx.moveTo(x, y1);
    ctx.lineTo(x + h, y0);
  }
  ctx.stroke();
}
