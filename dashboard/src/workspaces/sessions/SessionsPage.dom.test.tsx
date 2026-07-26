import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { SessionsPage } from './SessionsPage.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('SessionsPage degraded states', () => {
  it('does not turn an unavailable LCM store into zero sessions and zero tracked messages', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        return jsonResponse(
          url.includes('/timeline')
            ? { exists: false, buckets: [] }
            : { exists: false, latest_sessions: [] },
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

    expect(
      await screen.findAllByText(/LCM session store is unavailable/i),
    ).toHaveLength(2);
    expect(screen.queryByText(/no sessions in the current window/i)).toBeNull();
    expect(screen.queryByText(/0 across 0 days/i)).toBeNull();
  });

  it('labels timeline totals as a bounded recent window and reports undated messages', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        return jsonResponse(
          url.includes('/timeline')
            ? {
                exists: true,
                buckets: [
                  { bucket: '2026-07-24', count: 4, token_estimate: 40 },
                  { bucket: '2026-07-25', count: 6, token_estimate: 60 },
                ],
                undated: { count: 3, token_estimate: 30 },
                coverage: {
                  limit: 400,
                  returned_buckets: 2,
                  total_dated_buckets: 500,
                  truncated: true,
                  ordering: 'most_recent',
                },
              }
            : { exists: true, latest_sessions: [] },
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

    expect(await screen.findByText(/10 in 2 loaded recent days/i)).toBeTruthy();
    expect(screen.getByText(/2 of 500 dated day buckets/i)).toBeTruthy();
    expect(screen.getByText(/3 undated messages are separate/i)).toBeTruthy();
    expect(screen.queryByText(/^messages tracked$/i)).toBeNull();
    expect(screen.queryByText(/^daily volume$/i)).toBeNull();
  });
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
