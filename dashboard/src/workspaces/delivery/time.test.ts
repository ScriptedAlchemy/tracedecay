import { describe, expect, it } from 'vitest';
import {
  freshnessSteps,
  freshnessTier,
  relativeAge,
} from '../../ui/time.ts';

const DAY = 86_400;

describe('freshnessTier', () => {
  it('uses the documented ordered boundaries', () => {
    expect(freshnessTier(0)).toBe('live');
    expect(freshnessTier(DAY - 1)).toBe('live');
    expect(freshnessTier(DAY)).toBe('recent');
    expect(freshnessTier(7 * DAY)).toBe('aging');
    expect(freshnessTier(30 * DAY)).toBe('dormant');
  });

  it('clamps future observations to the live tier', () => {
    expect(freshnessTier(-120)).toBe('live');
  });
});

describe('freshnessSteps', () => {
  it('maps semantic tiers to an ordered non-colour shape', () => {
    expect(
      (['dormant', 'aging', 'recent', 'live'] as const).map((tier) =>
        freshnessSteps(tier),
      ),
    ).toEqual([1, 2, 3, 4]);
  });
});

describe('relativeAge', () => {
  it('keeps missing timestamps absent and floors values within a unit', () => {
    expect(relativeAge(undefined, 10_000)).toBeNull();
    expect(relativeAge(Number.NaN, 10_000)).toBeNull();
    expect(relativeAge(10_000 - 59 * 60 - 59, 10_000)).toBe('59m ago');
    expect(relativeAge(10_000 - 23 * 3600 - 3599, 10_000)).toBe('23h ago');
  });

  it('does not render a future timestamp as a negative age', () => {
    expect(relativeAge(10_120, 10_000)).toBe('just now');
  });
});
