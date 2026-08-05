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

/** A daemon the browser cannot reach on one route: `fetch` rejects, which is
 * the client-side connectivity condition — not anything a source said about
 * itself. */
function renderExplorerUnreachable(routes: Record<string, Route>, path: string) {
  const reachable = serve(routes);
  return mount(
    vi.fn(async (input: RequestInfo | URL) => {
      if (String(input).includes(path)) throw new TypeError('Failed to fetch');
      return reachable(input);
    }),
  );
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
  fact_id: 11,
  content: 'Graph search is bounded',
  category: 'project',
  trust_score: 0.8,
};

function source(
  sourceId: 'code_graph' | 'sessions' | 'knowledge',
  rows: Record<string, unknown>[],
  total: number | null,
) {
  return {
    source_id: sourceId,
    source_label:
      sourceId === 'code_graph'
        ? 'Code graph'
        : sourceId === 'sessions'
          ? 'Sessions'
          : 'Knowledge',
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

function plannerEnvelope(
  sources: unknown[] = [
    source('code_graph', [CODE_ROW], 1),
    source('sessions', [MESSAGE_ROW, SUMMARY_ROW], 2),
    source('knowledge', [FACT_ROW], null),
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
      eligible: 3,
      examined: state === 'completed' ? 3 : 2,
      matched: null,
      excluded: null,
      omitted: state === 'completed' ? 0 : 1,
      unknown: null,
      denominator: 3,
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
      required_source_ids: ['code_graph', 'sessions', 'knowledge'],
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
  sourceId: 'code_graph' | 'sessions' | 'knowledge',
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

function unavailable(sourceId: 'code_graph' | 'sessions' | 'knowledge', code: string, message: string) {
  return withoutAnswer(sourceId, 'unavailable', code, message);
}

/** The domain state a lane readout is actually drawing, taken off the chip
 * rather than off its wording: two conditions that read differently in prose
 * while lighting the same indicator are still one fact to anyone scanning the
 * indicators. */
function laneChipState(lane: HTMLElement): string | null {
  return lane.querySelector('[data-state]')?.getAttribute('data-state') ?? null;
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
          ],
          'partial',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');
    await screen.findByRole('button', { name: /graph_search/ });

    // Three rows arrived, but only two of the three memories answered. The
    // caption must not present the result set as spanning all three.
    expect(screen.queryByText(/across three memories/)).toBeNull();
    expect(screen.getByText(/across 2 of 3 memories/)).toBeTruthy();
  });

  it('refuses a confirmed-absence claim when a source reports unknown coverage', async () => {
    const knowledgeUnknownCoverage = {
      ...source('knowledge', [], null),
      coverage: {
        ...source('knowledge', [], null).coverage,
        completeness: 'unknown',
        denominator: null,
      },
    };
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: {
          ...plannerEnvelope(
            [
              source('code_graph', [], 0),
              source('sessions', [], 0),
              knowledgeUnknownCoverage,
            ],
            'completed',
            'missing',
          ),
          // A coordinator that over-claims canonical finality while one of its
          // own sources reports unknown coverage. The client carries the
          // contradicting evidence in the same payload, so it must not print a
          // global-absence claim on the strength of the scalar alone.
          payload: {
            ...plannerEnvelope(
              [
                source('code_graph', [], 0),
                source('sessions', [], 0),
                knowledgeUnknownCoverage,
              ],
              'completed',
              'missing',
            ).payload,
            finality: 'complete',
            state: 'completed',
          },
        },
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText(/No rows loaded for/)).toBeTruthy();
    expect(screen.queryByText(/No source matched/)).toBeNull();
    expect(screen.queryByText(/completed with known coverage/)).toBeNull();
  });

  /**
   * These two shapes both declared `completeness: 'complete'` over a real
   * denominator, which was everything the absence gate inspected — so both
   * earned "Every required source completed with known coverage" while the
   * source's own numbers on the same object said it knew the status of nothing,
   * or had examined nothing. They are separate cases because they are separate
   * facts, and the copy has to be able to tell a reader which one happened.
   */
  it('refuses absence when every unit a source counted is unknown', async () => {
    const allUnitsUnknown = {
      ...source('knowledge', [], 5),
      coverage: {
        ...source('knowledge', [], 5).coverage,
        completeness: 'complete',
        eligible: 5,
        examined: 5,
        matched: 0,
        excluded: 0,
        omitted: 0,
        unknown: 5,
        denominator: 5,
        unit: 'facts',
        omission_reasons: ['every unit resolved to unknown status'],
      },
      page: { offset: 0, limit: 50, total: 0, next_offset: null, rows: [], metadata: {} },
    };
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [source('code_graph', [], 0), source('sessions', [], 0), allUnitsUnknown],
          'completed',
          'missing',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText(/No rows loaded for/)).toBeTruthy();
    expect(screen.queryByText(/No source matched/)).toBeNull();
    expect(screen.queryByText(/completed with known coverage/)).toBeNull();
    expect(
      screen.getByText(/could not determine the status of any of its 5 facts/),
    ).toBeTruthy();
  });

  it('refuses absence when a source examined none of its units', async () => {
    const examinedNothing = {
      ...source('code_graph', [], 400),
      coverage: {
        ...source('code_graph', [], 400).coverage,
        completeness: 'complete',
        eligible: 400,
        examined: 0,
        matched: 0,
        excluded: 0,
        omitted: 400,
        unknown: 0,
        denominator: 400,
        unit: 'symbols',
        omission_reasons: ['result cap reached before any unit was examined'],
      },
      page: { offset: 0, limit: 50, total: 0, next_offset: null, rows: [], metadata: {} },
    };
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [examinedNothing, source('sessions', [], 0), source('knowledge', [], 0)],
          'completed',
          'missing',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText(/No rows loaded for/)).toBeTruthy();
    expect(screen.queryByText(/completed with known coverage/)).toBeNull();
    expect(screen.getByText(/examined none of its 400 symbols/)).toBeTruthy();
  });

  it('still confirms absence when every source examined its full denominator', async () => {
    // The counterweight to the two refusals above: the claim must remain
    // reachable, or the fix would have replaced a false statement with no
    // statement at all.
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [source('code_graph', [], 0), source('sessions', [], 0), source('knowledge', [], 0)],
          'completed',
          'missing',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText(/No source matched/)).toBeTruthy();
    expect(screen.getByText(/examined its full denominator/)).toBeTruthy();
  });

  it('never renders a count for a source that did not answer', async () => {
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
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');
    await screen.findByRole('button', { name: /graph_search/ });

    const knowledge = screen.getByRole('button', { name: /^Knowledge/ });
    expect(knowledge.textContent).not.toMatch(/\b0\b/);
    expect(knowledge.textContent).toMatch(/no count reported/);
  });

  it('tells a cancelled read, an unavailable source, and an empty answer apart', async () => {
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [
            source('code_graph', [], 0),
            withoutAnswer('sessions', 'cancelled', 'session_read_cancelled', 'the read was cancelled'),
            withoutAnswer(
              'knowledge',
              'unavailable',
              'fact_store_unavailable',
              'the fact authority is not mounted',
            ),
          ],
          'partial',
          'missing',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');
    await screen.findByText('Some sources did not answer');

    // The source that answered may show its zero — it counted, and the count
    // was nought. The two that never answered may not, and each has to say
    // which of the two things happened to it.
    const code = screen.getByRole('button', { name: /^Code graph/ });
    expect(code.textContent).toMatch(/Code graph\s*0\s*loaded/);
    expect(code.textContent).toContain('loaded of 0 matching rows reported');

    const sessions = screen.getByRole('button', { name: /^Sessions/ });
    expect(laneChipState(sessions)).toBe('cancelled');
    expect(sessions.textContent).toMatch(/read cancelled/);
    expect(sessions.textContent).toMatch(/no count reported/);
    expect(sessions.textContent).not.toMatch(/\b0\b/);

    const knowledge = screen.getByRole('button', { name: /^Knowledge/ });
    // The chip itself carries the condition, and the clause beside it carries
    // the reason the source gave rather than a second copy of the state.
    expect(laneChipState(knowledge)).toBe('unavailable');
    expect(knowledge.textContent).toMatch(/Source unavailable/);
    expect(knowledge.textContent).toMatch(/fact_store_unavailable/);
    expect(knowledge.textContent).toMatch(/no count reported/);
    expect(knowledge.textContent).not.toMatch(/\b0\b/);
  });

  it('never reads a daemon the browser cannot reach as a source that failed', async () => {
    renderExplorerUnreachable(SEARCH_ROUTES, '/api/explorer/queries');
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');
    await screen.findByText('Some sources did not answer');

    // No source said anything about itself — the browser never got a reply —
    // so no lane may be shown as an unavailable or broken source. Held at the
    // chip as well as in the prose: source-level unavailability is now its own
    // chip, so a lane that merely worded the difference while drawing the same
    // indicator would still be lying to anyone reading the indicator.
    for (const name of [/^Code graph/, /^Sessions/, /^Knowledge/]) {
      const lane = screen.getByRole('button', { name });
      expect(laneChipState(lane)).toBe('offline');
      expect(lane.textContent).toMatch(/Offline/);
      expect(lane.textContent).toMatch(/daemon unreachable/);
      expect(lane.textContent).toMatch(/no count reported/);
      expect(lane.textContent).not.toMatch(/Source unavailable/);
    }
  });

  it('does not leave a source the run never named looking like it is still reading', async () => {
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope([source('code_graph', [], 0)], 'completed', 'missing'),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');
    await screen.findByText('Some sources did not answer');

    expect(screen.getByRole('button', { name: /^Code graph/ }).textContent).toContain(
      'loaded of 0 matching rows reported',
    );
    for (const name of [/^Sessions/, /^Knowledge/]) {
      const lane = screen.getByRole('button', { name });
      expect(lane.textContent).toMatch(/the run never named this source/);
      expect(lane.textContent).not.toMatch(/\b0\b/);
    }
    // The coordinator declared canonical finality while omitting two of its own
    // required sources, so the absence claim stays unearned.
    expect(screen.queryByText(/No source matched/)).toBeNull();
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
        ]),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');

    const meter = await screen.findByRole('img', { name: 'degree 0' });
    expect(meter.querySelector('.td-meter-fill')).toBeNull();
  });

  it('never turns a failed lane plus zero successful hits into complete-zero copy', async () => {
    const unavailableSessions = {
      ...source('sessions', [], 0),
      outcome: 'unavailable',
      total_units: null,
      coverage: {
        ...source('sessions', [], 0).coverage,
        completeness: 'unknown',
        eligible: null,
        examined: null,
        denominator: null,
      },
      error_code: 'session_query_failed',
      message: 'LCM unavailable',
      page: null,
    };
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [source('code_graph', [], 0), unavailableSessions, source('knowledge', [], null)],
          'partial',
          'missing',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText('Some sources did not answer')).toBeTruthy();
    expect(screen.getByText(/A zero-result claim would be unsafe/)).toBeTruthy();
    expect(screen.queryByText(/genuinely absent from/)).toBeNull();
  });

  it('treats an HTTP-200 LCM response with no mounted store as unavailable', async () => {
    const unavailableSessions = {
      ...source('sessions', [], 0),
      outcome: 'unavailable',
      total_units: null,
      coverage: {
        ...source('sessions', [], 0).coverage,
        completeness: 'unknown',
        eligible: null,
        examined: null,
        denominator: null,
      },
      error_code: 'session_store_unavailable',
      message: 'the active-project session store is not mounted',
      page: null,
    };
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [source('code_graph', [], 0), unavailableSessions, source('knowledge', [], null)],
          'partial',
          'missing',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText('Some sources did not answer')).toBeTruthy();
    // Scoped to the coordinator's own source list, which is where the code and
    // the message it reported are printed verbatim. Unscoped `getByText` for
    // the code now has a second, legitimate match — the lane chip's clause
    // carries the reported reason — and would fail on multiplicity rather than
    // on the thing under test.
    const progress = within(screen.getByRole('list', { name: 'Source progress' }));
    expect(progress.getByText(/session_store_unavailable/)).toBeTruthy();
    expect(progress.getByText(/session store is not mounted/)).toBeTruthy();
    // The source said it could not serve; the browser reached the daemon fine.
    expect(laneChipState(screen.getByRole('button', { name: /^Sessions/ }))).toBe('unavailable');
    expect(screen.queryByText(/genuinely absent from/)).toBeNull();
  });

  it('does not claim complete absence when every compatibility endpoint returns zero rows', async () => {
    renderExplorer({
      ...SEARCH_ROUTES,
      '/api/explorer/queries': {
        status: 200,
        body: plannerEnvelope(
          [
            source('code_graph', [], 0),
            source('sessions', [], 0),
            source('knowledge', [], null),
          ],
          'partial',
          'missing',
        ),
      },
    });
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'missing');
    await user.keyboard('{Enter}');

    expect(await screen.findByText('No rows loaded for “missing”')).toBeTruthy();
    // Names the source and the coverage it declared, rather than a generic
    // sentence about incomplete coverage that applies to every refusal alike.
    expect(screen.getByText(/Knowledge reports unknown coverage/)).toBeTruthy();
    expect(screen.getByText(/cannot establish global absence/)).toBeTruthy();
    expect(screen.queryByText(/genuinely absent from/)).toBeNull();
  });

  it('reports unavailable temporal retrieval in the session inspector', async () => {
    renderExplorer(SEARCH_ROUTES);
    const user = userEvent.setup();
    await user.type(screen.getByRole('searchbox'), 'graph');
    await user.keyboard('{Enter}');
    await user.click(await screen.findByRole('button', { name: /Using graph search/ }));

    expect(await screen.findByText('Session context')).toBeTruthy();
    expect(await screen.findByText(/lcm_temporal_retrieval_not_mounted/)).toBeTruthy();
    expect(screen.queryByText('Raw token estimate')).toBeNull();
    expect(screen.queryByText('Session read context returned by the daemon')).toBeNull();
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
