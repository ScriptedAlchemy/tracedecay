import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ScopedBrain } from './ScopedBrain.tsx';
import { useScope } from '../../data/scope/store.ts';
import { resolveFixture } from '../../../stories/fixtures/data.ts';

// The canvas is a WebGL renderer; this suite is about which reads compose the
// surface and what it says when one of them is legitimately unavailable.
vi.mock('../../viz/graph/GraphCanvas.tsx', () => ({
  GraphCanvas: ({ nodes, caption }: { nodes: unknown[]; caption: unknown }) => (
    <div data-testid="graph-canvas" data-node-count={nodes.length}>
      {caption as never}
    </div>
  ),
}));

/**
 * Wire-true bodies come from the shared fixture module, which is gated against
 * the generated contracts by `endpoint-fixtures.test.ts`. Taking them from
 * there rather than restating them means these five reads cannot drift into
 * shapes the daemon does not send, and each test overrides only the figures its
 * assertions actually name.
 */
const wire = (path: string) => resolveFixture(path) as Record<string, unknown>;

/**
 * The scoped readout renders `<dt>label</dt><dd>figure</dd>` per statistic.
 * Read the figure through its term: counting em dashes anywhere on the page
 * says only that something was withheld, not that the *right* figure was, so
 * it passes just as happily when a failed read is printed as a measured zero
 * and three unrelated readouts supply the dashes.
 */
function readout(label: string): string | null {
  return screen.getByText(label, { selector: 'dt' }).nextElementSibling?.textContent ?? null;
}

/** The registry backbone (src/dashboard/projects.rs `context`) — resolves for
 * every registered project whether or not its graph is mounted. Its store
 * carries a `release/2.4` graph scope, which is the branch these tests read
 * back to prove the backbone survived a failed graph read. */
const CONTEXT = wire('/api/projects/proj_x');

/** Wire-true unseeded slice, cut down to two nodes and the edge between them.
 * `graph_service.rs::subgraph_payload` writes `seed_id`, `mode`, `nodes`,
 * `edges` and `capped` on every one of its three return paths, and each node is
 * a full `GraphNodeV1` — a body carrying only `id`/`kind`/`name`/`degree` is
 * one the daemon cannot produce, which is what this fixture used to be back
 * when Brain read the scoped gateway through its own all-optional copy of the
 * subgraph shape. */
const SUBGRAPH = (() => {
  const shared = wire('/api/plugins/graph/subgraph');
  const nodes = (shared['nodes'] as Record<string, unknown>[]).slice(0, 2);
  const [alpha, beta] = nodes;
  return {
    ...shared,
    nodes,
    edges: [
      {
        source: alpha!['id'],
        target: beta!['id'],
        kind: 'calls',
        line: 41,
        source_name: null,
        target_name: null,
      },
    ],
  };
})();

/** An empty slice the daemon really can send: the read succeeded and found
 * nothing to draw. */
const SUBGRAPH_EMPTY = { ...SUBGRAPH, nodes: [], edges: [] };

const graphOverview = (totals: Record<string, number>) => ({
  ...wire('/api/plugins/graph/overview'),
  totals,
});

const MEMORY_STATUS = wire('/api/plugins/holographic/status');
const ANALYTICS = wire('/api/plugins/analytics/overview');

/** Routes each request to a canned body, mirroring the daemon: the scoped
 * gateway 404s with `not_found` when a project's graph is not mounted. */
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

function renderScoped() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ScopedBrain projectId="proj_x" label="ai-train" />
    </QueryClientProvider>,
  );
}

describe('ScopedBrain', () => {
  beforeEach(() => {
    // The scoped reads must be rewritten through the project gateway, which is
    // what `useLegacy` does off the store's current scope.
    useScope.getState().selectProject('proj_x', 'ai-train');
  });

  afterEach(() => {
    useScope.getState().selectAllProjects();
    vi.unstubAllGlobals();
  });

  it('reads this project through the scoped gateway, not the active project', async () => {
    const fetchMock = serve({
      '/api/projects/proj_x/plugins/graph/subgraph': { status: 200, body: SUBGRAPH },
      '/api/projects/proj_x/plugins/graph/overview': {
        status: 200,
        body: graphOverview({ nodes: 12_873, edges: 41_206, files: 642 }),
      },
      '/api/projects/proj_x/plugins/holographic/status': { status: 200, body: MEMORY_STATUS },
      '/api/projects/proj_x/plugins/analytics/overview': { status: 200, body: ANALYTICS },
      '/api/projects/proj_x': { status: 200, body: CONTEXT },
    });
    vi.stubGlobal('fetch', fetchMock);
    renderScoped();

    await waitFor(() => expect(screen.getByTestId('graph-canvas')).toBeTruthy());
    expect(screen.getByTestId('graph-canvas').dataset['nodeCount']).toBe('2');

    // Every scoped read went through `/api/projects/{id}/…`; nothing asked the
    // daemon for the active project's state and labelled it as this one's.
    const scoped = fetchMock.mock.calls
      .map(([input]) => String(input))
      .filter((url) => url.includes('/plugins/'));
    expect(scoped.length).toBe(4);
    expect(scoped.every((url) => url.startsWith('/api/projects/proj_x/plugins/'))).toBe(
      true,
    );

    // Real readouts, from the project's own stores.
    expect(screen.getByText('12.9')).toBeTruthy();
    expect(screen.getByText('173')).toBeTruthy();
    expect(screen.getByText('release/2.4')).toBeTruthy();
  });

  it('does not infer an unmounted graph from a generic scoped read failure', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        '/api/projects/proj_x/plugins/graph/subgraph': {
          status: 500,
          body: { status: 'error' },
        },
        '/api/projects/proj_x': { status: 200, body: CONTEXT },
      }),
    );
    renderScoped();

    await waitFor(() => expect(screen.getByText(/the read failed/i)).toBeTruthy());
    expect(screen.queryByText(/graph field · not mounted/i)).toBeNull();
    expect(screen.queryByTestId('graph-canvas')).toBeNull();
    // The independently successful registry backbone remains available.
    expect(screen.getByText('release/2.4')).toBeTruthy();
  });

  /**
   * An empty default slice is a measurement, and saying otherwise withheld one.
   *
   * `graph_response` maps every read failure to 500 `read_failed`, so a 200
   * carrying no nodes is the daemon reporting that the unseeded slice found
   * nothing to draw. The surface used to answer that with "the legacy response
   * cannot distinguish empty data from query failure" — a claim about the
   * contract that the contract contradicts, and one that left a genuinely
   * empty project looking like a broken read forever.
   */
  it('reports an empty default slice as an empty graph, not as an unverifiable one', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        '/api/projects/proj_x/plugins/graph/subgraph': { status: 200, body: SUBGRAPH_EMPTY },
        '/api/projects/proj_x': { status: 200, body: CONTEXT },
      }),
    );
    renderScoped();

    // Named, so the statement is about this project rather than the surface.
    await waitFor(() =>
      expect(screen.getByText(/no symbols are indexed for ai-train/i)).toBeTruthy(),
    );
    expect(screen.queryByText(/unverified/i)).toBeNull();
    expect(screen.queryByText(/the read failed/i)).toBeNull();
    expect(screen.queryByTestId('graph-canvas')).toBeNull();
  });

  /**
   * The other empty slice the same route can send, which is not the same fact.
   * A seeded request whose query matched nothing returns `seed_id: null` with
   * `mode: "seeded"` — that says the search found no symbol, and says nothing
   * about whether the project is indexed. Reading `mode` is what keeps the two
   * apart; asserting the empty-graph sentence for both would be a fabrication.
   */
  it('does not call a seeded miss an empty graph', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        '/api/projects/proj_x/plugins/graph/subgraph': {
          status: 200,
          body: { ...SUBGRAPH_EMPTY, mode: 'seeded', seed_id: null },
        },
        '/api/projects/proj_x': { status: 200, body: CONTEXT },
      }),
    );
    renderScoped();

    await waitFor(() => expect(screen.getByText(/no symbol matched/i)).toBeTruthy());
    expect(screen.queryByText(/no symbols are indexed/i)).toBeNull();
  });

  /**
   * The mirror of the case above, at the readout. Zero totals on a 200 are
   * counts that were really taken (`get_stats` runs `SELECT COUNT(*)`, and a
   * failure 500s), so withholding them printed a dash for a project that had
   * genuinely been measured as empty. The old rule was worse than that: it
   * blanked all three figures whenever ANY one was zero, so an indexed graph
   * with no edges lost its node and file counts too.
   */
  it('prints measured zero totals rather than withholding all three', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        '/api/projects/proj_x/plugins/graph/subgraph': { status: 200, body: SUBGRAPH },
        '/api/projects/proj_x/plugins/graph/overview': {
          status: 200,
          body: graphOverview({ nodes: 0, edges: 0, files: 0 }),
        },
        '/api/projects/proj_x': { status: 200, body: CONTEXT },
      }),
    );
    renderScoped();

    await waitFor(() => expect(screen.getByTestId('graph-canvas')).toBeTruthy());
    expect(screen.queryByText(/graph totals are unverified/i)).toBeNull();
    expect(readout('nodes')).toBe('0');
    expect(readout('edges')).toBe('0');
    expect(readout('files')).toBe('0');
  });

  it('keeps a real figure when a neighbouring one is zero', async () => {
    // The specific loss the all-or-nothing rule caused: one zero took the
    // other two measurements with it.
    vi.stubGlobal(
      'fetch',
      serve({
        '/api/projects/proj_x/plugins/graph/subgraph': { status: 200, body: SUBGRAPH },
        '/api/projects/proj_x/plugins/graph/overview': {
          status: 200,
          body: graphOverview({ nodes: 1204, edges: 0, files: 88 }),
        },
        '/api/projects/proj_x': { status: 200, body: CONTEXT },
      }),
    );
    renderScoped();

    await waitFor(() => expect(screen.getByTestId('graph-canvas')).toBeTruthy());
    expect(readout('nodes')).toBe('1,204');
    expect(readout('edges')).toBe('0');
    expect(readout('files')).toBe('88');
  });

  /**
   * The generated `available` and `exists` flags, honoured per source.
   *
   * Both payloads carry their counts as required non-nullable integers, so an
   * absent store answers with zeros — `available: false` and `exists: false`
   * are the only fields that say those zeros are not measurements. Reading the
   * numbers without the flags turned "there is no analytics store here" and
   * "this project has no memory bank" into "nothing has happened here", which
   * is the one reading the reader cannot tell apart from real quiet.
   */
  it('withholds counts a source flagged unavailable, and says why', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        '/api/projects/proj_x/plugins/graph/subgraph': { status: 200, body: SUBGRAPH },
        '/api/projects/proj_x/plugins/graph/overview': {
          status: 200,
          body: graphOverview({ nodes: 1204, edges: 3300, files: 88 }),
        },
        '/api/projects/proj_x/plugins/holographic/status': {
          status: 200,
          body: {
            ...MEMORY_STATUS,
            exists: false,
            error: 'no memory bank at /store/proj_x/memory.db',
            memory: { ...(MEMORY_STATUS['memory'] as object), fact_count: 0, entity_count: 0 },
          },
        },
        '/api/projects/proj_x/plugins/analytics/overview': {
          status: 200,
          body: {
            ...ANALYTICS,
            available: false,
            usage: { ...(ANALYTICS['usage'] as object), available: false, event_count: 0 },
          },
        },
        '/api/projects/proj_x': { status: 200, body: CONTEXT },
      }),
    );
    renderScoped();

    await waitFor(() => expect(screen.getByTestId('graph-canvas')).toBeTruthy());
    // The graph read is fine and stays measured: unavailability is per source.
    expect(readout('nodes')).toBe('1,204');
    expect(readout('files')).toBe('88');
    // The flagged sources withhold rather than reporting their zeros.
    expect(readout('facts')).toBe('—');
    expect(readout('entities')).toBe('—');
    expect(readout('events')).toBe('—');
    // And each dash is accounted for, in the source's own words where it sent
    // any — a withheld figure the reader cannot explain reads as a bug.
    expect(screen.getByText(/no memory bank at \/store\/proj_x\/memory\.db/)).toBeTruthy();
    expect(screen.getByText(/no analytics store/i)).toBeTruthy();
  });

  it('keeps a measured zero from an available source', async () => {
    // The counterpart, and the reason the flag is read instead of the number:
    // a store that IS available and counted zero has measured something, and
    // must not be blanked alongside one that measured nothing.
    vi.stubGlobal(
      'fetch',
      serve({
        '/api/projects/proj_x/plugins/graph/subgraph': { status: 200, body: SUBGRAPH },
        '/api/projects/proj_x/plugins/holographic/status': {
          status: 200,
          body: {
            ...MEMORY_STATUS,
            exists: true,
            error: '',
            memory: { ...(MEMORY_STATUS['memory'] as object), fact_count: 0, entity_count: 12 },
          },
        },
        '/api/projects/proj_x': { status: 200, body: CONTEXT },
      }),
    );
    renderScoped();

    await waitFor(() => expect(screen.getByTestId('graph-canvas')).toBeTruthy());
    expect(readout('facts')).toBe('0');
    expect(readout('entities')).toBe('12');
    expect(screen.queryByText(/no memory bank/i)).toBeNull();
  });

  /**
   * The registry backbone failing, told apart from a project that holds
   * nothing.
   *
   * `projects.rs::context` answers 503 with a complete payload whose `status`
   * is `registry_unavailable` and whose `stores`/`aliases` are empty — so a
   * rail that read those arrays without checking `status` drew the exact
   * picture an empty project draws. The reason it sent is the difference
   * between "this project has no stores" and "nothing could be read".
   */
  it('does not draw an unreadable registry as a project holding nothing', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        '/api/projects/proj_x/plugins/graph/subgraph': { status: 200, body: SUBGRAPH },
        '/api/projects/proj_x': {
          status: 503,
          body: {
            status: 'registry_unavailable',
            error: 'unable to open /home/x/.tracedecay/global.db',
            is_active: null,
            project: null,
            aliases: [],
            stores: [],
          },
        },
      }),
    );
    renderScoped();

    await waitFor(() =>
      expect(screen.getByText(/registry reported: registry_unavailable/i)).toBeTruthy(),
    );
    expect(screen.getByText(/unable to open \/home\/x\/\.tracedecay\/global\.db/)).toBeTruthy();
    // The store card the successful backbone draws must not be there.
    expect(screen.queryByText('release/2.4')).toBeNull();
  });

  it('prints an em dash rather than a number it was never given', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        '/api/projects/proj_x/plugins/graph/subgraph': {
          status: 500,
          body: { status: 'error' },
        },
        '/api/projects/proj_x': { status: 200, body: CONTEXT },
      }),
    );
    renderScoped();

    await waitFor(() => expect(screen.getByText(/the read failed/i)).toBeTruthy());
    for (const label of ['nodes', 'edges', 'files', 'facts', 'entities', 'events']) {
      expect(readout(label), `${label} was resolved from a failed read`).toBe('—');
    }
  });
});
