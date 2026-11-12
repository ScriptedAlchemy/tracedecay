/**
 * An Explorer search is one streamed response: each source appears when it
 * finishes, the final frame ends the search, and closing the response is the
 * only way a search is stopped. These cases drive the real controller against
 * a scripted daemon whose response bodies the test feeds frame by frame.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, fireEvent, render, renderHook, screen } from '@testing-library/react';
import { createElement, useLayoutEffect, type ReactNode } from 'react';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useScope } from '../../data/scope/store.ts';
import { useExplorerController, type ExplorerController } from './controller.ts';
import { HitInspector } from './Inspector.tsx';
import { laneHits } from './laneModel.ts';
import { streamExplorerSearch } from './searchStream.ts';

const SEARCH_PATH = '/api/explorer/queries';

function sourceFrame(
  sourceId: 'code_graph' | 'sessions' | 'knowledge',
  outcome: 'ready' | 'stale' | 'error' = 'ready',
  rows: Record<string, unknown>[] = [],
) {
  return {
    event: 'source',
    source: {
      source_id: sourceId,
      source_label: sourceId,
      outcome,
      completed_units: rows.length,
      total_units: rows.length,
      coverage: {
        completeness: 'complete',
        eligible: rows.length,
        examined: rows.length,
        matched: null,
        excluded: null,
        omitted: 0,
        unknown: 0,
        denominator: rows.length,
        unit: 'rows',
        omission_reasons: [],
      },
      freshness: 'fresh',
      watermark: null,
      error_code: outcome === 'ready' ? null : `${outcome}_code`,
      message: outcome === 'ready' ? null : `${outcome} detail`,
      page: outcome === 'ready' ? { limit: 50, total: rows.length, rows, metadata: {} } : null,
    },
  };
}

const finalFrame = (outcome: 'completed' | 'partial' = 'completed') => ({
  event: 'final',
  outcome,
  elapsed_micros: 4200,
});

function sse(frame: { event: string }): string {
  return `event: ${frame.event}\ndata: ${JSON.stringify(frame)}\n\n`;
}

/** One search response the test feeds by hand, and notices being closed. */
class OpenSearch {
  aborted = false;
  private body!: ReadableStreamDefaultController<Uint8Array>;
  private readonly encoder = new TextEncoder();
  readonly response: Response;

  constructor(readonly query: string, signal: AbortSignal) {
    this.response = new Response(
      new ReadableStream<Uint8Array>({
        start: (controller) => {
          this.body = controller;
        },
      }),
      { status: 200, headers: { 'content-type': 'text/event-stream' } },
    );
    signal.addEventListener('abort', () => {
      this.aborted = true;
      this.body.error(new DOMException('aborted', 'AbortError'));
    });
  }

  send(frame: { event: string }) {
    this.body.enqueue(this.encoder.encode(sse(frame)));
  }

  end() {
    this.body.close();
  }
}

function scriptedDaemon(reply?: () => Response | Promise<Response>) {
  const searches: OpenSearch[] = [];
  const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    if (!String(input).endsWith('/explorer/queries')) {
      return new Response(JSON.stringify({ error: 'not found' }), { status: 404 });
    }
    if (reply !== undefined) return reply();
    const body = JSON.parse(String(init?.body)) as { query: string };
    const search = new OpenSearch(body.query, init?.signal as AbortSignal);
    searches.push(search);
    return search.response;
  });
  return { fetchMock, searches };
}

function wrapper({ children }: { children: ReactNode }) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return createElement(QueryClientProvider, { client }, children);
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

async function settle() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
}

async function search(view: ReturnType<typeof mount>, query: string) {
  await act(async () => {
    view.result.current.setQuery(query);
  });
  await act(async () => {
    view.result.current.submit();
  });
  await settle();
}

function mount() {
  return renderHook(() => useExplorerController(), { wrapper });
}

const laneStates = (view: ReturnType<typeof mount>) =>
  new Map(view.result.current.lanes.map((lane) => [lane.lane, lane.state]));

describe('explorer search stream', () => {
  it('hides the old scope answer and selection before effects can read its session in the new scope', async () => {
    const originalScope = useScope.getState().scope;
    const { fetchMock, searches } = scriptedDaemon();
    vi.stubGlobal('fetch', fetchMock);
    useScope.getState().selectProject('first-project', 'First', 'active');
    let controller!: ExplorerController;
    const committed: Array<{
      projectId: string;
      rows: string[];
      result: ExplorerController['searchResult'];
      progress: ExplorerController['searchProgress'];
      absence: ExplorerController['absence'];
      selected: ExplorerController['selected'];
      peeked: ExplorerController['peeked'];
    }> = [];
    function Surface() {
      controller = useExplorerController();
      const scope = useScope((state) => state.scope);
      useLayoutEffect(() => {
        committed.push({
          projectId: scope.kind === 'project' ? scope.projectId : 'all',
          rows: controller.lanes.flatMap((lane) => laneHits(lane).map((hit) => hit.title)),
          result: controller.searchResult,
          progress: controller.searchProgress,
          absence: controller.absence,
          selected: controller.selected,
          peeked: controller.peeked,
        });
      });
      const session = controller.lanes.flatMap(laneHits).find((hit) => hit.lane === 'sessions');
      return <>
        <button onClick={() => controller.submit('cache')}>Search</button>
        <button onClick={() => {
          if (session) {
            controller.select(session);
            controller.peek(session);
          }
        }}>Select session</button>
        {controller.selected && <HitInspector
          hit={controller.selected}
          mode="selected"
          terms={controller.terms}
          onClose={() => controller.select(null)}
        />}
      </>;
    }
    const view = render(<MemoryRouter><Surface /></MemoryRouter>, { wrapper });
    try {
      fireEvent.click(screen.getByText('Search'));
      await settle();
      await act(async () => {
        searches[0]!.send(sourceFrame('sessions', 'ready', [{
          message_id: 'message-first',
          session_id: 'session-first',
          source: 'cursor',
          role: 'assistant',
          snippet: 'First project cache',
        }]));
        searches[0]!.send(sourceFrame('code_graph'));
        searches[0]!.send(sourceFrame('knowledge'));
        searches[0]!.send(finalFrame());
      });
      await settle();
      expect(controller.absence).toEqual({ confirmed: true, quality: 'measured' });
      expect(controller.searchProgress).toEqual({ concluded: 3, total: 3 });
      fireEvent.click(screen.getByText('Select session'));
      await settle();
      expect(controller.selected?.raw.session_id).toBe('session-first');
      expect(fetchMock.mock.calls.some(([url]) =>
        String(url).includes('/api/projects/first-project/explorer/sessions/session-first/'))).toBe(true);

      await act(async () => {
        useScope.getState().selectProject('second-project', 'Second', 'active');
      });
      await settle();

      const firstCommit = committed.find((value) => value.projectId === 'second-project')!;
      expect(firstCommit.rows).toEqual([]);
      expect(firstCommit.result).toBeUndefined();
      expect(firstCommit.progress).toBeNull();
      expect(firstCommit.absence).toEqual({
        confirmed: false,
        quality: 'unknown',
        reason: 'no search has answered for this query',
      });
      expect(firstCommit.selected).toBeNull();
      expect(firstCommit.peeked).toBeNull();
      expect(fetchMock.mock.calls.some(([url]) =>
        String(url).includes('/api/projects/second-project/explorer/sessions/session-first/'))).toBe(false);
      expect(searches.map((open) => open.aborted)).toEqual([true, false]);
      expect(screen.queryByText('Session context')).toBeNull();
    } finally {
      view.unmount();
      useScope.setState({ scope: originalScope });
    }
  });

  it('shows a source the moment it finishes while the others are still reading', async () => {
    const { fetchMock, searches } = scriptedDaemon();
    vi.stubGlobal('fetch', fetchMock);
    const view = mount();

    await search(view, 'graph');
    expect(laneStates(view).get('code')).toBe('pending');
    expect(view.result.current.cancel).toBeDefined();

    await act(async () => {
      searches[0]!.send(sourceFrame('code_graph', 'ready', [{ id: 'n1', name: 'graph_fn' }]));
    });
    await settle();

    const code = view.result.current.lanes.find((lane) => lane.lane === 'code')!;
    expect(code.state).toBe('ready');
    expect(laneHits(code)).toHaveLength(1);
    expect(laneStates(view).get('sessions')).toBe('pending');
    expect(laneStates(view).get('knowledge')).toBe('pending');
    expect(view.result.current.searchProgress).toEqual({ concluded: 1, total: 3 });
    expect(view.result.current.search?.status).toBe('streaming');

    await act(async () => {
      searches[0]!.send(sourceFrame('sessions', 'stale'));
      searches[0]!.send(sourceFrame('knowledge', 'error'));
      searches[0]!.send(finalFrame('partial'));
      searches[0]!.end();
    });
    await settle();

    expect(laneStates(view).get('sessions')).toBe('stale');
    expect(laneStates(view).get('knowledge')).toBe('error');
    expect(view.result.current.search?.status).toBe('partial');
    expect(view.result.current.search?.elapsedMicros).toBe(4200);
    expect(view.result.current.cancel).toBeUndefined();
  });

  it('closes a superseded search and never lets it write over the newer one', async () => {
    const { fetchMock, searches } = scriptedDaemon();
    vi.stubGlobal('fetch', fetchMock);
    const view = mount();

    await search(view, 'first');
    await search(view, 'second');

    expect(searches.map((open) => open.aborted)).toEqual([true, false]);
    await settle();

    expect(laneStates(view).get('code')).toBe('pending');
    await act(async () => {
      searches[1]!.send(sourceFrame('code_graph', 'ready', [{ id: 'new', name: 'second_hit' }]));
    });
    await settle();
    const code = view.result.current.lanes.find((lane) => lane.lane === 'code')!;
    expect(laneHits(code).map((hit) => hit.title)).toEqual(['second_hit']);
    expect(view.result.current.search?.query).toBe('second');
  });

  it('discards a superseded response that resolves after the newer source has been published', async () => {
    let release!: (response: Response) => void;
    const delayed = new Promise<Response>((resolve) => { release = resolve; });
    const current = scriptedDaemon();
    const fetchMock = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      if (String(input).includes(SEARCH_PATH) && JSON.parse(String(init?.body)).query === 'first') {
        return delayed;
      }
      return current.fetchMock(input, init);
    });
    vi.stubGlobal('fetch', fetchMock);
    const view = mount();

    await search(view, 'first');
    const oldSignal = fetchMock.mock.calls.find(([, init]) =>
      String(init?.body).includes('"first"'))![1]!.signal!;
    await search(view, 'second');
    expect(oldSignal.aborted).toBe(true);
    await act(async () => {
      current.searches[0]!.send(sourceFrame('code_graph', 'ready', [{ id: 'new', name: 'second_hit' }]));
    });
    await settle();

    let cancelled = false;
    await act(async () => {
      release(new Response(new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode(
            sse(sourceFrame('code_graph', 'ready', [{ id: 'old', name: 'first_hit' }])) +
            sse(finalFrame()),
          ));
        },
        cancel() { cancelled = true; },
      }), { headers: { 'content-type': 'text/event-stream' } }));
    });
    await settle();

    expect(cancelled).toBe(true);
    const code = view.result.current.lanes.find((lane) => lane.lane === 'code')!;
    expect(laneHits(code).map((hit) => hit.title)).toEqual(['second_hit']);
    expect(view.result.current.search?.query).toBe('second');
    expect(view.result.current.search?.status).toBe('streaming');
    expect(view.result.current.searchProgress).toEqual({ concluded: 1, total: 3 });
  });

  it('cancel closes the response and settles every unpublished lane as cancelled', async () => {
    const { fetchMock, searches } = scriptedDaemon();
    vi.stubGlobal('fetch', fetchMock);
    const view = mount();

    await search(view, 'graph');
    await act(async () => {
      searches[0]!.send(sourceFrame('code_graph', 'ready', [{ id: 'n1', name: 'graph_fn' }]));
    });
    await settle();
    await act(async () => {
      view.result.current.cancel?.();
    });
    await settle();

    expect(searches[0]!.aborted).toBe(true);
    const states = laneStates(view);
    expect(states.get('code')).toBe('ready');
    expect(states.get('sessions')).toBe('cancelled');
    expect(states.get('knowledge')).toBe('cancelled');
    expect(view.result.current.anyPending).toBe(false);
    expect(view.result.current.cancel).toBeUndefined();
  });

  it('closes the response when the surface unmounts or the search is reset', async () => {
    const { fetchMock, searches } = scriptedDaemon();
    vi.stubGlobal('fetch', fetchMock);
    const view = mount();

    await search(view, 'graph');
    await act(async () => {
      view.result.current.reset();
    });
    await search(view, 'again');
    expect(searches.map((open) => open.aborted)).toEqual([true, false]);

    view.unmount();
    expect(searches[1]!.aborted).toBe(true);
  });

  it('keeps answered lanes and marks the rest offline when the response ends early', async () => {
    const { fetchMock, searches } = scriptedDaemon();
    vi.stubGlobal('fetch', fetchMock);
    const view = mount();

    await search(view, 'graph');
    await act(async () => {
      searches[0]!.send(sourceFrame('code_graph', 'ready', [{ id: 'n1', name: 'graph_fn' }]));
      searches[0]!.end();
    });
    await settle();

    const states = laneStates(view);
    expect(states.get('code')).toBe('ready');
    expect(states.get('sessions')).toBe('offline');
    expect(states.get('knowledge')).toBe('offline');
    expect(view.result.current.search?.status).toBe('interrupted');
  });

  it('renders a standing refusal and an unreachable daemon as distinct lane states', async () => {
    const refused = scriptedDaemon(() => new Response('{}', { status: 403 }));
    vi.stubGlobal('fetch', refused.fetchMock);
    const denied = mount();
    await search(denied, 'graph');
    expect(laneStates(denied).get('code')).toBe('denied');
    expect(denied.result.current.anyPending).toBe(false);

    const unreachable = scriptedDaemon(() => {
      throw new TypeError('Failed to fetch');
    });
    vi.stubGlobal('fetch', unreachable.fetchMock);
    const offline = mount();
    await search(offline, 'graph');
    expect(laneStates(offline).get('code')).toBe('offline');
    expect(laneStates(offline).get('semantic')).toBe('unregistered');
  });

  it('refuses a successful response with the wrong media type or mismatched event tag', async () => {
    for (const response of [
      new Response(sse(finalFrame()), { headers: { 'content-type': 'application/json' } }),
      new Response(`event: source\ndata: ${JSON.stringify(finalFrame())}\n\n`, {
        headers: { 'content-type': 'text/event-stream' },
      }),
    ]) {
      vi.stubGlobal('fetch', vi.fn(async () => response));
      const onEvent = vi.fn();
      const result = await streamExplorerSearch(
        SEARCH_PATH, { query: 'cache', limit: 5 }, new AbortController().signal, onEvent,
      );
      expect(result).toEqual({ outcome: 'transport', state: 'unsupported_schema' });
      expect(onEvent).not.toHaveBeenCalled();
    }
  });
});
