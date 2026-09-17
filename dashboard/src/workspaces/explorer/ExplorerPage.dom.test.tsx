import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, Route, Routes, useLocation } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { FIXTURES } from '../../../stories/fixtures/data.ts';
import { useScope } from '../../data/scope/store.ts';
import { ExplorerPage } from './ExplorerPage.tsx';

type Route = { status: number; body: unknown };

function serve(routes: Record<string, Route>) {
  return vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) => {
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

/** Prints the router's current search string so a test can read the URL the
 * page writes without reaching into history. */
function LocationProbe() {
  const location = useLocation();
  return <output data-testid="location">{location.search}</output>;
}

function mount(fetchImpl: unknown, initialEntry = '/explorer') {
  vi.stubGlobal('fetch', fetchImpl);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[initialEntry]}>
        <Routes>
          <Route
            path="/explorer"
            element={
              <>
                <ExplorerPage />
                <LocationProbe />
              </>
            }
          />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function renderExplorer(routes: Record<string, Route>, initialEntry?: string) {
  return mount(serve(routes), initialEntry);
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

/** A source the coordinator is still reading. */
function reading(sourceId: SourceId) {
  const base = source(sourceId, [], null);
  return {
    ...base,
    phase: 'reading',
    outcome: 'pending',
    completed_units: null,
    total_units: null,
    page: null,
  };
}

type RunState = 'pending' | 'partial' | 'completed';

function plannerEnvelope(
  sources: unknown[] = [
    source('code_graph', [CODE_ROW], 1),
    source('sessions', [MESSAGE_ROW, SUMMARY_ROW], 2),
    source('knowledge', [FACT_ROW], null),
  ],
  state: RunState = 'partial',
  query = 'graph',
) {
  const domainState = state === 'completed' ? 'ready' : state === 'pending' ? 'loading' : 'partial';
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
    domain_state: domainState,
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
      completed_at_micros: state === 'pending' ? null : 10,
      elapsed_micros: 9,
      state,
      finality: state === 'completed' ? 'complete' : state === 'pending' ? 'pending' : 'partial',
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
  // Browse reads are the same canonical overview envelopes the visual-audit
  // fixtures serve, so a browse row here is one the daemon's contract admits.
  '/api/plugins/graph/overview': {
    status: 200,
    body: FIXTURES['/api/plugins/graph/overview'],
  },
  '/api/plugins/hermes-lcm/overview': {
    status: 200,
    body: FIXTURES['/api/plugins/hermes-lcm/overview'],
  },
  '/api/plugins/holographic/': {
    status: 200,
    body: FIXTURES['/api/plugins/holographic/'],
  },
};

afterEach(() => {
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
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

function lane(name: 'Code' | 'Sessions' | 'Knowledge' | 'Semantic') {
  return screen.getByRole('region', { name: `${name} lane` });
}

async function search(query = 'graph') {
  const user = userEvent.setup();
  await user.type(screen.getByRole('searchbox'), query);
  await user.keyboard('{Enter}');
  return user;
}

describe('ExplorerPage independent lane lifecycle', () => {
  it('draws four lanes, and a ready lane never makes an unavailable neighbour look ready', async () => {
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [
            source('code_graph', [CODE_ROW], 1),
            source('sessions', [MESSAGE_ROW, SUMMARY_ROW], 2),
            unavailable('knowledge', 'fact_store_unavailable', 'the fact authority is not mounted'),
          ],
          'partial',
        ),
      },
    });
    await search();
    await screen.findByRole('button', { name: /graph_search/ });

    expect(lane('Code').getAttribute('data-lane-state')).toBe('ready');
    expect(lane('Sessions').getAttribute('data-lane-state')).toBe('ready');
    expect(lane('Knowledge').getAttribute('data-lane-state')).toBe('unavailable');
    expect(lane('Semantic').getAttribute('data-lane-state')).toBe('unregistered');

    // The unavailable lane prints the source's own reason and no count —
    // a dash, never a zero, because zero is what a source that looked says.
    const knowledge = within(lane('Knowledge'));
    expect(knowledge.getAllByText(/the fact authority is not mounted/).length).toBeGreaterThan(0);
    expect(knowledge.getByText('—', { selector: '[data-cell="numeric"]' })).toBeTruthy();
    expect(knowledge.queryByText('0')).toBeNull();
    expect(knowledge.getByText('UNAVAILABLE')).toBeTruthy();

    // The ready lanes carry their own counts and grade, untouched.
    expect(within(lane('Code')).getByText('1')).toBeTruthy();
    expect(within(lane('Code')).getByText('EXACT')).toBeTruthy();
    expect(within(lane('Sessions')).getByText('2')).toBeTruthy();

    // The summary counts only lanes that answered.
    expect(screen.getByText(/2 of 4 lanes answered · 2 not served/)).toBeTruthy();
  });

  it('renders the semantic lane as a standing typed absence in browse and in search', async () => {
    renderExplorer(SEARCH_ROUTES);
    // Browse mode: the three source lanes hold their overview rows.
    await screen.findByRole('button', { name: /graph_api\.rs/ });
    expect(lane('Code').getAttribute('data-lane-state')).toBe('ready');
    expect(within(lane('Code')).getByText('shown from the overview endpoint')).toBeTruthy();

    const browse = within(lane('Semantic'));
    expect(browse.getByText('No production authority')).toBeTruthy();
    // Named in the header chip and again in the body, so the reason survives
    // whichever of the two a narrow layout keeps in view.
    expect(browse.getAllByText(/no semantic retrieval source is registered/).length).toBe(2);
    expect(browse.getAllByText('Source unavailable').length).toBeGreaterThan(0);
    expect(browse.queryByRole('button')).toBeNull();

    await search();
    await screen.findByText(/hits for/);
    // Three sources answered; the fourth lane still says exactly why it has
    // no rows, and nothing about the ready lanes leaked into it.
    expect(lane('Semantic').getAttribute('data-lane-state')).toBe('unregistered');
    expect(within(lane('Semantic')).getByText('No production authority')).toBeTruthy();
    expect(within(lane('Semantic')).queryByText('EXACT')).toBeNull();
  });

  it('keeps a lane the coordinator is still reading distinct from lanes that answered', async () => {
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [source('code_graph', [CODE_ROW], 1), reading('sessions'), reading('knowledge')],
          'pending',
        ),
      },
    });
    await search();
    await screen.findByRole('button', { name: /graph_search/ });

    expect(lane('Code').getAttribute('data-lane-state')).toBe('ready');
    expect(lane('Sessions').getAttribute('data-lane-state')).toBe('pending');
    expect(within(lane('Sessions')).getByText('Reading')).toBeTruthy();
    // Progress is counted in sources concluded — the only figure with a real
    // denominator — and the explicit cancel is offered while the run is live.
    expect(screen.getByRole('img', { name: '1 of 3 sources concluded' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeTruthy();
  });

  it('cancels only through the explicit control, never through Escape', async () => {
    const fetchImpl = serve({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [source('code_graph', [CODE_ROW], 1), reading('sessions'), reading('knowledge')],
          'pending',
        ),
      },
    });
    mount(fetchImpl);
    const user = await search();
    const cancel = await screen.findByRole('button', { name: 'Cancel' });

    // Escape from anywhere but the dirty search field (whose own Escape clears
    // the search) does nothing to a live run.
    cancel.focus();
    await user.keyboard('{Escape}');
    expect(screen.getByRole('button', { name: 'Cancel' })).toBe(cancel);
    expect(fetchImpl.mock.calls.some(([, init]) => init?.method === 'DELETE')).toBe(false);

    await user.click(cancel);
    await waitFor(() => {
      expect(
        fetchImpl.mock.calls.some(
          ([input, init]) =>
            String(input).includes('/api/explorer/queries/explorer-run-fixture') &&
            init?.method === 'DELETE',
        ),
      ).toBe(true);
    });
  });
});

describe('ExplorerPage scope truth', () => {
  it('refuses to run a query under a selected non-active project and dispatches nothing', async () => {
    useScope.getState().selectProject('proj_other', 'Other project', 'selected');
    const fetchImpl = serve(SEARCH_ROUTES);
    mount(fetchImpl);
    await search();

    // Every source lane carries the scope authority's own refusal; nothing
    // was asked of the gateway that would have refused it.
    await waitFor(() => {
      expect(lane('Code').getAttribute('data-lane-state')).toBe('locked');
    });
    expect(lane('Sessions').getAttribute('data-lane-state')).toBe('locked');
    expect(lane('Knowledge').getAttribute('data-lane-state')).toBe('locked');
    expect(lane('Semantic').getAttribute('data-lane-state')).toBe('unregistered');
    expect(
      within(lane('Code')).getAllByText(/Other project is not the active project/).length,
    ).toBeGreaterThan(0);
    expect(within(lane('Code')).getByText('Read-only scope')).toBeTruthy();
    expect(
      fetchImpl.mock.calls.some(([input]) => String(input).includes('/api/explorer/queries')),
    ).toBe(false);
  });

  it('routes the run through the project gateway for an active selected project', async () => {
    useScope.getState().selectProject('proj_active', 'Active project', 'active');
    const fetchImpl = serve({
      ...SEARCH_ROUTES,
      '/api/projects/proj_active/explorer/queries': { status: 200, body: plannerEnvelope() },
    });
    mount(fetchImpl);
    await search();
    await screen.findByRole('button', { name: /graph_search/ });

    const created = fetchImpl.mock.calls.find(([, init]) => init?.method === 'POST');
    expect(String(created?.[0])).toBe('/api/projects/proj_active/explorer/queries');
  });
});

describe('ExplorerPage inspection', () => {
  it('hover inspects without selecting, and click selects', async () => {
    const fetchImpl = serve(SEARCH_ROUTES);
    mount(fetchImpl);
    const user = await search();
    const row = await screen.findByRole('button', { name: /Using graph search/ });

    await user.hover(row);
    const peek = await screen.findByRole('complementary', { name: 'Inspector' });
    expect(peek.querySelector('[data-inspect-mode="peek"]')).toBeTruthy();
    expect(row.getAttribute('aria-pressed')).toBe('false');
    // A peek shows what is already on screen and opens no reads.
    expect(within(peek).getByText(/Select this row to read its size and context/)).toBeTruthy();
    expect(within(peek).queryByRole('button', { name: 'Close inspector' })).toBeNull();
    expect(fetchImpl.mock.calls.some(([input]) => String(input).includes('/size'))).toBe(false);

    await user.unhover(row);
    expect(screen.queryByRole('complementary', { name: 'Inspector' })).toBeNull();

    await user.click(row);
    const selected = await screen.findByRole('complementary', { name: 'Inspector' });
    expect(row.getAttribute('aria-pressed')).toBe('true');
    expect(selected.querySelector('[data-inspect-mode="selected"]')).toBeTruthy();
    expect(within(selected).getByRole('button', { name: 'Close inspector' })).toBeTruthy();
    await waitFor(() => {
      expect(fetchImpl.mock.calls.some(([input]) => String(input).includes('/size'))).toBe(true);
    });
  });

  it('grades a result: exact identity, explicit transcript text, exact code text', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = await search();

    await user.click(await screen.findByRole('button', { name: /Using graph search/ }));
    let inspector = within(await screen.findByRole('complementary', { name: 'Inspector' }));
    expect(inspector.getByText('TRANSCRIPT')).toBeTruthy();
    expect(inspector.getAllByText('EXACT').length).toBeGreaterThan(0);
    expect(inspector.getByText('EXPLICIT')).toBeTruthy();
    // The identity key is printed beside its grade as well as in the payload.
    expect(inspector.getAllByText('message-1').length).toBeGreaterThan(1);

    await user.click(screen.getByRole('button', { name: /graph_search/ }));
    inspector = within(await screen.findByRole('complementary', { name: 'Inspector' }));
    expect(inspector.getByText('GRAPH')).toBeTruthy();
    expect(inspector.queryByText('EXPLICIT')).toBeNull();
    // The one real pivot: the graph node id is the identity Code focuses on.
    const pivot = inspector.getByRole('link', { name: /Open symbol in Code/ });
    expect(pivot.getAttribute('href')).toBe('/code?symbol=node-1');
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
        ]),
      },
    });
    const user = await search();

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

    // And the lanes that answered empty say so, as their own answer.
    expect(within(lane('Sessions')).getByText('Served empty')).toBeTruthy();
    expect(within(lane('Sessions')).getByText('0')).toBeTruthy();
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
        ]),
      },
    });
    await search();

    const meter = await screen.findByRole('img', { name: 'degree 0' });
    expect(meter.querySelector('.td-meter-fill')).toBeNull();
  });

  it('shows the exact payload fields behind a selected row', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = await search();
    await user.click(await screen.findByRole('button', { name: /graph_search/ }));

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

  it('keeps every definition term and description in a valid definition-list group', async () => {
    const { container } = renderExplorer(SEARCH_ROUTES);
    const user = await search();
    await user.click(await screen.findByRole('button', { name: /Using graph search/ }));
    expect(await screen.findByText('Session context')).toBeTruthy();

    const items = [...container.querySelectorAll('dt, dd')];
    expect(items.length).toBeGreaterThan(0);
    for (const item of items) {
      const parent = item.parentElement;
      const grouped =
        parent?.tagName === 'DL' ||
        (parent?.tagName === 'DIV' && parent.parentElement?.tagName === 'DL');
      expect(grouped, `${item.tagName} outside a dl: ${item.outerHTML.slice(0, 120)}`).toBe(true);
    }
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
});

describe('ExplorerPage keyboard', () => {
  it('closes the selection on Escape and returns focus to the invoking row without reopening', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = await search();

    const row = await screen.findByRole('button', { name: /graph_search/ });
    row.focus();
    await user.keyboard('{Enter}');
    const inspector = await screen.findByRole('complementary', { name: 'Inspector' });
    // Focus rests on the row it just selected: that is the selection, not a
    // peek, so the panel offers its close control.
    expect(inspector.querySelector('[data-inspect-mode="selected"]')).toBeTruthy();

    screen.getByRole('button', { name: 'Close inspector' }).focus();
    await user.keyboard('{Escape}');

    expect(screen.queryByRole('complementary', { name: 'Inspector' })).toBeNull();
    expect(document.activeElement).toBe(row);
  });

  it('inspects the focused row as the arrows move, and crosses lanes horizontally', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = await search();
    const first = await screen.findByRole('button', { name: /graph_search/ });
    first.focus();
    const inspector = await screen.findByRole('complementary', { name: 'Inspector' });
    expect(inspector.querySelector('[data-inspect-mode="peek"]')).toBeTruthy();

    await user.keyboard('{ArrowRight}');
    const sessionRow = screen.getByRole('button', { name: /Using graph search/ });
    expect(document.activeElement).toBe(sessionRow);
    expect(within(screen.getByRole('complementary', { name: 'Inspector' })).getByText('TRANSCRIPT')).toBeTruthy();

    await user.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Graph route investigation/ }));

    await user.keyboard('{ArrowLeft}');
    expect(document.activeElement).toBe(first);
  });

  it('leaves focus in a dirty search field when its Escape clears the search', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = await search();

    const row = await screen.findByRole('button', { name: /graph_search/ });
    row.focus();
    await user.keyboard('{Enter}');
    expect(await screen.findByText('Payload provenance')).toBeTruthy();

    const searchbox = screen.getByRole('searchbox');
    searchbox.focus();
    await user.keyboard('{Escape}');
    expect(searchbox).toHaveProperty('value', '');
    expect(screen.queryByText('Payload provenance')).toBeNull();
    expect(document.activeElement).toBe(searchbox);
  });
});

describe('ExplorerPage filters and address', () => {
  it('narrows to one lane and pivots on loaded rows without touching the query', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = await search();
    await screen.findByRole('button', { name: /graph_search/ });

    await user.selectOptions(screen.getByRole('combobox', { name: 'Lanes' }), 'sessions');
    expect(screen.queryByRole('region', { name: 'Code lane' })).toBeNull();
    expect(lane('Sessions')).toBeTruthy();
    expect(screen.getByTestId('location').textContent).toContain('lane=sessions');
    expect(screen.getByTestId('location').textContent).toContain('q=graph');

    // The pivot is over loaded rows: two session rows, one carrying a role.
    await user.selectOptions(screen.getByRole('combobox', { name: 'Sessions Role' }), 'assistant');
    expect(screen.getByRole('button', { name: /Using graph search/ })).toBeTruthy();
    expect(screen.queryByRole('button', { name: /Graph route investigation/ })).toBeNull();
    expect(within(lane('Sessions')).getByText('1 shown')).toBeTruthy();

    await user.click(screen.getByRole('button', { name: 'Clear' }));
    expect(lane('Code')).toBeTruthy();
    expect(screen.getByRole('button', { name: /Graph route investigation/ })).toBeTruthy();
    expect(screen.getByTestId('location').textContent).not.toContain('lane=');
  });

  it('restores a query from the address by running it against the current scope', async () => {
    const fetchImpl = serve(SEARCH_ROUTES);
    mount(fetchImpl, '/explorer?q=graph&lane=code');

    await screen.findByRole('button', { name: /graph_search/ });
    expect((screen.getByRole('searchbox') as HTMLInputElement).value).toBe('graph');
    expect(screen.queryByRole('region', { name: 'Sessions lane' })).toBeNull();
    const created = fetchImpl.mock.calls.find(([, init]) => init?.method === 'POST');
    expect(JSON.parse(String(created?.[1]?.body))).toMatchObject({ query: 'graph' });
    expect(screen.getByTestId('location').textContent).toContain('q=graph');
  });
});
