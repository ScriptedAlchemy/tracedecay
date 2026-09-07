import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { AgentsPage } from './AgentsPage.tsx';
import type {
  AnalyticsDiagnosticsPayloadV1,
  AnalyticsUnderusedPayloadV1,
  AnalyticsUsageSummaryV1,
} from '../../contracts/generated.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('AgentsPage read coverage', () => {
  it('keeps an unavailable usage count distinct from a measured zero', async () => {
    stubAnalytics({
      usage: usageSummary({ message_count: 8 }),
      diagnostics: diagnosticsPayload({ available: false }),
      underused: underusedPayload({ available: false }),
    });
    renderAgents();

    expect((await screen.findAllByText(/event count unavailable/i)).length).toBeGreaterThan(0);
    expect(screen.queryByText(/0 events/i)).toBeNull();
    expect(screen.queryByText(/no analytics events recorded/i)).toBeNull();
    expect(screen.getAllByText(/analytics diagnostics unavailable/i).length).toBeGreaterThan(0);
    expect(screen.getByText(/hint diagnostics unavailable/i)).toBeTruthy();
    expect(screen.queryByText(/no tool calls recorded/i)).toBeNull();
    expect(screen.queryByText(/no tool families reported/i)).toBeNull();
  });

  it('keeps an underused-family query failure distinct from an empty family list', async () => {
    stubAnalytics({
      usage: usageSummary({ message_count: 8 }),
      diagnostics: diagnosticsPayload({ available: false }),
      underused: unavailableAnalyticsEnvelope(
        underusedPayload({ available: false }),
        'session-message query failed: no such table: session_messages',
      ),
    });
    renderAgents();

    expect(
      await screen.findByText(
        /Hint diagnostics unavailable: session-message query failed: no such table: session_messages/,
      ),
    ).toBeTruthy();
    expect(screen.queryByText(/no tool families reported/i)).toBeNull();
  });

  /** The fast usage read and slower diagnostics fold are separate snapshots.
   * An unavailable usage total must stay unknown rather than borrowing the
   * categorized sum, while diagnostics composition uses the count served with
   * its own rows instead of the fixture's former fabricated zero. */
  it('keeps an unknown usage window distinct from diagnostics composition', async () => {
    stubAnalytics({
      usage: usageSummary({
        source: 'analytics_events',
        message_count: 6,
        event_count: null,
        by_category: [
          { kind: 'tool', category: 'shell', events: 4 },
          { kind: 'tool', category: 'read', events: 2 },
        ],
      }),
      diagnostics: diagnosticsPayload({
        event_count: 4,
        by_event_kind: [{ event_kind: 'pre_tool_use', count: 4 }],
        by_outcome: [{ outcome: 'ok', count: 4 }],
      }),
      underused: underusedPayload(),
    });
    renderAgents();

    expect(
      await screen.findByText(/window's own event count was not reported/i),
    ).toBeTruthy();
    // The usage disclosure still has no denominator, so it makes no quantified
    // completeness claim. The two diagnostics figures have their own exact
    // four-event denominator from the same diagnostics snapshot.
    expect(screen.queryByText(/events in the window carry no tool/i)).toBeNull();
    expect(screen.queryAllByText(/share of 0$/)).toHaveLength(0);
    expect(screen.getAllByText(/share of 4$/)).toHaveLength(2);
  });

  it('discloses that hook counts come from a truncated recent suffix', async () => {
    stubAnalytics({
      usage: usageSummary({
        source: 'analytics_events',
        message_count: 2,
        event_count: 2,
        by_category: [{ kind: 'tool', category: 'shell', events: 2 }],
      }),
      diagnostics: diagnosticsPayload({
        event_count: 2,
        hook_call_count: 77,
        mcp_tool_call_count: 1,
        hook_window: hookWindow({
          window_rows: 10_000,
          rows_scanned: 10_000,
          rows_included: 10_000,
          truncated: true,
          total_rows_known: false,
        }),
      }),
      underused: underusedPayload(),
    });
    renderAgents();

    expect(await screen.findByText(/recent suffix · 10,000 rows scanned/i)).toBeTruthy();
    expect(screen.queryByText(/all time, hook log/i)).toBeNull();
  });

  it('renders subagent delegation counts from the session store', async () => {
    stubAnalytics({
      usage: usageSummary({ message_count: 2 }),
      diagnostics: diagnosticsPayload({ available: false }),
      underused: underusedPayload(),
      agents: {
        available: true,
        source: 'sessions',
        by_agent: [
          { agent: 'Codex', sessions: 42 },
          { agent: 'Claude', sessions: 3 },
        ],
      },
    });
    renderAgents();

    expect(await screen.findByText('Codex')).toBeTruthy();
    expect(screen.getByText('42')).toBeTruthy();
    expect(screen.getByText('Claude')).toBeTruthy();
    expect(screen.getByText(/sessions per managed subagent · source: sessions/i)).toBeTruthy();
  });

  /**
   * The three measures beside the delegation rollup. The stub
   * `fetch` below answers `/api/work/views` with a dashboard envelope rather
   * than the application envelope that route actually carries, so the graph
   * read is refused as a shape this build cannot decode — which is exactly the
   * case the two graph-fed surfaces must render as a refusal rather than as a
   * frontier of nothing and a failure count of zero.
   */
  it('carries handoff, tool-activity and failure-context surfaces beside the rollup', async () => {
    stubAnalytics({
      usage: usageSummary({
        source: 'analytics_events',
        message_count: 100,
        event_count: 400,
        by_category: [{ kind: 'tool', category: 'shell', events: 400 }],
      }),
      diagnostics: diagnosticsPayload({
        event_count: 400,
        tool_call_count: 200,
        mcp_tool_call_count: 150,
        tracedecay_call_count: 120,
        by_tool_category: [{ tool_category: 'mcp', count: 150 }],
        by_outcome: [
          { outcome: 'success', count: 380 },
          { outcome: 'error', count: 20 },
        ],
        by_mcp_tool: [{ tool_name: 'tracedecay_grep', count: 150 }],
        recent_hooks: [
          {
            agent: 'Codex',
            tool_name: 'tracedecay_grep',
            session_id: 's1',
            hook_name: 'pre_tool_use',
            prompt_category: 'search',
            ts_unix_ms: 1_700_000_000_000,
          },
        ],
        recent_events: [
          {
            timestamp: 1_700_000_000,
            tool_name: 'tracedecay_read',
            outcome: 'error',
            event_kind: 'post_tool_use',
            hook_name: 'post_tool_use',
          },
        ],
      }),
      underused: underusedPayload(),
      agents: { available: true, source: 'sessions', by_agent: [] },
    });
    renderAgents();

    expect(await screen.findByText('Handoff frontier')).toBeTruthy();
    // Tool activity lives on the demoted telemetry register, which renders
    // once its own usage read lands — awaited rather than assumed.
    expect(await screen.findByText('Tool activity')).toBeTruthy();
    expect(screen.getByText('Failure context')).toBeTruthy();

    // Tool activity is fed by the diagnostics read that landed.
    expect(await screen.findByText('through MCP')).toBeTruthy();
    expect(screen.getByText('not through MCP')).toBeTruthy();
    expect(screen.getByText(/Codex/)).toBeTruthy();

    // The failure accounting off the same read.
    expect(screen.getByText(/5\.00%/)).toBeTruthy();

    // Both graph-fed surfaces refuse rather than report a zero.
    expect(document.querySelector('[data-agent-handoffs="refused"]')).toBeTruthy();
    expect(document.querySelector('[data-agent-attempt-failures="refused"]')).toBeTruthy();
    expect(screen.getByText(/there is no frontier to be empty/)).toBeTruthy();
    expect(screen.getByText(/there is nothing to report as zero/)).toBeTruthy();
    expect(screen.queryByText(/no handoff on graph version/)).toBeNull();
  });

  it('keeps an unavailable subagent read distinct from zero delegations', async () => {
    stubAnalytics({
      usage: usageSummary({ message_count: 2 }),
      diagnostics: diagnosticsPayload({ available: false }),
      underused: underusedPayload(),
      agents: unavailableAnalyticsEnvelope(
        { available: false, source: 'session_store_unavailable', by_agent: [] },
        'analytics_agents_source_unavailable',
      ),
    });
    renderAgents();

    expect(
      await screen.findByText(
        /Subagent sessions unavailable: analytics_agents_source_unavailable/,
      ),
    ).toBeTruthy();
    expect(screen.queryByText(/no subagent sessions are recorded/i)).toBeNull();
  });
});

/**
 * The usage summary as `analytics_api` sends it: every field present, with an
 * unmeasured event count arriving as an explicit null rather than an absent key.
 */
function usageSummary(
  overrides: Partial<AnalyticsUsageSummaryV1> = {},
): AnalyticsUsageSummaryV1 {
  return {
    available: true,
    source: null,
    message_count: 0,
    event_count: null,
    by_category: [],
    ...overrides,
  };
}

/**
 * The diagnostics fold as `analytics_api` sends it. Every member of
 * `AnalyticsDiagnosticsPayloadV1` is present because the page decodes this
 * endpoint with the generated contract schema: a partial stub is rejected as
 * an undecodable shape, which reads on screen as "could not be read" rather
 * than as the unavailability a test means to set up.
 */
function diagnosticsPayload(
  overrides: Partial<AnalyticsDiagnosticsPayloadV1> = {},
): AnalyticsDiagnosticsPayloadV1 {
  return {
    available: true,
    source: 'analytics_events',
    message_count: 0,
    event_count: 0,
    tool_call_count: 0,
    mcp_tool_call_count: 0,
    tracedecay_call_count: 0,
    hook_call_count: 0,
    hook_sources: [],
    hook_readiness: null,
    events_per_hour: null,
    ratios: {
      events_per_message: 0,
      hook_calls_per_message: 0,
      mcp_tool_calls_per_message: 0,
      tool_calls_per_message: 0,
    },
    by_event_kind: [],
    by_tool: [],
    by_mcp_tool: [],
    by_tool_category: [],
    by_outcome: [],
    by_hook: [],
    by_prompt_category: [],
    hint_efficacy: {
      available: false,
      source: 'hook_analytics',
      totals: { acted: 0, emitted: 0, ignored: 0, unresolved: 0 },
      by_category: [],
    },
    hook_window: hookWindow(),
    recent_events: [],
    recent_hooks: [],
    ...overrides,
  };
}

function hookWindow(
  overrides: Partial<AnalyticsDiagnosticsPayloadV1['hook_window']> = {},
): AnalyticsDiagnosticsPayloadV1['hook_window'] {
  return {
    window_rows: 0,
    rows_scanned: 0,
    rows_included: 0,
    truncated: false,
    total_rows_known: true,
    newest_ts_unix_ms: null,
    oldest_ts_unix_ms: null,
    ...overrides,
  };
}

/** The underused-family read as `analytics_api` sends it, `db` included. */
function underusedPayload(
  overrides: Partial<AnalyticsUnderusedPayloadV1> = {},
): AnalyticsUnderusedPayloadV1 {
  return { available: true, db: 'analytics.db', families: [], ...overrides };
}

function stubAnalytics(payloads: Record<string, unknown>) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const endpoint = String(input).split('/').pop() ?? '';
      const response = payloads[endpoint] ?? {};
      const body = isEnvelope(response) ? response : fixtureEnvelope(response);
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

function unavailableAnalyticsEnvelope(payload: unknown, reason: string): Record<string, unknown> {
  return {
    ...fixtureEnvelope(payload, 'unknown'),
    coverage: {
      completeness: 'unknown',
      eligible: null,
      examined: null,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: null,
      unit: 'tool_families',
      omission_reasons: [reason],
    },
    freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
  };
}

function isEnvelope(value: unknown): value is Record<string, unknown> {
  return (
    typeof value === 'object' &&
    value !== null &&
    'domain_state' in value &&
    'payload' in value
  );
}

function renderAgents() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  render(
    <QueryClientProvider client={client}>
      <AgentsPage />
    </QueryClientProvider>,
  );
}
