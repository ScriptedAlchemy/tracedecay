import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ScopedBrain } from './ScopedBrain.tsx';
import { useScope } from '../../data/scope/store.ts';

// The canvas is a WebGL renderer; this suite is about which reads compose the
// surface and what it says when one of them is legitimately unavailable.
vi.mock('../../viz/graph/GraphCanvas.tsx', () => ({
  GraphCanvas: ({ nodes, caption }: { nodes: unknown[]; caption: unknown }) => (
    <div data-testid="graph-canvas" data-node-count={nodes.length}>
      {caption as never}
    </div>
  ),
}));

const NOW = Math.floor(Date.now() / 1000);

/** The registry backbone (src/dashboard/projects.rs `context`) — resolves for
 * every registered project whether or not its graph is mounted. */
const CONTEXT = {
  status: 'ok',
  is_active: false,
  project: {
    project_id: 'proj_x',
    label: 'ai-train',
    project_root: '/fast/projects/ai-train',
    canonical_root: '/fast/projects/ai-train',
    kind: 'primary',
    default_branch: 'main',
    branches: ['main'],
    store_count: 1,
    graph_scope_count: 2,
    artifact_count: 4,
    alias_count: 3,
    last_seen_at: NOW - 3600,
  },
  aliases: [
    { alias_path: '/fast/projects/ai-train', last_seen_at: NOW - 3600 },
    { alias_path: '/fast/projects/ai-train/.worktrees/fix', last_seen_at: NOW - 7200 },
  ],
  stores: [
    {
      store: {
        store_id: 'store:proj_x:profile_sharded',
        store_kind: 'code_project',
        storage_mode: 'profile_sharded',
      },
      graph_scopes: [
        { graph_scope_id: 's1', branch_name: 'main', last_synced_at: NOW - 3600 },
        { graph_scope_id: 's2', branch_name: 'release/2.4', last_synced_at: NOW - 86_400 },
      ],
      artifacts: [
        { artifact_kind: 'graph_db', relpath: 'p/tracedecay.db', size_bytes: 131_088_384 },
        { artifact_kind: 'branch_meta', relpath: 'p/branch-meta.json', size_bytes: 14_704 },
      ],
    },
  ],
};

const SUBGRAPH = {
  nodes: [
    { id: 'a', kind: 'function', name: 'alpha', degree: 4 },
    { id: 'b', kind: 'struct', name: 'Beta', degree: 2 },
  ],
  edges: [{ source: 'a', target: 'b', kind: 'calls' }],
  capped: { nodes: false, edges: false },
};

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
        body: { totals: { nodes: 12_873, edges: 41_206, files: 642 } },
      },
      '/api/projects/proj_x/plugins/holographic/status': {
        status: 200,
        body: { exists: true, memory: { fact_count: 173, entity_count: 1186 } },
      },
      '/api/projects/proj_x/plugins/analytics/overview': {
        status: 200,
        body: { usage: { event_count: 42, by_category: [{ category: 'memory', events: 42 }] } },
      },
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
        '/api/projects/proj_x/plugins/graph/subgraph': {
          status: 200,
          body: { nodes: [], edges: [], capped: { nodes: false, edges: false } },
        },
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
          body: { totals: { nodes: 0, edges: 0, files: 0 } },
        },
        '/api/projects/proj_x': { status: 200, body: CONTEXT },
      }),
    );
    renderScoped();

    await waitFor(() => expect(screen.getByTestId('graph-canvas')).toBeTruthy());
    expect(screen.getByText(/graph totals are unverified/i)).toBeTruthy();
    expect(screen.getAllByText('—').length).toBeGreaterThanOrEqual(3);
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
    // nodes / edges / files / facts / entities / events all unresolved.
    expect(screen.getAllByText('—').length).toBeGreaterThanOrEqual(6);
  });
});
