import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { DeliveryPage } from './DeliveryPage.tsx';

/**
 * Delivery's failure mode is not a crash, it is a plausible lie: a surface
 * about shipping code, showing a "recency" axis that a reader will take for
 * commit recency when it is really index recency, and showing "0 branches" for
 * directories that are not repositories at all.
 *
 * Every test below pins one of those distinctions to a sentence on the page.
 */

const NOW = Math.floor(Date.now() / 1000);
const DAY = 86_400;

function checkout(over: Record<string, unknown> = {}) {
  return {
    project_id: 'p1',
    label: 'repo',
    project_root: '/src/repo',
    canonical_root: '/src/repo',
    kind: 'primary',
    default_branch: 'main',
    branches: [],
    store_count: 1,
    graph_scope_count: 1,
    artifact_count: 2,
    alias_count: 1,
    last_seen_at: NOW - 3600,
    ...over,
  };
}

const PROJECTS = {
  status: 'ok',
  truncated: false,
  active_project_id: 'p1',
  summary: { project_count: 4, repo_count: 3, truncated: false },
  project_tree: [
    {
      label: 'tracedecay',
      git_common_dir: '/src/tracedecay/.git',
      project_count: 2,
      branches: ['main', 'feat/a', 'feat/b', 'fix/c'],
      projects: [
        checkout({ project_id: 'p1', label: 'tracedecay', is_active: true }),
        checkout({
          project_id: 'p2',
          label: 'tracedecay (worktree)',
          kind: 'worktree',
          project_root: '/src/tracedecay-wt',
          last_seen_at: NOW - 5 * DAY,
        }),
      ],
    },
    {
      label: 'lynx',
      git_common_dir: '/src/lynx/.git',
      project_count: 1,
      branches: ['main'],
      projects: [
        checkout({ project_id: 'p3', label: 'lynx', last_seen_at: NOW - 40 * DAY }),
      ],
    },
    {
      label: 'notes',
      git_common_dir: null,
      project_count: 1,
      branches: [],
      projects: [
        checkout({
          project_id: 'p4',
          label: 'notes',
          kind: 'project',
          default_branch: null,
          last_seen_at: NOW - 2 * DAY,
        }),
      ],
    },
  ],
};

const INDEX_FRESHNESS = {
  schema_revision: 1,
  scope: {
    project_id: 'p1',
    storage_mode: 'profile_sharded',
    store_root: '/data/projects/p1',
  },
  version: { entity_version: null, graph_version: null },
  time: { valid_time_micros: null, observation_time_micros: 100 },
  source_watermark: null,
  authorization: { outcome: 'authorized' },
  coverage: {
    completeness: 'unsupported',
    eligible: null,
    examined: null,
    matched: null,
    excluded: null,
    omitted: null,
    unknown: null,
    denominator: null,
    unit: null,
    omission_reasons: [],
  },
  freshness: {
    state: 'unsupported',
    observed_at_micros: null,
    watermark: null,
  },
  domain_state: 'unsupported',
  legal_actions: [
    {
      kind: 'refresh',
      operation: 'use-case.dashboard.code-index.freshness.refresh',
    },
  ],
  payload: {
    worktrees: [],
    required_source: 'CodeIndexSchedulerRegistry read port',
    note: 'generation source is not wired',
  },
};

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

function renderDelivery(
  body: unknown = PROJECTS,
  status = 200,
  freshness: unknown = INDEX_FRESHNESS,
) {
  vi.stubGlobal(
    'fetch',
    serve({
      '/api/projects': { status, body },
      '/api/code-index/freshness': { status: 200, body: freshness },
    }),
  );
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <DeliveryPage />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('DeliveryPage', () => {
  it('says the recency axis is index time, not commit time', async () => {
    renderDelivery();
    await screen.findByText('last indexed across · branches up');
    expect(
      screen.getByText(/not when it was last committed to; the daemon serves no commit\s+times/),
    ).toBeTruthy();
  });

  it('draws non-git entries as unknown branches rather than zero', async () => {
    renderDelivery();
    await screen.findByText('last indexed across · branches up');
    expect(
      screen.getByText(
        /they have no git directory, so their branch count is unknown rather than zero/,
      ),
    ).toBeTruthy();
    // ...and the table agrees with the field.
    expect(screen.getByText('unknown')).toBeTruthy();
  });

  it('names the whole unserved pipeline instead of showing empty tables', async () => {
    renderDelivery();
    await screen.findByText('Changes & commits');
    expect(screen.getByText('Pull requests & review')).toBeTruthy();
    expect(screen.getByText('Continuous integration')).toBeTruthy();
    expect(screen.getByText('Releases')).toBeTruthy();
    expect(screen.getByText('Index freshness')).toBeTruthy();
    expect(screen.getByText(/no commit route; branch names only/)).toBeTruthy();
  });

  it('renders generation freshness from the typed daemon envelope', async () => {
    renderDelivery(PROJECTS, 200, {
      ...INDEX_FRESHNESS,
      coverage: {
        ...INDEX_FRESHNESS.coverage,
        completeness: 'partial',
        eligible: 2,
        examined: 1,
        omitted: 1,
        denominator: 2,
        unit: 'worktrees',
        omission_reasons: ['one scheduler generation is unavailable'],
      },
      freshness: {
        state: 'stale',
        observed_at_micros: 90,
        watermark: 'generation:4',
      },
      domain_state: 'partial',
    });

    await screen.findByText('Partial');
    expect(
      screen.getByText(/generation state stale · 1 of 2 worktrees examined/),
    ).toBeTruthy();
  });

  it('uses the shared ordered freshness meter beside explicit index age', async () => {
    renderDelivery();
    expect((await screen.findAllByTitle('live — under a day')).length).toBeGreaterThan(0);
    expect(screen.getAllByText('1h ago').length).toBeGreaterThan(0);
  });

  it('scans as one row per repository, not a header-plus-row pair', async () => {
    renderDelivery();
    await screen.findByRole('table');
    const rows = screen.getAllByRole('row');
    // One header row plus one row per repository — three repositories here.
    expect(rows).toHaveLength(4);
  });

  it('expands a selected repository to its checkouts and branch names', async () => {
    renderDelivery();
    const row = await screen.findByText('tracedecay');
    await userEvent.click(row);
    await screen.findByText('branch names');
    expect(screen.getByText('/src/tracedecay-wt')).toBeTruthy();
    expect(screen.getByText('feat/a')).toBeTruthy();
    expect(
      screen.getByText(/records no tip commit, author or time/),
    ).toBeTruthy();
  });

  it('reports a non-git repository as unsupported rather than zero-findings', async () => {
    renderDelivery();
    const row = await screen.findByText('notes');
    await userEvent.click(row);
    await screen.findByText('branch names');
    expect(screen.getByText(/not a git checkout/)).toBeTruthy();
  });

  it('separates a missing registry from an empty one', async () => {
    renderDelivery({ status: 'missing_registry', project_tree: [] });
    await screen.findByText(/registry reporting itself absent/);
    expect(screen.queryByText(/holds no repositories/)).toBeNull();
  });

  it('renders an empty registry as an answered question', async () => {
    renderDelivery({ status: 'ok', project_tree: [] });
    await screen.findByText(/answered and holds no repositories in this workspace/);
  });

  it('renders a distinct error state when the read fails', async () => {
    renderDelivery({ error: 'boom' }, 500);
    await waitFor(() => {
      expect(
        screen.getByText(/nothing is being invented in its place/),
      ).toBeTruthy();
    });
  });

  it('gives the field an accessible description naming both axes and the gap', async () => {
    renderDelivery();
    const figure = await screen.findByRole('img', { name: /Delivery field:/ });
    const label = figure.getAttribute('aria-label') ?? '';
    expect(label).toContain('last indexed them');
    expect(label).toContain('branch count');
    expect(label).toContain('no git directory and no branch measurement');
  });
});
