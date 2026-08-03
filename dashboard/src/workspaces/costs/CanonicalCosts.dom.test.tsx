import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { MetricValueV1 } from '../../contracts/generated.ts';
import { CanonicalCosts } from './CanonicalCosts.tsx';

/**
 * The canonical cost read exists to make an unpriced ledger visible. Prices are
 * recorded at ingest, so a read over turns that were never priced arrives with
 * `provider_cost: null` and `pricing_revision_unavailable` — and $0.00 is the
 * single most damaging thing this surface could print in its place.
 *
 * Latency is the other assertion class here. The projection carries no latency
 * measurement at all, so the panel must say that rather than borrowing the
 * retrieval-side percentile Observatory reports over a different population.
 */

const NOW_MICROS = 1_753_003_600_000_000;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Canonical cost observations', () => {
  it('renders usage measurements with their eligible population and coverage', async () => {
    renderCosts(
      readModel({
        usage: [
          costMetric('provider_tokens', 1_284_000, 'tokens', 'ingested_provider_turns', 8_412),
          costMetric('saved_tokens', 96_400, 'tokens', 'eligible_savings_calls', 512),
        ],
        estimated_cost: [pricedCost()],
      }),
    );

    expect(await screen.findByText('provider tokens')).toBeTruthy();
    expect(screen.getByText('1,284,000')).toBeTruthy();
    expect(screen.getByText('per ingested provider turns · 8,412')).toBeTruthy();
    expect(screen.getByText('saved tokens')).toBeTruthy();
    expect(screen.getByText('per eligible savings calls · 512')).toBeTruthy();
  });

  it('renders an unpriced cost as its reason, never as $0.00 or a zero', async () => {
    renderCosts(
      readModel({
        usage: [
          costMetric('provider_tokens', 1_284_000, 'tokens', 'ingested_provider_turns', 8_412),
        ],
        estimated_cost: [pricedCost()],
      }),
    );

    const cost = await screen.findByText('provider cost');
    const plate = cost.closest('[data-metric]');
    expect(plate?.getAttribute('data-metric-available')).toBe('false');
    expect(plate?.textContent).toContain('pricing_revision_unavailable');
    expect(plate?.textContent).toContain('—');
    expect(screen.queryByText('$0.00')).toBeNull();
    expect(plate?.textContent).not.toContain('0 usd');
  });

  it('distinguishes an unattached pricing revision from a priced read', async () => {
    renderCosts(
      readModel({
        usage: [],
        estimated_cost: [pricedCost()],
        pricing_revision: null,
      }),
    );

    expect(await screen.findByText('none attached to this read')).toBeTruthy();
  });

  it('renders an all-time horizon as unbounded rather than as 1970', async () => {
    renderCosts(
      readModel({
        usage: [],
        estimated_cost: [pricedCost()],
        horizon: { since_micros: 0, until_micros: NOW_MICROS },
      }),
    );

    await screen.findByText('provider cost');
    expect(screen.getByText(/^unbounded → /)).toBeTruthy();
    expect(screen.queryByText(/1970-01-01/)).toBeNull();
  });

  it('states that no provider latency is measured instead of borrowing one', async () => {
    renderCosts(
      readModel({ usage: [], estimated_cost: [pricedCost()] }),
    );

    expect(await screen.findByText('latency breakdown')).toBeTruthy();
    const panel = document.querySelector('[data-costs-latency="unavailable"]');
    expect(panel?.textContent).toContain('no provider latency is measured');
    expect(panel?.textContent).toContain(
      'Neither the accounting-turn ledger nor the savings ledger records a per-call duration',
    );
    // Retrieval latency is named as living elsewhere, not shown as a figure.
    expect(panel?.textContent).toContain('feedback latency p95');
    expect(panel?.textContent).not.toMatch(/\dms/);
  });

  it('reports an unreachable daemon as offline rather than as a zero bill', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('connection refused');
      }),
    );
    renderWith();

    expect(await screen.findByText('Offline')).toBeTruthy();
    expect(screen.queryByText('$0.00')).toBeNull();
  });
});

function renderCosts(payload: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => new Response(JSON.stringify(envelope(payload)), { status: 200 })),
  );
  renderWith();
}

function renderWith() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <CanonicalCosts />
    </QueryClientProvider>,
  );
}

function readModel(overrides: {
  usage: MetricValueV1[];
  estimated_cost: MetricValueV1[];
  pricing_revision?: string | null;
  horizon?: { since_micros: number; until_micros: number };
}) {
  return {
    authorized_scope_ref: 'all',
    horizon: overrides.horizon ?? { since_micros: 0, until_micros: NOW_MICROS },
    watermark: 'turns:8412:1753000000;savings:1753000000',
    observed_at_micros: NOW_MICROS,
    current: false,
    usage: overrides.usage,
    estimated_cost: overrides.estimated_cost,
    pricing_revision: overrides.pricing_revision ?? null,
  };
}

function envelope(payload: unknown) {
  return {
    schema_revision: 1,
    scope: { project_id: null, storage_mode: 'user', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'partial',
      eligible: 3,
      examined: 2,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: 3,
      unit: 'metrics',
      omission_reasons: ['incomplete_metric_coverage'],
    },
    freshness: { state: 'fresh', observed_at_micros: NOW_MICROS, watermark: null },
    domain_state: 'partial',
    legal_actions: [{ kind: 'refresh', operation: 'use-case.dashboard.costs.refresh' }],
    payload,
  };
}

function costMetric(
  name: string,
  value: number | null,
  unit: string,
  denominator: string,
  denominatorValue: number | null,
): MetricValueV1 {
  return {
    descriptor_revision: 'accounting-cost.v1',
    metric: name,
    value,
    unit,
    denominator,
    denominator_value: denominatorValue,
    coverage: {
      state: value == null ? 'unknown' : 'known',
      eligible: denominatorValue,
      observed: denominatorValue ?? 0,
      completed: denominatorValue ?? 0,
      censored: 0,
      excluded: 0,
      unknown: value == null ? 1 : 0,
    },
    evidence_class: 'measurement',
    provenance: {
      source: 'accounting_turn',
      source_revision: 'accounting-turn.v1',
      projector_revision: 'costs-projector.v1',
      watermark: 'turns:8412:1753000000',
    },
    cohort: { descriptor_revision: `${denominator}.v1`, eligible_population: denominator },
    temporal: {
      horizon: { since_micros: 0, until_micros: NOW_MICROS },
      baseline_watermark: null,
      delta: null,
    },
    uncertainty: { lower: value, upper: value, reason: null },
    calibration: null,
    unavailable_reason: null,
  };
}

/** The real shape `costs_read_model` emits when turns were counted but never
 * priced: a null cost carrying `pricing_revision_unavailable`. */
function pricedCost(): MetricValueV1 {
  return {
    ...costMetric('provider_cost', null, 'usd', 'priced_provider_turns', null),
    unavailable_reason: 'pricing_revision_unavailable',
    uncertainty: { lower: null, upper: null, reason: 'pricing_revision_unavailable' },
  };
}
