import { describe, expect, it } from 'vitest';
import { logFraction } from './scale.ts';

/**
 * The log band exists because these distributions are 1,945-to-1: on a linear
 * rail every row below the leader draws nothing. The band has to stay one shared
 * scale, or two plates rank the same events differently.
 */
describe('logFraction', () => {
  it('puts zero at the floor and the ceiling at the top', () => {
    expect(logFraction(0, 6_774)).toBe(0);
    expect(logFraction(6_774, 6_774)).toBe(1);
  });

  it('lifts a single event to a visible length where a linear rail vanishes', () => {
    const one = logFraction(1, 6_774);
    expect(one).not.toBeNull();
    expect(one).toBeGreaterThan(0.07);
    expect(one).toBeLessThan(0.09);
    // What the same row would measure on a linear rail: a fifth of a pixel.
    expect(1 / 6_774).toBeLessThan(0.001);
  });

  it('ranks larger values longer', () => {
    expect(logFraction(10, 1_000)).toBeLessThan(logFraction(100, 1_000) ?? 0);
  });

  it('returns null rather than a length when there is no band to measure', () => {
    expect(logFraction(5, 0)).toBeNull();
    expect(logFraction(5, -1)).toBeNull();
    expect(logFraction(5, Number.NaN)).toBeNull();
    expect(logFraction(5, Number.POSITIVE_INFINITY)).toBeNull();
    expect(logFraction(Number.NaN, 10)).toBeNull();
  });

  it('clamps rather than overflowing the rail in either direction', () => {
    expect(logFraction(10_000, 100)).toBe(1);
    expect(logFraction(-5, 100)).toBe(0);
  });
});
