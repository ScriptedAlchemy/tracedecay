import { describe, expect, it } from 'vitest';
import {
  composeTrustDistribution,
  factsBelow,
  summarizeLoadedTrust,
  trustSourceNote,
} from './trust.ts';

/** A canonical empty histogram for an empty store. */
const EMPTY_HISTOGRAM = Array.from({ length: 10 }, (_, bucket) => ({
  bucket,
  label: `${(bucket / 10).toFixed(1)}–${((bucket + 1) / 10).toFixed(1)}`,
  count: 0,
}));

/** The same store, via `/api/plugins/holographic/status`. 182 facts. */
const LIVE_STATUS = {
  trust_0_025_count: 0,
  trust_025_050_count: 0,
  trust_050_075_count: 21,
  trust_075_100_count: 161,
};

describe('composeTrustDistribution', () => {
  it('uses the current status bands when a canonical histogram carries no facts', () => {
    const distribution = composeTrustDistribution(EMPTY_HISTOGRAM, LIVE_STATUS, []);
    expect(distribution.source).toBe('status_bands');
    expect(distribution.total).toBe(182);
    expect(distribution.occupied).toBe(2);
    expect(distribution.degenerate).toBe(false);
    expect(distribution.bands.map((band) => band.count)).toEqual([0, 0, 21, 161]);
  });

  it('prefers the ten-bucket histogram whenever it carries any mass', () => {
    const histogram = EMPTY_HISTOGRAM.map((bucket) =>
      bucket.bucket === 9 ? { ...bucket, count: 40 } : bucket,
    );
    const distribution = composeTrustDistribution(histogram, LIVE_STATUS, []);
    expect(distribution.source).toBe('histogram');
    expect(distribution.total).toBe(40);
    // One occupied bucket out of ten: nothing to draw, and the view must say so
    // rather than render one bar beside nine empty ones.
    expect(distribution.degenerate).toBe(true);
  });

  it('falls all the way through to the loaded facts when no store source answers', () => {
    const distribution = composeTrustDistribution(EMPTY_HISTOGRAM, undefined, [
      { trust_score: 1 },
      { trust_score: 0.95 },
      { trust_score: 0.42 },
    ]);
    expect(distribution.source).toBe('loaded_facts');
    expect(distribution.total).toBe(3);
    expect(distribution.occupied).toBe(2);
    expect(distribution.bands[9]?.count).toBe(2);
    expect(distribution.bands[4]?.count).toBe(1);
  });

  it('reports nothing rather than a plate of zeroes when every source is empty', () => {
    const distribution = composeTrustDistribution(EMPTY_HISTOGRAM, undefined, []);
    expect(distribution.source).toBe('none');
    expect(distribution.bands).toEqual([]);
    expect(distribution.total).toBe(0);
  });

  it('names what the counts of each source actually cover', () => {
    expect(trustSourceNote('status_bands')).toContain('every fact in the store');
    expect(trustSourceNote('loaded_facts')).toContain('only the facts loaded');
    expect(trustSourceNote('none')).toContain('no source');
  });
});

describe('summarizeLoadedTrust', () => {
  /** The 96 facts the top-100 slice returns: 46 at exactly 1.00 and the rest
   * between 0.90 and 0.99. Nothing below 0.90 reaches this slice. */
  const LOADED = [
    ...Array.from({ length: 46 }, () => ({ trust_score: 1 })),
    ...Array.from({ length: 50 }, (_, i) => ({ trust_score: 0.9 + (i % 10) / 100 })),
  ];

  it('measures the loaded slice and calls it flat', () => {
    const summary = summarizeLoadedTrust(LOADED)!;
    expect(summary.total).toBe(96);
    expect(summary.measured).toBe(96);
    expect(summary.unavailable).toBe(0);
    expect(summary.min).toBeCloseTo(0.9, 5);
    expect(summary.max).toBe(1);
    expect(summary.atMax).toBe(46);
    expect(summary.spread).toBeCloseTo(0.1, 5);
    // A per-row rail scaled 0-1 would be between 90% and 100% full on every
    // single row: the same length, drawn 96 times.
    expect(summary.flat).toBe(true);
  });

  it('does not call a genuinely spread slice flat', () => {
    const summary = summarizeLoadedTrust([
      { trust_score: 0.99 },
      { trust_score: 0.5 },
      { trust_score: 0.08 },
    ])!;
    expect(summary.flat).toBe(false);
    expect(summary.spread).toBeCloseTo(0.91, 5);
  });

  it('keeps loaded rows separate from the subset with a trust measurement', () => {
    const summary = summarizeLoadedTrust([{ trust_score: 0.8 }, { trust_score: null }])!;
    expect(summary.total).toBe(2);
    expect(summary.measured).toBe(1);
    expect(summary.unavailable).toBe(1);
    expect(summary.min).toBe(0.8);
    expect(summary.max).toBe(0.8);
  });

  it('has nothing to measure in an empty list', () => {
    expect(summarizeLoadedTrust([])).toBeNull();
  });
});

describe('factsBelow', () => {
  const distribution = composeTrustDistribution(EMPTY_HISTOGRAM, LIVE_STATUS, []);

  it('counts the store facts the loaded slice never reaches', () => {
    expect(factsBelow(distribution, 0.75)).toBe(21);
    expect(factsBelow(distribution, 0.5)).toBe(0);
  });

  it('refuses to interpolate inside a band', () => {
    // 0.90 falls inside the occupied 0.75-1.00 band; any answer would be a
    // guess about how that band's 161 facts are spread.
    expect(factsBelow(distribution, 0.9)).toBeNull();
  });

  it('answers null when there is no distribution at all', () => {
    expect(factsBelow(composeTrustDistribution([], undefined, []), 0.75)).toBeNull();
  });
});
