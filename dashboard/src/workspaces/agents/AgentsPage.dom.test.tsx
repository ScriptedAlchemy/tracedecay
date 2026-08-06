import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { AgentsPage } from './AgentsPage.tsx';
import type { AnalyticsUsageSummaryV1 } from '../../contracts/generated.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('AgentsPage read coverage', () => {
  it('keeps an unavailable usage count distinct from a measured zero', async () => {
    stubAnalytics({
      usage: usageSummary({ message_count: 8 }),
      diagnostics: { available: false, hook_call_count: 0, by_mcp_tool: [] },
      underused: { available: false, families: [] },
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
      diagnostics: { available: false, hook_call_count: 0, by_mcp_tool: [] },
      underused: unavailableAnalyticsEnvelope(
        { available: false, families: [] },
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

  /** An unreported window size used to be substituted with the categorized
   * total, which made the two agree by construction: the uncategorized
   * remainder came out as zero and the whole disclosure disappeared, so unknown
   * coverage read as complete coverage. The composition caption had the mirror
   * defect — "share of 0" under counts that were really served. */
  it('does not let an unreported window size read as complete categorization', async () => {
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
      diagnostics: {
        available: true,
        by_event_kind: [{ event_kind: 'pre_tool_use', count: 4 }],
        by_outcome: [{ outcome: 'ok', count: 4 }],
        by_mcp_tool: [],
        recent_events: [],
      },
      underused: { available: true, families: [] },
    });
    renderAgents();

    expect(
      await screen.findByText(/window's own event count was not reported/i),
    ).toBeTruthy();
    // The quantified disclosure needs a window size; only the unknown reading
    // is on screen, and no count of uncategorized events is claimed.
    expect(screen.queryByText(/events in the window carry no tool/i)).toBeNull();
    expect(screen.queryByText(/share of 0$/)).toBeNull();
    expect(screen.getAllByText(/window total unreported/i).length).toBe(2);
  });

  it('discloses that hook counts come from a truncated recent suffix', async () => {
    stubAnalytics({
      usage: usageSummary({
        source: 'analytics_events',
        message_count: 2,
        event_count: 2,
        by_category: [{ kind: 'tool', category: 'shell', events: 2 }],
      }),
      diagnostics: {
        available: true,
        event_count: 2,
        hook_call_count: 77,
        mcp_tool_call_count: 1,
        by_mcp_tool: [],
        recent_events: [],
        hook_window: {
          window_rows: 10_000,
          rows_scanned: 10_000,
          rows_included: 10_000,
          truncated: true,
        },
      },
      underused: { available: true, families: [] },
    });
    renderAgents();

    expect(await screen.findByText(/recent suffix · 10,000 rows scanned/i)).toBeTruthy();
    expect(screen.queryByText(/all time, hook log/i)).toBeNull();
  });

  it('renders subagent delegation counts from the session store', async () => {
    stubAnalytics({
      usage: usageSummary({ message_count: 2 }),
      diagnostics: { available: false, hook_call_count: 0, by_mcp_tool: [] },
      underused: { available: true, families: [] },
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

  it('keeps an unavailable subagent read distinct from zero delegations', async () => {
    stubAnalytics({
      usage: usageSummary({ message_count: 2 }),
      diagnostics: { available: false, hook_call_count: 0, by_mcp_tool: [] },
      underused: { available: true, families: [] },
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
