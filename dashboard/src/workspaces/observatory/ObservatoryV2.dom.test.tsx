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

    // No source reading is silently promoted to a complete window because the
    // diagnostics envelope itself says its record window was partial.
    expect(document.querySelector('[data-coverage-window="capped"]')).toBeTruthy();

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
});

function renderObservatory() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={client}>
      <ObservatoryPage />
    </QueryClientProvider>,
  );
}

function observatoryModel() {
  return {
    authorized_scope_ref: 'project.tracedecay',
    current: true,
    horizon: { since_micros: 0, until_micros: NOW_MICROS },
    metrics: [],
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

function envelope(payload: unknown) {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: { source: 'analytics', watermark: 'analytics:4821' },
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'partial',
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
    domain_state: 'partial',
    legal_actions: [{ kind: 'refresh', operation: 'use-case.dashboard.observatory.refresh' }],
    payload,
  };
}

function response(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
