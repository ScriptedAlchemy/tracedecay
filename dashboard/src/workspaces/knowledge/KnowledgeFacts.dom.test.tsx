/**
 * The Facts camera's interaction and truth contract.
 *
 * Three verbs the plate approves, kept apart: INSPECT (hover or focus, a
 * preview of the bounded row with no fetch), SELECT (click or Enter, in the
 * address, reads the canonical detail and audit) and SEARCH/SORT (in the
 * address). And the absences it must keep typed: a withheld payload, a graph
 * sub-read that did not serve a topology, a rung of the detail ladder no
 * authority serves.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, useLocation } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type {
  MemoryFactRowV1,
  MemoryGraphPayloadV1,
  MemoryHolographicPayloadV1,
} from '../../contracts/generated.ts';
import { useScope } from '../../data/scope/store.ts';
import { useStatusRegistersStore } from '../../data/shell/statusRegisters.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { KnowledgePage } from './KnowledgePage.tsx';

// jsdom ships no 2D canvas; the trust trace's ECharts instance is inert here
// and the exact audit rows beneath it are what these cases assert.
vi.mock('../../viz/chart/echarts.ts', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../viz/chart/echarts.ts')>()),
  init: () => ({ setOption: () => {}, resize: () => {}, dispose: () => {} }),
}));

afterEach(() => {
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
  // Registers are withdrawn on unmount; cleanup runs after this hook in the
  // setup file, so the store is reset here as well to keep cases independent.
  useStatusRegistersStore.setState({ owners: new Map() });
});

function fact(over: Partial<MemoryFactRowV1> & { fact_id: string }): MemoryFactRowV1 {
  return {
    payload_access: 'eligible',
    trust_score: 0.9,
    retrieval_count: 4,
    access_count: 6,
    helpful_count: 3,
    unhelpful_count: 1,
    created_at: 1_784_000_000_000_000,
    updated_at: 1_784_100_000_000_000,
    last_recalled_at: 1_784_200_000_000_000,
    projected_as_of: 1_784_300_000_000_000,
    content: `content of ${over.fact_id}`,
    category: 'decision',
    tags: ['fixture'],
    entities: [],
    metadata: {},
    source_label: 'hook: session-ingest',
    linked_entities: null,
    ...over,
  };
}

const FACTS = [
  fact({ fact_id: 'fact-alpha', trust_score: 0.95, content: 'alpha fact content', category: 'decision' }),
  fact({ fact_id: 'fact-beta', trust_score: 0.6, content: 'beta fact content', category: 'tool', created_at: 1_785_000_000_000_000 }),
  fact({
    fact_id: 'fact-gamma',
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
    source_label: null,
  }),
];

function graph(facts: readonly MemoryFactRowV1[], over: Partial<MemoryGraphPayloadV1> = {}): MemoryGraphPayloadV1 {
  const roots = facts.filter((row) => row.payload_access === 'eligible');
  return {
    nodes: [
      ...roots.map((row) => ({
        id: `fact:${row.fact_id}`,
        kind: 'fact' as const,
        label: row.content ?? row.fact_id,
        fact_id: row.fact_id,
        payload_access: row.payload_access,
        projected_as_of: row.projected_as_of,
        content: row.content,
        category: row.category,
        trust_score: row.trust_score,
        retrieval_count: row.retrieval_count,
        helpful_count: row.helpful_count,
      })),
      { id: 'entity:Rspeedy', kind: 'entity' as const, entity_id: 'Rspeedy', label: 'Rspeedy' },
    ],
    edges: [
      { kind: 'mentions', source: 'fact:fact-alpha', target: 'entity:Rspeedy' },
      { kind: 'supports', source: 'fact:fact-alpha', target: 'fact:fact-beta' },
    ],
    coverage: {
      completeness: 'unknown',
      eligible: null,
      examined: null,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: null,
      unit: null,
      omission_reasons: ['fact_universe_bounded'],
    },
    fact_universe_count: 4128,
    fact_candidates_examined: facts.length,
    unavailable_fact_candidates: facts.length - roots.length,
    root_count: roots.length,
    relation_limit: 100,
    relation_count: 2,
    ...over,
  };
}

function overviewEnvelope(
  facts: readonly MemoryFactRowV1[],
  holographic: Partial<MemoryHolographicPayloadV1> = {},
  query = '',
) {
  return fixtureEnvelope({
    query,
    limit: 100,
    providers: {},
    holographic: {
      path: '/tmp/memory.db',
      exists: true,
      error: '',
      overview: { facts: 4128, entities: 612, categories: [{ category: 'decision', count: 2 }], trust_histogram: [], growth: [] },
      facts,
      entities: [],
      graph: graph(facts),
      facts_coverage: { completeness: 'partial', limit: 100, eligible: 4128, examined: facts.length },
      reads: {
        facts: { state: 'ready' },
        entities: { state: 'ready' },
        graph: { state: 'partial', code: 'graph_coverage_incomplete' },
      },
      ...holographic,
    },
  });
}

const STATUS = fixtureEnvelope({
  path: '/tmp/memory.db',
  exists: true,
  error: '',
  memory: {
    algebra: { name: 'amari_fhrr', hrr_dim: 2048, estimated_capacity: 354_304 },
    entity_count: 612,
    fact_count: 4128,
    below_default_recall_threshold_count: 0,
    trust_0_025_count: 0,
    trust_025_050_count: 0,
    trust_050_075_count: 1,
    trust_075_100_count: 1,
    helpful_count: 3,
    unhelpful_count: 1,
    feedback_funnel: {
      access_count_total: 6,
      feedback_total: 4,
      rated_fact_count: 2,
      retrieval_count_total: 4,
      retrieved_fact_count: 2,
      seen_to_feedback_ratio: 1,
    },
  },
});

const TRUST_HISTORY = {
  fact_id: 'fact-alpha',
  trust_history: [
    { event_id: 'e1', timestamp: 1_784_000_000_000_000, action: 'helpful', old_trust: 0.5, new_trust: 0.7, delta: 0.2, details_availability: 'available', note: 'confirmed by operator' },
    { event_id: 'e2', timestamp: 1_784_100_000_000_000, action: 'unhelpful', old_trust: 0.7, new_trust: 0.65, delta: -0.05, details_availability: 'redacted' },
    { event_id: 'e3', timestamp: 1_784_200_000_000_000, action: 'helpful', old_trust: 0.65, new_trust: 0.95, delta: 0.3, details_availability: 'available' },
  ],
  limit: 300,
  completeness: 'complete',
  next_after: null,
  error: '',
};

function stub(options: { holographic?: Partial<MemoryHolographicPayloadV1>; facts?: readonly MemoryFactRowV1[] } = {}) {
  const requested: string[] = [];
  const facts = options.facts ?? FACTS;
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      requested.push(url);
      const path = url.split('?')[0] ?? url;
      if (path.endsWith('/trust-history')) return json(TRUST_HISTORY);
      const detail = /\/fact\/([^/]+)$/.exec(path);
      if (detail) {
        const id = decodeURIComponent(detail[1]!);
        const row = facts.find((candidate) => candidate.fact_id === id);
        if (!row) return json(fixtureEnvelope(null, 'complete_zero_findings'));
        return json(
          fixtureEnvelope({
            error: '',
            fact: {
              ...row,
              content: row.content ? `${row.content}, canonical and complete` : null,
              linked_entities: [{ entity_id: 'Rspeedy', name: 'Rspeedy', fact_count: 21 }],
            },
          }),
        );
      }
      if (path.endsWith('/status')) return json(STATUS);
      const query = new URLSearchParams(url.split('?')[1] ?? '').get('q') ?? '';
      return json(overviewEnvelope(facts, options.holographic ?? {}, query));
    }),
  );
  return requested;
}

function json(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

let lastSearch = '';
function LocationProbe() {
  lastSearch = useLocation().search;
  return null;
}

function renderPage(initialEntry = '/knowledge') {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[initialEntry]}>
        <LocationProbe />
        <KnowledgePage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function ledgerRow(name: RegExp): Promise<HTMLElement> {
  return screen.findByRole('button', { name });
}

function inspector(): HTMLElement {
  return screen.getByTestId('fact-inspector');
}

function rungState(panel: HTMLElement, rung: string): string | null | undefined {
  return panel.querySelector(`[data-ladder-rung="${rung}"]`)?.getAttribute('data-ladder-state');
}

/** The registers the workspace has posted to the shell strip, keyed by their id tail. */
function publishedRegisters(): Record<string, { value: string; state: string; detail?: string | undefined }> {
  const out: Record<string, { value: string; state: string; detail?: string | undefined }> = {};
  for (const registers of useStatusRegistersStore.getState().owners.values()) {
    for (const register of registers) {
      out[register.id.split(':').at(-1) ?? register.id] = {
        value: register.value,
        state: register.state,
        detail: register.detail,
      };
    }
  }
  return out;
}

describe('Facts camera: inspect, select, and the address', () => {
  it('previews the bounded row on hover without fetching, and reads the canonical detail on select', async () => {
    const requested = stub();
    renderPage();
    const row = await ledgerRow(/alpha fact content/);

    fireEvent.pointerMove(row);
    expect(inspector().getAttribute('data-fact-id')).toBe('fact-alpha');
    expect(within(inspector()).getByText(/inspecting · hover or focus/)).toBeTruthy();
    expect(within(inspector()).getByText('alpha fact content')).toBeTruthy();
    expect(requested.some((url) => url.includes('/fact/fact-alpha'))).toBe(false);
    expect(rungState(inspector(), 'canonical_detail')).toBe('unknown');
    expect(screen.queryByTestId('store-summary')).toBeNull();

    await userEvent.click(row);
    expect(await within(inspector()).findByText('alpha fact content, canonical and complete')).toBeTruthy();
    expect(within(inspector()).getByText('selected fact')).toBeTruthy();
    expect(lastSearch).toContain('fact=fact-alpha');
    expect(requested.some((url) => url.includes('/fact/fact-alpha'))).toBe(true);
    await waitFor(() => expect(rungState(inspector(), 'canonical_detail')).toBe('ready'));
    // Canonical detail carries the entities the summary row never attaches.
    expect(within(inspector()).getByText('Rspeedy')).toBeTruthy();
    expect(within(inspector()).getByText(/21 facts/)).toBeTruthy();
  });

  it('inspects and selects from a camera row, and marks it on the field', async () => {
    stub();
    const { container } = renderPage();
    await ledgerRow(/alpha fact content/);
    const row = container.querySelector('[data-node="fact:fact-beta"]');
    expect(row).not.toBeNull();

    fireEvent.pointerMove(row!);
    expect(inspector().getAttribute('data-fact-id')).toBe('fact-beta');
    expect(row!.getAttribute('data-inspected')).toBe('true');
    // Hover lights the relation beta is wired by: alpha supports beta.
    expect(container.querySelector('[data-relation="supports"]')!.getAttribute('data-lit')).toBe('true');

    fireEvent.click(row!);
    await waitFor(() => expect(lastSearch).toContain('fact=fact-beta'));
    expect(row!.getAttribute('data-selected')).toBe('true');
    expect(await within(inspector()).findByText('selected fact')).toBeTruthy();
    // Selected, its relation lifts with the halo.
    expect(container.querySelector('[data-relation="supports"]')!.getAttribute('data-lifted')).toBe('true');
  });

  it('returns from inspection to the selection on Escape, then clears the selection', async () => {
    stub();
    renderPage();
    await userEvent.click(await ledgerRow(/alpha fact content/));
    expect(await within(inspector()).findByText('selected fact')).toBeTruthy();

    fireEvent.pointerMove(await ledgerRow(/beta fact content/));
    expect(inspector().getAttribute('data-fact-id')).toBe('fact-beta');

    await userEvent.keyboard('{Escape}');
    expect(inspector().getAttribute('data-fact-id')).toBe('fact-alpha');

    await userEvent.keyboard('{Escape}');
    expect(screen.queryByTestId('fact-inspector')).toBeNull();
    expect(screen.getByTestId('store-summary')).toBeTruthy();
    expect(lastSearch).not.toContain('fact=');
  });

  it('inspects the focused row so the keyboard path reaches what the pointer does', async () => {
    stub();
    renderPage();
    const alpha = await ledgerRow(/alpha fact content/);
    act(() => alpha.focus());
    expect(inspector().getAttribute('data-fact-id')).toBe('fact-alpha');
    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement?.textContent).toContain('beta fact content');
    expect(inspector().getAttribute('data-fact-id')).toBe('fact-beta');
    await userEvent.keyboard('{Enter}');
    await waitFor(() => expect(lastSearch).toContain('fact=fact-beta'));
  });

  it('reopens the exact position from the address: query, sort and selected fact', async () => {
    const requested = stub();
    renderPage('/knowledge?q=needle&sort=created&fact=fact-beta');
    expect(await within(await screen.findByTestId('fact-inspector')).findByText('selected fact')).toBeTruthy();
    expect(requested.some((url) => url.includes('q=needle'))).toBe(true);
    expect((screen.getByLabelText('Sort loaded facts') as HTMLSelectElement).value).toBe('created');
    // Created newest first puts beta (created later) above alpha.
    const rows = screen.getAllByRole('button', { name: /fact content/ });
    expect(rows[0]?.textContent).toContain('beta fact content');
    expect(rows[1]?.textContent).toContain('alpha fact content');
  });

  it('writes a sort change to the address and reorders only the loaded slice', async () => {
    stub();
    renderPage();
    await ledgerRow(/alpha fact content/);
    await userEvent.selectOptions(screen.getByLabelText('Sort loaded facts'), 'content');
    expect(lastSearch).toContain('sort=content');
    expect(screen.getByTestId('fact-ledger-bound').textContent).toMatch(/Sorted within this slice only/);
    expect(screen.getByTestId('fact-ledger-bound').textContent).toMatch(/4,125 more facts lie beyond it/);
    expect(screen.getByTestId('fact-ledger-bound').textContent).toMatch(/paging is unavailable rather than hidden/);
  });

  it('drops the selected fact when the scope changes, since identity is owned by the project', async () => {
    stub();
    useScope.getState().selectProject('project-a', 'Project A', 'active');
    renderPage('/knowledge?fact=fact-alpha');
    expect(await within(await screen.findByTestId('fact-inspector')).findByText('selected fact')).toBeTruthy();
    act(() => {
      useScope.getState().selectProject('project-b', 'Project B', 'active');
    });
    await waitFor(() => expect(lastSearch).not.toContain('fact='));
    expect(screen.queryByTestId('fact-inspector')).toBeNull();
  });
});

describe('Facts camera: typed absences', () => {
  it('keeps a withheld payload typed on the row and in the inspector, never reconstructing content', async () => {
    stub();
    renderPage();
    const row = await ledgerRow(/payload redacted/);
    expect(row.querySelector('[data-payload-access="redacted"]')).not.toBeNull();
    await userEvent.click(row);
    const content = await within(inspector()).findByText(/Identity fact-gamma is retained; its content is not shown/);
    expect(content.closest('[data-content-state]')?.getAttribute('data-content-state')).toBe('redacted');
    await waitFor(() => expect(rungState(inspector(), 'payload_access')).toBe('redacted'));
    expect(within(inspector()).getByText(/trust unavailable while payload access is redacted/)).toBeTruthy();
    // The redacted fact is not a graph root, and the drawing says so.
    expect(screen.getByTestId('fact-constellation-coverage').textContent).toMatch(/1 fact candidate unavailable and not drawn/);
  });

  it('never ticks a ladder rung no authority serves', async () => {
    stub();
    renderPage();
    await userEvent.click(await ledgerRow(/alpha fact content/));
    const ladder = within(inspector()).getByRole('list', { name: 'Detail availability' });
    const verification = within(ladder).getByText('source verification').closest('[data-ladder-rung]');
    expect(verification?.getAttribute('data-ladder-state')).toBe('unavailable');
    expect(verification?.textContent).toMatch(/no verification authority is served/);
    const geometry = within(ladder).getByText('geometry membership').closest('[data-ladder-rung]');
    expect(geometry?.getAttribute('data-ladder-state')).toBe('unknown');
    expect(within(inspector()).getByText(/no repository or path authority is joined to memory facts/)).toBeTruthy();
  });

  it('draws the trust trace from the real audit and keeps withheld detail as its own state', async () => {
    stub();
    renderPage();
    await userEvent.click(await ledgerRow(/alpha fact content/));
    expect(await screen.findByTestId('trust-trace')).toBeTruthy();
    const events = await screen.findByRole('region', { name: 'Trust history events' });
    expect(within(events).getByText('confirmed by operator')).toBeTruthy();
    expect(within(events).getByText(/feedback detail withheld/).closest('[data-state]')?.getAttribute('data-state')).toBe('redacted');
    await waitFor(() => expect(rungState(inspector(), 'trust_history')).toBe('ready'));
  });

  it('reports a fact the store does not hold as unavailable rather than empty', async () => {
    stub();
    renderPage('/knowledge?fact=fact-unknown');
    const panel = await screen.findByTestId('fact-inspector');
    await waitFor(() => expect(panel.querySelector('[data-content-state="missing"]')).not.toBeNull());
    const content = panel.querySelector('[data-content-state="missing"]')!;
    expect(content.textContent).toMatch(/the store holds no fact under this identity/);
    expect(content.querySelector('[data-state]')?.getAttribute('data-state')).toBe('unavailable');
    expect(rungState(panel, 'canonical_detail')).toBe('unavailable');
  });

  it('does not draw the cameras over a graph sub-read that served no topology', async () => {
    stub({
      holographic: {
        reads: {
          facts: { state: 'ready' },
          entities: { state: 'ready' },
          graph: { state: 'error', code: 'graph_reset_required', error: 'the graph schema changed' },
        },
      },
    });
    renderPage();
    await ledgerRow(/alpha fact content/);
    expect(screen.queryByTestId('fact-constellation-svg')).toBeNull();
    const aperture = screen.getByTestId('knowledge-aperture');
    expect(aperture.querySelector('[data-state="error"]')).not.toBeNull();
    expect(aperture.textContent).toMatch(/did not serve a topology, so no field is drawn/);
    await waitFor(() => expect(publishedRegisters()['graph']?.state).toBe('error'));
    expect(publishedRegisters()['graph']?.detail).toBe('the graph schema changed');
    expect(publishedRegisters()['memory']?.state).toBe('ready');
  });

  it('prints the daemon coverage under the cameras and the sub-read states on the register', async () => {
    stub();
    renderPage();
    await ledgerRow(/alpha fact content/);
    const coverage = screen.getByTestId('fact-constellation-coverage');
    expect(coverage.textContent).toMatch(/graph coverage unknown: fact_universe_bounded/);
    expect(coverage.textContent).toMatch(/2 of 4,128 facts in the store drawn/);
    expect(coverage.textContent).toMatch(/2 of 2 relations resolved, limit 100/);
    const svg = screen.getByTestId('fact-constellation-svg');
    expect(svg.getAttribute('aria-label')).toMatch(/2 fact roots in 2 category frames \(rows\)/);
    expect(svg.getAttribute('aria-label')).toMatch(/fact ledger below this field is the exact accessible equivalent/);
    await waitFor(() => expect(publishedRegisters()['memory']?.state).toBe('ready'));
    const registers = publishedRegisters();
    expect(registers['memory']).toMatchObject({ state: 'ready', detail: '4,128 facts' });
    expect(registers['graph']).toMatchObject({ state: 'partial', detail: '2 roots · 2 relations' });
    expect(registers['camera']).toMatchObject({ state: 'identity', value: 'Facts' });
    expect(Object.keys(registers).sort()).toEqual(['camera', 'graph', 'memory']);
    // The canonical envelope truth header (coverage, freshness, refresh) is drawn.
    expect(screen.getByLabelText('Evidence').textContent).toMatch(/coverage/);
  });

  it('switches to the Geometry camera from the inspector and keeps the selected fact in the address', async () => {
    stub();
    renderPage();
    await userEvent.click(await ledgerRow(/alpha fact content/));
    await userEvent.click(await within(inspector()).findByRole('button', { name: /Open Geometry camera/ }));
    expect(lastSearch).toContain('view=geometry');
    expect(lastSearch).toContain('fact=fact-alpha');
    expect(screen.getByRole('tab', { name: 'Geometry', selected: true })).toBeTruthy();
  });
});
