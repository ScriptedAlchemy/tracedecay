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

const SEARCH_ROUTES = {
  '/api/plugins/graph/search': {
    status: 200,
    body: {
      total: 1,
      results: [
        {
          id: 'node-1',
          name: 'graph_search',
          kind: 'function',
          file_path: 'src/dashboard/graph_service.rs',
          degree: 7,
        },
      ],
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
        messages: [
          {
            message_id: 'message-1',
            session_id: 'session-1',
            source: 'cursor',
            role: 'assistant',
            snippet: 'Using graph search',
          },
        ],
        summary_nodes: [
          {
            node_id: 'summary-1',
            session_id: 'session-2',
            summary: 'Graph route investigation',
          },
        ],
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
        facts: [
          {
            fact_id: 11,
            content: 'Graph search is bounded',
            category: 'project',
            trust_score: 0.8,
          },
        ],
      },
    },
  },
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('ExplorerPage', () => {
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
    expect(screen.getByText('Compatibility fan-out')).toBeTruthy();
    expect(
      screen.getByText(
        /No planner run, source progress, cancellation, canonical finality, or evidence packet is present/,
      ),
    ).toBeTruthy();
    expect(screen.getByText(/global store · messages fts · summaries like/)).toBeTruthy();
  });

  it('never turns a failed lane plus zero successful hits into complete-zero copy', async () => {
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/plugins/hermes-lcm/search': {
        status: 500,
        body: { error: 'LCM unavailable' },
      },
      '/api/plugins/graph/search': {
        status: 200,
        body: { total: 0, results: [] },
      },
      '/api/plugins/holographic/': {
        status: 200,
        body: { limit: 25, holographic: { exists: true, error: '', facts: [] } },
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText('Some memories did not answer')).toBeTruthy();
    expect(screen.getByText(/A zero-result claim would be unsafe/)).toBeTruthy();
    expect(screen.queryByText(/genuinely absent from/)).toBeNull();
  });

  it('treats an HTTP-200 LCM response with no mounted store as unavailable', async () => {
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/plugins/hermes-lcm/search': {
        status: 200,
        body: {
          path: null,
          storage_scope: 'project_local',
          exists: false,
          engine: 'none',
          engine_detail: { messages: 'none', summary_nodes: 'none' },
          total: { messages: 0, summary_nodes: 0 },
          matches: { messages: [], summary_nodes: [] },
        },
      },
      '/api/plugins/graph/search': {
        status: 200,
        body: { total: 0, results: [] },
      },
      '/api/plugins/holographic/': {
        status: 200,
        body: { limit: 25, holographic: { exists: true, error: '', facts: [] } },
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText('Some memories did not answer')).toBeTruthy();
    expect(screen.getByText(/Sessions.*store is not mounted/)).toBeTruthy();
    expect(screen.queryByText(/genuinely absent from/)).toBeNull();
  });

  it('does not claim complete absence when every compatibility endpoint returns zero rows', async () => {
    renderExplorer({
      '/api/plugins/graph/search': {
        status: 200,
        body: { total: 0, results: [] },
      },
      '/api/plugins/hermes-lcm/search': {
        status: 200,
        body: {
          path: '/data/sessions.db',
          storage_scope: 'project_local',
          exists: true,
          engine: 'like',
          engine_detail: { messages: 'like', summary_nodes: 'like' },
          total: { messages: 0, summary_nodes: 0 },
          matches: { messages: [], summary_nodes: [] },
        },
      },
      '/api/plugins/holographic/': {
        status: 200,
        body: { limit: 25, holographic: { exists: true, error: '', facts: [] } },
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

  it('shows the exact payload fields behind an inspected row', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');
    await user.click(await screen.findByRole('button', { name: /graph_search/ }));

    expect(screen.getByText('Payload provenance')).toBeTruthy();
    expect(screen.getByText('name')).toBeTruthy();
    expect(screen.getByText('file_path')).toBeTruthy();
    expect(screen.getByText('degree')).toBeTruthy();
    expect(screen.getByText(/Position 1 in graph endpoint rows/)).toBeTruthy();
  });
});
