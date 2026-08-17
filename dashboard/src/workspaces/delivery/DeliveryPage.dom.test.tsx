import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type {
  DeliveryOverviewV1,
  DeliveryPullRequestV1,
} from '../../contracts/generated.ts';
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

const SNAPSHOT = {
  coverage: 'complete' as const,
  fetched_at_micros: 101,
  merge_base_commit_id: 'd'.repeat(40),
  outcome: 'complete' as const,
  provider_base_commit_id: 'b'.repeat(40),
  provider_head_commit_id: 'a'.repeat(40),
};

function pullRequest(provider: string, pullRequestId = '42'): DeliveryPullRequestV1 {
  return {
    id: `${provider}:${pullRequestId}`,
    label: `${provider} PR ${pullRequestId}`,
    provider,
    pull_request_id: pullRequestId,
    operations: [
      {
        operation: 'pull_request' as const,
        latest_attempt: SNAPSHOT,
        last_complete: { ...SNAPSHOT, fetched_at_micros: 99 },
      },
    ],
  };
}

const RELEASE = {
  assets: [
    {
      asset_id: 9007199254740001,
      content_type: 'application/octet-stream',
      created_at_micros: 104,
      digest: 'sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789',
      download_count: 7,
      download_url: 'javascript:alert(2)',
      label: null,
      name: 'tracedecay-linux.tar.zst',
      size_bytes: 2048,
      updated_at_micros: 105,
    },
  ],
  created_at_micros: 102,
  draft: false,
  id: 'github:release:9007199254740000',
  label: 'v2.0.0',
  name: null,
  prerelease: false,
  published_at_micros: null,
  release_id: 9007199254740000,
  source_url: 'javascript:alert(1)',
  tag: 'v2.0.0',
};

function overviewWith(payload: Partial<DeliveryOverviewV1>) {
  return {
    ...DELIVERY_OVERVIEW,
    payload: { ...DELIVERY_OVERVIEW.payload, ...payload },
  };
}

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
      value: null,
    },
    review_comments: {
      state: 'unavailable',
      required_authority: 'ProjectGitHubReviewStoreV1 in DashboardState',
      reason: 'GitHub review authority is not mounted',
      value: null,
    },
    ci_checks: {
      state: 'unavailable',
      required_authority: 'CiReadOnlyProviderArchiveV1 in DashboardState',
      reason: 'CI archive is not mounted',
      value: null,
    },
    failure_localization: {
      state: 'unavailable',
      required_authority: 'CiExactEvidenceAuthorityV1 in DashboardState',
      reason: 'failure localization is not configured',
      value: null,
    },
    releases: {
      state: 'not_published',
      required_authority: 'read-only release authority in DashboardState',
      reason: 'release history has not been published',
    },
    generation_freshness: {
      state: 'ready',
      value: {
        comparison: 'current',
        head_commit: 'a'.repeat(40),
        indexed_commit: 'a'.repeat(40),
      },
    },
  } satisfies DeliveryOverviewV1,
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
    expect(screen.getAllByText('unknown').length).toBeGreaterThan(0);
  });

  it('renders real Git projections and typed external authority gaps', async () => {
    renderDelivery();
    await screen.findByText('Changes & commits');
    expect(screen.getByText('Pull requests & review')).toBeTruthy();
    expect(screen.getByText('Continuous integration')).toBeTruthy();
    expect(screen.getByText('Releases')).toBeTruthy();
    expect(screen.getByText('Index freshness')).toBeTruthy();
    expect(screen.getByText('2 commits')).toBeTruthy();
    expect(screen.getByText(/1 changed path/)).toBeTruthy();
    expect(
      screen.getAllByText(/unavailable · GitHub review authority is not mounted/),
    ).toHaveLength(2);
    expect(screen.getByText(/release history has not been published/)).toBeTruthy();
    expect(screen.getByText('measured')).toBeTruthy();
  });

  it('keeps stale provider-qualified PR evidence visible at its retained head', async () => {
    renderDelivery(PROJECTS, 200, overviewWith({
      pull_requests: {
        state: 'stale',
        value: {
          expected_head_commit: 'c'.repeat(40),
          retained_head_commit: 'a'.repeat(40),
          truncated: false,
          items: [pullRequest('github'), pullRequest('gitlab')],
        },
      },
    }));

    const projection = await screen.findByRole('region', { name: 'Pull requests' });
    expect(projection.getAttribute('data-projection-state')).toBe('stale');
    expect(within(projection).getByText('github · PR 42')).toBeTruthy();
    expect(within(projection).getByText('gitlab · PR 42')).toBeTruthy();
    expect(within(projection).getByText('cccccccc')).toBeTruthy();
    expect(within(projection).getAllByText('aaaaaaaa').length).toBeGreaterThan(0);
    expect(within(projection).getAllByLabelText('latest attempt')).toHaveLength(2);
    expect(within(projection).getAllByLabelText('last complete')).toHaveLength(2);
  });

  it('preserves every review observation and suppresses absent optional identities', async () => {
    renderDelivery(PROJECTS, 200, overviewWith({
      review_comments: {
        state: 'partial',
        value: {
          expected_head_commit: 'a'.repeat(40),
          retained_head_commit: 'a'.repeat(40),
          truncated: true,
          items: [
            {
              comment_id: 'comment-7',
              id: 'github:42:comment-7',
              label: 'review comment 7',
              provider: 'github',
              pull_request_id: '42',
              observations: [
                {
                  author_class: 'maintainer',
                  kind: 'latest_attempt',
                  lifecycle: 'edited',
                  observed_at_micros: 108,
                  operation: 'review_comments',
                  provider_outcome: 'partial',
                  reply_to_comment_id: null,
                  repository_id: 'repo-1',
                  review_id: 'review-9',
                  review_state: 'changes_requested',
                  source_url: null,
                  thread_id: null,
                  version_digest: 'sha256:latest-review-version',
                },
                {
                  author_class: 'bot',
                  kind: 'last_complete',
                  lifecycle: 'resolved',
                  observed_at_micros: 100,
                  operation: 'review_comments',
                  provider_outcome: 'complete',
                  reply_to_comment_id: 'comment-1',
                  repository_id: 'repo-1',
                  review_id: null,
                  review_state: 'approved',
                  source_url: 'javascript:alert(1)',
                  thread_id: 'thread-3',
                  version_digest: 'sha256:complete-review-version',
                },
              ],
            },
          ],
        },
      },
    }));

    const projection = await screen.findByRole('region', { name: 'Review observations' });
    expect(projection.getAttribute('data-projection-state')).toBe('partial');
    expect(within(projection).getByText('github · PR 42 · comment comment-7')).toBeTruthy();
    expect(within(projection).getByText('latest attempt')).toBeTruthy();
    expect(within(projection).getByText('last complete')).toBeTruthy();
    expect(within(projection).getByText('review-9')).toBeTruthy();
    expect(within(projection).getByText('thread-3')).toBeTruthy();
    expect(within(projection).getByText('comment-1')).toBeTruthy();
    expect(within(projection).getByText(/more evidence not shown/)).toBeTruthy();
    expect(within(projection).queryByText('null')).toBeNull();
    expect(within(projection).queryByText(/javascript:/)).toBeNull();
    expect(within(projection).queryByRole('link')).toBeNull();
  });

  it('keeps opaque CI identities and nullable conclusions exact', async () => {
    renderDelivery(PROJECTS, 200, overviewWith({
      ci_checks: {
        state: 'ready',
        value: {
          expected_head_commit: 'a'.repeat(40),
          retained_head_commit: 'a'.repeat(40),
          truncated: false,
          items: [
            {
              annotation_count: 3,
              check_conclusion: 'failure',
              check_status: 'completed',
              failed_step: null,
              failure_kind: 'test_failure',
              id: 'ci-secondary-label',
              job_conclusion: null,
              job_status: 'in_progress',
              label: 'unit tests',
              observation_id: 'observation-ci-1',
              observed_at_micros: 106,
              provider_head_commit: 'a'.repeat(40),
              run: {
                attempt_id: 'attempt-001-alpha',
                check_run_id: 'check-009-z',
                check_suite_id: 'suite-007-x',
                job_id: 'job-0004-y',
                run_id: 'run-0002-beta',
                workflow_id: 'workflow-0001-alpha',
              },
              workflow_conclusion: null,
              workflow_path: '.github/workflows/ci.yml',
              workflow_status: 'queued',
            },
          ],
        },
      },
      failure_localization: {
        state: 'unavailable',
        required_authority: 'retained exact graph localization authority',
        reason: 'failure localization is not configured',
        value: null,
      },
    }));

    const checks = await screen.findByRole('region', { name: 'CI checks' });
    expect(within(checks).getByText('workflow-0001-alpha')).toBeTruthy();
    expect(within(checks).getByText('run-0002-beta')).toBeTruthy();
    expect(within(checks).getByText('attempt-001-alpha')).toBeTruthy();
    expect(within(checks).getByText('suite-007-x')).toBeTruthy();
    expect(within(checks).getByText('job-0004-y')).toBeTruthy();
    expect(within(checks).getByText('check-009-z')).toBeTruthy();
    expect(within(checks).getByText('queued')).toBeTruthy();
    expect(within(checks).getByText('in progress')).toBeTruthy();
    expect(within(checks).getByText('completed · failure')).toBeTruthy();
    expect(within(checks).queryByText(/success/)).toBeNull();

    const localization = screen.getByRole('region', { name: 'Failure localization' });
    expect(localization.getAttribute('data-projection-state')).toBe('unavailable');
    expect(within(localization).getByText(/not configured/)).toBeTruthy();
    expect(within(localization).queryByText('observation-ci-1')).toBeNull();
  });

  it.each([
    ['ready', { state: 'ready', value: { items: [RELEASE], truncated: false } }, true],
    ['partial', { state: 'partial', value: { items: [RELEASE], truncated: false } }, true],
    ['stale', { state: 'stale', value: { items: [RELEASE], truncated: false } }, true],
    ['failed', { state: 'failed', value: { items: [RELEASE], truncated: false } }, true],
    [
      'rate_limited',
      {
        state: 'rate_limited',
        checkpoint: { limit: 60, remaining: 0, reset_at_micros: 111 },
        retry_at_micros: 112,
        value: { items: [RELEASE], truncated: false },
      },
      true,
    ],
    [
      'unavailable',
      {
        state: 'unavailable',
        reason: 'provider read interrupted',
        required_authority: 'release read authority',
        value: { items: [RELEASE], truncated: false },
      },
      true,
    ],
    ['denied', { state: 'denied', value: { items: [RELEASE], truncated: false } }, false],
    [
      'not_published',
      {
        state: 'not_published',
        reason: 'no release page retained',
        required_authority: 'release read authority',
      },
      false,
    ],
    ['empty_measured', { state: 'empty_measured', value: { items: [], truncated: false } }, false],
  ] satisfies Array<[string, DeliveryOverviewV1['releases'], boolean]>)(
    'renders the %s release projection without changing its state',
    async (state, releases, showsRetainedValue) => {
      renderDelivery(PROJECTS, 200, overviewWith({ releases }));
      const projection = await screen.findByRole('region', { name: 'Release history' });
      expect(projection.getAttribute('data-projection-state')).toBe(state);
      if (showsRetainedValue) {
        expect(within(projection).getByText('v2.0.0')).toBeTruthy();
      } else {
        expect(within(projection).queryByText('v2.0.0')).toBeNull();
      }
    },
  );

  it('draws rate-limited ingress as its own chip, never as an ordinary partial', async () => {
    renderDelivery(PROJECTS, 200, overviewWith({
      releases: {
        state: 'rate_limited',
        checkpoint: { limit: 60, remaining: 0, reset_at_micros: 111 },
        retry_at_micros: 112,
        value: { items: [RELEASE], truncated: false },
      },
    }));

    const projection = await screen.findByRole('region', { name: 'Release history' });
    // The condition is carried by the chip itself — label, glyph, data-state —
    // not only by detail text a reader would have to parse out of a clause.
    const chip = projection.querySelector('[data-state]');
    expect(chip?.getAttribute('data-state')).toBe('rate_limited');
    expect(within(projection).getByText('Rate limited')).toBeTruthy();
    expect(within(projection).queryByText('Partial')).toBeNull();
    // The quota evidence stays in the detail: what remains, when it resets,
    // and when to retry.
    expect(within(projection).getByText(/0\/60 remaining · reset 111 µs · retry 112 µs/)).toBeTruthy();
    // Retained evidence still renders under the chip.
    expect(within(projection).getByText('v2.0.0')).toBeTruthy();
  });

  it('renders release assets as inert evidence and suppresses null fields and URLs', async () => {
    renderDelivery(PROJECTS, 200, overviewWith({
      releases: { state: 'ready', value: { items: [RELEASE], truncated: true } },
    }));

    const projection = await screen.findByRole('region', { name: 'Release history' });
    expect(within(projection).getByText('tracedecay-linux.tar.zst')).toBeTruthy();
    expect(within(projection).getByText(/2048 bytes · 7 downloads/)).toBeTruthy();
    expect(within(projection).getByText(/more evidence not shown/)).toBeTruthy();
    expect(within(projection).queryByText('null')).toBeNull();
    expect(within(projection).queryByText(/javascript:/)).toBeNull();
    expect(within(projection).queryByRole('link')).toBeNull();
  });

  it('suppresses a non-authorized envelope even when it carries a payload', async () => {
    renderDelivery(PROJECTS, 200, {
      ...overviewWith({
        releases: { state: 'ready', value: { items: [RELEASE], truncated: false } },
      }),
      authorization: { outcome: 'denied' },
      domain_state: 'denied',
    });

    await screen.findByText(/delivery evidence was not disclosed/);
    expect(screen.queryByRole('region', { name: 'Release history' })).toBeNull();
    expect(screen.queryByText('v2.0.0')).toBeNull();
  });

  it('suppresses a null latest snapshot without backfilling it from last complete', async () => {
    const item = pullRequest('github');
    const operation = item.operations[0];
    if (!operation) throw new Error('pull request fixture has no operation');
    item.operations[0] = {
      ...operation,
      latest_attempt: null,
    };
    renderDelivery(PROJECTS, 200, overviewWith({
      pull_requests: {
        state: 'partial',
        value: {
          expected_head_commit: 'c'.repeat(40),
          retained_head_commit: 'a'.repeat(40),
          items: [item],
          truncated: true,
        },
      },
    }));

    const projection = await screen.findByRole('region', { name: 'Pull requests' });
    expect(within(projection).queryByLabelText('latest attempt')).toBeNull();
    expect(within(projection).getByLabelText('last complete')).toBeTruthy();
    expect(within(projection).getByText(/more evidence not shown/)).toBeTruthy();
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
      await screen.findByText(/2 commits shown · more evidence not shown/),
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
            comparison: 'mismatch',
            head_commit: 'c'.repeat(40),
            indexed_commit: 'a'.repeat(40),
          },
        },
      },
    });

    await screen.findByText('Stale');
    const staleRead = screen.getByText(/mismatch · HEAD cccccccc · indexed aaaaaaaa/);
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
