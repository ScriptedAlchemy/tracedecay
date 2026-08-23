/**
 * The run-status poll is the only thing that resolves an admitted Explorer run:
 * the coordinator publishes no targeted SSE invalidation for query completion,
 * so whatever ends this poll ends the run's visible life in the UI.
 *
 * These cases pin the three outcomes that must stay distinct — a transient
 * transport failure keeps polling, a standing refusal stops it, and a transport
 * failure that never clears stops it too rather than retrying forever — by
 * driving the real controller against a scripted daemon on a fake clock.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, renderHook } from '@testing-library/react';
import { createElement, type ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useExplorerController } from './controller.ts';

const RUN_ID = 'explorer-run-poll';
const QUERY = 'graph';

/** The status route for the admitted run, as the controller spells it. */
const STATUS_PATH = `/api/explorer/queries/${RUN_ID}`;

type RunState = 'pending' | 'completed';

function runEnvelope(state: RunState) {
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
      run_id: RUN_ID,
      request: { query: QUERY, limit: 50, offset: 0 },
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
  /** The browser could not reach the daemon at all — `fetch` rejects. */
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
  const fetchMock = vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
    const url = String(input);
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

  it('gives up on a transport failure that never clears rather than retrying forever', async () => {
    // The retry is bounded: an unreachable daemon must not be polled for the
    // lifetime of the page, and the failure must still end up on screen.
    const { fetchMock, statusReads } = scriptedDaemon([
      { kind: 'run', state: 'pending' },
      { kind: 'unreachable' },
    ]);
    vi.stubGlobal('fetch', fetchMock);

    const view = await submitQuery();
    await runPollClock(120_000);
    const afterGivingUp = statusReads.length;
    await runPollClock(120_000);

    expect(statusReads.length).toBe(afterGivingUp);
    // One read that found the run pending, then the whole retry budget: four
    // consecutive unreachable reads and no fifth.
    expect(afterGivingUp).toBe(5);
    const result = view.result.current.runResult;
    expect(result?.outcome).toBe('transport');
    expect(result?.outcome === 'transport' ? result.state : null).toBe('offline');
  });
});
