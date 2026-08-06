import { describe, expect, it } from 'vitest';
import { logFraction } from '../../viz/scale.ts';
import {
  costPerTurn,
  summarizeCoverage,
  summarizeProjectSpread,
  summarizeTokenMix,
} from './spend.ts';

/** The 25 rows the daemon served on 2026-07-25. Twenty of them are within a
 * few percent of 1.80B because every worktree shares one cache. */
const LIVE_PROJECTS = [
  { path: '/fast/projects/tracedecay', tokens_saved: 2_939_894_592 },
  { path: '/w/sqlite-storage-runtime-current', tokens_saved: 2_140_723_247 },
  { path: '/w/sqlite-storage-runtime', tokens_saved: 2_101_200_356 },
  { path: '/w/live-repair', tokens_saved: 2_078_590_272 },
  { path: '/w/runtime-hardening', tokens_saved: 1_946_100_344 },
  { path: '/w/pr8-migration', tokens_saved: 1_831_192_520 },
  { path: '/w/pr8-acceptance-runner', tokens_saved: 1_824_171_535 },
  { path: '/w/pr8-live-tools', tokens_saved: 1_824_065_209 },
  { path: '/w/pr8-move-symbol', tokens_saved: 1_802_722_260 },
  { path: '/w/pr8-kernel', tokens_saved: 1_801_796_023 },
  { path: '/w/plan-topology-integration', tokens_saved: 1_799_923_909 },
  { path: '/w/pr8-refresh', tokens_saved: 1_799_813_188 },
  { path: '/w/pr8-runtime', tokens_saved: 1_799_356_160 },
  { path: '/w/pr8-compat', tokens_saved: 1_796_821_496 },
  { path: '/w/plan-dashboard', tokens_saved: 1_799_400_000 },
  { path: '/w/plan-task-runtime', tokens_saved: 1_799_400_000 },
  { path: '/w/plan-lsp-hooks', tokens_saved: 1_799_400_000 },
  { path: '/w/plan-git-stack', tokens_saved: 1_799_400_000 },
  { path: '/w/plan-policy-anchors', tokens_saved: 1_799_400_000 },
  { path: '/w/pr8-automation', tokens_saved: 1_799_400_000 },
  { path: '/w/pr8-benchmark', tokens_saved: 1_799_400_000 },
  { path: '/w/pr8-refresh-surfaces', tokens_saved: 1_799_400_000 },
  { path: '/w/pr8-transport', tokens_saved: 1_799_400_000 },
  { path: '/w/pr8-context', tokens_saved: 1_799_400_000 },
  { path: '/fast/projects/tracedecay-astgrep', tokens_saved: 380_000_000 },
];

describe('summarizeProjectSpread', () => {
  it('finds the flat body of the real distribution and names only the deviations', () => {
    const spread = summarizeProjectSpread(LIVE_PROJECTS)!;
    expect(spread.count).toBe(25);
    expect(spread.median).toBeCloseTo(1_799_400_000, -6);
    expect(spread.flat).toBe(true);
    // Twenty rows sit within a tenth of the median — under three pixels apart
    // on a 24px rail, which is to say identical.
    expect(spread.typicalCount).toBe(20);
    expect(spread.deviations.map((row) => row.path)).toEqual([
      '/fast/projects/tracedecay-astgrep',
      '/fast/projects/tracedecay',
      '/w/sqlite-storage-runtime-current',
      '/w/sqlite-storage-runtime',
      '/w/live-repair',
    ]);
    expect(spread.deviations[0]!.deviation).toBeLessThan(-0.5);
    expect(spread.deviations[1]!.deviation).toBeGreaterThan(0.5);
  });

  it('does not claim flatness when the set has no flat body', () => {
    const spread = summarizeProjectSpread([
      { path: 'a', tokens_saved: 1000 },
      { path: 'b', tokens_saved: 500 },
      { path: 'c', tokens_saved: 100 },
      { path: 'd', tokens_saved: 10 },
    ])!;
    expect(spread.flat).toBe(false);
    // Median 300; not one of the four is within a tenth of it, so every row is
    // a reading and none of them belongs to a "typical" body.
    expect(spread.typicalCount).toBe(0);
    expect(spread.deviations).toHaveLength(4);
  });

  it('drops rows with no path or no saving rather than plotting a zero', () => {
    const spread = summarizeProjectSpread([
      { path: 'a', tokens_saved: 100 },
      { path: null, tokens_saved: 900 },
      { path: 'c', tokens_saved: 0 },
      { path: 'd', tokens_saved: null },
    ])!;
    expect(spread.count).toBe(1);
  });

  it('has nothing to summarize when nothing was saved', () => {
    expect(summarizeProjectSpread([])).toBeNull();
    expect(summarizeProjectSpread([{ path: 'a', tokens_saved: 0 }])).toBeNull();
  });
});

describe('summarizeTokenMix', () => {
  /** The live session ledger, 2026-07-25. */
  const LIVE = {
    cache_read_tokens: 365_936_726_111,
    cache_write_tokens: 243_694_418,
    input_tokens: 7_256_407_982,
    output_tokens: 858_386_349,
  };

  it('reports cache reads as the overwhelming majority', () => {
    const mix = summarizeTokenMix(LIVE)!;
    expect(mix.leader?.label).toBe('cache read');
    expect(Math.round(mix.leader!.share * 100)).toBe(98);
    expect(mix.dominant).toBe(true);
    expect(mix.classes.map((entry) => entry.label)).toEqual([
      'cache read',
      'input',
      'output',
      'cache write',
    ]);
  });

  it('does not claim dominance for an even mix', () => {
    const mix = summarizeTokenMix({ input_tokens: 100, output_tokens: 90 })!;
    expect(mix.dominant).toBe(false);
  });

  it('omits classes the ledger did not record rather than drawing them at zero', () => {
    const mix = summarizeTokenMix({ input_tokens: 10, output_tokens: 5 })!;
    expect(mix.classes).toHaveLength(2);
  });

  it('has nothing to report for an empty ledger', () => {
    expect(summarizeTokenMix({})).toBeNull();
    expect(summarizeTokenMix({ input_tokens: 0 })).toBeNull();
  });
});

/** The shared band, pinned against this surface's magnitudes: token classes run
 * 1,500-to-1, three orders of magnitude above the event counts `viz/scale.test`
 * covers, and the smallest class still has to draw a length. */
describe('logFraction over ledger magnitudes', () => {
  it('keeps the smallest live token class visible against the largest', () => {
    const smallest = logFraction(243_694_418, 365_936_726_111)!;
    expect(smallest).toBeGreaterThan(0.7);
    expect(logFraction(365_936_726_111, 365_936_726_111)).toBe(1);
  });

  it('returns null rather than a length when there is no ceiling', () => {
    expect(logFraction(5, 0)).toBeNull();
  });
});

describe('costPerTurn', () => {
  it('derives the live figure', () => {
    expect(costPerTurn(8148.9744974, 57_704)).toBeCloseTo(0.1412, 4);
  });

  it('returns null rather than zero when either side is missing', () => {
    expect(costPerTurn(undefined, 100)).toBeNull();
    expect(costPerTurn(100, 0)).toBeNull();
    expect(costPerTurn(100, undefined)).toBeNull();
  });
});

describe('summarizeCoverage', () => {
  it('measures how much of the ledger is provider-reported', () => {
    const coverage = summarizeCoverage({
      messages: 1_751_214,
      usage_messages: 138_317,
      tokenized_messages: 400_000,
      estimated_messages: 1_212_897,
      unknown_model_messages: 187_066,
    })!;
    // "cost_basis: mixed" in one word; this is what the mix actually is.
    expect(Math.round(coverage.measuredShare! * 100)).toBe(8);
    expect(coverage.tokenized).toBe(400_000);
    expect(coverage.estimated! + coverage.tokenized! + coverage.usage!).toBe(
      coverage.messages,
    );
  });

  /** An endpoint that never reported a class and one that counted the class at
   * zero were indistinguishable while every field was coalesced: both arrived
   * as `0`, and the provider share of a ledger nobody measured came out as a
   * confident 0%. */
  it('keeps a class the ledger never reported apart from one it counted at zero', () => {
    const coverage = summarizeCoverage({
      messages: 41_204,
      tokenized_messages: 0,
    })!;
    expect(coverage.messages).toBe(41_204);
    expect(coverage.usage).toBeNull();
    expect(coverage.measuredShare).toBeNull();
    // Served, and served as none — the one figure here that is a measurement.
    expect(coverage.tokenized).toBe(0);
    expect(coverage.estimated).toBeNull();
    expect(coverage.unknownModel).toBeNull();
  });

  it('still measures the share when only the other classes are missing', () => {
    const coverage = summarizeCoverage({ messages: 200, usage_messages: 50 })!;
    expect(coverage.measuredShare).toBe(0.25);
    expect(coverage.estimated).toBeNull();
  });

  it('has nothing to measure with no messages', () => {
    expect(summarizeCoverage({})).toBeNull();
    expect(summarizeCoverage({ messages: 0 })).toBeNull();
  });
});
