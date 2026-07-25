import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { KnowledgePage } from './KnowledgePage.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('KnowledgePage fact detail', () => {
  it('loads the canonical detail row instead of presenting list-truncated content as complete', async () => {
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        calls.push(url);
        if (url.includes('/api/plugins/holographic/fact/7')) {
          return jsonResponse({
            fact: {
              fact_id: 7,
              trust_score: 0.8,
              content: 'full authoritative fact detail',
              entities: [{ entity_id: 2, name: 'FactDetail', entity_type: 'type' }],
            },
            error: '',
          });
        }
        if (url.includes('/api/plugins/holographic/status')) {
          return jsonResponse({
            exists: true,
            memory: {
              fact_count: 1,
              trust_0_025_count: 0,
              trust_025_050_count: 0,
              trust_050_075_count: 0,
              trust_075_100_count: 1,
            },
          });
        }
        return jsonResponse({
          query: '',
          holographic: {
            exists: true,
            error: '',
            overview: {
              facts: 1,
              entities: 1,
              banks: 0,
              categories: [],
              hrr_coverage: [],
              trust_histogram: [
                { bucket: 8, label: '0.8–0.9', count: 1 },
              ],
              growth: [],
            },
            facts: [
              {
                fact_id: 7,
                trust_score: 0.8,
                content: 'list-truncated fact…',
              },
            ],
            entities: [],
          },
        });
      }),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, gcTime: 0 } },
    });
    render(
      <QueryClientProvider client={client}>
        <KnowledgePage />
      </QueryClientProvider>,
    );

    await userEvent.click(await screen.findByText('list-truncated fact…'));

    expect(await screen.findByText('full authoritative fact detail')).toBeTruthy();
    expect(calls.some((url) => url.includes('/api/plugins/holographic/fact/7'))).toBe(true);
  });

  it('does not render a failed fact sub-read as an empty memory store', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/status')) return jsonResponse({ exists: true, memory: {} });
        return jsonResponse({
          query: '',
          limit: 100,
          holographic: {
            exists: true,
            error: '',
            overview: { facts: 42, entities: 0 },
            facts: [],
            entities: [],
            reads: {
              facts: { state: 'error', error: 'facts query failed' },
              entities: { state: 'ready' },
              graph: { state: 'ready' },
            },
          },
        });
      }),
    );
    renderKnowledge();

    expect(await screen.findByText(/fact list read failed/i)).toBeTruthy();
    expect(screen.queryByText(/no facts recorded/i)).toBeNull();
  });

  it('labels a negative query as bounded to the loaded top-100 slice', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/status')) return jsonResponse({ exists: true, memory: {} });
        return jsonResponse({
          query: url.includes('q=needle') ? 'needle' : '',
          limit: 100,
          holographic: {
            exists: true,
            error: '',
            overview: { facts: 420, entities: 0 },
            facts: [],
            entities: [],
            reads: {
              facts: { state: 'ready' },
              entities: { state: 'ready' },
              graph: { state: 'ready' },
            },
            facts_coverage: {
              completeness: 'bounded',
              limit: 100,
              query_applied_after_limit: true,
            },
          },
        });
      }),
    );
    renderKnowledge();

    const input = await screen.findByLabelText('Search facts');
    await userEvent.type(input, 'needle');
    await userEvent.keyboard('{Enter}');

    expect(await screen.findByText(/no match in the loaded top-100 slice/i)).toBeTruthy();
    expect(screen.queryByText(/no facts match/i)).toBeNull();
  });
});

function renderKnowledge() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  render(
    <QueryClientProvider client={client}>
      <KnowledgePage />
    </QueryClientProvider>,
  );
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
