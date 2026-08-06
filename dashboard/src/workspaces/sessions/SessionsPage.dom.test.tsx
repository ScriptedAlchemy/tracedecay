import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';

import { SessionsPage } from './SessionsPage.tsx';

const TEMPORAL_RETRIEVAL_UNAVAILABLE = {
  schema_revision: 1,
  scope: { project_id: 'project.sessions', storage_mode: 'profile_sharded', store_root: '/data' },
  version: { entity_version: null, graph_version: null },
  time: { valid_time_micros: null, observation_time_micros: 1 },
  source_watermark: null,
  authorization: { outcome: 'authorized' },
  coverage: {
    completeness: 'unknown',
    eligible: null,
    examined: null,
    matched: null,
    excluded: null,
    omitted: null,
    unknown: null,
    denominator: null,
    unit: 'records',
    omission_reasons: ['lcm_temporal_retrieval_not_mounted'],
  },
  freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
  domain_state: 'unknown',
  legal_actions: [],
  payload: null,
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('SessionsPage temporal retrieval state', () => {
  it('reports the unavailable canonical temporal authority without fake zero rows', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => jsonResponse(TEMPORAL_RETRIEVAL_UNAVAILABLE)),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, gcTime: 0 } },
    });
    render(
      <QueryClientProvider client={client}>
        <SessionsPage />
      </QueryClientProvider>,
    );

    expect(await screen.findAllByText(/lcm_temporal_retrieval_not_mounted/)).toHaveLength(2);
    expect(screen.queryByText(/no sessions in the current window/i)).toBeNull();
    expect(screen.queryByText(/0 across 0 days/i)).toBeNull();
  });

  it('labels server-accounted timeline tokens by their provenance', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const isTimeline = String(input).includes('/timeline');
        return jsonResponse(
          fixtureEnvelope(
            isTimeline
              ? {
                  path: 'daemon://session-temporal',
                  storage_scope: 'project',
                  exists: true,
                  bucket: 'day',
                  session_id: null,
                  buckets: [
                    {
                      bucket: '2026-08-05',
                      count: 2,
                      token_count: 21,
                      token_count_provenance: 'o200k_approximate',
                      known_message_count: 2,
                      unknown_message_count: 0,
                    },
                  ],
                  node_buckets: [],
                  undated: {
                    count: 0,
                    token_count: null,
                    token_count_provenance: 'unavailable',
                    known_message_count: 0,
                    unknown_message_count: 0,
                  },
                  coverage: {
                    limit: 400,
                    returned_buckets: 1,
                    total_dated_buckets: 1,
                    truncated: false,
                    ordering: 'most_recent',
                    next_before_bucket: null,
                  },
                }
              : {
                  path: 'daemon://session-temporal',
                  storage_scope: 'project',
                  exists: true,
                  overview: {
                    messages_total: 0,
                    sessions_total: 0,
                    summary_nodes_total: 0,
                    summary_node_sessions_total: 0,
                    max_summary_depth: 0,
                    role_counts: [],
                    source_counts: [],
                    depth_counts: [],
                    compression: {
                      source_token_count: null,
                      token_count: null,
                      ratio: null,
                      node_count: 0,
                    },
                  },
                  latest_sessions: [],
                  latest_summary_nodes: [],
                  matches: { messages: [], summary_nodes: [] },
                  query: '',
                  limit: 25,
                },
          ),
        );
      }),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, gcTime: 0 } },
    });
    render(
      <QueryClientProvider client={client}>
        <SessionsPage />
      </QueryClientProvider>,
    );

    const chart = await screen.findByRole('img', { name: /Activity over 1 days/ });
    const bar = chart.querySelector('rect');
    expect(bar).not.toBeNull();
    fireEvent.mouseEnter(bar!);
    await waitFor(() => {
      expect(chart.parentElement?.textContent).toContain(
        '~21 tokens · o200k approximate',
      );
    });
  });
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
