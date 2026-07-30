/**
 * What happens to a read when the scope it was issued for stops being the
 * scope on screen.
 *
 * Every scoped read carries `scopeKey(scope)` in its query key, so selecting
 * another project replaces the observer rather than re-using it. That much was
 * already true, and it is why a late answer cannot be painted into the new
 * project's panel. What was NOT true is that the abandoned request stopped:
 * the query function ignored React Query's `AbortSignal`, so every scope
 * change and every registry SSE event left a request running that nothing
 * would read, against a daemon this dashboard is otherwise careful not to
 * stack refetches against.
 *
 * The second half is the truthfulness half. Once a request can be aborted,
 * `fetch` rejects — and the transport used to answer every rejection with
 * `offline`. That would file a daemon-is-down reading against the abandoned
 * project's cache entry, so switching back would show a failure that never
 * happened. These tests hold both halves: the request really is cancelled, and
 * the cancellation is never mistaken for a reading.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { z } from 'zod';

import { useLegacy } from './useLegacy.ts';
import { useScope } from '../scope/store.ts';

const ProbeSchema = z.object({ project: z.string() });

/** One scoped read, rendering whatever it currently holds. */
function Probe() {
  const query = useLegacy(['probe'], '/api/probe', ProbeSchema);
  const result = query.data;
  if (!result) return <p data-probe="pending">pending</p>;
  return (
    <p data-probe={result.outcome}>
      {result.outcome === 'ok' ? result.data.project : result.outcome}
    </p>
  );
}

interface Attempt {
  readonly url: string;
  /** `null` is what `RequestInit` uses for "explicitly no signal", and it is
   * distinct from the field being absent — both mean nothing will be
   * cancelled, which is the state these tests exist to rule out. */
  readonly signal: AbortSignal | null | undefined;
  settle: (project: string) => void;
}

/** Every request, held open until the test decides to answer it. */
let attempts: Attempt[] = [];

function stubHeldRequests(): void {
  attempts = [];
  vi.stubGlobal(
    'fetch',
    vi.fn((url: string, init?: RequestInit) => {
      return new Promise<Response>((resolve, reject) => {
        const attempt: Attempt = {
          url: String(url),
          signal: init?.signal,
          settle: (project) =>
            resolve(
              new Response(JSON.stringify({ project }), {
                status: 200,
                headers: { 'content-type': 'application/json' },
              }),
            ),
        };
        attempts.push(attempt);
        init?.signal?.addEventListener('abort', () =>
          reject(new DOMException('The operation was aborted.', 'AbortError')),
        );
      });
    }),
  );
}

function renderProbe() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: 0, gcTime: 0 } },
  });
  return {
    client,
    ...render(
      <QueryClientProvider client={client}>
        <Probe />
      </QueryClientProvider>,
    ),
  };
}

beforeEach(() => {
  useScope.getState().selectAllProjects();
  stubHeldRequests();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('a scoped read whose scope changed', () => {
  it('aborts the request issued for the project the reader left', async () => {
    useScope.getState().selectProject('proj_a', 'Project A', 'active');
    renderProbe();
    await waitFor(() => expect(attempts).toHaveLength(1));
    const first = attempts[0]!;
    expect(first.signal?.aborted).toBe(false);

    act(() => useScope.getState().selectProject('proj_b', 'Project B', 'active'));

    // The claim this file exists for: the abandoned read really stops.
    await waitFor(() => expect(first.signal?.aborted).toBe(true));
  });

  it('never paints the abandoned project’s late answer into the new scope', async () => {
    useScope.getState().selectProject('proj_a', 'Project A', 'active');
    const { findByText } = renderProbe();
    await waitFor(() => expect(attempts).toHaveLength(1));
    const first = attempts[0]!;

    act(() => useScope.getState().selectProject('proj_b', 'Project B', 'active'));
    await waitFor(() => expect(attempts.length).toBeGreaterThanOrEqual(2));

    // The daemon answers the OLD request late, as a slow read does.
    act(() => first.settle('Project A data'));
    act(() => attempts[attempts.length - 1]!.settle('Project B data'));

    expect(await findByText('Project B data')).toBeTruthy();
    expect(document.body.textContent).not.toContain('Project A data');
  });

  it('leaves no fabricated offline reading behind for the project it left', async () => {
    // The regression the transport guard prevents. An aborted `fetch` rejects,
    // and a transport that answered every rejection with `offline` would file
    // one against `proj_a` — so coming back to that project would open on a
    // daemon-is-down panel for a request that was cancelled, not failed.
    useScope.getState().selectProject('proj_a', 'Project A', 'active');
    const { client } = renderProbe();
    await waitFor(() => expect(attempts).toHaveLength(1));

    act(() => useScope.getState().selectProject('proj_b', 'Project B', 'active'));
    await waitFor(() => expect(attempts[0]!.signal?.aborted).toBe(true));

    const abandoned = client
      .getQueryCache()
      .findAll({ queryKey: ['probe'] })
      .filter((q) => JSON.stringify(q.queryKey).includes('proj_a'));
    for (const query of abandoned) {
      expect(query.state.data).toBeUndefined();
    }
  });
});
