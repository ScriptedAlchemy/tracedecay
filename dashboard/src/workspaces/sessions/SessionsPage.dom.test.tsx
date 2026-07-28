import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { SessionsPage } from './SessionsPage.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

/** `lcm_service::timeline_payload` seeds these seven fields before it touches
 * the store and returns them unchanged when there is no store to read, so an
 * unavailable timeline still arrives fully shaped — `exists: false` beside
 * empty buckets, not a body missing its scope and bucket width. */
function timeline(over: Record<string, unknown> = {}) {
  return {
    path: '/home/zack/.tracedecay/sessions.db',
    storage_scope: 'profile_sharded',
    exists: true,
    bucket: 'day',
    session_id: null,
    buckets: [],
    node_buckets: [],
    undated: { count: 0, token_estimate: 0 },
    coverage: null,
    ...over,
  };
}

describe('SessionsPage degraded states', () => {
  it('does not turn an unavailable LCM store into zero sessions and zero tracked messages', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        return jsonResponse(
          url.includes('/timeline')
            ? timeline({ exists: false })
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
            ? timeline({
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
                  next_before_bucket: '2026-07-24',
                },
              })
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

  it('keeps search selection distinct when providers reuse a message id', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/search')) {
          return jsonResponse({
            matches: {
              messages: [
                {
                  store_id: 'store-claude',
                  message_id: 'shared-message',
                  source: 'claude',
                  role: 'assistant',
                  snippet: 'Claude result',
                },
                {
                  store_id: 'store-codex',
                  message_id: 'shared-message',
                  source: 'codex',
                  role: 'assistant',
                  snippet: 'Codex result',
                },
              ],
            },
            total: { messages: 2, summary_nodes: 0 },
          });
        }
        return jsonResponse(
          url.includes('/timeline')
            ? timeline()
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

    const search = screen.getByRole('searchbox', { name: 'Search transcripts' });
    fireEvent.change(search, { target: { value: 'shared-message' } });
    fireEvent.submit(search.closest('form')!);

    const claude = await screen.findByRole('button', { name: /claude.*Claude result/i });
    const codex = screen.getByRole('button', { name: /codex.*Codex result/i });
    fireEvent.click(claude);

    expect(claude.getAttribute('aria-pressed')).toBe('true');
    expect(codex.getAttribute('aria-pressed')).toBe('false');
  });
});

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
