/**
 * The run-status poll is the only thing that resolves an admitted Explorer run:
 * the coordinator publishes no targeted SSE invalidation for query completion,
 * so whatever ends this poll ends the run's visible life in the UI.
 *
 * These cases pin the outcomes that must stay distinct, a transient transport
 * failure keeps polling however many times it repeats, a standing refusal stops
 * it at once, a failure that never clears backs off to a slow tick instead of
 * either hammering or abandoning the run, and the poll ends with the surface , 
 * by driving the real controller against a scripted daemon on a fake clock.
 */
import { MutationCache, QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, renderHook } from '@testing-library/react';
import { createElement, type ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useExplorerController } from './controller.ts';

const RUN_ID = 'explorer-run-poll';
const QUERY = 'graph';

/** The status route for the admitted run, as the controller spells it. */
const STATUS_PATH = `/api/explorer/queries/${RUN_ID}`;

type RunState = 'pending' | 'completed';

function runEnvelope(state: RunState, runId = RUN_ID, query = QUERY) {
  return {
    schema_revision: 1,
    scope: {
      project_id: 'project.explorer',
      storage_mode: 'profile_sharded',
      store_root: '/data/project',
    },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 10 },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: state === 'completed' ? 'complete' : 'partial',
      eligible: 0,
      examined: 0,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: 0,
      unit: 'sources',
      omission_reasons: [],
    },
    freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
    domain_state: state === 'completed' ? 'ready' : 'partial',
    legal_actions: [],
    payload: {
      run_id: runId,
      request: { query, limit: 50, offset: 0 },
      request_revision: 'explorer-query-request-v1',
      plan_revision: 'explorer-query-plan-v1',
      merge_revision: 'source-local-no-merge-v1',
      required_source_ids: [],
      ordering_policy: 'source_local_no_cross_source_merge',
      explanation: 'poll fixture',
      submitted_at_micros: 1,
      completed_at_micros: state === 'completed' ? 10 : null,
      elapsed_micros: 9,
      state,
      finality: state === 'completed' ? 'complete' : 'pending',
      sources: [],
    },
  };
}

function jsonResponse(status: number, body: unknown): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  } as Response;
}

/** One scripted status read: what the daemon does on the Nth poll. */
type StatusStep =
  | { kind: 'run'; state: RunState }
  /** The browser could not reach the daemon at all, `fetch` rejects. */
  | { kind: 'unreachable' }
  | { kind: 'status'; code: number };

/**
 * A daemon that admits the run, then answers each status read from `script`,
 * repeating the final step once the script runs out. Every status read is
 * counted so a case can prove the poll stopped rather than merely that the
 * last value looked wrong.
 */
function scriptedDaemon(script: readonly StatusStep[]) {
  const statusReads: StatusStep[] = [];
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input);
    if (url.includes(STATUS_PATH) && init?.method === 'DELETE') {
      return jsonResponse(200, runEnvelope('completed'));
    }
    if (url.includes(STATUS_PATH)) {
      const step = script[Math.min(statusReads.length, script.length - 1)];
      if (step === undefined) throw new Error('scripted daemon has no status step');
      statusReads.push(step);
      if (step.kind === 'unreachable') throw new TypeError('Failed to fetch');
      if (step.kind === 'status') return jsonResponse(step.code, { error: 'refused' });
      return jsonResponse(200, runEnvelope(step.state));
    }
    if (url.includes('/api/explorer/queries')) {
      return jsonResponse(200, runEnvelope('pending'));
    }
    return jsonResponse(404, { error: 'not found' });
  });
  return { fetchMock, statusReads };
}

function wrapper({ children }: { children: ReactNode }) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return createElement(QueryClientProvider, { client }, children);
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

/** Submits `QUERY` and lets the create + first status read settle. */
async function submitQuery() {
  const view = renderHook(() => useExplorerController(), { wrapper });
  await act(async () => {
    view.result.current.setQuery(QUERY);
  });
  await act(async () => {
    view.result.current.submit();
    await vi.advanceTimersByTimeAsync(0);
  });
  return view;
}

/** Runs the poll clock long enough for every scripted step to be reached. */
async function runPollClock(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

describe('explorer run-status polling', () => {
  it('keeps polling after one transient transport failure and resolves the run', async () => {
    // Read 1 finds the run pending, read 2 cannot reach the daemon at all, and
    // read 3 would find the completed run. Only read 2 is a failure, and it is
    // the kind a repeat read clears by itself.
    const { fetchMock, statusReads } = scriptedDaemon([
      { kind: 'run', state: 'pending' },
      { kind: 'unreachable' },
      { kind: 'run', state: 'completed' },
    ]);
    vi.stubGlobal('fetch', fetchMock);

    const view = await submitQuery();
    await runPollClock(30_000);

    expect(statusReads.length).toBeGreaterThanOrEqual(3);
    expect(view.result.current.runResult?.outcome).toBe('envelope');
    expect(view.result.current.run?.state).toBe('completed');
  });

  it('stops polling on a standing refusal and leaves it on screen', async () => {
    // A 403 is not something a repeat read clears: the answer is the same until
    // the operator changes something, so the poll must stop and show it.
    const { fetchMock, statusReads } = scriptedDaemon([
      { kind: 'run', state: 'pending' },
      { kind: 'status', code: 403 },
    ]);
    vi.stubGlobal('fetch', fetchMock);

    const view = await submitQuery();
    await runPollClock(30_000);
    const afterRefusal = statusReads.length;
    await runPollClock(60_000);

    expect(statusReads.length).toBe(afterRefusal);
    expect(afterRefusal).toBe(2);
    const result = view.result.current.runResult;
    expect(result?.outcome).toBe('transport');
    expect(result?.outcome === 'transport' ? result.state : null).toBe('denied');
  });

  it('resolves the run after more than four consecutive transient failures', async () => {
    // The failure budget is the bug: a run whose daemon blinks six times and
    // then answers is still a live run, and the only thing that can surface it
    // is this poll. Six unreachable reads is deliberately past any small cap.
    const { fetchMock, statusReads } = scriptedDaemon([
      { kind: 'run', state: 'pending' },
      { kind: 'unreachable' },
      { kind: 'unreachable' },
      { kind: 'unreachable' },
      { kind: 'unreachable' },
      { kind: 'unreachable' },
      { kind: 'unreachable' },
      { kind: 'run', state: 'completed' },
    ]);
    vi.stubGlobal('fetch', fetchMock);

    const view = await submitQuery();
    await runPollClock(300_000);

    expect(statusReads.filter((step) => step.kind === 'unreachable').length).toBe(6);
    expect(view.result.current.runResult?.outcome).toBe('envelope');
    expect(view.result.current.run?.state).toBe('completed');
  });

  it('backs off to a slow tick on a transport failure that never clears, and shows it', async () => {
    // What is bounded is the rate, not the attempts. A daemon that never comes
    // back must cost a couple of reads a minute rather than four a second, and
    // the reader must be looking at the failure the whole time, an offline
    // lane, not a spinner that hides it.
    const { fetchMock, statusReads } = scriptedDaemon([
      { kind: 'run', state: 'pending' },
      { kind: 'unreachable' },
    ]);
    vi.stubGlobal('fetch', fetchMock);

    const view = await submitQuery();
    await runPollClock(300_000);
    const afterFirstWindow = statusReads.length;
    await runPollClock(300_000);
    const secondWindow = statusReads.length - afterFirstWindow;

    // Still asking, so a daemon that returns is still found.
    expect(secondWindow).toBeGreaterThan(0);
    // And asking at the 30 s ceiling: five minutes buys about ten reads, never
    // the hundreds a fast tick would.
    expect(secondWindow).toBeLessThanOrEqual(11);
    const result = view.result.current.runResult;
    expect(result?.outcome).toBe('transport');
    expect(result?.outcome === 'transport' ? result.state : null).toBe('offline');
    // The failure is on screen, and no lane is pretending to still be working.
    const byLane = new Map(view.result.current.lanes.map((lane) => [lane.lane, lane.state]));
    expect(byLane.get('code')).toBe('offline');
    expect(byLane.get('sessions')).toBe('offline');
    expect(byLane.get('knowledge')).toBe('offline');
    expect(view.result.current.anyPending).toBe(false);
  });

  it('stops polling when the surface unmounts', async () => {
    // Slow polling is only affordable because it ends with the surface: an
    // unmounted Explorer must not leave a timer asking about a dead run.
    const { fetchMock, statusReads } = scriptedDaemon([
      { kind: 'run', state: 'pending' },
      { kind: 'unreachable' },
    ]);
    vi.stubGlobal('fetch', fetchMock);

    const view = await submitQuery();
    await runPollClock(60_000);
    const afterUnmount = statusReads.length;
    view.unmount();
    await runPollClock(300_000);

    expect(statusReads.length).toBe(afterUnmount);
  });

  it('cancels an accepted run when unmounted before mutation callbacks finish', async () => {
    let releaseCallbacks!: () => void;
    const pending = new Promise<void>((resolve) => { releaseCallbacks = resolve; });
    const afterAdmission = vi.fn(() => pending);
    const client = new QueryClient({ mutationCache: new MutationCache({ onSuccess: afterAdmission }) });
    const { fetchMock } = scriptedDaemon([{ kind: 'run', state: 'pending' }]);
    vi.stubGlobal('fetch', fetchMock);
    const view = renderHook(() => useExplorerController(), {
      wrapper: ({ children }) => createElement(QueryClientProvider, { client }, children),
    });
    try {
      await act(async () => {
        view.result.current.submit(QUERY);
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(afterAdmission).toHaveBeenCalledOnce();
      view.unmount();
      expect(fetchMock.mock.calls.filter(([url, init]) =>
        String(url).endsWith(STATUS_PATH) && init?.method === 'DELETE',
      )).toHaveLength(1);
    } finally {
      releaseCallbacks();
      client.clear();
    }
  });

  it('stops polling when the search is reset', async () => {
    // Reset drops the active run, so the poll it was the only reader of ends
    // with it rather than surviving into the next search.
    const { fetchMock, statusReads } = scriptedDaemon([
      { kind: 'run', state: 'pending' },
      { kind: 'unreachable' },
    ]);
    vi.stubGlobal('fetch', fetchMock);

    const view = await submitQuery();
    await runPollClock(60_000);
    await act(async () => {
      view.result.current.reset();
      await vi.advanceTimersByTimeAsync(0);
    });
    const afterReset = statusReads.length;
    await runPollClock(300_000);

    expect(statusReads.length).toBe(afterReset);
  });
});

/** A held admission POST can complete after a newer query or an unmount.
 * The result must cancel its exact server run rather than reclaim the UI. */
describe('explorer query admission ownership', () => {
  it.each(['pending', 'completed'] as const)('keeps the newer query after a late %s admission', async (state) => {
    let finishOlder!: (response: Response) => void;
    const heldOlder = new Promise<Response>((resolve) => {
      finishOlder = resolve;
    });
    const cancellations: string[] = [];
    const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = String(input);
      if (init?.method === 'POST' && url.endsWith('/api/explorer/queries')) {
        const { query } = JSON.parse(String(init.body)) as { query: string };
        if (query === 'older') return heldOlder;
        return Promise.resolve(jsonResponse(202, runEnvelope('pending', 'run-newer', 'newer')));
      }
      if (init?.method === 'DELETE') {
        cancellations.push(url);
        return Promise.resolve(jsonResponse(200, runEnvelope('completed', 'run-newer', 'newer')));
      }
      if (url.endsWith('/api/explorer/queries/run-newer')) {
        return Promise.resolve(jsonResponse(200, runEnvelope('pending', 'run-newer', 'newer')));
      }
      return Promise.resolve(jsonResponse(404, { error: 'unknown run' }));
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderHook(() => useExplorerController(), { wrapper });

    await act(async () => {
      view.result.current.submit('older');
      await vi.advanceTimersByTimeAsync(0);
    });
    await act(async () => {
      view.result.current.submit('newer');
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(view.result.current.run?.run_id).toBe('run-newer');

    await act(async () => {
      finishOlder(jsonResponse(202, runEnvelope(state, 'run-older', 'older')));
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(cancellations).toEqual(state === 'pending' ? ['/api/explorer/queries/run-older'] : []);
    expect(view.result.current.run?.run_id).toBe('run-newer');
    view.unmount();
  });

  it('cancels the old run on supersession and the live run on unmount', async () => {
    const cancellations: string[] = [];
    const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = String(input);
      if (init?.method === 'POST' && url.endsWith('/api/explorer/queries')) {
        const { query } = JSON.parse(String(init.body)) as { query: string };
        return Promise.resolve(jsonResponse(202, runEnvelope('pending', `run-${query}`, query)));
      }
      if (init?.method === 'DELETE') {
        cancellations.push(url);
        return Promise.resolve(jsonResponse(200, runEnvelope('completed')));
      }
      if (url.includes('/api/explorer/queries/run-')) {
        const query = url.substring(url.lastIndexOf('run-') + 4);
        return Promise.resolve(jsonResponse(200, runEnvelope('pending', `run-${query}`, query)));
      }
      return Promise.resolve(jsonResponse(404, { error: 'unknown run' }));
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = renderHook(() => useExplorerController(), { wrapper });

    await act(async () => {
      view.result.current.submit('first');
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(view.result.current.run?.run_id).toBe('run-first');
    await act(async () => {
      view.result.current.submit('second');
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(cancellations).toEqual(['/api/explorer/queries/run-first']);
    expect(view.result.current.run?.run_id).toBe('run-second');

    view.unmount();
    expect(cancellations).toEqual([
      '/api/explorer/queries/run-first',
      '/api/explorer/queries/run-second',
    ]);
  });
});
