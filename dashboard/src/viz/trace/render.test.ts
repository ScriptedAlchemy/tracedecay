/**
 * The measurement→mark laws in `render.ts`, tested without a canvas.
 *
 * These are the functions that turn a counted quantity into pixels, so they
 * are exactly where a "nicer looking" tweak can quietly stop telling the
 * truth. The taper is the live example: it is a shape choice, and a shape
 * choice is only allowed here if the measured widths survive it intact.
 */
import { describe, expect, it } from 'vitest';

import { channelWidth, sillWidth, taperAt } from './render.ts';

describe('channel and sill widths', () => {
  it('puts a square root between a call-site count and a width', () => {
    // Magnitude reaches the eye through area, not radius — the same rule the
    // Code spine's mark diameter follows.
    expect(channelWidth(4) - channelWidth(1)).toBeCloseTo(1.15, 5);
    expect(channelWidth(16) - channelWidth(9)).toBeCloseTo(1.15, 5);
  });

  it('draws an unmeasured degree at the floor rather than as a measured zero', () => {
    expect(sillWidth(null)).toBe(sillWidth(0));
    expect(sillWidth(null)).toBeGreaterThan(0);
  });
});

describe('the hydrological taper', () => {
  it('lands exactly on both measured widths', () => {
    // The whole licence for shaping the run is that the ends are untouched.
    // If either endpoint drifts, the taper has started editing a measurement.
    expect(taperAt(0, 0.55)).toBeCloseTo(0.55, 12);
    expect(taperAt(1, 0.55)).toBe(1);
    expect(taperAt(0, 0.3)).toBeCloseTo(0.3, 12);
    expect(taperAt(1, 0.3)).toBe(1);
  });

  it('widens monotonically from head to mouth', () => {
    let previous = -Infinity;
    for (let i = 0; i <= 40; i += 1) {
      const width = taperAt(i / 40);
      expect(width).toBeGreaterThan(previous);
      previous = width;
    }
  });

  it('clamps outside the run instead of extrapolating a width', () => {
    expect(taperAt(-3)).toBe(taperAt(0));
    expect(taperAt(9)).toBe(taperAt(1));
  });
});
