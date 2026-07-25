import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ExplorerPage } from './ExplorerPage.tsx';

type Route = { status: number; body: unknown };

function serve(routes: Record<string, Route>) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    const hit = Object.entries(routes).find(([path]) => url.includes(path));
    const response = hit?.[1] ?? { status: 404, body: { error: 'not found' } };
    return {
      ok: response.status >= 200 && response.status < 300,
      status: response.status,
      json: async () => response.body,
    } as Response;
  });
}

function renderExplorer(routes: Record<string, Route>) {
  vi.stubGlobal('fetch', serve(routes));
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ExplorerPage />
    </QueryClientProvider>,
  );
}

const CODE_ROW = {
  id: 'node-1',
  name: 'graph_search',
  kind: 'function',
  file_path: 'src/dashboard/graph_service.rs',
  degree: 7,
};

const MESSAGE_ROW = {
  message_id: 'message-1',
  session_id: 'session-1',
  source: 'cursor',
  role: 'assistant',
  snippet: 'Using graph search',
};

const SUMMARY_ROW = {
  node_id: 'summary-1',
  session_id: 'session-2',
  summary: 'Graph route investigation',
};

const FACT_ROW = {
  fact_id: 11,
  content: 'Graph search is bounded',
  category: 'project',
  trust_score: 0.8,
};

function source(
  sourceId: 'code_graph' | 'sessions' | 'knowledge',
  rows: Record<string, unknown>[],
  total: number | null,
) {
  return {
    source_id: sourceId,
    source_label:
      sourceId === 'code_graph'
        ? 'Code graph'
        : sourceId === 'sessions'
          ? 'Sessions'
          : 'Knowledge',
    phase: 'completed',
    outcome: 'ready',
    completed_units: rows.length,
    total_units: total,
    coverage: {
      completeness: total === null ? 'unknown' : 'complete',
      eligible: total,
      examined: rows.length,
      matched: total,
      excluded: total === null ? null : 0,
      omitted: total === null ? null : 0,
      unknown: total === null ? null : 0,
      denominator: total,
      unit: sourceId === 'code_graph' ? 'symbols' : 'rows',
      omission_reasons:
        total === null ? ['matching fact total is not exposed'] : [],
    },
    freshness: 'unknown',
    watermark: null,
    error_code: null,
    message: null,
    page: {
      offset: 0,
      limit: 50,
      total,
      next_offset: null,
      rows,
      metadata: {},
    },
  };
}

function plannerEnvelope(
  sources: unknown[] = [
    source('code_graph', [CODE_ROW], 1),
    source('sessions', [MESSAGE_ROW, SUMMARY_ROW], 2),
    source('knowledge', [FACT_ROW], null),
  ],
  state: 'partial' | 'completed' = 'partial',
  query = 'graph',
) {
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
      eligible: 3,
      examined: state === 'completed' ? 3 : 2,
      matched: null,
      excluded: null,
      omitted: state === 'completed' ? 0 : 1,
      unknown: null,
      denominator: 3,
      unit: 'sources',
      omission_reasons: state === 'completed' ? [] : ['knowledge coverage is unknown'],
    },
    freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
    domain_state: state === 'completed' ? 'ready' : 'partial',
    legal_actions: [],
    payload: {
      run_id: 'explorer-run-fixture',
      request: { query, limit: 50, offset: 0 },
      request_revision: 'explorer-query-request-v1',
      plan_revision: 'explorer-query-plan-v1',
      merge_revision: 'source-local-no-merge-v1',
      required_source_ids: ['code_graph', 'sessions', 'knowledge'],
      ordering_policy: 'source_local_no_cross_source_merge',
      explanation:
        'Search the code graph, active-project session store, and bounded project fact authority in parallel; preserve each source own order and coverage.',
      submitted_at_micros: 1,
      completed_at_micros: 10,
      elapsed_micros: 9,
      state,
      finality: state === 'completed' ? 'complete' : 'partial',
      sources,
    },
  };
}

const SEARCH_ROUTES = {
  '/api/explorer/sessions/session-1/size': {
    status: 200,
    body: {
      ...plannerEnvelope(),
      domain_state: 'ready',
      payload: {
        session_id: 'session-1',
        storage_scope: 'profile_sharded',
        counts: {
          message_count: 4,
          summary_node_count: 1,
          token_estimate_total: 120,
          summary_token_count: 30,
          source_token_count: 90,
        },
      },
    },
  },
  '/api/explorer/sessions/session-1/read-context': {
    status: 200,
    body: {
      ...plannerEnvelope(),
      payload: {
        session_id: 'session-1',
        storage_scope: 'profile_sharded',
        limit: 25,
        offset: 0,
        order: 'asc',
        counts: {
          message_count: 4,
          summary_node_count: 1,
          token_estimate_total: 120,
          summary_token_count: 30,
          source_token_count: 90,
        },
        messages: [MESSAGE_ROW],
        summary_nodes: [SUMMARY_ROW],
        has_more: true,
        has_more_messages: true,
        has_more_summary_nodes: false,
      },
    },
  },
  '/api/explorer/queries': {
    status: 200,
    body: plannerEnvelope(),
  },
  '/api/plugins/graph/overview': {
    status: 200,
    body: { top_connected: [CODE_ROW] },
  },
  '/api/plugins/hermes-lcm/overview': {
    status: 200,
    body: { latest_summary_nodes: [SUMMARY_ROW], overview: { messages_total: 1 } },
  },
  '/api/plugins/graph/search': {
    status: 200,
    body: {
      total: 1,
      results: [CODE_ROW],
    },
  },
  '/api/plugins/hermes-lcm/search': {
    status: 200,
    body: {
      path: '/data/sessions.db',
      storage_scope: 'global',
      exists: true,
      engine: 'like',
      engine_detail: { messages: 'fts', summary_nodes: 'like' },
      total: { messages: 1, summary_nodes: 1 },
      matches: {
        messages: [MESSAGE_ROW],
        summary_nodes: [SUMMARY_ROW],
      },
    },
  },
  '/api/plugins/holographic/': {
    status: 200,
    body: {
      limit: 25,
      holographic: {
        path: '/data/memory.db',
        exists: true,
        error: '',
        facts: [FACT_ROW],
      },
    },
  },
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('ExplorerPage', () => {
  it('keeps every definition term and description in a valid definition-list group', async () => {
    const { container } = renderExplorer(SEARCH_ROUTES);

    expect(await screen.findByText('What each lane searches')).toBeTruthy();
    const items = [...container.querySelectorAll('dl dt, dl dd')];
    expect(items.length).toBeGreaterThan(0);
    expect(
      items.every((item) => {
        const parent = item.parentElement;
        return parent?.tagName === 'DL' || parent?.parentElement?.tagName === 'DL';
      }),
    ).toBe(true);
  });

  it('renders every result family from the real graph, LCM, and memory shapes', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');

    expect(await screen.findByRole('button', { name: /graph_search/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /Using graph search/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /Graph route investigation/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /Graph search is bounded/ })).toBeTruthy();
    expect(
      screen.getByRole('button', {
        name: /Code graph\s*1\s*loaded\s*of 1 matching rows reported/,
      }),
    ).toBeTruthy();
    expect(
      screen.getByRole('button', {
        name: /Sessions\s*2\s*loaded\s*of 2 matching rows reported/,
      }),
    ).toBeTruthy();
    expect(screen.getByText('Coordinator run')).toBeTruthy();
    expect(screen.getByText('explorer-run-fixture')).toBeTruthy();
    expect(screen.getByText('source_local_no_cross_source_merge')).toBeTruthy();
    expect(screen.getByText(/active-project session store/)).toBeTruthy();
  });

  it('never turns a failed lane plus zero successful hits into complete-zero copy', async () => {
    const unavailableSessions = {
      ...source('sessions', [], 0),
      outcome: 'unavailable',
      total_units: null,
      coverage: {
        ...source('sessions', [], 0).coverage,
        completeness: 'unknown',
        eligible: null,
        examined: null,
        denominator: null,
      },
      error_code: 'session_query_failed',
      message: 'LCM unavailable',
      page: null,
    };
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [source('code_graph', [], 0), unavailableSessions, source('knowledge', [], null)],
          'partial',
          'missing',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText('Some sources did not answer')).toBeTruthy();
    expect(screen.getByText(/A zero-result claim would be unsafe/)).toBeTruthy();
    expect(screen.queryByText(/genuinely absent from/)).toBeNull();
  });

  it('treats an HTTP-200 LCM response with no mounted store as unavailable', async () => {
    const unavailableSessions = {
      ...source('sessions', [], 0),
      outcome: 'unavailable',
      total_units: null,
      coverage: {
        ...source('sessions', [], 0).coverage,
        completeness: 'unknown',
        eligible: null,
        examined: null,
        denominator: null,
      },
      error_code: 'session_store_unavailable',
      message: 'the active-project session store is not mounted',
      page: null,
    };
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [source('code_graph', [], 0), unavailableSessions, source('knowledge', [], null)],
          'partial',
          'missing',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText('Some sources did not answer')).toBeTruthy();
    expect(screen.getByText(/session_store_unavailable/)).toBeTruthy();
    expect(screen.getByText(/session store is not mounted/)).toBeTruthy();
    expect(screen.queryByText(/genuinely absent from/)).toBeNull();
  });

  it('does not claim complete absence when every compatibility endpoint returns zero rows', async () => {
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [
            source('code_graph', [], 0),
            source('sessions', [], 0),
            source('knowledge', [], null),
          ],
          'partial',
          'missing',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText('No rows loaded for “missing”')).toBeTruthy();
    expect(
      screen.getByText(/do not report complete coverage or planner finality/),
    ).toBeTruthy();
    expect(screen.queryByText(/genuinely absent from/)).toBeNull();
  });

  it('binds LCM session size and read context into the session inspector', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');
    await user.click(await screen.findByRole('button', { name: /Using graph search/ }));

    expect(await screen.findByText('Session context')).toBeTruthy();
    expect(screen.getByText('Raw token estimate')).toBeTruthy();
    expect(screen.getAllByText('120').length).toBeGreaterThan(0);
    expect(
      screen.getByText(/Loaded 1 raw messages and 1 summary nodes in asc order; more rows remain/),
    ).toBeTruthy();
    expect(screen.getByText('Session read context returned by the daemon')).toBeTruthy();
  });

  it('shows the exact payload fields behind an inspected row', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');
    await user.click(await screen.findByRole('button', { name: /graph_search/ }));

    expect(screen.getByText('Payload provenance')).toBeTruthy();
    expect(screen.getAllByText('name').length).toBeGreaterThan(0);
    expect(screen.getByText('file_path')).toBeTruthy();
    expect(screen.getAllByText('degree').length).toBeGreaterThan(0);
    expect(
      screen.getByText(
        (_content, element) =>
          element?.tagName === 'P' &&
          element.textContent?.includes('Position 1 in graph endpoint rows') === true,
      ),
    ).toBeTruthy();
  });
});
