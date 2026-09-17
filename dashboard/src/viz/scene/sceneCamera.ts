/**
 * Orthographic 2.5D camera arithmetic for a measured field, kept pure so the
 * same numbers drive the scene runtime, the DOM label overlay, the SVG axis
 * overlay and the minimap — and so a fit, a zoom and a projection can be
 * asserted without a renderer.
 *
 * World is y-up (heavier bodies sit higher). Screen is y-down CSS pixels.
 */

export interface Viewport {
  readonly width: number;
  readonly height: number;
}

export interface WorldBounds {
  readonly x: readonly [number, number];
  readonly y: readonly [number, number];
}

export interface CameraState {
  /** World coordinate at the viewport centre. */
  readonly cx: number;
  readonly cy: number;
  /** World units per CSS pixel. Smaller is closer. */
  readonly scale: number;
}

/** Contain-fit `bounds` inside `viewport` with `padPx` of clearance on every
 * side. A degenerate viewport yields a unit camera rather than NaN. */
export function fitBounds(bounds: WorldBounds, viewport: Viewport, padPx = 0): CameraState {
  const width = Math.max(1, viewport.width - 2 * padPx);
  const height = Math.max(1, viewport.height - 2 * padPx);
  const spanX = Math.max(1e-6, bounds.x[1] - bounds.x[0]);
  const spanY = Math.max(1e-6, bounds.y[1] - bounds.y[0]);
  return {
    cx: (bounds.x[0] + bounds.x[1]) / 2,
    cy: (bounds.y[0] + bounds.y[1]) / 2,
    scale: Math.max(spanX / width, spanY / height),
  };
}

export function project(
  camera: CameraState,
  viewport: Viewport,
  x: number,
  y: number,
): { px: number; py: number } {
  return {
    px: viewport.width / 2 + (x - camera.cx) / camera.scale,
    py: viewport.height / 2 - (y - camera.cy) / camera.scale,
  };
}

export function unproject(
  camera: CameraState,
  viewport: Viewport,
  px: number,
  py: number,
): { x: number; y: number } {
  return {
    x: camera.cx + (px - viewport.width / 2) * camera.scale,
    y: camera.cy - (py - viewport.height / 2) * camera.scale,
  };
}

/** Zoom by `factor` (>1 closes in) keeping the world point under `anchor`
 * fixed on screen, so the zoom is pointer-centred rather than centre-centred. */
export function zoomAbout(
  camera: CameraState,
  viewport: Viewport,
  factor: number,
  anchor: { px: number; py: number },
): CameraState {
  const before = unproject(camera, viewport, anchor.px, anchor.py);
  const scale = camera.scale / factor;
  const after = unproject({ ...camera, scale }, viewport, anchor.px, anchor.py);
  return { cx: camera.cx + (before.x - after.x), cy: camera.cy + (before.y - after.y), scale };
}

export function panBy(camera: CameraState, dxPx: number, dyPx: number): CameraState {
  return { ...camera, cx: camera.cx - dxPx * camera.scale, cy: camera.cy + dyPx * camera.scale };
}

/** Bound the zoom to a window around the fit: no closer than `maxZoom`× and
 * no further than `minZoom`× the fit scale, so a wheel cannot lose the field. */
export function clampZoom(
  camera: CameraState,
  fit: CameraState,
  minZoom = 0.6,
  maxZoom = 12,
): CameraState {
  const scale = Math.min(fit.scale / minZoom, Math.max(fit.scale / maxZoom, camera.scale));
  return scale === camera.scale ? camera : { ...camera, scale };
}

/** Zoom factor relative to the fit, for the readout: 1 is Fit. */
export function zoomLevel(camera: CameraState, fit: CameraState): number {
  return fit.scale / camera.scale;
}

/** The world rectangle the viewport currently shows. */
export function visibleBounds(camera: CameraState, viewport: Viewport): WorldBounds {
  const halfW = (viewport.width * camera.scale) / 2;
  const halfH = (viewport.height * camera.scale) / 2;
  return { x: [camera.cx - halfW, camera.cx + halfW], y: [camera.cy - halfH, camera.cy + halfH] };
}

export function cameraEquals(a: CameraState, b: CameraState, epsilon = 1e-9): boolean {
  return (
    Math.abs(a.cx - b.cx) <= epsilon
    && Math.abs(a.cy - b.cy) <= epsilon
    && Math.abs(a.scale - b.scale) <= epsilon
  );
}
