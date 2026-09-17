import { describe, expect, it } from 'vitest';
import {
  clampZoom,
  fitBounds,
  panBy,
  project,
  unproject,
  visibleBounds,
  zoomAbout,
  zoomLevel,
} from './sceneCamera.ts';

const BOUNDS = { x: [-0.5, 4.5] as const, y: [-0.3, 3.2] as const };
const VIEWPORT = { width: 800, height: 400 };

describe('scene camera', () => {
  it('contain-fits the bounds so the whole axis is visible with padding', () => {
    const camera = fitBounds(BOUNDS, VIEWPORT, 20);
    const shown = visibleBounds(camera, VIEWPORT);
    expect(shown.x[0]).toBeLessThanOrEqual(BOUNDS.x[0]);
    expect(shown.x[1]).toBeGreaterThanOrEqual(BOUNDS.x[1]);
    expect(shown.y[0]).toBeLessThanOrEqual(BOUNDS.y[0]);
    expect(shown.y[1]).toBeGreaterThanOrEqual(BOUNDS.y[1]);
    // The limiting axis (height here) exactly fills the padded viewport.
    const padded = (BOUNDS.y[1] - BOUNDS.y[0]) / camera.scale;
    expect(padded).toBeCloseTo(VIEWPORT.height - 40, 6);
    expect(zoomLevel(camera, camera)).toBe(1);
  });

  it('projects y-up world to y-down screen and back', () => {
    const camera = fitBounds(BOUNDS, VIEWPORT);
    const low = project(camera, VIEWPORT, 2, BOUNDS.y[0]);
    const high = project(camera, VIEWPORT, 2, BOUNDS.y[1]);
    expect(high.py).toBeLessThan(low.py);
    const back = unproject(camera, VIEWPORT, low.px, low.py);
    expect(back.x).toBeCloseTo(2, 9);
    expect(back.y).toBeCloseTo(BOUNDS.y[0], 9);
  });

  it('keeps the world point under the pointer fixed while zooming', () => {
    const camera = fitBounds(BOUNDS, VIEWPORT);
    const anchor = { px: 600, py: 100 };
    const before = unproject(camera, VIEWPORT, anchor.px, anchor.py);
    const zoomed = zoomAbout(camera, VIEWPORT, 2, anchor);
    const after = unproject(zoomed, VIEWPORT, anchor.px, anchor.py);
    expect(after.x).toBeCloseTo(before.x, 9);
    expect(after.y).toBeCloseTo(before.y, 9);
    expect(zoomLevel(zoomed, camera)).toBeCloseTo(2, 9);
  });

  it('pans in screen pixels and clamps zoom to a window around the fit', () => {
    const fit = fitBounds(BOUNDS, VIEWPORT);
    const panned = panBy(fit, 100, 0);
    expect(project(panned, VIEWPORT, fit.cx, fit.cy).px).toBeCloseTo(VIEWPORT.width / 2 + 100, 9);
    expect(clampZoom({ ...fit, scale: fit.scale / 1000 }, fit).scale).toBeCloseTo(fit.scale / 12, 12);
    expect(clampZoom({ ...fit, scale: fit.scale * 1000 }, fit).scale).toBeCloseTo(fit.scale / 0.6, 12);
    expect(clampZoom(fit, fit)).toBe(fit);
  });

  it('never divides by a zero viewport', () => {
    const camera = fitBounds(BOUNDS, { width: 0, height: 0 });
    expect(Number.isFinite(camera.scale)).toBe(true);
    expect(camera.scale).toBeGreaterThan(0);
  });
});
