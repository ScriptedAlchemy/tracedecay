import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type {
  MemoryFactRowV1,
  MemoryGraphPayloadV1,
  MemoryHolographicPayloadV1,
  MemoryOverviewPayloadV1,
  MemoryOverviewSummaryV1,
} from '../../contracts/generated.ts';
import { useScope } from '../../data/scope/store.ts';
import { FeedbackSplit, KnowledgePage } from './KnowledgePage.tsx';

afterEach(() => {
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
});

/** A `MemoryFactRowV1` as `fact_summary_json` emits it: the summary never
 * attaches entities, and the counters are real columns rather than absences. */
function fact(over: Partial<MemoryFactRowV1>): MemoryFactRowV1 {
  return {
    fact_id: 'fact-project-default',
    payload_access: 'eligible',
    trust_score: 0,
    retrieval_count: 0,
    access_count: 0,
    helpful_count: 0,
    unhelpful_count: 0,
    created_at: 1_784_000_000_000_000,
    updated_at: 1_784_000_000_000_000,
    last_recalled_at: null,
    projected_as_of: 1_784_000_000_000_000,
    content: 'fixture fact',
    category: 'general',
    tags: [],
    entities: [],
    metadata: {},
    source_label: null,
    linked_entities: null,
    ...over,
  };
}

function memoryGraph(facts: readonly MemoryFactRowV1[]): MemoryGraphPayloadV1 {
  const graphRoots = facts.filter(
    (fact) => fact.payload_access === 'eligible',
  );
  const unavailableFactCandidates = facts.length - graphRoots.length;
  const coverage: MemoryGraphPayloadV1['coverage'] =
    unavailableFactCandidates === 0
      ? {
          completeness: 'complete',
          eligible: graphRoots.length,
          examined: graphRoots.length,
          matched: graphRoots.length,
          excluded: 0,
          omitted: 0,
          unknown: 0,
          denominator: graphRoots.length,
          unit: 'memory_graph_roots',
          omission_reasons: [],
        }
      : {
          completeness: 'unknown',
          eligible: null,
          examined: null,
          matched: null,
          excluded: null,
          omitted: null,
          unknown: null,
          denominator: null,
          unit: null,
          omission_reasons: ['unavailable_fact_roots'],
        };
  return {
    nodes: graphRoots.map((fact) => ({
      id: `fact:${fact.fact_id}`,
      kind: 'fact',
      label: fact.content === null ? fact.fact_id : fact.content,
      fact_id: fact.fact_id,
      payload_access: fact.payload_access,
      projected_as_of: fact.projected_as_of,
      content: fact.content,
      category: fact.category,
      trust_score: fact.trust_score,
      retrieval_count: fact.retrieval_count,
      helpful_count: fact.helpful_count,
    })),
    edges: [],
    coverage,
    fact_universe_count: facts.length,
    fact_candidates_examined: facts.length,
    unavailable_fact_candidates: unavailableFactCandidates,
    root_count: graphRoots.length,
    relation_limit: 100,
    relation_count: 0,
  };
}

/** `memory_api::overview` seeds the whole holographic block before it reads
 * anything, so `reads` and `facts_coverage` are always present — a body without
 * them is one the route cannot produce. */
function memoryOverview(
  holographic: Partial<MemoryHolographicPayloadV1>,
  over: Partial<MemoryOverviewPayloadV1> = {},
): MemoryOverviewPayloadV1 {
  const facts = holographic.facts ?? [];
  return {
    query: '',
    limit: 100,
    providers: {},
    holographic: {
      path: '/fast/projects/tracedecay/.tracedecay/memory.db',
      exists: true,
      error: '',
      facts,
      entities: [],
      graph: memoryGraph(facts),
      reads: {
        facts: { state: 'ready' },
        entities: { state: 'ready' },
        graph: { state: 'ready' },
      },
      facts_coverage: { completeness: 'partial', limit: 100 },
      overview: null,
      ...holographic,
    },
    ...over,
  };
}

function memorySummary(
  over: Partial<MemoryOverviewSummaryV1> = {},
): MemoryOverviewSummaryV1 {
  return {
    facts: 0,
    entities: 0,
    categories: [],
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
    memory: {
      algebra: {
        name: 'amari_fhrr',
        hrr_dim: 2048,
        estimated_capacity: 354_304,
      },
      entity_count: 0,
      fact_count: 0,
      below_default_recall_threshold_count: 0,
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
        if (url.includes('/api/plugins/holographic/fact/fact-project-7')) {
          return jsonResponse(envelope({
            error: '',
            fact: fact({
              fact_id: 'fact-project-7',
              trust_score: 0.8,
              content: 'full authoritative fact detail',
              linked_entities: [
                {
                  entity_id: 'entity-project-2',
                  name: 'FactDetail',
                  fact_count: 1,
                },
              ],
            }),
          }));
        }
        if (url.includes('/api/plugins/holographic/status')) {
          return jsonResponse(envelope(memoryStatus({ fact_count: 1, trust_075_100_count: 1 })));
        }
        return jsonResponse(
          envelope(memoryOverview({
            overview: memorySummary({
              facts: 1,
              entities: 1,
              trust_histogram: [{ bucket: 8, label: '0.8–0.9', count: 1 }],
            }),
            facts: [
              fact({
                fact_id: 'fact-project-7',
                trust_score: 0.8,
                content: 'list-truncated fact…',
              }),
            ],
          })),
        );
      }),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, gcTime: 0 } },
    });
    render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <KnowledgePage />
        </MemoryRouter>
      </QueryClientProvider>,
    );

    await userEvent.click(await screen.findByText('list-truncated fact…'));

    expect(await screen.findByText('full authoritative fact detail')).toBeTruthy();
    expect(screen.getByText('amari_fhrr')).toBeTruthy();
    expect(screen.getByText(/2,048 dimensions/)).toBeTruthy();
    expect(
      calls.some((url) => url.includes('/api/plugins/holographic/fact/fact-project-7')),
    ).toBe(true);
  });

  it("drops project A's selected fact immediately when scope changes to project B", async () => {
    const pendingProjectB = new Promise<Response>(() => {});
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/api/projects/project-b/')) return pendingProjectB;
        if (url.includes('/fact/fact-project-a')) {
          return jsonResponse(envelope({
            error: '',
            fact: fact({
              fact_id: 'fact-project-a',
              content: 'canonical project A detail',
            }),
          }));
        }
        if (url.includes('/status')) return jsonResponse(envelope(memoryStatus()));
        return jsonResponse(envelope(memoryOverview({
          overview: memorySummary({ facts: 1 }),
          facts: [fact({ fact_id: 'fact-project-a', content: 'project A summary' })],
        })));
      }),
    );
    useScope.getState().selectProject('project-a', 'Project A', 'active');
    renderKnowledge();

    await userEvent.click(await screen.findByText('project A summary'));
    expect(await screen.findByText('canonical project A detail')).toBeTruthy();

    act(() => {
      useScope.getState().selectProject('project-b', 'Project B', 'active');
    });
    expect(screen.queryByText('canonical project A detail')).toBeNull();
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

  it('separates loaded facts from the subset with a trust measurement', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/status')) {
          return jsonResponse(envelope(memoryStatus({ fact_count: 2, trust_075_100_count: 1 })));
        }
        return jsonResponse(
          envelope(
            memoryOverview({
              overview: memorySummary({ facts: 2 }),
              facts: [
                fact({ fact_id: 'fact-project-eligible', trust_score: 0.8 }),
                fact({
                  fact_id: 'fact-project-redacted',
                  payload_access: 'redacted',
                  trust_score: null,
                  retrieval_count: null,
                  access_count: null,
                  helpful_count: null,
                  unhelpful_count: null,
                  created_at: null,
                  updated_at: null,
                  last_recalled_at: null,
                  content: null,
                  category: null,
                  tags: null,
                  entities: null,
                  metadata: null,
                }),
              ],
            }),
          ),
        );
      }),
    );
    renderKnowledge();

    expect(await screen.findByText(/2 facts loaded · 1 with trust · 1 unavailable/)).toBeTruthy();
  });

  it('does not render a failed fact sub-read as an empty memory store', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/status')) return jsonResponse(envelope(memoryStatus()));
        return jsonResponse(
          envelope(memoryOverview({
            overview: memorySummary({ facts: 42 }),
            reads: {
              facts: { state: 'error', error: 'facts query failed' },
              entities: { state: 'ready' },
              graph: { state: 'ready' },
            },
          })),
        );
      }),
    );
    renderKnowledge();

    expect(await screen.findByText(/fact list read is error/i)).toBeTruthy();
    expect(screen.queryByText(/no facts recorded/i)).toBeNull();
  });

  it.each([
    ['unknown', 'unknown'],
    ['cancelled', 'cancelled'],
    ['timed_out', 'timed out'],
    ['offline', 'offline'],
  ] as const)(
    'does not render a %s fact sub-read as an empty memory store',
    async (state, label) => {
      vi.stubGlobal(
        'fetch',
        vi.fn(async (input: RequestInfo | URL) => {
          const url = String(input);
          if (url.includes('/status')) return jsonResponse(envelope(memoryStatus()));
          return jsonResponse(
            envelope(memoryOverview({
              overview: memorySummary({ facts: 42 }),
              reads: {
                facts: { state, error: 'the bounded read did not finish' },
                entities: { state: 'ready' },
                graph: { state: 'ready' },
              },
            })),
          );
        }),
      );
      renderKnowledge();

      expect(await screen.findByText(new RegExp(`fact list read is ${label}`, 'i'))).toBeTruthy();
      expect(screen.queryByText(/no facts recorded/i)).toBeNull();
    },
  );

  it('states partial fact coverage instead of calling an incomplete result empty', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/status')) return jsonResponse(envelope(memoryStatus()));
        return jsonResponse(
          envelope(memoryOverview({
            overview: memorySummary({ facts: 42 }),
            facts_coverage: { completeness: 'partial', limit: 100 },
            reads: {
              facts: { state: 'partial', code: 'fact_coverage_incomplete' },
              entities: { state: 'ready' },
              graph: { state: 'ready' },
            },
          })),
        );
      }),
    );
    renderKnowledge();

    expect(await screen.findByText(/fact coverage is partial/i)).toBeTruthy();
    expect(screen.queryByText(/no facts recorded/i)).toBeNull();
  });

  it('does not call a partial fact read empty when its coverage field says complete', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/status')) return jsonResponse(envelope(memoryStatus()));
        return jsonResponse(
          envelope(memoryOverview({
            overview: memorySummary({ facts: 42 }),
            facts_coverage: { completeness: 'complete', limit: 100 },
            reads: {
              facts: { state: 'partial', code: 'fact_coverage_incomplete' },
              entities: { state: 'ready' },
              graph: { state: 'ready' },
            },
          })),
        );
      }),
    );
    renderKnowledge();

    expect(await screen.findByText(/fact read is partial/i)).toBeTruthy();
    expect(screen.queryByText(/no facts recorded/i)).toBeNull();
  });

  it('surfaces graph reset and graph coverage instead of implying complete topology', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/status')) return jsonResponse(envelope(memoryStatus()));
        const reading = memoryOverview({
          overview: memorySummary(),
          reads: {
            facts: { state: 'ready' },
            entities: { state: 'ready' },
            graph: {
              state: 'error',
              code: 'graph_reset_required',
              error: 'the graph schema changed',
            },
          },
        });
        reading.holographic.graph.coverage = {
          completeness: 'unknown',
          eligible: null,
          examined: null,
          matched: null,
          excluded: null,
          omitted: null,
          unknown: null,
          denominator: null,
          unit: null,
          omission_reasons: ['graph_reset_required'],
        };
        return jsonResponse(envelope(reading));
      }),
    );
    renderKnowledge();

    expect(await screen.findByText(/memory graph reset required: the graph schema changed/i)).toBeTruthy();
    expect(screen.getByText(/memory graph coverage is unknown/i)).toBeTruthy();
  });

  it('accepts a complete zero-finding graph read as complete coverage', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/status')) return jsonResponse(envelope(memoryStatus()));
        return jsonResponse(
          envelope(
            memoryOverview({
              overview: memorySummary(),
              facts_coverage: { completeness: 'complete', limit: 100 },
              reads: {
                facts: { state: 'complete_zero_findings' },
                entities: { state: 'complete_zero_findings' },
                graph: { state: 'complete_zero_findings' },
              },
            }),
          ),
        );
      }),
    );
    renderKnowledge();

    expect(await screen.findByText(/no facts recorded/i)).toBeTruthy();
    expect(screen.queryByText(/memory graph coverage is/i)).toBeNull();
  });

  it('reports a canonical query with no matching facts', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url.includes('/status')) return jsonResponse(envelope(memoryStatus()));
        return jsonResponse(
          envelope(memoryOverview(
            {
              overview: memorySummary({ facts: 420 }),
              facts_coverage: {
                completeness: 'partial',
                limit: 100,
              },
            },
            { query: url.includes('q=needle') ? 'needle' : '' },
          )),
        );
      }),
    );
    renderKnowledge();

    const input = await screen.findByLabelText('Search facts');
    await userEvent.type(input, 'needle');
    await userEvent.keyboard('{Enter}');

    expect(await screen.findByText(/no loaded facts match “needle”/i)).toBeTruthy();
    expect(screen.getByText(/fact coverage is partial/i)).toBeTruthy();
    expect(screen.queryByText(/loaded top-100 slice/i)).toBeNull();
  });
});

function renderKnowledge() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  render(
    <QueryClientProvider client={client}>
      {/* The workspace's view camera lives in the address (`?view=`), so the
        * page needs a router to read it. `MemoryRouter` rather than the real
        * one: these cases assert reads and states, not navigation. */}
      <MemoryRouter>
        <KnowledgePage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function envelope(payload: unknown) {
  return {
    schema_revision: 1,
    scope: { project_id: 'project.knowledge', storage_mode: 'profile_sharded', store_root: '/data' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: 1 },
    source_watermark: null,
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: 'complete',
      eligible: 1,
      examined: 1,
      matched: 1,
      excluded: 0,
      omitted: 0,
      unknown: 0,
      denominator: 1,
      unit: 'facts',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: 1, watermark: null },
    domain_state: 'ready',
    legal_actions: [],
    payload,
  };
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}
