import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

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
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
