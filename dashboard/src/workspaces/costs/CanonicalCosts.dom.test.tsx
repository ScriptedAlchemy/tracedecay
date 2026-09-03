import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type {
  MetricValueV1,
  ProviderLatencyReadModelV1,
} from '../../contracts/generated.ts';
import { CanonicalCosts } from './CanonicalCosts.tsx';

/**
 * The canonical cost read exists to make an unpriced ledger visible. Prices are
 * resolved from provider usage, so a read with unpriced usage arrives with
 * `provider_cost: null` and `pricing_revision_unavailable` — and $0.00 is the
 * single most damaging thing this surface could print in its place.
 *
 * Latency is the other assertion class here. The projection carries retained
 * operation-resource percentiles per provider/model cohort; when the route has
 * no authorized project scope it emits one typed unavailable cohort instead of
 * omitting latency or borrowing Observatory's retrieval-side percentile.
 */

const NOW_MICROS = 1_753_003_600_000_000;

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('Canonical cost observations', () => {
  it('renders usage measurements with their eligible population and coverage', async () => {
    renderCosts(
      readModel({
        usage: [
          costMetric('provider_tokens', 1_284_000, 'tokens', 'provider_usage_events', 8_412),
          costMetric('saved_tokens', 96_400, 'tokens', 'eligible_savings_calls', 512),
        ],
        estimated_cost: [pricedCost()],
      }),
    );

    expect(await screen.findByText('provider tokens')).toBeTruthy();
    expect(screen.getByText('1,284,000')).toBeTruthy();
    expect(screen.getByText('per provider usage events · 8,412')).toBeTruthy();
    expect(screen.getByText('saved tokens')).toBeTruthy();
    expect(screen.getByText('per eligible savings calls · 512')).toBeTruthy();
  });

  it('renders an unpriced cost as its reason, never as $0.00 or a zero', async () => {
    renderCosts(
      readModel({
        usage: [
          costMetric('provider_tokens', 1_284_000, 'tokens', 'provider_usage_events', 8_412),
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

  it('renders typed scope-unavailable provider latency without a zero or borrowed value', async () => {
    renderCosts(
      readModel({ usage: [], estimated_cost: [pricedCost()] }),
    );

    expect(await screen.findByText('provider queue latency p50')).toBeTruthy();
    const plate = document.querySelector('[data-metric="provider_queue_latency_p50"]');
    expect(plate?.getAttribute('data-metric-available')).toBe('false');
    expect(plate?.textContent).toContain('provider_latency_scope_unavailable');
    expect(plate?.textContent).toContain('provider operation resource observations');
    expect(plate?.textContent).not.toContain('0 microseconds');
    expect(screen.queryByText('feedback latency p95')).toBeNull();
  });

  it('keeps repeated latency metrics separated by provider cohort and names unresolved identity', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    renderCosts(
      readModel({
        usage: [],
        estimated_cost: [pricedCost()],
        latency: [
          measuredLatency('anthropic', 'claude-sonnet-4', null),
          measuredLatency(null, null, 'provider_model_identity_unavailable'),
        ],
      }),
    );

    expect(
      await screen.findByRole('heading', { name: 'anthropic · claude-sonnet-4' }),
    ).toBeTruthy();
    expect(
      screen.getByRole('heading', {
        name: 'provider/model unavailable · provider_model_identity_unavailable',
      }),
    ).toBeTruthy();
    expect(document.querySelectorAll('[data-metric="provider_queue_latency_p50"]')).toHaveLength(2);
    expect(consoleError.mock.calls.flat().join(' ')).not.toContain(
      'Encountered two children with the same key',
    );
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
  latency?: ProviderLatencyReadModelV1[];
}) {
  const horizon = overrides.horizon ?? { since_micros: 0, until_micros: NOW_MICROS };
  return {
    authorized_scope_ref: 'all',
    horizon,
    watermark: 'provider-usage:8412;savings:1753000000',
    observed_at_micros: NOW_MICROS,
    current: false,
    usage: overrides.usage,
    estimated_cost: overrides.estimated_cost,
    latency: overrides.latency ?? [scopeUnavailableLatency(horizon)],
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
      source: 'provider_usage_observation',
      source_revision: 'provider-usage-observation.v1',
      projector_revision: 'costs-projector.v1',
      watermark: 'provider-usage:8412',
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

/** The real shape `costs_read_model` emits when provider usage was observed but never
 * priced: a null cost carrying `pricing_revision_unavailable`. */
function pricedCost(): MetricValueV1 {
  return {
    ...costMetric('provider_cost', null, 'usd', 'priced_provider_usage_events', null),
    unavailable_reason: 'pricing_revision_unavailable',
    uncertainty: { lower: null, upper: null, reason: 'pricing_revision_unavailable' },
  };
}

/** The exact cohort emitted by `unavailable_provider_latency` when the Costs
 * route has no project scope to authorize an observability read. */
function scopeUnavailableLatency(horizon: {
  since_micros: number;
  until_micros: number;
}): ProviderLatencyReadModelV1 {
  const reason = 'provider_latency_scope_unavailable';
  const provenance = {
    source: 'observability_envelope' as const,
    source_revision: 'operation-resource-observation.v1',
    projector_revision: 'costs-provider-latency-projector.v1',
    watermark: 'analytics:unavailable',
  };
  const metric = (stage: string, percentile: number): MetricValueV1 => ({
    descriptor_revision: 'provider-latency.v1',
    metric: `provider_${stage}_latency_p${percentile}`,
    value: null,
    unit: 'microseconds',
    denominator: 'provider_operation_resource_observations',
    denominator_value: null,
    coverage: {
      state: 'unknown',
      eligible: null,
      observed: 0,
      completed: 0,
      censored: 0,
      excluded: 0,
      unknown: 1,
    },
    evidence_class: 'measurement',
    provenance,
    cohort: {
      descriptor_revision: 'provider_operation_resource_observations.v1',
      eligible_population: 'provider_operation_resource_observations',
    },
    temporal: { horizon, baseline_watermark: null, delta: null },
    uncertainty: { lower: null, upper: null, reason },
    calibration: null,
    unavailable_reason: reason,
  });
  const distribution = (stage: string) => ({
    p50: metric(stage, 50),
    p95: metric(stage, 95),
    p99: metric(stage, 99),
  });
  return {
    provider: null,
    model: null,
    identity_provenance: provenance,
    identity_unavailable_reason: reason,
    queue: distribution('queue'),
    start: distribution('start'),
    first_progress: distribution('first_progress'),
    service: distribution('service'),
    terminal: distribution('terminal'),
  };
}

function measuredLatency(
  provider: string | null,
  model: string | null,
  identityReason: string | null,
): ProviderLatencyReadModelV1 {
  const horizon = { since_micros: 0, until_micros: NOW_MICROS };
  const metricProvenance = {
    source: 'observability_envelope' as const,
    source_revision: 'operation-resource-observation.v1',
    projector_revision: 'costs-provider-latency-projector.v1',
    watermark: 'operations:42',
  };
  const metric = (stage: string, percentile: number): MetricValueV1 => {
    const value = 1_000 + percentile;
    return {
      descriptor_revision: 'provider-latency.v1',
      metric: `provider_${stage}_latency_p${percentile}`,
      value,
      unit: 'microseconds',
      denominator: 'provider_operation_resource_observations',
      denominator_value: 4,
      coverage: {
        state: 'known',
        eligible: 4,
        observed: 4,
        completed: 4,
        censored: 0,
        excluded: 0,
        unknown: 0,
      },
      evidence_class: 'measurement',
      provenance: metricProvenance,
      cohort: {
        descriptor_revision: 'provider_operation_resource_observations.v1',
        eligible_population: 'provider_operation_resource_observations',
      },
      temporal: { horizon, baseline_watermark: null, delta: null },
      uncertainty: { lower: value, upper: value, reason: null },
      calibration: null,
      unavailable_reason: null,
    };
  };
  const distribution = (stage: string) => ({
    p50: metric(stage, 50),
    p95: metric(stage, 95),
    p99: metric(stage, 99),
  });
  return {
    provider,
    model,
    identity_provenance: {
      source: provider === null ? 'observability_envelope' : 'provider_usage_observation',
      source_revision:
        provider === null
          ? 'operation-resource-observation.v1'
          : 'provider-usage-observation.v1',
      projector_revision: 'costs-provider-latency-projector.v1',
      watermark: 'operations:42',
    },
    identity_unavailable_reason: identityReason,
    queue: distribution('queue'),
    start: distribution('start'),
    first_progress: distribution('first_progress'),
    service: distribution('service'),
    terminal: distribution('terminal'),
  };
}
