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
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
