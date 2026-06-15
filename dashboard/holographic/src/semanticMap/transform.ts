import type { PlacedPoint } from "./hitTest";

export interface MapTransform {
  k: number;
  tx: number;
  ty: number;
}

export const IDENTITY: MapTransform = { k: 1, tx: 0, ty: 0 };
export const MIN_ZOOM = 0.5;
export const MAX_ZOOM = 24;
export const FIT_MAX_ZOOM = 8;
export const HOVER_RADIUS = 22;
const PAN_STEP = 48;

export function screenToBaseCoords(transform: MapTransform, sx: number, sy: number) {
  const { k, tx, ty } = transform;
  return [(sx - tx) / k, (sy - ty) / k] as const;
}

export function zoomTransformAt(
  previous: MapTransform,
  sx: number,
  sy: number,
  factor: number,
): MapTransform {
  const k = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, previous.k * factor));
  if (k === previous.k) return previous;
  const scale = k / previous.k;
  return {
    k,
    tx: sx - (sx - previous.tx) * scale,
    ty: sy - (sy - previous.ty) * scale,
  };
}

export function fitTransformToPlaced(
  targets: PlacedPoint[],
  width: number,
  height: number,
  margin = 48,
): MapTransform {
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const point of targets) {
    x0 = Math.min(x0, point.x);
    y0 = Math.min(y0, point.y);
    x1 = Math.max(x1, point.x);
    y1 = Math.max(y1, point.y);
  }
  const spanX = Math.max(x1 - x0, 1e-6);
  const spanY = Math.max(y1 - y0, 1e-6);
  const fitK = Math.min((width - margin * 2) / spanX, (height - margin * 2) / spanY);
  const k = Math.min(FIT_MAX_ZOOM, Math.max(MIN_ZOOM, fitK));
  const cx = (x0 + x1) / 2;
  const cy = (y0 + y1) / 2;
  return { k, tx: width / 2 - cx * k, ty: height / 2 - cy * k };
}

export function semanticMapKeyResult({
  key,
  transform,
  width,
  height,
}: {
  key: string;
  transform: MapTransform;
  width: number;
  height: number;
}) {
  switch (key) {
    case "ArrowLeft":
      return {
        handled: true,
        nextTransform: { ...transform, tx: transform.tx + PAN_STEP, ty: transform.ty },
        clearSelection: false,
        clearSelected: false,
      };
    case "ArrowRight":
      return {
        handled: true,
        nextTransform: { ...transform, tx: transform.tx - PAN_STEP, ty: transform.ty },
        clearSelection: false,
        clearSelected: false,
      };
    case "ArrowUp":
      return {
        handled: true,
        nextTransform: { ...transform, tx: transform.tx, ty: transform.ty + PAN_STEP },
        clearSelection: false,
        clearSelected: false,
      };
    case "ArrowDown":
      return {
        handled: true,
        nextTransform: { ...transform, tx: transform.tx, ty: transform.ty - PAN_STEP },
        clearSelection: false,
        clearSelected: false,
      };
    case "+":
    case "=":
      return {
        handled: true,
        nextTransform: zoomTransformAt(transform, width / 2, height / 2, 1.3),
        clearSelection: false,
        clearSelected: false,
      };
    case "-":
    case "_":
      return {
        handled: true,
        nextTransform: zoomTransformAt(transform, width / 2, height / 2, 1 / 1.3),
        clearSelection: false,
        clearSelected: false,
      };
    case "0":
      return {
        handled: true,
        nextTransform: IDENTITY,
        clearSelection: false,
        clearSelected: false,
      };
    case "Escape":
      return {
        handled: true,
        nextTransform: transform,
        clearSelection: true,
        clearSelected: true,
      };
    default:
      return {
        handled: false,
        nextTransform: transform,
        clearSelection: false,
        clearSelected: false,
      };
  }
}
