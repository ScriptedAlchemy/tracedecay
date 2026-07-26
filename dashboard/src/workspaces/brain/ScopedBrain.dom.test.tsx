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

  it('separates an HTTP-success empty slice from transport failure without claiming zero', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        '/api/projects/proj_x/plugins/graph/subgraph': { status: 200, body: SUBGRAPH_EMPTY },
        '/api/projects/proj_x': { status: 200, body: CONTEXT },
      }),
    );
    renderScoped();

    await waitFor(() => expect(screen.getByText(/graph slice is unverified/i)).toBeTruthy());
    expect(screen.queryByText(/the read failed/i)).toBeNull();
    expect(screen.queryByText(/graph field · not mounted/i)).toBeNull();
    expect(screen.queryByTestId('graph-canvas')).toBeNull();
  });

  it('does not present collapsed scoped overview zeros as measured graph totals', async () => {
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
    expect(screen.getByText(/graph totals are unverified/i)).toBeTruthy();
    // The three figures the collapsed overview reported as zero are the claim.
    expect(readout('nodes')).toBe('—');
    expect(readout('edges')).toBe('—');
    expect(readout('files')).toBe('—');
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
