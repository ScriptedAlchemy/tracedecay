import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useScope } from '../../data/scope/store.ts';
import { ObservatoryPage } from './ObservatoryPage.tsx';

const NOW_MICROS = 1_753_003_600_000_000;

/**
 * The V2 accounting views share two source reads. This fixture deliberately
 * gives the record-count source a partial window: a missing family is
 * censored by that cap, while a reported four-record family is withheld by the
 * local suppression floor. Those are distinct states and neither is zero.
 */
beforeEach(() => {
  useScope.getState().selectAllProjects();
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = new URL(String(input), 'http://localhost');
      if (url.pathname === '/api/observatory') return response(envelope(observatoryModel()));
      if (url.pathname === '/api/plugins/analytics/diagnostics') {
        return response(envelope(diagnosticsModel()));
      }
      return new Response('{}', { status: 503, headers: { 'content-type': 'application/json' } });
    }),
  );
});

afterEach(() => {
  useScope.getState().selectAllProjects();
  vi.unstubAllGlobals();
});

describe('the mounted Observatory V2 accounting surface', () => {
  it('mounts all three V2 views and keeps capped and suppressed family states distinct', async () => {
    renderObservatory();

    for (const heading of ['Adoption coverage', 'Adoption outcomes', 'Retrieval quality']) {
      expect(await screen.findByRole('heading', { name: heading })).toBeTruthy();
    }

    // All three panels bind to one diagnostics snapshot. Separate panel-local
    // query keys would issue three calls and could put their ledgers under
    // different watermarks.
    const diagnosticsCalls = vi
      .mocked(fetch)
      .mock.calls.filter(([input]) => new URL(String(input), 'http://localhost').pathname === '/api/plugins/analytics/diagnostics');
    expect(diagnosticsCalls).toHaveLength(1);

    // The observability metric is absent, so its status is unknown even though
    // the separate diagnostics record window is partial.
    const coverageWindow = await screen.findByLabelText('Window truthfulness');
    expect(coverageWindow.getAttribute('data-coverage-window')).toBe('missing');
    expect(coverageWindow.querySelector('[data-state]')?.getAttribute('data-state')).toBe('unknown');
    expect(coverageWindow.textContent).toContain('partial · bounded at 10,000 rows');

    const denominator = await screen.findByLabelText('Denominator failures');
    expect(denominator.getAttribute('data-coverage-failures')).toBe('0');
    expect(denominator.querySelector('[data-state]')?.getAttribute('data-state')).toBe('unknown');
    expect(denominator.textContent).toContain('empty audit is unknown');

    const suppressed = document.querySelector(
      '[data-family-ledger="adoption"] [data-family="adoption.eligibility_observed.v1"]',
    );
    expect(suppressed?.getAttribute('data-family-state')).toBe('redacted');
    expect(suppressed?.textContent).toContain('fewer than 5 units observed');
    expect(suppressed?.textContent).not.toContain('4 records observed');

    const censored = document.querySelector(
      '[data-family-ledger="retrieval"] [data-family="retrieval.query.completed.v1"]',
    );
    expect(censored?.getAttribute('data-family-state')).toBe('partial');
    expect(censored?.textContent).toContain('cannot tell a family that produced nothing');
    expect(censored?.querySelector('[data-cell="numeric"]')?.textContent).toBe('—');
  });

  it('preserves a stale metric coverage state even while the read model is current', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), 'http://localhost');
        if (url.pathname === '/api/observatory') {
          return response(envelope(observatoryModel([eventMetric('stale')]), 'complete'));
        }
        if (url.pathname === '/api/plugins/analytics/diagnostics') {
          return response(envelope(diagnosticsModel(), 'complete'));
        }
        return new Response('{}', { status: 503, headers: { 'content-type': 'application/json' } });
      }),
    );

    renderObservatory();

    const window = await screen.findByLabelText('Window truthfulness');
    expect(window?.getAttribute('data-coverage-window')).toBe('stale');
    expect(window?.querySelector('[data-state]')?.getAttribute('data-state')).toBe('stale');
  });

  it('renders an under-floor eligible-versus-observed pair as partial instead of unsupported', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), 'http://localhost');
        if (url.pathname === '/api/observatory') {
          return response(envelope(observatoryModel([eventMetric('known', 4, 5)]), 'complete'));
        }
        if (url.pathname === '/api/plugins/analytics/diagnostics') {
          return response(envelope(diagnosticsModel(), 'complete'));
        }
        return new Response('{}', { status: 503, headers: { 'content-type': 'application/json' } });
      }),
    );

    renderObservatory();

    const pair = await screen.findByLabelText('Eligible versus observed');
    expect(pair.getAttribute('data-coverage-ratio')).toBe('under_rate_floor');
    expect(pair.querySelector('[data-state]')?.getAttribute('data-state')).toBe('partial');
    expect(pair.textContent).toContain('a rate requires 20 eligible units');
    expect(pair.textContent).not.toContain('Unsupported');
  });

  it('does not promote a numerically populated partial metric to a measured ready pair', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), 'http://localhost');
        if (url.pathname === '/api/observatory') {
          return response(envelope(observatoryModel([eventMetric('partial', 24, 30)]), 'complete'));
        }
        if (url.pathname === '/api/plugins/analytics/diagnostics') {
          return response(envelope(diagnosticsModel(), 'complete'));
        }
        return new Response('{}', { status: 503, headers: { 'content-type': 'application/json' } });
      }),
    );

    renderObservatory();

    const pair = await screen.findByLabelText('Eligible versus observed');
    expect(pair.getAttribute('data-coverage-ratio')).toBe('coverage_limited');
    expect(pair.querySelector('[data-state]')?.getAttribute('data-state')).toBe('partial');
    expect(pair.textContent).toContain('not rendered as a complete pair');
    expect(pair.textContent).not.toContain('24 observed of 30 eligible');
  });

  it('renders a self-referential denominator as non-independent conflict, not unsupported', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = new URL(String(input), 'http://localhost');
        if (url.pathname === '/api/observatory') {
          return response(envelope(observatoryModel([eventMetric('known', 24, 24)]), 'complete'));
        }
        if (url.pathname === '/api/plugins/analytics/diagnostics') {
          return response(envelope(diagnosticsModel(), 'complete'));
        }
        return new Response('{}', { status: 503, headers: { 'content-type': 'application/json' } });
      }),
    );

    renderObservatory();

    const pair = await screen.findByLabelText('Eligible versus observed');
    expect(pair.getAttribute('data-coverage-ratio')).toBe('self_referential');
    expect(pair.querySelector('[data-state]')?.getAttribute('data-state')).toBe('conflicting');
    expect(pair.textContent).toContain('non-independent denominator');
    expect(pair.textContent).not.toContain('Unsupported');
  });
});

function renderObservatory() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={client}>
      <ObservatoryPage />
    </QueryClientProvider>,
  );
}

function observatoryModel(metrics: unknown[] = []) {
  return {
    authorized_scope_ref: 'project.tracedecay',
    current: true,
    horizon: { since_micros: 0, until_micros: NOW_MICROS },
    metrics,
    observed_at_micros: NOW_MICROS,
    watermark: 'analytics:4821',
  };
}

function diagnosticsModel() {
  return {
    available: true,
    by_event_kind: [{ event_kind: 'adoption.eligibility_observed.v1', count: 4 }],
    by_hook: [],
    by_mcp_tool: [],
    by_outcome: [],
    by_prompt_category: [],
    by_tool: [],
    by_tool_category: [],
    event_count: 4,
    events_per_hour: null,
    hint_efficacy: {
      available: false,
      by_category: [],
      source: 'analytics_events',
      totals: { acted: 0, emitted: 0, ignored: 0, unresolved: 0 },
    },
    hook_call_count: 0,
    hook_readiness: null,
    hook_sources: [],
    hook_window: {
      newest_ts_unix_ms: null,
      oldest_ts_unix_ms: null,
      rows_included: 4,
      rows_scanned: 10_000,
      total_rows_known: false,
      truncated: true,
      window_rows: 10_000,
    },
    mcp_tool_call_count: 0,
    message_count: 0,
    ratios: {
      events_per_message: 0,
      hook_calls_per_message: 0,
      mcp_tool_calls_per_message: 0,
      tool_calls_per_message: 0,
    },
    recent_events: [],
    recent_hooks: [],
    source: 'analytics_events',
    tool_call_count: 0,
    tracedecay_call_count: 0,
  };
}

function envelope(payload: unknown, completeness: 'complete' | 'partial' = 'partial') {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: { source: 'analytics', watermark: 'analytics:4821' },
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness,
      eligible: 10_000,
      examined: 4,
      matched: null,
      excluded: null,
      omitted: 9_996,
      unknown: null,
      denominator: 10_000,
      unit: 'analytics_events',
      omission_reasons: ['diagnostics window capped at 10,000 rows'],
    },
    freshness: { state: 'fresh', observed_at_micros: NOW_MICROS, watermark: 'analytics:4821' },
    domain_state: completeness === 'complete' ? 'ready' : 'partial',
    legal_actions: [{ kind: 'refresh', operation: 'use-case.dashboard.observatory.refresh' }],
    payload,
  };
}

function eventMetric(
  state: 'capped' | 'known' | 'partial' | 'sampled' | 'stale' | 'unknown',
  observed = 24,
  eligible = 24,
) {
  return {
    calibration: null,
    cohort: {
      descriptor_revision: 'eligible_observability_events.v1',
      eligible_population: 'eligible_observability_events',
    },
    coverage: {
      censored: 0,
      completed: observed,
      eligible,
      excluded: 0,
      observed,
      state,
      unknown: 0,
    },
    denominator: 'eligible_observability_events',
    denominator_value: eligible,
    descriptor_revision: 'analytics-observability.v1',
    evidence_class: 'measurement',
    metric: 'observability_events',
    provenance: {
      projector_revision: 'observatory-projector.v1',
      source: 'observability_envelope',
      source_revision: 'observability-envelope.v1',
      watermark: 'analytics:4821',
    },
    temporal: {
      baseline_watermark: null,
      delta: null,
      horizon: { since_micros: 0, until_micros: NOW_MICROS },
    },
    unavailable_reason: null,
    uncertainty: { lower: observed, reason: null, upper: observed },
    unit: 'events',
    value: observed,
  };
}

function response(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
