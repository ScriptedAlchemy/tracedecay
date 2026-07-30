/**
 * The legend ribbon is the renderer's curve, not a picture of it.
 *
 * A key that merely looks like the field is the classic place a second source of
 * truth grows: the field's channels taper by `taperAt`, and a sample drawn
 * freehand would go on explaining a shape the field stopped having. These
 * assertions read the sample's own path back out and hold it to `taperAt`
 * point-for-point, so an inlined easing curve here fails rather than drifts.
 */
import { describe, expect, it } from 'vitest';

import { taperAt } from '../../viz/trace/render.ts';
import { SAMPLE_STEPS, sampleRibbon } from './TraceLegend.tsx';

const WIDTH = 68;
const HEIGHT = 14;
/**
 * Coordinates are emitted at two decimals, so a comparison between two of them
 * carries two roundings. Anything looser than this would stop being a check on
 * the curve: a linear ramp misses `taperAt` by about a pixel at mid-run.
 */
const ROUNDING = 0.011;

function points(path: string): ReadonlyArray<{ x: number; y: number }> {
  return path
    .replace(/^M/, '')
    .replace(/Z$/, '')
    .split('L')
    .map((pair) => {
      const [x, y] = pair.split(',');
      return { x: Number(x), y: Number(y) };
    });
}

describe('sampleRibbon', () => {
  it('draws one closed outline of both edges', () => {
    const path = sampleRibbon(WIDTH, HEIGHT);
    expect(path.startsWith('M')).toBe(true);
    expect(path.endsWith('Z')).toBe(true);
    expect(points(path)).toHaveLength(SAMPLE_STEPS * 2);
  });

  it('spans exactly the box it was given', () => {
    const all = points(sampleRibbon(WIDTH, HEIGHT));
    const xs = all.map((point) => point.x);
    expect(Math.min(...xs)).toBe(0);
    expect(Math.max(...xs)).toBe(WIDTH);
    for (const point of all) {
      expect(point.y).toBeGreaterThanOrEqual(0);
      expect(point.y).toBeLessThanOrEqual(HEIGHT);
    }
  });

  it('takes its half-width from taperAt at every step', () => {
    const all = points(sampleRibbon(WIDTH, HEIGHT));
    const upper = all.slice(0, SAMPLE_STEPS);
    const maxHalf = HEIGHT / 2 - 0.5;
    upper.forEach((point, i) => {
      const t = i / (SAMPLE_STEPS - 1);
      // The renderer's own law: accumulate flow linearly down the channel, then
      // take its root. Both endpoints are measurements — the head fraction at
      // t=0 and the full measured width at the mouth — and the shaping happens
      // strictly between them.
      expect(Math.abs(HEIGHT / 2 - point.y - maxHalf * taperAt(t))).toBeLessThanOrEqual(ROUNDING);
    });
    expect(Math.abs(HEIGHT / 2 - upper[SAMPLE_STEPS - 1]!.y - maxHalf)).toBeLessThanOrEqual(
      ROUNDING,
    );
  });

  it('is symmetric about the mid line and widens monotonically', () => {
    const all = points(sampleRibbon(WIDTH, HEIGHT));
    const upper = all.slice(0, SAMPLE_STEPS);
    // The lower edge is emitted reversed so the path closes; pairing it back up
    // is what makes the two edges comparable.
    const lower = [...all.slice(SAMPLE_STEPS)].reverse();
    upper.forEach((point, i) => {
      expect(point.x).toBeCloseTo(lower[i]!.x, 2);
      expect(Math.abs(point.y + lower[i]!.y - HEIGHT)).toBeLessThanOrEqual(ROUNDING);
      if (i > 0) expect(point.y).toBeLessThanOrEqual(upper[i - 1]!.y);
    });
  });
});
