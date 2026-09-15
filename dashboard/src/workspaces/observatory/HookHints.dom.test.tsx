import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { HookHints } from './HookHints.tsx';

/**
 * `/api/plugins/analytics/hints` is the typed hint summary: per category, how
 * many hints the hooks emitted and what the agent did with them. The rules
 * under test are the workspace ones — a category's counts appear in their own
 * labelled cells, an unavailable hint store renders as its reason rather than
 * as an empty table, and no funnel or percentage is fabricated from tallies
 * the daemon declared independent.
 */

const NOW_MICROS = 1_753_003_600_000_000;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('Observatory hook hints', () => {
  it('renders each category as a labelled row of independent tallies', async () => {
    renderHints(
      payload([
        category('exploring-code', { emitted: 41, followed: 28, ignored: 9, suppressed: 4 }),
        category('project-memory', { emitted: 7, followed: 7, ignored: 0, suppressed: 0 }),
      ]),
    );

    expect(await screen.findByRole('rowheader', { name: 'exploring-code' })).toBeTruthy();
    const row = screen.getByRole('rowheader', { name: 'exploring-code' }).closest('tr');
    expect(row?.textContent).toContain('41');
    expect(row?.textContent).toContain('28');
    // Independent tallies stay tallies: nothing renders a rate out of them.
    expect(screen.queryByText(/%/)).toBeNull();
    expect(screen.getByText(/not a funnel/i)).toBeTruthy();
  });

  it('reports an empty window as recorded-nothing, not as a failed read', async () => {
    renderHints(payload([]));
    expect(await screen.findByText(/no hook hints have been recorded/i)).toBeTruthy();
  });

  it('prints the payload error sentence when the hint store could not be read', async () => {
    renderHints(
      { ...payload([]), available: true, error: 'hint event table missing: run doctor' },
      'partial',
    );
    expect(await screen.findByText('hint event table missing: run doctor')).toBeTruthy();
  });

  it('renders the unavailable state instead of an empty table when the source is down', async () => {
    renderHints({ ...payload([]), available: false }, 'unavailable');
    // The boundary renders the domain state; the empty-window sentence is a
    // success claim and must not appear.
    expect(await screen.findByText(/unavailable/i)).toBeTruthy();
    expect(screen.queryByText(/no hook hints have been recorded/i)).toBeNull();
  });
});

function category(
  name: string,
  counts: { emitted: number; followed: number; ignored: number; suppressed: number },
) {
  return { category: name, ...counts };
}

function payload(categories: unknown[]) {
  return { available: true, by_category: categories, error: null, source: 'durable_analytics' };
}

function envelope(payload: unknown, domainState: string) {
  return {
    schema_revision: 1,
    scope: { project_id: 'tracedecay', storage_mode: 'project', store_root: '/store' },
    version: { entity_version: null, graph_version: null },
    time: { valid_time_micros: null, observation_time_micros: NOW_MICROS },
    source_watermark: { source: 'analytics', watermark: 'analytics:4821' },
    authorization: { outcome: 'authorized' },
    coverage: {
      completeness: domainState === 'ready' ? 'complete' : 'partial',
      eligible: 2,
      examined: 2,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: 2,
      unit: 'hint_categories',
      omission_reasons: [],
    },
    freshness: { state: 'fresh', observed_at_micros: NOW_MICROS, watermark: 'analytics:4821' },
    domain_state: domainState,
    legal_actions: [],
    payload,
  };
}

function renderHints(body: unknown, domainState = 'ready') {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(JSON.stringify(envelope(body, domainState)), { status: 200 }),
    ),
  );
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={client}>
      <HookHints />
    </QueryClientProvider>,
  );
}
