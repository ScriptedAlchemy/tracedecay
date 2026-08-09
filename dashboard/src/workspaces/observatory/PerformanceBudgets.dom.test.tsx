import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { MetricValueV1 } from '../../contracts/generated.ts';
import { PerformanceBudgets } from './PerformanceBudgets.tsx';

/** The Plan 26 `performance-budgets` view over `/api/observatory`. */

const NOW_MICROS = 1_753_003_600_000_000;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Observatory performance budgets', () => {
  it('renders the p95 the wire publishes, converted with the exact figure kept', async () => {
    renderBudgets(readModel([latency('operation_latency_p95', 43_250)]));

    expect(await screen.findByText('latency p95')).toBeTruthy();
    const card = document.querySelector('[data-dimension="latency_p95"]');
    expect(card?.getAttribute('data-dimension-available')).toBe('true');
    expect(card?.textContent).toContain('43.25');
    expect(card?.textContent).toContain('(43,250 µs)');
    expect(card?.textContent).toContain('96 observed');
  });

  it('renders p50 and p99 as server-unknown with their reason, never as zero', async () => {
    renderBudgets(readModel([latency('operation_latency_p95', 43_250)]));

    await screen.findByText('latency p95');
    for (const id of ['latency_p50', 'latency_p99']) {
      const card = document.querySelector(`[data-dimension="${id}"]`);
      expect(card?.getAttribute('data-dimension-available')).toBe('false');
      expect(card?.getAttribute('data-dimension-state')).toBe('unknown');
      expect(card?.textContent).toContain('—');
      expect(card?.textContent).toContain('not_observed');
      // The two failures this card exists to prevent.
      expect(card?.textContent).not.toContain('43.25');
      expect(card?.textContent).not.toContain('0 ms');
    }
  });

  it('states every mandatory row on an unknown card rather than omitting it', async () => {
    renderBudgets(readModel([]));

    await screen.findByText('queue span');
    const card = document.querySelector('[data-dimension="queue_span"]');
    const text = card?.textContent ?? '';
    for (const term of [
      'support',
      'denominator',
      'censoring',
      'interval',
      'horizon',
      'descriptor revision',
      'anchors',
    ]) {
      expect(text).toContain(term);
    }
    expect(text).toContain('not_observed');
    // Safe anchors: the scope and the watermark, and nothing path-shaped.
    expect(text).toContain('scope project.tracedecay');
  });

  it('keeps distinct unavailable reasons for distinct canonical metrics', async () => {
    renderBudgets(
      readModel([
        { ...latency('operation_latency_p95', null), unavailable_reason: 'no_latency_samples' },
      ]),
    );

    await screen.findByText('latency p95');
    const unmeasured = document.querySelector('[data-dimension="latency_p95"]');
    const otherUnknown = document.querySelector('[data-dimension="latency_p50"]');
    expect(unmeasured?.getAttribute('data-dimension-state')).toBe('unknown');
    expect(otherUnknown?.getAttribute('data-dimension-state')).toBe('unknown');
    expect(unmeasured?.textContent).toContain('no_latency_samples');
    // The daemon did publish this metric's frame, so the frame stays visible.
    expect(unmeasured?.textContent).toContain('latency samples');
  });

  it('states how many requirements the wire actually answered', async () => {
    renderBudgets(
      readModel([
        latency('operation_latency_p95', 43_250),
        latency('feedback_revocation_propagation_p95', 1_200),
      ]),
    );

    expect(await screen.findByText(/2 of 13 required budget dimensions/)).toBeTruthy();
    const horizon = document.querySelector('[data-budgets-measured]');
    expect(horizon?.getAttribute('data-budgets-measured')).toBe('2');
    expect(horizon?.getAttribute('data-budgets-required')).toBe('13');
  });

  it('exposes each band as a named region with list semantics', async () => {
    renderBudgets(readModel([]));

    await screen.findByText('queue span');
    // Named regions, so a screen reader can move between the plan's four bands
    // rather than through thirteen undifferentiated cards.
    expect(screen.getByRole('region', { name: 'Latency percentiles dimensions' })).toBeTruthy();
    expect(
      screen.getByRole('region', { name: 'Queue, lock, and provider spans dimensions' }),
    ).toBeTruthy();
    expect(screen.getByRole('region', { name: 'RSS, CPU, and I/O dimensions' })).toBeTruthy();
    expect(screen.getAllByRole('listitem').length).toBe(13);
  });

  it('does not render the superseded projection-gap section', async () => {
    renderBudgets(readModel([]));

    await screen.findByText('queue span');
    expect(document.querySelector('[data-budgets-gap="unprojected"]')).toBeNull();
  });

  it('renders the server domain state and omission reasons without overriding them', async () => {
    renderBudgets(readModel([]), 'partial', ['incomplete_metric_coverage']);

    expect(await screen.findByText('Partial')).toBeTruthy();
    expect(screen.getByText('incomplete_metric_coverage')).toBeTruthy();
  });

  it('reports a daemon that never answered as offline rather than as zero budgets', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('connection refused');
      }),
    );
    renderWith();

    expect(await screen.findByText('Offline')).toBeTruthy();
    expect(screen.queryByText(/required budget dimensions/)).toBeNull();
  });
});

function renderBudgets(payload: unknown, domainState = 'ready', omissionReasons: string[] = []) {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(JSON.stringify(envelope(payload, domainState, omissionReasons)), {
          status: 200,
        }),
    ),
  );
  renderWith();
}

function renderWith() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <PerformanceBudgets />
    </QueryClientProvider>,
  );
}

function readModel(metrics: MetricValueV1[]) {
  return {
    authorized_scope_ref: 'project.tracedecay',
    horizon: { since_micros: 0, until_micros: NOW_MICROS },
    watermark: 'analytics:4821',
    observed_at_micros: NOW_MICROS,
    current: true,
    metrics: withRequiredMetrics(metrics),
    analytics_mode: {
      current: null,
      transition_watermark: null,
      coverage: { eligible: null, observed: 0, completed: 0, censored: 0, unknown: 1, excluded: 0, state: 'unknown' },
      unavailable_reason: 'not_observed',
    },
    comparison: {
      baseline_build: null, candidate_build: null, workload: null, corpus: null,
      environment: null, oracle: null, configuration: null, platform: null,
      rollback_profile: null, eligible_outcomes: null, paired_outcomes: null,
      regression_observed: null, disposition: 'insufficient_evidence',
      coverage: { eligible: null, observed: 0, completed: 0, censored: 0, unknown: 1, excluded: 0, state: 'unknown' },
      unavailable_reason: 'not_observed',
    },
  };
}

const REQUIRED_METRICS = [
  'operation_latency_p50', 'operation_latency_p95', 'operation_latency_p99',
  'queue_span_p95', 'store_lock_span_p95', 'index_lock_span_p95',
  'provider_negotiation_span_p95', 'process_rss_peak', 'cpu_time_total',
  'io_amplification', 'no_progress_outcomes', 'accepted_budget_revision',
] as const;

function withRequiredMetrics(metrics: MetricValueV1[]): MetricValueV1[] {
  const present = new Set(metrics.map((metric) => metric.metric));
  return [
    ...metrics,
    ...REQUIRED_METRICS.filter((metric) => !present.has(metric)).map((metric) => ({
      ...latency(metric, null),
      unavailable_reason: 'not_observed',
      uncertainty: { lower: null, upper: null, reason: 'not_observed' },
    })),
  ];
}

function envelope(payload: unknown, domainState: string, omissionReasons: string[]) {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: { source: 'analytics', watermark: 'analytics:4821' },
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: domainState === 'ready' ? 'complete' : 'partial',
      eligible: 13,
      examined: 2,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: 13,
      unit: 'metrics',
      omission_reasons: omissionReasons,
    },
    freshness: { state: 'fresh', observed_at_micros: NOW_MICROS, watermark: 'analytics:4821' },
    domain_state: domainState,
    legal_actions: [{ kind: 'refresh', operation: 'use-case.dashboard.observatory.refresh' }],
    payload,
  };
}

function latency(name: string, value: number | null): MetricValueV1 {
  return {
    descriptor_revision: 'analytics-feedback.v1',
    metric: name,
    value,
    unit: 'microseconds',
    denominator: 'latency_samples',
    denominator_value: 96,
    coverage: {
      state: value == null ? 'unknown' : 'known',
      eligible: value == null ? null : 96,
      observed: value == null ? 0 : 96,
      completed: value == null ? 0 : 96,
      censored: 0,
      excluded: 0,
      unknown: value == null ? 1 : 0,
    },
    evidence_class: 'measurement',
    provenance: {
      source: 'feedback_observations',
      source_revision: 'feedback-observations.v1',
      projector_revision: 'feedback-system-quality-projector.v1',
      watermark: 'feedback:311',
    },
    cohort: { descriptor_revision: 'latency_samples.v1', eligible_population: 'latency_samples' },
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
