import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { FeedbackSplit, KnowledgePage } from './KnowledgePage.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
});

/** A `MemoryFactRowV1` as `fact_summary_json` emits it: the summary never
 * attaches entities, and the counters are real columns rather than absences. */
function fact(over: Record<string, unknown>) {
  return {
    fact_id: 0,
    trust_score: 0,
    retrieval_count: 0,
    access_count: 0,
    helpful_count: 0,
    unhelpful_count: 0,
    created_at: 1_784_000_000,
    updated_at: 1_784_000_000,
    last_recalled_at: null,
    has_hrr: 1,
    content: null,
    category: null,
    tags: null,
    entities: null,
    ...over,
  };
}

/** `memory_api::overview` seeds the whole holographic block before it reads
 * anything, so `reads` and `facts_coverage` are always present — a body without
 * them is one the route cannot produce. */
function memoryOverview(
  holographic: Record<string, unknown>,
  over: Record<string, unknown> = {},
) {
  return {
    query: '',
    limit: 100,
    providers: {},
    holographic: {
      path: '/fast/projects/tracedecay/.tracedecay/memory.db',
      exists: true,
      error: '',
      facts: [],
      entities: [],
      graph: { nodes: [], edges: [] },
      reads: {
        facts: { state: 'ready' },
        entities: { state: 'ready' },
        graph: { state: 'ready' },
      },
      facts_coverage: { completeness: 'bounded', limit: 100, query_applied_after_limit: false },
      overview: null,
      ...holographic,
    },
    ...over,
  };
}

function memorySummary(over: Record<string, unknown> = {}) {
  return {
    facts: 0,
    entities: 0,
    banks: 0,
    categories: [],
    entity_types: [],
    hrr_coverage: [],
    memory_banks: [],
    trust_histogram: [],
    growth: [],
    ...over,
  };
}

/** `memory_api::status` reports a store that exists but holds nothing. */
function memoryStatus(memory: Record<string, unknown> = {}) {
  return {
    path: '/fast/projects/tracedecay/.tracedecay/memory.db',
    exists: true,
    error: '',
    largest_bank_fact_count: 0,
    largest_bank_utilization_pct: 0,
    feedback_history_repair: { state: 'not_required', processed: 0, remaining: null },
    memory: {
      algebra_name: 'amari_fhrr',
      bank_count: 0,
      hrr_dim: 2048,
      entity_count: 0,
      estimated_capacity: 354_304,
      fact_count: 0,
      below_default_recall_threshold_count: 0,
      missing_vector_count: 0,
      repair: { banks_rebuilt: 0, missing_vectors_repaired: 0 },
      trust_0_025_count: 0,
      trust_025_050_count: 0,
      trust_050_075_count: 0,
      trust_075_100_count: 0,
      helpful_count: 0,
      unhelpful_count: 0,
      feedback_funnel: {
        access_count_total: 0,
        feedback_total: 0,
        rated_fact_count: 0,
        retrieval_count_total: 0,
        retrieved_fact_count: 0,
        seen_to_feedback_ratio: 0,
      },
      ...memory,
    },
  };
}

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
            error: '',
            fact: fact({
              fact_id: 7,
              trust_score: 0.8,
              content: 'full authoritative fact detail',
              entities: [
                {
                  entity_id: 2,
                  name: 'FactDetail',
                  entity_type: 'type',
                  aliases: [],
                  created_at: 1_784_000_000,
                  fact_count: 1,
                },
              ],
            }),
          });
        }
        if (url.includes('/api/plugins/holographic/status')) {
          return jsonResponse(memoryStatus({ fact_count: 1, trust_075_100_count: 1 }));
        }
        return jsonResponse(
          memoryOverview({
            overview: memorySummary({
              facts: 1,
              entities: 1,
              trust_histogram: [{ bucket: 8, label: '0.8–0.9', count: 1 }],
            }),
            facts: [fact({ fact_id: 7, trust_score: 0.8, content: 'list-truncated fact…' })],
          }),
        );
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

  it('distinguishes unreported feedback counts from a reported zero', () => {
    const unknown = render(<FeedbackSplit helpful={null} unhelpful={null} />);
    expect(screen.getByText('feedback counts not reported')).toBeTruthy();
    expect(screen.queryByText('no feedback recorded')).toBeNull();

    unknown.unmount();
    render(<FeedbackSplit helpful={0} unhelpful={0} />);
    expect(screen.getByText('no feedback recorded')).toBeTruthy();
    expect(screen.queryByText('feedback counts not reported')).toBeNull();
  });

  it('does not render a failed fact sub-read as an empty memory store', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/status')) return jsonResponse(memoryStatus());
        return jsonResponse(
          memoryOverview({
            overview: memorySummary({ facts: 42 }),
            reads: {
              facts: { state: 'error', error: 'facts query failed' },
              entities: { state: 'ready' },
              graph: { state: 'ready' },
            },
          }),
        );
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
        if (url.includes('/status')) return jsonResponse(memoryStatus());
        return jsonResponse(
          memoryOverview(
            {
              overview: memorySummary({ facts: 420 }),
              facts_coverage: {
                completeness: 'bounded',
                limit: 100,
                query_applied_after_limit: true,
              },
            },
            { query: url.includes('q=needle') ? 'needle' : '' },
          ),
        );
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
