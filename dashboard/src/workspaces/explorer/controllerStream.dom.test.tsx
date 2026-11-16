import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, renderHook } from '@testing-library/react';
import { createElement, type ReactNode } from 'react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useScope } from '../../data/scope/store.ts';
import { useExplorerController } from './controller.ts';

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
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
  useScope.getState().selectAllProjects();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

it('aborts exact requests on reset, scope change, and unmount', async () => {
  const requests: Array<{ url: string; signal: AbortSignal }> = [];
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    if (init?.method !== 'POST' || !String(input).endsWith('/explorer/queries')) {
      return jsonResponse(404, { error: 'not found' });
    }
    const signal = init.signal;
    if (!signal) throw new Error('Explorer read needs an abort signal');
    requests.push({ url: String(input), signal });
    return new Response(new ReadableStream({
      start(controller) {
        signal.addEventListener('abort', () => controller.error(new DOMException('aborted', 'AbortError')), { once: true });
      },
    }), { status: 200, headers: { 'content-type': 'text/event-stream' } });
  }));
  const view = renderHook(() => useExplorerController(), { wrapper });
  await act(async () => {
    view.result.current.submit('first');
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(requests).toHaveLength(1);
  await act(async () => { view.result.current.reset(); });
  expect(requests[0]?.signal.aborted).toBe(true);
  await act(async () => {
    view.result.current.submit('second');
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(requests).toHaveLength(2);
  await act(async () => {
    useScope.getState().selectProject('proj_other', 'Other project', 'selected');
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(requests[1]?.signal.aborted).toBe(true);
  expect(requests[2]?.url).toBe('/api/projects/proj_other/explorer/queries');
  view.unmount();
  expect(requests[2]?.signal.aborted).toBe(true);
});

it('superseded_explorer_stream_cannot_replace_new_results', async () => {
  const requests: string[] = [];
  let releaseOld!: () => void;
  const oldFrame = new Promise<void>((resolve) => { releaseOld = resolve; });
  let oldSignal: AbortSignal | undefined;
  const source = (query: string) => ({
    kind: 'source',
    source: {
      source_id: 'code_graph', source_label: 'Code graph', phase: 'completed',
      outcome: 'ready', completed_units: 1, total_units: 1,
      coverage: { completeness: 'complete', eligible: 1, examined: 1, matched: 1,
        excluded: 0, omitted: 0, unknown: 0, denominator: 1, unit: 'symbols',
        omission_reasons: [] },
      freshness: 'fresh', watermark: query, error_code: null, message: null,
      page: { offset: 0, limit: 50, total: 1, next_offset: null,
        rows: [{ name: query }], metadata: {} },
    },
  });
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (!url.endsWith('/api/explorer/queries')) {
      requests.push(url);
      return jsonResponse(404, { error: 'retired polling route' });
    }
    const { query } = JSON.parse(String(init?.body)) as { query: string };
    requests.push(`${init?.method} ${query}`);
    if (query === 'older') oldSignal = init?.signal ?? undefined;
    const encoder = new TextEncoder();
    const body = new ReadableStream<Uint8Array>({
      async start(controller) {
        if (query === 'older') await oldFrame;
        controller.enqueue(encoder.encode(`data: ${JSON.stringify(source(query))}\n\n`));
        controller.enqueue(encoder.encode('data: {"kind":"final","state":"completed","elapsed_micros":1}\n\n'));
        controller.close();
      },
    });
    return new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } });
  }));
  const view = renderHook(() => useExplorerController(), { wrapper });
  try {
    await act(async () => {
      view.result.current.submit('older');
      await vi.advanceTimersByTimeAsync(0);
    });
    await act(async () => {
      view.result.current.submit('newer');
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(requests.filter((url) => url.includes('/api/explorer/queries') || url.startsWith('POST ')),
      'controller polled a run id instead of reading the stream').toEqual(['POST older', 'POST newer']);
    expect(oldSignal?.aborted, 'controller polled a run id instead of reading the stream').toBe(true);
    expect(view.result.current.run?.sources[0]?.watermark).toBe('newer');
    await act(async () => {
      releaseOld();
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(view.result.current.run?.sources[0]?.watermark).toBe('newer');
  } finally {
    releaseOld();
    view.unmount();
  }
});

it('reports premature EOF without hiding a delivered source', async () => {
  const source = {
    kind: 'source',
    source: {
      source_id: 'code_graph', source_label: 'Code graph', phase: 'completed',
      outcome: 'ready', completed_units: 0, total_units: 0,
      coverage: { completeness: 'complete', eligible: 0, examined: 0, matched: 0,
        excluded: 0, omitted: 0, unknown: 0, denominator: 0, unit: 'symbols',
        omission_reasons: [] },
      freshness: 'fresh', watermark: 'partial-result', error_code: null, message: null,
      page: { offset: 0, limit: 50, total: 0, next_offset: null, rows: [], metadata: {} },
    },
  };
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    if (init?.method === 'POST' && String(input).endsWith('/api/explorer/queries')) {
      return new Response(`data: ${JSON.stringify(source)}\n\n`, {
        status: 200, headers: { 'content-type': 'text/event-stream' },
      });
    }
    return jsonResponse(404, { error: 'not found' });
  }));
  const view = renderHook(() => useExplorerController(), { wrapper });
  try {
    await act(async () => {
      view.result.current.submit('graph');
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(view.result.current.run?.sources[0]?.watermark).toBe('partial-result');
    expect(view.result.current.transport).toMatchObject({
      state: 'error',
      detail: 'Explorer stream ended without a final frame',
    });
    expect(view.result.current.absence.confirmed).toBe(false);
  } finally {
    view.unmount();
  }
});

it('keeps HTTP authorization refusal distinct from a source failure', async () => {
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) =>
    init?.method === 'POST' && String(input).endsWith('/api/explorer/queries')
      ? jsonResponse(403, { status: 'denied' })
      : jsonResponse(404, { error: 'not found' }),
  ));
  const view = renderHook(() => useExplorerController(), { wrapper });
  try {
    await act(async () => {
      view.result.current.submit('forbidden');
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(view.result.current.transport).toMatchObject({ state: 'denied', detail: 'HTTP 403' });
    expect(view.result.current.lanes.map((lane) => lane.state)).toEqual(['denied', 'denied', 'denied']);
  } finally {
    view.unmount();
  }
});

it('preserves a typed registry-unavailable refusal before any source frame', async () => {
  const registry = {
    schema_revision: 1,
    scope: { project_id: 'proj_active', storage_mode: 'profile_sharded', store_root: '/data/active' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 1 },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: { completeness: 'unknown', eligible: null, examined: null, matched: null,
      excluded: null, omitted: null, unknown: null, denominator: null, unit: null,
      omission_reasons: ['missing_registry'] },
    freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
    domain_state: 'unknown',
    legal_actions: [],
    payload: { status: 'registry_unavailable' },
  };
  const fetchImpl = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) =>
    init?.method === 'POST'
      ? jsonResponse(200, registry)
      : jsonResponse(404, { error: 'not found' }),
  );
  vi.stubGlobal('fetch', fetchImpl);
  useScope.getState().selectProject('proj_other', 'Other project', 'selected');
  const view = renderHook(() => useExplorerController(), { wrapper });
  try {
    await act(async () => {
      view.result.current.submit('fixture');
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(view.result.current.transport).toMatchObject({
      state: 'unknown',
      detail: 'missing_registry',
    });
    expect(fetchImpl.mock.calls.filter(([, init]) => init?.method === 'POST')
      .map(([input]) => String(input))).toEqual(['/api/projects/proj_other/explorer/queries']);
    expect(view.result.current.run?.sources).toEqual([]);
  } finally {
    view.unmount();
  }
});
