import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, within } from '@testing-library/react';
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

function mount(fetchImpl: unknown) {
  vi.stubGlobal('fetch', fetchImpl);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ExplorerPage />
    </QueryClientProvider>,
  );
}

function renderExplorer(routes: Record<string, Route>) {
  return mount(serve(routes));
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
  fact_id: `fact.${'a'.repeat(64)}.${'b'.repeat(64)}`,
  content: 'Graph search is bounded',
  category: 'project',
  trust_score: 0.8,
};

const SOURCE_LABELS = {
  code_graph: 'Code graph',
  sessions: 'Sessions',
  knowledge: 'Knowledge',
  semantic: 'Semantic',
} as const;

type SourceId = keyof typeof SOURCE_LABELS;

function source(sourceId: SourceId, rows: Record<string, unknown>[], total: number | null) {
  return {
    source_id: sourceId,
    source_label: SOURCE_LABELS[sourceId],
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

/** The live semantic source today: not activated, a typed absence carrying
 * the complete accounting of an empty domain. */
function semanticAbsent() {
  const base = source('semantic', [], 0);
  return {
    ...base,
    outcome: 'absent',
    completed_units: 0,
    total_units: 0,
    coverage: {
      ...base.coverage,
      eligible: 0,
      examined: 0,
      matched: 0,
      denominator: 0,
      unit: 'indexed vectors',
    },
    error_code: 'semantic_not_activated',
    message: 'semantic search is not activated for this project',
    page: null,
  };
}

function plannerEnvelope(
  sources: unknown[] = [
    source('code_graph', [CODE_ROW], 1),
    source('sessions', [MESSAGE_ROW, SUMMARY_ROW], 2),
    source('knowledge', [FACT_ROW], null),
    semanticAbsent(),
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
      eligible: 4,
      examined: state === 'completed' ? 4 : 3,
      matched: null,
      excluded: null,
      omitted: state === 'completed' ? 0 : 1,
      unknown: null,
      denominator: 4,
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
      required_source_ids: ['code_graph', 'sessions', 'knowledge', 'semantic'],
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

function temporalRetrievalUnavailableEnvelope() {
  return {
    ...plannerEnvelope(),
    coverage: {
      completeness: 'unknown',
      eligible: null,
      examined: null,
      matched: null,
      excluded: null,
      omitted: null,
      unknown: null,
      denominator: null,
      unit: 'records',
      omission_reasons: ['lcm_temporal_retrieval_not_mounted'],
    },
    freshness: { state: 'unknown', observed_at_micros: null, watermark: null },
    domain_state: 'unknown',
    payload: null,
  };
}

const SEARCH_ROUTES = {
  '/api/explorer/sessions/session-1/size': {
    status: 200,
    body: temporalRetrievalUnavailableEnvelope(),
  },
  '/api/explorer/sessions/session-1/read-context': {
    status: 200,
    body: temporalRetrievalUnavailableEnvelope(),
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

/** A source that reached a terminal outcome without returning a page. */
function withoutAnswer(
  sourceId: SourceId,
  outcome: 'unavailable' | 'cancelled' | 'error',
  code: string,
  message: string,
) {
  const base = source(sourceId, [], 0);
  return {
    ...base,
    outcome,
    completed_units: null,
    total_units: null,
    coverage: {
      ...base.coverage,
      completeness: 'unknown',
      eligible: null,
      examined: null,
      denominator: null,
    },
    error_code: code,
    message,
    page: null,
  };
}

function unavailable(sourceId: SourceId, code: string, message: string) {
  return withoutAnswer(sourceId, 'unavailable', code, message);
}

describe('ExplorerPage no-falsified-UI invariant', () => {
  it('never counts a source that did not answer in the result caption', async () => {
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [
            source('code_graph', [CODE_ROW], 1),
            source('sessions', [MESSAGE_ROW, SUMMARY_ROW], 2),
            unavailable('knowledge', 'fact_store_unavailable', 'the fact authority is not mounted'),
            semanticAbsent(),
          ],
          'partial',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');
    await screen.findByRole('button', { name: /graph_search/ });

    // Three rows arrived, but only two of the four memories answered with
    // rows. The caption must not present the result set as spanning them all.
    expect(screen.queryByText(/across four memories/)).toBeNull();
    expect(screen.getByText(/across 2 of 4 memories/)).toBeTruthy();
  });

  it('renders a field the row omitted as absent rather than as a zero', async () => {
    const rowWithoutDegree = {
      id: 'node-2',
      name: 'graph_without_degree',
      kind: 'function',
      file_path: 'src/dashboard/graph_service.rs',
    };
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope([
          source('code_graph', [rowWithoutDegree], 1),
          source('sessions', [], 0),
          source('knowledge', [], 0),
          semanticAbsent(),
        ]),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');

    const row = await screen.findByRole('button', { name: /graph_without_degree/ });
    expect(screen.queryByRole('img', { name: /^degree/ })).toBeNull();
    expect(screen.queryByText(/edges/)).toBeNull();

    await user.click(row);
    expect(await screen.findByText('Payload provenance')).toBeTruthy();
    // Nothing to measure is not a measurement of nothing: the section, the
    // rail, and the payload key are all simply absent.
    expect(screen.queryByText('Measured')).toBeNull();
    expect(screen.queryByRole('img', { name: /^degree/ })).toBeNull();
    const provenance = screen.getByText('Payload provenance').closest('details');
    expect(within(provenance as HTMLElement).queryByText('degree')).toBeNull();
  });
});

describe('ExplorerPage', () => {
  it('keeps every definition term and description in a valid definition-list group', async () => {
    const { container } = renderExplorer(SEARCH_ROUTES);
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');
    // Drive to the inspector so the session-context and payload-provenance
    // lists are mounted too: the axe `dlitem` / `definition-list` failures this
    // locks down were reachable in every one of those states, so scanning only
    // the browse state would let two thirds of them back in.
    await user.click(await screen.findByRole('button', { name: /Using graph search/ }));
    expect(await screen.findByText('Session context')).toBeTruthy();
    expect(screen.getByText('What each lane searches')).toBeTruthy();

    // Deliberately unscoped: a `dl dt` selector can only ever see terms that
    // are already inside a list, which is precisely the defect it is supposed
    // to detect. Every `dt`/`dd` in the tree has to be accounted for.
    const items = [...container.querySelectorAll('dt, dd')];
    expect(items.length).toBeGreaterThan(0);
    for (const item of items) {
      const parent = item.parentElement;
      const grouped =
        parent?.tagName === 'DL' ||
        (parent?.tagName === 'DIV' && parent.parentElement?.tagName === 'DL');
      expect(grouped, `${item.tagName} outside a dl: ${item.outerHTML.slice(0, 120)}`).toBe(true);
    }

    // axe `definition-list`: a dl may directly contain only dt, dd, div,
    // script and template.
    const lists = [...container.querySelectorAll('dl')];
    expect(lists.length).toBeGreaterThan(0);
    for (const list of lists) {
      for (const child of [...list.children]) {
        expect(
          ['DT', 'DD', 'DIV', 'SCRIPT', 'TEMPLATE'].includes(child.tagName),
          `<dl> directly contains <${child.tagName.toLowerCase()}>`,
        ).toBe(true);
      }
    }
  });

  it('derives lane readout names from visible text rather than an aria-label', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');

    const sessions = await screen.findByRole('button', {
      name: /Sessions\s*2\s*loaded\s*of 2 matching rows reported/,
    });
    // An `aria-label` here would silently replace the computed name, letting
    // the visible label drift out of the accessible name (WCAG 2.5.3). The
    // readout has to say the same sentence to both readers.
    expect(sessions.getAttribute('aria-label')).toBeNull();
    expect(sessions.textContent).toContain('loaded of 2 matching rows reported');
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

  it('does not draw a measured signal bar without a denominator', async () => {
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope([
          source('code_graph', [{ ...CODE_ROW, degree: 0 }], 1),
          source('sessions', [MESSAGE_ROW, SUMMARY_ROW], 2),
          source('knowledge', [FACT_ROW], null),
          semanticAbsent(),
        ]),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');

    const meter = await screen.findByRole('img', { name: 'degree 0' });
    expect(meter.querySelector('.td-meter-fill')).toBeNull();
  });

  it('closes the inspector on Escape and returns focus to the invoking row', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');

    // Open with the keyboard: the row is a native button, Enter activates it.
    const row = await screen.findByRole('button', { name: /graph_search/ });
    row.focus();
    await user.keyboard('{Enter}');
    expect(await screen.findByText('Payload provenance')).toBeTruthy();

    // Focus moves into the inspector, as a reader tabbing into the panel
    // does. Escape must close it AND return focus to the row that opened it —
    // otherwise focus dies on a removed node and the reader is dropped at the
    // top of the document.
    screen.getByRole('button', { name: 'Close inspector' }).focus();
    await user.keyboard('{Escape}');

    expect(screen.queryByText('Payload provenance')).toBeNull();
    expect(document.activeElement).toBe(row);
  });

  it('leaves focus in a dirty search field when its Escape clears the search', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');

    const row = await screen.findByRole('button', { name: /graph_search/ });
    row.focus();
    await user.keyboard('{Enter}');
    expect(await screen.findByText('Payload provenance')).toBeTruthy();

    // Escape inside the dirty search field is the field's own action: it
    // clears back to the browse state (which withdraws the selection with the
    // search it belonged to). The inspector's document-level Escape must not
    // also fire, or focus would be yanked out of the field to the row.
    const searchbox = screen.getByRole('searchbox');
    searchbox.focus();
    await user.keyboard('{Escape}');
    expect(searchbox).toHaveProperty('value', '');
    expect(screen.queryByText('Payload provenance')).toBeNull();
    expect(document.activeElement).toBe(searchbox);
  });

  it('shows the exact payload fields behind an inspected row', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');
    await user.click(await screen.findByRole('button', { name: /graph_search/ }));

    // `name` and `degree` each appear twice on purpose: once as the label of
    // the section that names the field a value was read from, and once as a key
    // in the raw payload table. Asserting `getAllByText(...).length > 0` would
    // pass on either one alone and on any number of accidental extras, so the
    // payload keys are pinned inside the provenance region instead — one match
    // each, in the region that is actually under test.
    const provenance = screen.getByText('Payload provenance').closest('details');
    expect(provenance).toBeTruthy();
    const payload = within(provenance as HTMLElement);
    expect(payload.getByText('name')).toBeTruthy();
    expect(payload.getByText('file_path')).toBeTruthy();
    expect(payload.getByText('degree')).toBeTruthy();
    expect(payload.getByText('graph_search')).toBeTruthy();
    expect(payload.getByText('7')).toBeTruthy();
    expect(screen.getByText(/Position 1 in graph endpoint rows/)).toBeTruthy();
  });
});
