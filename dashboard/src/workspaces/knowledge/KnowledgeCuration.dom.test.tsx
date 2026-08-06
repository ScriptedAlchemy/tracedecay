import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { KnowledgeCuration } from './KnowledgeCuration.tsx';

/**
 * The curation surface reads the curator's status and its current plan. Under
 * test: supersession candidates render as reviewable pairs (older fact, the
 * newer fact that may supersede it, the measured similarity), a failed plan
 * computation renders as its error and never as a clean empty plan, and a
 * store with nothing to propose says so against the counted total.
 */

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Knowledge curation panel', () => {
  it('renders supersession candidates as reviewable pairs with tier counts', async () => {
    stubRoutes({
      status: status(3, 'similarity dedup: 2 deletes applied'),
      plan: {
        actions: [candidate(11, 'Near-duplicate of #12')],
        hygiene: {
          secret_like: [],
          transient: [candidate(31, 'Transient run output')],
          supersession: [
            {
              ...candidate(7, 'Possible supersession'),
              superseded_by: 21,
              similarity: 0.8123,
              content: 'we use React for the dashboard',
            },
          ],
        },
        counts: { delete: 1 },
        total_facts: 412,
        error: '',
      },
    });
    renderPanel();

    expect(await screen.findByText(/3 apply runs recorded/)).toBeTruthy();
    expect(screen.getByText(/last: similarity dedup: 2 deletes applied/)).toBeTruthy();
    expect(screen.getByText(/possibly superseded by #21/)).toBeTruthy();
    expect(screen.getByText(/similarity 0\.8123/)).toBeTruthy();
    expect(screen.getByText('we use React for the dashboard')).toBeTruthy();
    expect(screen.getByText(/candidates for review, not decisions/i)).toBeTruthy();
  });

  it('renders a failed plan computation as its error, never a clean plan', async () => {
    stubRoutes({
      status: status(0, null),
      plan: {
        actions: [],
        hygiene: null,
        counts: {},
        total_facts: 0,
        error: 'memory store unavailable: db is locked',
      },
    });
    renderPanel();

    expect(
      await screen.findByText(/curation plan could not be computed: memory store unavailable/),
    ).toBeTruthy();
    expect(screen.queryByText(/nothing proposed/i)).toBeNull();
  });

  it('states a truthful empty plan against the counted total', async () => {
    stubRoutes({
      status: status(0, null),
      plan: {
        actions: [],
        hygiene: { secret_like: [], transient: [], supersession: [] },
        counts: {},
        total_facts: 96,
        error: '',
      },
    });
    renderPanel();

    expect(await screen.findByText(/nothing proposed across 96 facts/)).toBeTruthy();
    expect(
      screen.getByText(/the similarity curator has never applied a run/i),
    ).toBeTruthy();
  });
});

function candidate(factId: number, reason: string) {
  return {
    recommended_op: 'delete',
    fact_id: factId,
    reason,
    content: null,
    confidence: 0.7,
    review_required: true,
    status: 'candidate',
  };
}

function status(runCount: number, summary: string | null) {
  return {
    provider: 'tracedecay',
    state: {
      paused: false,
      last_run_at: null,
      run_count: runCount,
      last_run_summary: summary,
      last_run_id: null,
    },
    config: { enabled: true, mode: 'similarity_dedup' },
    snapshots: [],
  };
}

function stubRoutes(bodies: { status: unknown; plan: unknown }) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      const body = url.endsWith('/curation/plan') ? bodies.plan : bodies.status;
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <KnowledgeCuration />
    </QueryClientProvider>,
  );
}
