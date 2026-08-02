import { describe, expect, it } from 'vitest';
import {
  type MemoryHrrCoverageV1,
} from '../../contracts/generated.ts';
import {
  composeTrustDistribution,
  factsBelow,
  hrrStatusLabel,
  summarizeHrrCoverage,
  summarizeLoadedTrust,
  trustSourceNote,
} from './trust.ts';

/** Exactly what the daemon served on 2026-07-25: ten buckets, every one zero,
 * because the producer names its rows `trust-<n>` and the consumer parses them
 * as a bare integer. */
const BROKEN_HISTOGRAM = Array.from({ length: 10 }, (_, bucket) => ({
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
  it('falls through the empty histogram to the status bands', () => {
    const distribution = composeTrustDistribution(BROKEN_HISTOGRAM, LIVE_STATUS, []);
    expect(distribution.source).toBe('status_bands');
    expect(distribution.total).toBe(182);
    expect(distribution.occupied).toBe(2);
    expect(distribution.degenerate).toBe(false);
    expect(distribution.bands.map((band) => band.count)).toEqual([0, 0, 21, 161]);
  });

  it('prefers the ten-bucket histogram whenever it carries any mass', () => {
    const histogram = BROKEN_HISTOGRAM.map((bucket) =>
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
    const distribution = composeTrustDistribution(BROKEN_HISTOGRAM, undefined, [
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
    const distribution = composeTrustDistribution(BROKEN_HISTOGRAM, undefined, []);
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
    expect(summary.count).toBe(96);
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

  it('has nothing to measure in an empty list', () => {
    expect(summarizeLoadedTrust([])).toBeNull();
  });
});

describe('factsBelow', () => {
  const distribution = composeTrustDistribution(BROKEN_HISTOGRAM, LIVE_STATUS, []);

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

describe('summarizeHrrCoverage', () => {
  const row = (
    category: string,
    coverage: number,
    status: MemoryHrrCoverageV1['status'],
  ): MemoryHrrCoverageV1 =>
    ({ category, coverage, status, facts: 10, hrr_vectors: 10 }) as MemoryHrrCoverageV1;

  /** The live six: uniformly near-total coverage, four banks not ready. */
  const LIVE = [
    row('decision', 0.9558, 'missing_vectors'),
    row('user_pref', 0.9729, 'missing_vectors'),
    row('project', 1, 'stale_bank'),
    row('code_area', 1, 'ready'),
    row('tool', 1, 'stale_bank'),
    row('general', 1, 'ready'),
  ];

  it('states the uniformity once and draws only the exceptions', () => {
    const summary = summarizeHrrCoverage(LIVE)!;
    expect(summary.categories).toBe(6);
    expect(summary.line).toContain('All 6 categories are at least 95%');
    expect(summary.line).toContain('4 of 6 banks are not ready');
    expect(summary.exceptions.map((e) => e.category)).toEqual([
      'decision',
      'user_pref',
      'project',
      'tool',
    ]);
  });

  it('says so plainly when every bank is ready', () => {
    const summary = summarizeHrrCoverage([row('a', 1, 'ready'), row('b', 0.99, 'ready')])!;
    expect(summary.exceptions).toHaveLength(0);
    expect(summary.line).toContain('Every bank is ready');
  });

  it('reports a real range instead of claiming uniformity when there is spread', () => {
    const summary = summarizeHrrCoverage([
      row('a', 1, 'ready'),
      row('b', 0.4, 'missing_vectors'),
    ])!;
    expect(summary.line).toContain('40% to 100%');
  });

  it('has nothing to say about no categories', () => {
    expect(summarizeHrrCoverage([])).toBeNull();
  });

  it('spells statuses out rather than printing identifiers', () => {
    expect(hrrStatusLabel('missing_vectors')).toBe('missing vectors');
    expect(hrrStatusLabel('stale_bank')).toBe('stale bank');
    expect(hrrStatusLabel('missing_bank')).toBe('no bank');
    expect(hrrStatusLabel('ready')).toBe('ready');
  });
});
