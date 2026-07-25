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

  it('composes the unmounted-graph state out of what the registry does know', async () => {
    // Exactly the daemon's behaviour: the registry answers, the scoped gateway
    // does not, because only one project's graph is mounted at a time.
    vi.stubGlobal(
      'fetch',
      serve({ '/api/projects/proj_x': { status: 200, body: CONTEXT } }),
    );
    renderScoped();

    await waitFor(() =>
      expect(screen.getByText(/keeps one project's code graph mounted/i)).toBeTruthy(),
    );
    // No canvas is drawn, because there is no neighbourhood to draw.
    expect(screen.queryByTestId('graph-canvas')).toBeNull();
    // And the space is spent on the registry facts rather than on an apology:
    // one store, two branches indexed, 125.0 MiB on disk, two checkouts.
    // 125.0 MiB appears three times on purpose: the field's own reading, the
    // store card's total, and the graph_db artifact row that accounts for
    // essentially all of it.
    expect(screen.getAllByText('125.0').length).toBe(3);
    expect(screen.getAllByText('2').length).toBeGreaterThan(0);
    // The project's root, once as its identity and once as the checkout that
    // was most recently seen there.
    expect(screen.getAllByText('/fast/projects/ai-train').length).toBe(2);
  });

  it('prints an em dash rather than a number it was never given', async () => {
    vi.stubGlobal(
      'fetch',
      serve({ '/api/projects/proj_x': { status: 200, body: CONTEXT } }),
    );
    renderScoped();

    await waitFor(() =>
      expect(screen.getByText(/keeps one project's code graph mounted/i)).toBeTruthy(),
    );
    // nodes / edges / files / facts / entities / events all unresolved.
    expect(screen.getAllByText('—').length).toBeGreaterThanOrEqual(6);
  });
});
