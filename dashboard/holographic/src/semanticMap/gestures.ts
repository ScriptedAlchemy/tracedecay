import type { PlacedPoint } from "./hitTest";
import { HOVER_RADIUS, screenToBaseCoords, type MapTransform } from "./transform";

export function panTransformFromDrag(
  origin: MapTransform,
  start: { x: number; y: number },
  current: { x: number; y: number },
): MapTransform {
  return {
    k: origin.k,
    tx: origin.tx + (current.x - start.x),
    ty: origin.ty + (current.y - start.y),
  };
}

export function selectIdsInScreenRect(
  placed: PlacedPoint[],
  transform: MapTransform,
  drag: { startX: number; startY: number; x: number; y: number },
): Set<number> | null {
  const x0 = Math.min(drag.startX, drag.x);
  const x1 = Math.max(drag.startX, drag.x);
  const y0 = Math.min(drag.startY, drag.y);
  const y1 = Math.max(drag.startY, drag.y);
  if (x1 - x0 < 4 && y1 - y0 < 4) return null;
  const ids = new Set<number>();
  const { k, tx, ty } = transform;
  for (const point of placed) {
    const sx = point.x * k + tx;
    const sy = point.y * k + ty;
    if (sx >= x0 && sx <= x1 && sy >= y0 && sy <= y1) {
      ids.add(point.point.fact_id);
    }
  }
  return ids.size > 0 ? ids : null;
}

export function pickPointAtScreen(
  placed: PlacedPoint[],
  transform: MapTransform,
  sx: number,
  sy: number,
  radiusPx = HOVER_RADIUS,
): PlacedPoint | null {
  const [bx, by] = screenToBaseCoords(transform, sx, sy);
  const radius = radiusPx / transform.k;
  let best: PlacedPoint | null = null;
  let bestDistance = Infinity;
  for (const point of placed) {
    const dx = point.x - bx;
    const dy = point.y - by;
    const distance = Math.hypot(dx, dy);
    if (distance <= radius && distance < bestDistance) {
      best = point;
      bestDistance = distance;
    }
  }
  return best;
}
