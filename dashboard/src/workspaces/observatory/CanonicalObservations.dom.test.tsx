import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { MetricValueV1 } from '../../contracts/generated.ts';
import { CanonicalObservations } from './CanonicalObservations.tsx';

/**
 * `/api/observatory` is the Plan 26 canonical read. Its whole value is that a
 * measurement it could not take arrives as `value: null` plus a reason, so the
 * assertions here are mostly about what must NOT appear: no zero standing in
 * for an unavailable latency, no percentage against an unknown denominator, and
 * no merging of the two producing sources into one figure.
 */

const NOW_MICROS = 1_753_003_600_000_000;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Observatory canonical observations', () => {
  it('renders event-flow and latency measurements with denominator and coverage', async () => {
    renderObservations(
      readModel([
        eventMetric('observability_events', 4_812),
        eventMetric('observability_failures', 17),
        eventMetric('telemetry_drops_lower_bound', 0),
        feedbackMetric('feedback_latency_p95', 43_250, 'microseconds', 'latency_samples'),
      ]),
    );

    // Event flow: the count, and the population it counted out of.
    expect(await screen.findByText('observability events')).toBeTruthy();
    expect(screen.getByText('4,812')).toBeTruthy();
    expect(
      screen.getAllByText('per eligible observability events · 4,812').length,
    ).toBe(3);

    // A measured zero is a real reading and prints as one — it is the ABSENT
    // measurements below that must never look like this.
    const drops = document.querySelector('[data-metric="telemetry_drops_lower_bound"]');
    expect(drops?.getAttribute('data-metric-available')).toBe('true');
    expect(drops?.textContent).toContain('0');

    // Latency, converted for reading, with the server's own microseconds kept.
    expect(screen.getByText('feedback latency p95')).toBeTruthy();
    expect(screen.getByText('43.25')).toBeTruthy();
    expect(screen.getByText('(43,250 µs)')).toBeTruthy();
  });

  it('shows an unavailable measurement as its server reason, never as zero', async () => {
    renderObservations(
      readModel([
        eventMetric('observability_events', 4_812),
        {
          ...feedbackMetric('feedback_latency_p95', null, 'microseconds', 'latency_samples'),
          unavailable_reason: 'no_latency_samples',
          coverage: {
            state: 'unknown',
            eligible: null,
            observed: 0,
            completed: 0,
            censored: 0,
            excluded: 0,
            unknown: 1,
          },
        },
      ]),
    );

    const latency = await screen.findByText('feedback latency p95');
    const plate = latency.closest('[data-metric]');
    expect(plate?.getAttribute('data-metric-available')).toBe('false');
    expect(plate?.textContent).toContain('no_latency_samples');
    expect(plate?.textContent).toContain('—');
    // The two failures this plate exists to prevent.
    expect(plate?.textContent).not.toContain('0 ms');
    expect(plate?.textContent).toContain('eligible population unknown');
    expect(plate?.textContent).not.toContain('%');
  });

  it('keeps the two producing sources in separate groups with their own tallies', async () => {
    renderObservations(
      readModel([
        eventMetric('observability_events', 4_812),
        eventMetric('observability_failures', 17),
        {
          ...feedbackMetric('feedback_coverage', null, 'ratio', 'eligible_observations'),
          unavailable_reason: 'no_eligible_observations',
        },
      ]),
    );

    await screen.findByText('observability events');
    const envelopeSource = document.querySelector(
      '[data-metric-source="observability_envelope"]',
    );
    const feedbackSource = document.querySelector(
      '[data-metric-source="feedback_observations"]',
    );
    expect(envelopeSource?.textContent).toContain('2 of 2 measured');
    // One metric, none of it measured — stated, rather than shown as a
    // section of zeroes.
    expect(feedbackSource?.textContent).toContain('0 of 1 measured');
    expect(envelopeSource?.textContent).not.toContain('feedback coverage');
  });

  it('renders the server domain state and the window the numbers cover', async () => {
    renderObservations(readModel([eventMetric('observability_events', 4_812)]), 'partial', [
      'incomplete_metric_coverage',
    ]);

    expect(await screen.findByText('Partial')).toBeTruthy();
    expect(screen.getByText('incomplete_metric_coverage')).toBeTruthy();
    expect(screen.getByText(/not current · watermark analytics:4821/)).toBeTruthy();
  });

  it('renders a locked read state from the server without overriding it', async () => {
    // `locked` and `redacted` are in the taxonomy but were reachable from no
    // workspace. An envelope-driven surface renders whatever the server says,
    // so both are now reachable without the browser deciding either one.
    renderObservations(readModel([eventMetric('observability_events', 4_812)]), 'locked');

    expect(await screen.findByText('Locked')).toBeTruthy();
    expect(screen.queryByText('Ready')).toBeNull();
  });

  it('renders a redacted authorization outcome beside the read state', async () => {
    renderObservations(
      readModel([eventMetric('observability_events', 4_812)]),
      'partial',
      [],
      { outcome: 'redacted' },
    );

    // Two independent axes: the read is partial AND the identity behind it is
    // seeing a redacted projection. Neither is folded into the other.
    expect(await screen.findByText('Partial')).toBeTruthy();
    expect(screen.getByText('Redacted')).toBeTruthy();
    expect(screen.getByText(/read authorization/)).toBeTruthy();
  });

  it('exposes each source group as a named region with list semantics', async () => {
    renderObservations(
      readModel([
        eventMetric('observability_events', 4_812),
        feedbackMetric('feedback_latency_p95', 43_250, 'microseconds', 'latency_samples'),
      ]),
    );

    await screen.findByText('observability events');
    // Named regions rather than an undifferentiated wall of plates, so a
    // screen reader can move between the two producing sources.
    expect(
      screen.getByRole('region', { name: 'observability envelope measurements' }),
    ).toBeTruthy();
    expect(
      screen.getByRole('region', { name: 'feedback observations measurements' }),
    ).toBeTruthy();
    // Every plate is a list item, so the count is announced.
    expect(screen.getAllByRole('listitem').length).toBe(2);
  });

  it('reports a daemon that never answered as offline rather than as no findings', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('connection refused');
      }),
    );
    renderWith();

    expect(await screen.findByText('Offline')).toBeTruthy();
    expect(screen.queryByText(/Complete · zero findings/i)).toBeNull();
  });

  it('reports a body this build cannot decode as unsupported schema', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () => new Response(JSON.stringify({ unexpected: true }), { status: 200 }),
      ),
    );
    renderWith();

    expect(await screen.findByText('Unsupported schema')).toBeTruthy();
  });
});

function renderObservations(
  payload: unknown,
  domainState = 'ready',
  omissionReasons: string[] = [],
  authorization: { outcome: string } = { outcome: 'authorized' },
) {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(
          JSON.stringify(envelope(payload, domainState, omissionReasons, authorization)),
          { status: 200 },
        ),
    ),
  );
  renderWith();
}

function renderWith() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <CanonicalObservations />
    </QueryClientProvider>,
  );
}

function readModel(metrics: MetricValueV1[]) {
  return {
    authorized_scope_ref: 'project.tracedecay',
    horizon: { since_micros: NOW_MICROS - 30 * 86_400_000_000, until_micros: NOW_MICROS },
    watermark: 'analytics:4821',
    observed_at_micros: NOW_MICROS,
    current: false,
    metrics,
  };
}

function envelope(
  payload: unknown,
  domainState: string,
  omissionReasons: string[],
  authorization: { outcome: string } = { outcome: 'authorized' },
) {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: { source: 'analytics', watermark: 'analytics:4821' },
    authorization,
    coverage: {
      completeness: domainState === 'ready' ? 'complete' : 'partial',
      eligible: 12,
      examined: 3,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: 12,
      unit: 'metrics',
      omission_reasons: omissionReasons,
    },
    freshness: { state: 'fresh', observed_at_micros: NOW_MICROS, watermark: 'analytics:4821' },
    domain_state: domainState,
    legal_actions: [{ kind: 'refresh', operation: 'use-case.dashboard.observatory.refresh' }],
    payload,
  };
}

function eventMetric(name: string, value: number | null): MetricValueV1 {
  return {
    descriptor_revision: 'analytics-observability.v1',
    metric: name,
    value,
    unit: 'events',
    denominator: 'eligible_observability_events',
    denominator_value: 4_812,
    coverage: {
      state: 'known',
      eligible: 4_812,
      observed: 4_812,
      completed: 4_812,
      censored: 0,
      excluded: 0,
      unknown: 0,
    },
    evidence_class: 'measurement',
    provenance: {
      source: 'observability_envelope',
      source_revision: 'observability-envelope.v1',
      projector_revision: 'observatory-projector.v1',
      watermark: 'analytics:4821',
    },
    cohort: {
      descriptor_revision: 'eligible_observability_events.v1',
      eligible_population: 'eligible_observability_events',
    },
    temporal: {
      horizon: { since_micros: NOW_MICROS - 30 * 86_400_000_000, until_micros: NOW_MICROS },
      baseline_watermark: null,
      delta: null,
    },
    uncertainty: { lower: value, upper: value, reason: null },
    calibration: null,
    unavailable_reason: null,
  };
}

function feedbackMetric(
  name: string,
  value: number | null,
  unit: string,
  denominator: string,
): MetricValueV1 {
  return {
    ...eventMetric(name, value),
    unit,
    denominator,
    denominator_value: value == null ? null : 96,
    provenance: {
      source: 'feedback_observations',
      source_revision: 'feedback-observations.v1',
      projector_revision: 'feedback-system-quality-projector.v1',
      watermark: 'feedback:311',
    },
  };
}
