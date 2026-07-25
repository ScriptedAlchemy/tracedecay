import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ExplorerPage } from './ExplorerPage.tsx';

function serve(routes: Record<string, { status: number; body: unknown }>) {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    const hit = Object.entries(routes).find(([path]) => url.includes(path));
    const { status, body } = hit?.[1] ?? { status: 404, body: { status: 'not_found' } };
    return {
      ok: status >= 200 && status < 300,
      status,
      json: async () => body,
    } as Response;
  });
}

function renderExplorer(routes: Record<string, { status: number; body: unknown }>) {
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

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('ExplorerPage', () => {
  it('preserves the trunk index summary and ranked query seeds', async () => {
    renderExplorer({
      '/api/plugins/graph/overview': {
        status: 200,
        body: {
          totals: { nodes: 123, edges: 456, files: 12 },
          top_connected: [{ name: 'graph_search', kind: 'function', degree: 8 }],
        },
      },
      '/api/plugins/holographic/status': {
        status: 200,
        body: {
          exists: true,
          memory: { fact_count: 20, entity_count: 3, bank_count: 1 },
        },
      },
      '/api/plugins/holographic/': {
        status: 200,
        body: {
          holographic: {
            facts: [],
            entities: [{ name: 'ProjectId', fact_count: 4 }],
          },
        },
      },
    });

    expect(await screen.findByRole('button', { name: /graph_search/ })).toBeTruthy();
    expect(screen.getByText('what is indexed')).toBeTruthy();
    expect(screen.getByText('123')).toBeTruthy();
    expect(screen.getByRole('button', { name: /ProjectId/ })).toBeTruthy();
  });

  it('renders the nested matches shape served by Hermes LCM search', async () => {
    renderExplorer({
      '/api/plugins/graph/search': { status: 200, body: { results: [] } },
      '/api/plugins/hermes-lcm/search': {
        status: 200,
        body: {
          matches: {
            messages: [{ content: 'session result from the real envelope' }],
            summary_nodes: [],
          },
        },
      },
      '/api/plugins/holographic/': {
        status: 200,
        body: { holographic: { facts: [], entities: [] } },
      },
    });

    const user = userEvent.setup();
    const input = screen.getByRole('textbox', { name: 'Explorer search' });
    await user.type(input, 'session result{Enter}');

    expect(await screen.findByText('session result from the real envelope')).toBeTruthy();
  });
});
