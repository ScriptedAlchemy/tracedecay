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
import type { ReactNode } from 'react';
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

/** The same read against `/api/projects`, which `scopedUrl` leaves alone. */
function RegistryProbe() {
  const query = useLegacy(['registry-probe'], '/api/projects', ProbeSchema);
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

function renderProbe(gcTime = 0) {
  return mount(<Probe />, { staleTime: 0, gcTime });
}

/** The same probe against a route `scopedUrl` never rewrites.
 *
 * A real `staleTime` here, because the question is whether a scope change
 * re-asks a question whose answer has not changed — with `staleTime: 0` every
 * observer refetches on mount regardless of key, which would make the two
 * behaviours indistinguishable. */
function renderRegistryProbe() {
  return mount(<RegistryProbe />, { staleTime: 60_000, gcTime: 60_000 });
}

function mount(node: ReactNode, { staleTime, gcTime }: { staleTime: number; gcTime: number }) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime, gcTime } },
  });
  return {
    client,
    ...render(<QueryClientProvider client={client}>{node}</QueryClientProvider>),
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

/**
 * The reads that carry no project, and must not be filed under one.
 *
 * `scopedUrl` deliberately leaves `/api/projects` and `/api/dashboard` alone —
 * the registry is the thing that lists projects, and the chrome sits above all
 * of them — so the identical URL is fetched under every scope. Keying them by
 * scope anyway split one answer into an entry per project: switching project
 * refetched a listing that had not changed, and an entry warmed under one
 * scope was invisible under the next, which is why several surfaces each ended
 * up holding their own copy of the registry.
 *
 * The key is therefore derived from what the REQUEST carries, not from the
 * scope it was issued under.
 */
describe('a read that is never rewritten by scope', () => {
  it('holds one cache entry across scope changes', async () => {
    useScope.getState().selectProject('proj_a', 'Project A', 'active');
    const { client } = renderRegistryProbe();
    await waitFor(() => expect(attempts).toHaveLength(1));
    expect(attempts[0]!.url).toBe('/api/projects');
    act(() => attempts[0]!.settle('registry'));

    act(() => useScope.getState().selectProject('proj_b', 'Project B', 'active'));
    act(() => useScope.getState().selectAllProjects());

    const entries = client.getQueryCache().findAll({ queryKey: ['registry-probe'] });
    expect(entries).toHaveLength(1);
    // And nothing about a project in the key it is filed under.
    expect(JSON.stringify(entries[0]!.queryKey)).not.toContain('proj_');
  });

  it('serves the answer it already has instead of refetching per project', async () => {
    useScope.getState().selectProject('proj_a', 'Project A', 'active');
    const { findByText } = renderRegistryProbe();
    await waitFor(() => expect(attempts).toHaveLength(1));
    act(() => attempts[0]!.settle('registry'));
    expect(await findByText('registry')).toBeTruthy();

    act(() => useScope.getState().selectProject('proj_b', 'Project B', 'active'));

    // Same entry, already resolved: the reader keeps the answer rather than
    // dropping to pending and asking the daemon the same question again.
    await waitFor(() => expect(document.body.textContent).toContain('registry'));
    expect(attempts).toHaveLength(1);
  });

  it('still keys a genuinely scoped read by its project', async () => {
    // The other half, and the reason this is derived per request rather than
    // switched off wholesale: a read the gateway DOES rewrite must stay one
    // entry per project, or two projects would share a reading.
    // Retained rather than collected the moment it loses its observer, so the
    // entry the abandoned scope left behind is still there to be identified.
    useScope.getState().selectProject('proj_a', 'Project A', 'active');
    const { client } = renderProbe(60_000);
    await waitFor(() => expect(attempts).toHaveLength(1));
    act(() => attempts[0]!.settle('a'));

    act(() => useScope.getState().selectProject('proj_b', 'Project B', 'active'));
    await waitFor(() => expect(attempts.length).toBeGreaterThanOrEqual(2));

    const keys = client
      .getQueryCache()
      .findAll({ queryKey: ['probe'] })
      .map((query) => JSON.stringify(query.queryKey));
    expect(keys.some((key) => key.includes('proj_a'))).toBe(true);
    expect(keys.some((key) => key.includes('proj_b'))).toBe(true);
  });
});
