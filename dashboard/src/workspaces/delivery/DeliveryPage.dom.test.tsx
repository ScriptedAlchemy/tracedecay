import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
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
    artifact_count: 2,
    alias_count: 1,
    last_seen_at: NOW - 3600,
    ...over,
  };
}

/** The flat `projects` list `projects.rs::list` returns beside the grouped
 * tree. It is a `PublicCodeProject`, a narrower record than a registry entry. */
function publicProject(over: Record<string, unknown> = {}) {
  return {
    project_id: 'p1',
    label: 'repo',
    project_root: '/src/repo',
    canonical_root: '/src/repo',
    display_root: '/src/repo',
    git_common_dir: '/src/repo/.git',
    default_branch: 'main',
    created_at: NOW - 90 * DAY,
    last_seen_at: NOW - 3600,
    ...over,
  };
}

const PROJECT_TREE = [
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
];

/** `projects.rs::list` always answers with `limit`, `active_project_root` and
 * both views of the registry. The three nullable fields below come back null
 * together on the failure paths, never as empty collections. */
function registry(over: Record<string, unknown> = {}) {
  return {
    status: 'ok',
    limit: 100,
    truncated: false,
    active_project_id: 'p1',
    active_project_root: '/src/tracedecay',
    summary: { project_count: 4, repo_count: 3, truncated: false },
    projects: [
      publicProject({ project_id: 'p1', label: 'tracedecay', project_root: '/src/tracedecay' }),
      publicProject({
        project_id: 'p2',
        label: 'tracedecay (worktree)',
        project_root: '/src/tracedecay-wt',
        last_seen_at: NOW - 5 * DAY,
      }),
      publicProject({ project_id: 'p3', label: 'lynx', project_root: '/src/lynx' }),
      publicProject({
        project_id: 'p4',
        label: 'notes',
        project_root: '/src/notes',
        git_common_dir: null,
        default_branch: null,
      }),
    ],
    project_tree: PROJECT_TREE,
    ...over,
  };
}

const PROJECTS = registry();

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

const DELIVERY_OVERVIEW = {
  ...INDEX_FRESHNESS,
  coverage: {
    ...INDEX_FRESHNESS.coverage,
    completeness: 'partial',
    eligible: 8,
    examined: 3,
    omitted: 5,
    denominator: 8,
    unit: 'delivery sources',
    omission_reasons: ['external authorities are not mounted'],
  },
  freshness: {
    state: 'unknown',
    observed_at_micros: null,
    watermark: null,
  },
  domain_state: 'partial',
  payload: {
    changes: {
      state: 'ready',
      value: {
        schema_version: 'tracedecay.git-query.v1',
        repository: '/src/tracedecay',
        operation: 'none',
        head: { state: 'attached', branch: 'main', commit: 'a'.repeat(40) },
        staged: 0,
        unstaged: 1,
        conflicted: 0,
        untracked: 0,
        ignored: 0,
        changed_paths: ['src/lib.rs'],
      },
    },
    commits: {
      state: 'ready',
      value: {
        items: [
          {
            commit: 'a'.repeat(40),
            subject: 'bind delivery timeline',
            author_name: 'TraceDecay',
            author_email: 'dev@example.com',
            author_at_micros: 100,
            committer_at_micros: 100,
          },
          {
            commit: 'b'.repeat(40),
            subject: 'initial',
            author_name: 'TraceDecay',
            author_email: 'dev@example.com',
            author_at_micros: 90,
            committer_at_micros: 90,
          },
        ],
        truncated: false,
      },
    },
    pull_requests: {
      state: 'unavailable',
      required_authority: 'ProjectGitHubReviewStoreV1 in DashboardState',
      reason: 'GitHub review authority is not mounted',
    },
    review_comments: {
      state: 'unavailable',
      required_authority: 'ProjectGitHubReviewStoreV1 in DashboardState',
      reason: 'GitHub review authority is not mounted',
    },
    ci_checks: {
      state: 'unavailable',
      required_authority: 'CiReadOnlyProviderArchiveV1 in DashboardState',
      reason: 'CI archive is not mounted',
    },
    failure_localization: {
      state: 'unavailable',
      required_authority: 'CiExactEvidenceAuthorityV1 in DashboardState',
      reason: 'CI exact evidence authority is not mounted',
    },
    releases: {
      state: 'unsupported',
      required_authority: 'read-only release authority in DashboardState',
      reason: 'no reusable read-only release authority is implemented',
    },
    generation_freshness: {
      state: 'ready',
      value: {
        comparison: 'current',
        head_commit: 'a'.repeat(40),
        indexed_commit: 'a'.repeat(40),
      },
    },
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
  overview: unknown = DELIVERY_OVERVIEW,
) {
  const projectsBody =
    status === 200 && body !== null && typeof body === 'object' && 'status' in body
      ? fixtureEnvelope(body)
      : body;
  vi.stubGlobal(
    'fetch',
    serve({
      '/api/projects': { status, body: projectsBody },
      '/api/delivery/overview': { status: 200, body: overview },
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
      screen.getByText(
        /not when it was last committed to; commit history is shown separately\s+for the active checkout/,
      ),
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

  it('renders real Git projections and typed external authority gaps', async () => {
    renderDelivery();
    await screen.findByText('Changes & commits');
    expect(screen.getByText('Pull requests & review')).toBeTruthy();
    expect(screen.getByText('Continuous integration')).toBeTruthy();
    expect(screen.getByText('Releases')).toBeTruthy();
    expect(screen.getByText('Index freshness')).toBeTruthy();
    expect(screen.getByText(/2 commits · 1 changed path/)).toBeTruthy();
    expect(screen.getByText(/unavailable · GitHub review authority is not mounted/)).toBeTruthy();
    expect(screen.getByText(/no reusable read-only release authority is implemented/)).toBeTruthy();
    expect(screen.getByText('measured')).toBeTruthy();
  });

  it('discloses when the commit timeline is truncated', async () => {
    renderDelivery(PROJECTS, 200, {
      ...DELIVERY_OVERVIEW,
      payload: {
        ...DELIVERY_OVERVIEW.payload,
        commits: {
          ...DELIVERY_OVERVIEW.payload.commits,
          value: {
            ...DELIVERY_OVERVIEW.payload.commits.value,
            truncated: true,
          },
        },
      },
    });

    expect(
      await screen.findByText(/2 commits shown · more commits not shown/),
    ).toBeTruthy();
  });

  it('renders generation freshness from the reusable delivery projection', async () => {
    renderDelivery(PROJECTS, 200, {
      ...DELIVERY_OVERVIEW,
      payload: {
        ...DELIVERY_OVERVIEW.payload,
        generation_freshness: {
          state: 'ready',
          value: {
            comparison: 'behind',
            head_commit: 'c'.repeat(40),
            indexed_commit: 'a'.repeat(40),
          },
        },
      },
    });

    await screen.findByText('Stale');
    const staleRead = screen.getByText(/behind · HEAD cccccccc · indexed aaaaaaaa/);
    expect(staleRead).toBeTruthy();
    expect(
      within(staleRead.parentElement?.parentElement ?? document.body).getByText('unknown'),
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
    renderDelivery(
      registry({
        status: 'missing_registry',
        summary: null,
        projects: null,
        project_tree: null,
        truncated: null,
      }),
    );
    await screen.findByText(/registry reporting itself absent/);
    expect(screen.queryByText(/holds no repositories/)).toBeNull();
  });

  it('renders a failed registry read as a failure, never as an empty field', async () => {
    // `registry_unavailable` used to fall through `project_tree ?? []` and
    // draw the same "holds no repositories" pixels as a measured empty
    // registry — a refused read wearing an empty success.
    renderDelivery(
      registry({
        status: 'registry_unavailable',
        error: 'registry database could not be opened',
        summary: null,
        projects: null,
        project_tree: null,
        truncated: null,
      }),
    );
    await screen.findByText(/registry database could not be opened/);
    expect(screen.queryByText(/holds no repositories/)).toBeNull();
    expect(screen.queryByText(/Registry readings/)).toBeNull();
  });

  it('renders an empty registry as an answered question', async () => {
    renderDelivery(
      registry({
        status: 'ok',
        summary: { project_count: 0, repo_count: 0, truncated: false },
        projects: [],
        project_tree: [],
      }),
    );
    await screen.findByText(/answered and holds no repositories in this workspace/);
  });

  it('renders a distinct error state when the read fails', async () => {
    renderDelivery({ error: 'boom' }, 500);
    await waitFor(() => {
      expect(
        screen.getByText(/shape this build does not understand/),
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
