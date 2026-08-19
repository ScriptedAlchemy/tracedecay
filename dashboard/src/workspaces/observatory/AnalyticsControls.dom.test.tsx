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
 * Two real reads back parts of it — `/api/settings` for the profile upload
 * setting and `/api/storage/findings` for the typed retention-backlog status —
 * and they are read independently. The assertions pin the two reassuring
 * falsehoods this surface could most easily tell: that an unpublished
 * collection mode is `Off`, and that an absent exporter means zero egress
 * failures.
 */

const NOW_MICROS = 1_753_003_600_000_000;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Observatory analytics controls', () => {
  it('marks only the mode published by the canonical projection as current', async () => {
    renderControls();

    await screen.findByRole('region', { name: 'collection mode' });
    for (const mode of ['off', 'local_only', 'aggregate_share']) {
      const entry = document.querySelector(`[data-analytics-mode="${mode}"]`);
      expect(entry).toBeTruthy();
      expect(entry?.getAttribute('data-analytics-mode-current')).toBe(
        mode === 'local_only' ? 'true' : 'false',
      );
    }
    expect(screen.getByText(/no network exporter · default/)).toBeTruthy();
    expect(screen.getByText(/network exporter · explicit opt-in required/)).toBeTruthy();
  });

  it('reports the published mode without defaulting it in the browser', async () => {
    renderControls();

    const block = await screen.findByRole('region', { name: 'collection mode' });
    expect(block.getAttribute('data-analytics-control-state')).toBe('ready');
    expect(block.textContent).toContain('An unavailable mode is never read as Off');
    expect(block.textContent).toContain('Local only');
  });

  it('reports share staging age from the canonical projection', async () => {
    renderControls();

    const block = await screen.findByRole('region', { name: 'share staging age' });
    expect(block.getAttribute('data-analytics-control-state')).toBe('ready');
    expect(block.textContent).toContain('17 s');
  });

  it('never reports zero egress failures for an exporter that does not exist', async () => {
    renderControls();

    const block = await screen.findByRole('region', { name: 'egress failures' });
    expect(block.getAttribute('data-analytics-control-state')).toBe('unknown');
    expect(block.textContent).toContain('not zero failures');
    expect(document.querySelector('[data-egress-failures]')?.getAttribute('data-egress-failures'))
      .toBe('unknown');
  });

  it('reads the retention-backlog status the findings route publishes', async () => {
    renderControls();

    await screen.findByText('retention sweep observed 4 entries');
    const block = screen.getByRole('region', { name: 'retention and deletion' });
    expect(block.getAttribute('data-analytics-control-state')).toBe('ready');
    expect(document.querySelector('[data-retention-backlog-published]')
      ?.getAttribute('data-retention-backlog-published')).toBe('true');
  });

  it('labels the declared retention lifetimes as policy with no observed age', async () => {
    renderControls();

    await screen.findByText('retention sweep observed 4 entries');
    const declared = document.querySelector('[data-analytics-retention="declared_policy"]');
    expect(declared?.textContent).toContain('expires after 30 days');
    expect(declared?.textContent).toContain('expire after 395 days');
    expect(declared?.textContent).toContain('observed age not published');
    expect(screen.getByText(/declared policy, not measurements/)).toBeTruthy();
    // Product receipts and run history keep their own lifecycles and are never
    // adoption analytics, so this surface refuses to report them as retention.
    expect(screen.getByText(/never exported as adoption analytics/)).toBeTruthy();
  });

  it('reads the profile upload setting and says it is not the collection mode', async () => {
    renderControls();

    await screen.findByText('user.upload_enabled.v1');
    const block = screen.getByRole('region', { name: 'profile upload setting' });
    expect(block.textContent).toContain('disabled');
    expect(block.textContent).toContain('not the Plan 26 analytics collection mode');
  });

  it('keeps a failed settings read from blanking the retention evidence', async () => {
    // Independent sources: one refusing must not take the other down with it.
    renderControls({ settingsStatus: 503 });

    await screen.findByText('retention sweep observed 4 entries');
    const upload = screen.getByRole('region', { name: 'profile upload setting' });
    expect(upload.textContent).toContain('the profile settings read did not resolve');
    expect(upload.textContent).not.toContain('disabled');
  });

  it('keeps a failed findings read from blanking the profile setting', async () => {
    renderControls({ findingsStatus: 503 });

    await screen.findByText('user.upload_enabled.v1');
    const retention = screen.getByRole('region', { name: 'retention and deletion' });
    expect(retention.textContent).toContain('could not be read');
    expect(retention.textContent).not.toContain('retention sweep observed');
  });

  it('states an absent retention-backlog status as unpublished, not as zero entries', async () => {
    renderControls({ retentionStatuses: [] });

    await screen.findByText('the storage findings payload carried no retention-backlog status');
    const retention = screen.getByRole('region', { name: 'retention and deletion' });
    expect(retention.getAttribute('data-analytics-control-state')).toBe('unsupported');
    expect(
      document
        .querySelector('[data-retention-backlog-published]')
        ?.getAttribute('data-retention-backlog-published'),
    ).toBe('false');
  });
});

function renderControls(
  options: {
    settingsStatus?: number;
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
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('/api/settings')) {
        if (options.settingsStatus != null) {
          return new Response('{}', { status: options.settingsStatus });
        }
        return new Response(JSON.stringify(envelope(settingsPayload())), { status: 200 });
      }
      if (url.includes('/api/storage/findings')) {
        if (options.findingsStatus != null) {
          return new Response('{}', { status: options.findingsStatus });
        }
        return new Response(JSON.stringify(envelope(findingsPayload(statuses))), { status: 200 });
      }
      return new Response('{}', { status: 404 });
    }),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <AnalyticsControls reads={accountingReads()} />
    </QueryClientProvider>,
  );
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
    kind_statuses: kindStatuses,
    known_families: ['storage'],
    note: 'doctor storage findings',
    report_coverage: null,
  };
}

function settingsPayload() {
  return {
    automation: {
      availability: { available: true, reason: null, required_authority: null },
      backend: null,
      config_endpoint: '/api/plugins/holographic/curation/config',
      enabled: false,
      host_mode: null,
      source_coverage: { effective: 'global', global: 'default', project: 'absent' },
    },
    environment: {
      global_accounting_enabled: false,
      global_accounting_mode: 'off',
      pricing_offline: true,
      variables: [],
    },
    project: {
      config: {
        exclude: [],
        extract_docstrings: true,
        git_ignore: true,
        context_scout: false,
        include: [],
        max_file_size: 1_048_576,
        sync: { auto_track_pr_branches: false, auto_track_pr_poll_secs: 300 },
        telemetry: { timings: false },
        track_call_sites: true,
      },
      config_path: '/repo/.tracedecay/config.json',
      configuration_revision_id: 'revision-1',
      configuration_snapshot_id: 'snapshot-1',
      legacy_config_path: '/repo/.tracedecay/config.json',
      legacy_config_read_only: true,
      pr_autotrack: { tracked: [] },
      tracedecay_dir_gitignored: true,
    },
    restart_recommended: null,
    resync_recommended: null,
    storage: {
      dashboard_root: '/store/dashboard',
      graph_db: '/store/graph.db',
      lcm_db: '/store/lcm.db',
      lcm_scope: 'project',
      memory_db: '/store/memory.db',
      project_id: 'tracedecay',
      project_root: '/repo',
      savings_db: '/store/savings.db',
      storage_mode: 'profile_sharded',
      store_root: '/store',
    },
    user: {
      configuration_revision_id: 'revision-1',
      configuration_snapshot_id: 'snapshot-1',
      extraction_timeout_secs: 30,
      installed_agents: ['claude'],
      legacy_config_path: '/home/agent/.tracedecay/config.json',
      legacy_config_read_only: true,
      upload_enabled: false,
      watcher_debounce: '500ms',
    },
    version: { cached_latest_version: null, channel: 'stable', version: '0.0.0' },
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
