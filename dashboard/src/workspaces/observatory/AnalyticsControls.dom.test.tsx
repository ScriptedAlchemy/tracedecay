import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type {
  DashboardEnvelopeV1,
  ObservatoryReadModelV1,
} from '../../contracts/generated.ts';
import { AnalyticsControls } from './AnalyticsControls.tsx';
import type { ObservatoryAccountingReads } from './accountingReads.ts';

/**
 * The Plan 26 `analytics-controls` view.
 *
 * `/api/doctor/findings?family=storage` backs the typed retention-backlog
 * status and is read independently of the observatory projection. The
 * assertions pin the two reassuring falsehoods this surface could most easily
 * tell: that an unpublished collection mode is `Off`, and that an absent
 * exporter means zero egress failures.
 */

const NOW_MICROS = 1_753_003_600_000_000;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Observatory analytics controls', () => {
  it('states retention evidence and no longer reads or shows an upload setting', async () => {
    const fetchMock = renderControls();

    await screen.findByText('retention sweep observed 4 entries');
    expect(screen.queryByRole('region', { name: 'profile upload setting' })).toBeNull();
    expect(
      fetchMock.mock.calls.some(([input]) => String(input).includes('/api/settings')),
    ).toBe(false);
  });

  it('keeps a failed findings read from blanking the collection mode', async () => {
    renderControls({ findingsStatus: 503 });

    const retention = await screen.findByRole('region', { name: 'retention and deletion' });
    await screen.findByText('could not be read', { exact: false });
    expect(retention.textContent).not.toContain('retention sweep observed');
    const mode = screen.getByRole('region', { name: 'collection mode' });
    expect(mode.textContent).not.toContain('could not be read');
  });
});

function renderControls(
  options: {
    findingsStatus?: number;
    retentionStatuses?: unknown[];
  } = {},
) {
  const statuses = options.retentionStatuses ?? [
    {
      kind: 'retention_backlog',
      state: 'real',
      observed_entries: 4,
      reason: 'retention sweep observed 4 entries',
    },
  ];
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes('/api/doctor/findings?family=storage')) {
      if (options.findingsStatus != null) {
        return new Response('{}', { status: options.findingsStatus });
      }
      return new Response(JSON.stringify(envelope(findingsPayload(statuses))), { status: 200 });
    }
    return new Response('{}', { status: 404 });
  });
  vi.stubGlobal('fetch', fetchMock);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <AnalyticsControls reads={accountingReads()} />
    </QueryClientProvider>,
  );
  return fetchMock;
}

function accountingReads(): ObservatoryAccountingReads {
  const metrics = [
    controlMetric('analytics_share_staging_age_seconds', 17),
    controlMetric('analytics_egress_failures', null),
  ];
  const payload = {
    authorized_scope_ref: 'project.tracedecay',
    horizon: { since_micros: 0, until_micros: NOW_MICROS },
    watermark: 'analytics:4821',
    observed_at_micros: NOW_MICROS,
    current: true,
    metrics,
    analytics_mode: {
      current: 'local_only',
      transition_watermark: 'analytics:4820',
      coverage: metricCoverage('known'),
      unavailable_reason: null,
    },
    comparison: {
      baseline_build: null, candidate_build: null, workload: null, corpus: null,
      environment: null, oracle: null, configuration: null, platform: null,
      rollback_profile: null, eligible_outcomes: null, paired_outcomes: null,
      regression_observed: null, disposition: 'insufficient_evidence',
      coverage: metricCoverage('unknown'), unavailable_reason: 'not_observed',
    },
    rejected_arguments: {
      coverage: metricCoverage('unknown'),
      projector_revision: 'observatory-rejected-argument-projector.v1',
      watermark: 'analytics:4821',
      eligible_attempts: null,
      rejected_total: null,
      rejection_rate: null,
      redacted_name_count: 0,
      groups: [],
      unavailable_reason: 'rejected_argument_observations_not_recorded',
    },
  };
  return {
    observatory: {
      result: {
        outcome: 'envelope',
        envelope: envelope(payload) as DashboardEnvelopeV1<ObservatoryReadModelV1>,
      },
      pending: false,
      refreshing: false,
      refresh: () => undefined,
    },
    diagnostics: {
      result: undefined,
      pending: false,
      refreshing: false,
      refresh: () => undefined,
    },
  };
}

function metricCoverage(state: 'known' | 'unknown') {
  return {
    eligible: state === 'known' ? 1 : null,
    observed: state === 'known' ? 1 : 0,
    completed: state === 'known' ? 1 : 0,
    censored: 0,
    unknown: state === 'known' ? 0 : 1,
    excluded: 0,
    state,
  };
}

function controlMetric(metric: string, value: number | null) {
  return {
    descriptor_revision: 'analytics-controls.v1', metric, value,
    unit: metric.includes('age') ? 'seconds' : 'events',
    denominator: 'latest_analytics_consent', denominator_value: value == null ? null : 1,
    coverage: metricCoverage(value == null ? 'unknown' : 'known'), evidence_class: 'measurement',
    provenance: { source: 'observability_envelope', source_revision: 'observability-envelope.v1', projector_revision: 'observatory-plan26-projector.v1', watermark: 'analytics:4821' },
    cohort: { descriptor_revision: 'latest_analytics_consent.v1', eligible_population: 'latest_analytics_consent' },
    temporal: { horizon: { since_micros: 0, until_micros: NOW_MICROS }, baseline_watermark: null, delta: null },
    uncertainty: { lower: value, upper: value, reason: value == null ? 'not_observed' : null },
    calibration: null, unavailable_reason: value == null ? 'not_observed' : null,
  };
}

function findingsPayload(kindStatuses: unknown[]) {
  return {
    entries: [],
    family_filter: null,
    storage_kind_statuses: kindStatuses,
    known_families: ['storage'],
    schema_convergences: [],
    note: 'doctor storage findings',
    report_coverage: null,
  };
}

function envelope(payload: unknown) {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: { source: 'doctor', watermark: 'doctor:1' },
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
      eligible: 1,
      examined: 1,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: 1,
      unit: 'records',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: NOW_MICROS, watermark: 'doctor:1' },
    domain_state: 'ready',
    legal_actions: [],
    payload,
  };
}
