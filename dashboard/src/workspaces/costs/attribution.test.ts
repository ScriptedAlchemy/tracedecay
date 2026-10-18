import { describe, expect, it } from 'vitest';
import type {
  SavingsProviderDayPointV1,
  SavingsProviderSpendV1,
  SavingsProviderUsageAttributionV1,
} from '../../contracts/generated.ts';
import {
  buildSpendSeries,
  formatShare,
  formatUsd,
  formatUtcDay,
  pricingClassSentence,
  providerModelRows,
  shortRevision,
  sumReportedTokens,
  summarizeProviderLedger,
} from './attribution.ts';

function provider(overrides: Partial<SavingsProviderSpendV1> & { provider: string }): SavingsProviderSpendV1 {
  return {
    pricing: 'priced',
    usage_events: 10,
    priced_events: 10,
    unpriced_events: 0,
    unknown_model_events: 0,
    undated_events: 0,
    models: 1,
    priced_models: 1,
    unpriced_models: 0,
    sessions: 2,
    priced_cost_usd: 10,
    total_cost_usd: 10,
    total_tokens: 1_000,
    provider_actual: {
      input_tokens: 800,
      output_tokens: 200,
      cache_read_tokens: 5_000,
      cache_write_tokens: 40,
    },
    ...overrides,
  };
}

function attribution(
  rows: SavingsProviderSpendV1[],
  extra: Partial<SavingsProviderUsageAttributionV1> = {},
): SavingsProviderUsageAttributionV1 {
  return {
    available: true,
    pricing_revision: 'sha256:abcdef0123456789',
    undated_events: null,
    by_model: [],
    by_day: [],
    by_provider: rows,
    by_provider_day: [],
    ...extra,
  };
}

describe('summarizeProviderLedger', () => {
  it('returns null for an unserved attribution block, not an empty ledger', () => {
    expect(summarizeProviderLedger(attribution([], { available: false }))).toBeNull();
    const served = summarizeProviderLedger(attribution([]));
    expect(served).not.toBeNull();
    expect(served?.rows).toEqual([]);
    expect(served?.pricedTotalUsd).toBeNull();
    expect(served?.coverage).toBeNull();
    expect(served?.complete).toBe(false);
  });

  it('sums only priced dollars and discloses the coverage of that sum', () => {
    const ledger = summarizeProviderLedger(
      attribution([
        provider({ provider: 'claude', priced_cost_usd: 30, total_cost_usd: 30 }),
        provider({
          provider: 'codex',
          pricing: 'partial',
          usage_events: 10,
          priced_events: 6,
          unpriced_events: 4,
          models: 2,
          priced_models: 1,
          unpriced_models: 1,
          priced_cost_usd: 10,
          total_cost_usd: null,
        }),
        provider({
          provider: 'cursor',
          pricing: 'unpriced',
          usage_events: 5,
          priced_events: 0,
          unpriced_events: 5,
          unknown_model_events: 5,
          priced_models: 0,
          unpriced_models: 1,
          priced_cost_usd: null,
          total_cost_usd: null,
          total_tokens: null,
          provider_actual: null,
        }),
      ]),
    );
    expect(ledger).not.toBeNull();
    if (!ledger) return;
    expect(ledger.rows.map((row) => row.provider)).toEqual(['claude', 'codex', 'cursor']);
    expect(ledger.pricedTotalUsd).toBe(40);
    expect(ledger.usageEvents).toBe(25);
    expect(ledger.pricedEvents).toBe(16);
    expect(ledger.unpricedEvents).toBe(9);
    expect(ledger.coverage).toBeCloseTo(16 / 25);
    expect(ledger.complete).toBe(false);
    expect(ledger.unknownModelEvents).toBe(5);
    expect(ledger.classes).toEqual({ priced: ['claude'], partial: ['codex'], unpriced: ['cursor'] });
    // Shares are of the priced total, and an unpriced row has none.
    expect(ledger.rows[0]?.share).toBeCloseTo(0.75);
    expect(ledger.rows[1]?.share).toBeCloseTo(0.25);
    expect(ledger.rows[2]?.share).toBeNull();
    expect(ledger.rows[2]?.pricedCostUsd).toBeNull();
  });

  it('is complete only when every observed event priced', () => {
    const ledger = summarizeProviderLedger(
      attribution([provider({ provider: 'claude' }), provider({ provider: 'gemini', priced_cost_usd: 2, total_cost_usd: 2 })]),
    );
    expect(ledger?.complete).toBe(true);
    expect(ledger?.coverage).toBe(1);
    expect(ledger?.pricedTotalUsd).toBe(12);
  });

  it('prefers the served undated count over the per-provider sum', () => {
    const ledger = summarizeProviderLedger(
      attribution([provider({ provider: 'claude', undated_events: 3 })], { undated_events: 7 }),
    );
    expect(ledger?.undatedEvents).toBe(7);
    const summed = summarizeProviderLedger(
      attribution([provider({ provider: 'claude', undated_events: 3 })], { undated_events: null }),
    );
    expect(summed?.undatedEvents).toBe(3);
  });

  it('ranks unpriced providers below priced ones regardless of usage', () => {
    const ledger = summarizeProviderLedger(
      attribution([
        provider({
          provider: 'cursor',
          pricing: 'unpriced',
          usage_events: 9_000,
          priced_events: 0,
          unpriced_events: 9_000,
          priced_cost_usd: null,
          total_cost_usd: null,
        }),
        provider({ provider: 'claude', priced_cost_usd: 0.01, total_cost_usd: 0.01, usage_events: 1, priced_events: 1 }),
      ]),
    );
    expect(ledger?.rows.map((row) => row.provider)).toEqual(['claude', 'cursor']);
  });
});

describe('sumReportedTokens', () => {
  it('leaves an unreported provider out and counts it', () => {
    const ledger = summarizeProviderLedger(
      attribution([
        provider({ provider: 'a', total_tokens: 100 }),
        provider({ provider: 'b', total_tokens: null }),
        provider({ provider: 'c', total_tokens: 50 }),
      ]),
    );
    expect(sumReportedTokens(ledger?.rows ?? [])).toEqual({ tokens: 150, reported: 2, unreported: 1 });
    expect(sumReportedTokens([])).toEqual({ tokens: null, reported: 0, unreported: 0 });
  });
});

describe('buildSpendSeries', () => {
  const day = 1_760_054_400; // a UTC midnight
  const point = (
    overrides: Partial<SavingsProviderDayPointV1> & { provider: string; day: number },
  ): SavingsProviderDayPointV1 => ({
    usage_events: 4,
    priced_events: 4,
    unpriced_events: 0,
    priced_cost_usd: 1,
    total_cost_usd: 1,
    total_tokens: 400,
    ...overrides,
  });

  it('returns null with no dated points', () => {
    expect(buildSpendSeries([], ['claude'])).toBeNull();
  });

  it('keeps ledger order, leaves gaps as null, and counts unpriced buckets', () => {
    const series = buildSpendSeries(
      [
        point({ provider: 'codex', day, priced_cost_usd: 2, total_cost_usd: 2 }),
        point({ provider: 'claude', day: day + 86_400, priced_cost_usd: 3, total_cost_usd: 3 }),
        point({
          provider: 'codex',
          day: day + 86_400,
          priced_events: 2,
          unpriced_events: 2,
          priced_cost_usd: 1,
          total_cost_usd: null,
        }),
        point({
          provider: 'cursor',
          day,
          priced_events: 0,
          unpriced_events: 4,
          priced_cost_usd: null,
          total_cost_usd: null,
        }),
      ],
      ['claude', 'codex', 'cursor'],
    );
    expect(series).not.toBeNull();
    if (!series) return;
    expect(series.days).toEqual([day, day + 86_400]);
    expect(series.series.map((entry) => entry.provider)).toEqual(['claude', 'codex', 'cursor']);
    // claude has no usage on the first day: a gap, not a zero.
    expect(series.series[0]?.points[0]).toBeNull();
    expect(series.series[0]?.points[1]?.pricedCostUsd).toBe(3);
    expect(series.series[1]?.points[1]).toEqual({ pricedCostUsd: 1, usageEvents: 4, unpricedEvents: 2 });
    expect(series.series[2]?.points[0]?.pricedCostUsd).toBeNull();
    expect(series.unpricedBuckets).toBe(2);
    expect(series.partialBuckets).toBe(1);
  });

  it('appends a provider the ledger did not list rather than dropping its days', () => {
    const series = buildSpendSeries([point({ provider: 'zeta', day })], ['claude']);
    expect(series?.series.map((entry) => entry.provider)).toEqual(['zeta']);
  });
});

describe('providerModelRows', () => {
  it('filters to the provider and ranks priced groups first', () => {
    const rows = providerModelRows(
      [
        { provider: 'codex', model: 'gpt-x', usage_events: 3, cost_usd: null, total_tokens: 30, cost_basis: 'provider_reported_unpriced', provider_actual: null },
        { provider: 'codex', model: 'gpt-5', usage_events: 1, cost_usd: 4, total_tokens: 10, cost_basis: 'provider_reported_priced', provider_actual: null },
        { provider: 'claude', model: 'opus', usage_events: 9, cost_usd: 9, total_tokens: 90, cost_basis: 'provider_reported_priced', provider_actual: null },
        { provider: 'codex', model: null, usage_events: 2, cost_usd: null, total_tokens: null, cost_basis: 'provider_reported_unpriced', provider_actual: null },
      ],
      'codex',
    );
    expect(rows.map((row) => [row.model, row.costUsd])).toEqual([
      ['gpt-5', 4],
      ['gpt-x', null],
      [null, null],
    ]);
  });
});

describe('formatting', () => {
  it('never prints a manufactured figure for an absent value', () => {
    expect(formatUsd(null)).toBe('—');
    expect(formatUsd(Number.NaN)).toBe('—');
    expect(formatUsd(1234.5)).toBe('$1,234.50');
    expect(formatShare(null)).toBe('—');
    expect(formatShare(0.4791)).toBe('47.9%');
    expect(formatUtcDay(1_760_054_400)).toBe('2025-10-10');
    expect(shortRevision(null)).toBe('not reported');
    expect(shortRevision('sha256:abcdef0123456789')).toBe('sha256:abcdef0123…');
    expect(shortRevision('short')).toBe('short');
  });

  it('words each pricing class as a statement about coverage', () => {
    const ledger = summarizeProviderLedger(
      attribution([
        provider({ provider: 'a' }),
        provider({ provider: 'b', pricing: 'partial', priced_events: 4, unpriced_events: 6 }),
        provider({ provider: 'c', pricing: 'unpriced', priced_events: 0, unpriced_events: 10, priced_cost_usd: null }),
      ]),
    );
    const [a, b, c] = ledger?.rows ?? [];
    expect(a && pricingClassSentence(a)).toMatch(/every one of 10 usage events priced/);
    expect(b && pricingClassSentence(b)).toMatch(/4 of 10 usage events priced; the dollar figure excludes the other 6/);
    expect(c && pricingClassSentence(c)).toMatch(/none of 10 usage events could be priced/);
  });
});
