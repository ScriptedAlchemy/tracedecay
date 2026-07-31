import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor, within } from '@testing-library/react';
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
    eligible: 9,
    examined: 3,
    omitted: 6,
    denominator: 9,
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

/** The delivery envelope with named source projections replaced, and — when a
 * case is about the envelope itself rather than its payload — envelope fields
 * replaced too. */
function deliveryOverview(
  sources: Record<string, unknown> = {},
  envelope: Record<string, unknown> = {},
) {
  return {
    ...DELIVERY_OVERVIEW,
    ...envelope,
    payload: { ...DELIVERY_OVERVIEW.payload, ...sources },
  };
}

/** One CI check record. `source_degradation` is the field this page has to read:
 * the archive can hand back a validated run whose provider read was throttled or
 * failed underneath it. */
function ciCheck(over: Record<string, unknown> = {}) {
  return {
    provider: 'github',
    workflow_id: 'wf-1',
    run_id: 'run-1',
    attempt_id: '1',
    job_id: 'job-1',
    check_suite_id: 'suite-1',
    check_run_id: 'check-1',
    state: 'complete',
    coverage: 'complete',
    failures: 1,
    checks: 4,
    annotations: 2,
    source_degradation: null,
    ...over,
  };
}

function hostComponent(over: Record<string, unknown> = {}) {
  return {
    host: 'codex',
    component: 'core',
    state: 'current',
    registration: 'current',
    evidence_source: '/profile/host-components/codex-core.receipt',
    artifact_count: 2,
    ...over,
  };
}

/** The stage a pipeline row draws into, found by its own label. */
function stage(label: string): HTMLElement {
  return screen.getByText(label).parentElement as HTMLElement;
}

/** Domain states a stage rendered, in render order. The chip's `data-state` is
 * the stable selector for this — the label text is presentation. */
function statesIn(element: HTMLElement): string[] {
  return [...element.querySelectorAll('[data-state]')].map(
    (chip) => chip.getAttribute('data-state') ?? '',
  );
}

function renderDelivery(
  body: unknown = PROJECTS,
  status = 200,
  overview: unknown = DELIVERY_OVERVIEW,
) {
  vi.stubGlobal(
    'fetch',
    serve({
      '/api/projects': { status, body },
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
    expect(screen.getByText(/no reusable read-only release authority is implemented/)).toBeTruthy();
    expect(screen.getByText('measured')).toBeTruthy();
  });

  /** Two sources behind one authority report the same gap. Both are named, and
   * the authority's sentence is printed once. */
  it('names both sources of a shared authority gap without repeating its reason', async () => {
    renderDelivery();

    await screen.findByText('Pull requests & review');
    const review = stage('Pull requests & review');
    expect(statesIn(review)).toEqual(['unavailable']);
    expect(
      within(review).getAllByText(/GitHub review authority is not mounted/),
    ).toHaveLength(1);
    expect(
      within(review).getByText(/pull requests and review comments/),
    ).toBeTruthy();
  });

  /**
   * The envelope the payload arrives in, which this panel used to discard
   * entirely. Its coverage is the only statement of how much of the read
   * completed, and without it nine sources with six gaps looked the same as nine
   * sources with none.
   */
  it('states how much of the delivery read completed, in the unit the server named', async () => {
    renderDelivery();

    const truth = await screen.findByRole('group', { name: 'Delivery read truth' });
    expect(within(truth).getByText(/3 of 9 delivery sources complete/)).toBeTruthy();
    expect(statesIn(truth)).toEqual(['partial']);
    expect(within(truth).getByText(/freshness unknown · no observation stamp/)).toBeTruthy();
    expect(
      within(screen.getByRole('list', { name: 'Why this delivery read is incomplete' })).getByText(
        'external authorities are not mounted',
      ),
    ).toBeTruthy();
  });

  it('reports an unreported source count as unreported rather than as zero', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({}, {
      coverage: {
        ...DELIVERY_OVERVIEW.coverage,
        completeness: 'unknown',
        eligible: null,
        examined: null,
        omitted: null,
        denominator: null,
        omission_reasons: [],
      },
      domain_state: 'unknown',
    }));

    const truth = await screen.findByRole('group', { name: 'Delivery read truth' });
    expect(within(truth).getByText(/no delivery sources count reported/)).toBeTruthy();
    expect(within(truth).queryByText(/0 of/)).toBeNull();
  });

  /**
   * A refusal and an absence are different facts with different next actions.
   * `denied` is this identity not being allowed to see the read; `unavailable` is
   * a source that could not answer anyone. Rendering the first as the second
   * tells the reader to mount an authority that is already mounted.
   */
  it('renders a denied read authorization as denied, not as unavailable', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({}, {
      authorization: { outcome: 'denied' },
      domain_state: 'denied',
    }));

    const truth = await screen.findByRole('group', { name: 'Delivery read truth' });
    expect(statesIn(truth)).toEqual(['denied', 'denied']);
    expect(within(truth).getByText(/delivery read authorization/)).toBeTruthy();
    expect(truth.querySelector('[data-state="unavailable"]')).toBeNull();
    expect(truth.querySelector('[data-state="error"]')).toBeNull();
    expect(truth.querySelector('[data-state="partial"]')).toBeNull();
  });

  it('keeps a redacted read distinct from a denied one', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({}, {
      authorization: { outcome: 'redacted' },
    }));

    const truth = await screen.findByRole('group', { name: 'Delivery read truth' });
    expect(truth.querySelector('[data-state="redacted"]')).toBeTruthy();
    expect(truth.querySelector('[data-state="denied"]')).toBeNull();
  });

  it('renders retained partial provider evidence as partial, and says so once', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      pull_requests: {
        state: 'partial',
        value: { items: [] },
        reason: 'GitHub returned partial review coverage',
      },
    }));

    await screen.findByText('Pull requests & review');
    const review = stage('Pull requests & review');
    expect(statesIn(review)).toEqual(['unavailable', 'partial']);
    // Once. The reason used to be printed inside the chip and again beside it,
    // which reads as two separate findings about one source.
    expect(
      within(review).getAllByText(/GitHub returned partial review coverage/),
    ).toHaveLength(1);
    // A partial read still measured something, and the count survives the
    // degradation instead of vanishing with it.
    expect(within(review).getByText(/0 pull requests/)).toBeTruthy();
  });

  /**
   * The pairing defect. Each stage covers two sources, and the old panel drew
   * "the first one that is not ready" — so a source that could not be read at all
   * hid behind its partner being merely old.
   */
  it('leads a paired stage with the worse of its two source states', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      ci_checks: {
        state: 'stale',
        reason: 'the retained CI evidence is stale',
        value: { items: [ciCheck()] },
      },
      failure_localization: {
        state: 'failed',
        reason: 'the retained CI evidence read failed',
      },
    }));

    await screen.findByText('Continuous integration');
    const ci = stage('Continuous integration');
    expect(statesIn(ci)).toEqual(['error', 'stale']);
    expect(within(ci).getByText(/the retained CI evidence read failed/)).toBeTruthy();
    expect(within(ci).getByText(/the retained CI evidence is stale/)).toBeTruthy();
    expect(ci.querySelector('[data-state="ready"]')).toBeNull();
  });

  it('draws a stage as ready only when every source in it is ready', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      pull_requests: { state: 'ready', value: { items: [] } },
      review_comments: { state: 'ready', value: { items: [] } },
      ci_checks: { state: 'ready', value: { items: [ciCheck()] } },
      failure_localization: { state: 'ready', value: { items: [] } },
      releases: { state: 'ready', value: { items: [] } },
      host_evidence: { state: 'ready', value: { items: [hostComponent()] } },
    }, {
      domain_state: 'ready',
      coverage: {
        ...DELIVERY_OVERVIEW.coverage,
        completeness: 'complete',
        eligible: 9,
        examined: 9,
        omitted: 0,
        omission_reasons: [],
      },
    }));

    await screen.findByText('Continuous integration');
    expect(statesIn(stage('Continuous integration'))).toEqual(['ready']);
    expect(statesIn(stage('Pull requests & review'))).toEqual(['ready']);
    expect(statesIn(stage('Releases'))).toEqual(['ready']);
    const truth = screen.getByRole('group', { name: 'Delivery read truth' });
    expect(within(truth).getByText(/9 of 9 delivery sources complete/)).toBeTruthy();
  });

  /**
   * The CI archive can return a complete, validated run whose provider read was
   * degraded underneath it: the projection is legitimately `ready`, and drawing
   * that as a green Ready claimed the run was fully read when it was not.
   */
  it.each([
    ['failed', 'error', /the provider read failed for this run/],
    ['rate_limited', 'partial', /rate limited, so this run is not fully read/],
  ] as const)(
    'never greens a ready CI projection whose provider read was %s',
    async (degradation, state, detail) => {
      renderDelivery(PROJECTS, 200, deliveryOverview({
        ci_checks: {
          state: 'ready',
          value: { items: [ciCheck({ source_degradation: degradation })] },
        },
        failure_localization: { state: 'ready', value: { items: [] } },
      }));

      await screen.findByText('Continuous integration');
      const ci = stage('Continuous integration');
      expect(statesIn(ci)).toEqual([state]);
      expect(within(ci).getByText(detail)).toBeTruthy();
      expect(ci.querySelector('[data-state="ready"]')).toBeNull();
      // The counts the read did produce are still stated.
      expect(within(ci).getByText(/1 check · 0 localized failures/)).toBeTruthy();
    },
  );

  it('keeps a failed provider read distinct from a rate-limited one', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      ci_checks: {
        state: 'ready',
        value: {
          items: [
            ciCheck({ run_id: 'run-1', source_degradation: 'rate_limited' }),
            ciCheck({ run_id: 'run-2', provider: 'buildkite', source_degradation: 'failed' }),
          ],
        },
      },
      failure_localization: { state: 'ready', value: { items: [] } },
    }));

    await screen.findByText('Continuous integration');
    const ci = stage('Continuous integration');
    expect(statesIn(ci)).toEqual(['error', 'partial']);
  });

  it('names a source degradation this build does not recognize', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      ci_checks: {
        state: 'ready',
        value: { items: [ciCheck({ source_degradation: 'quota_exhausted' })] },
      },
      failure_localization: { state: 'ready', value: { items: [] } },
    }));

    await screen.findByText('Continuous integration');
    const ci = stage('Continuous integration');
    expect(statesIn(ci)).toEqual(['unknown']);
    expect(within(ci).getByText(/quota_exhausted, which this build does not recognize/)).toBeTruthy();
  });

  it('renders receipt-backed host evidence as accessible per-component rows', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      host_evidence: {
        state: 'ready',
        value: {
          items: [
            hostComponent(),
            hostComponent({
              host: 'first_party_catalog',
              component: null,
              state: 'observed',
              registration: null,
              evidence_source: 'checked_in_native_edit_stop_fixtures',
              artifact_count: 5,
            }),
          ],
        },
      },
    }));

    expect(await screen.findByText('Host integrations')).toBeTruthy();
    const components = screen.getByRole('list', { name: 'Host integration components' });
    expect(within(components).getAllByRole('group')).toHaveLength(2);

    const codex = screen.getByRole('group', { name: 'codex · core' });
    expect(statesIn(codex)).toEqual(['ready', 'ready']);
    expect(within(codex).getByText(/component current/)).toBeTruthy();
    expect(within(codex).getByText(/registration current/)).toBeTruthy();
    expect(within(codex).getByText('2 artifacts')).toBeTruthy();
    expect(
      within(codex).getByText('/profile/host-components/codex-core.receipt'),
    ).toBeTruthy();

    // A row the doctor observed without grading is neither ready nor degraded.
    const catalog = screen.getByRole('group', { name: 'first_party_catalog · bundle' });
    expect(statesIn(catalog)).toEqual(['unknown']);
    expect(within(stage('Host integrations')).getByText(/2 component rows · 0 degraded · 7 artifacts/))
      .toBeTruthy();
  });

  it.each([
    ['repairable', 'partial'],
    ['ownership_conflict', 'conflicting'],
    ['missing', 'unavailable'],
    ['corrupt', 'error'],
  ] as const)(
    'renders a %s component as its own degradation rather than as complete',
    async (state, expected) => {
      renderDelivery(PROJECTS, 200, deliveryOverview({
        host_evidence: {
          state: 'partial',
          reason: `host receipt evidence includes a ${state.replace('_', ' ')} component`,
          value: { items: [hostComponent({ state })] },
        },
      }));

      await screen.findByText('Host integrations');
      const row = screen.getByRole('group', { name: 'codex · core' });
      expect(statesIn(row)).toEqual([expected, 'ready']);
      expect(within(row).getByText(new RegExp(`component ${state.replace('_', ' ')}`))).toBeTruthy();
      const hosts = stage('Host integrations');
      expect(within(hosts).getByText(/Retained by this read: 1 component row · 1 degraded/))
        .toBeTruthy();
      expect(hosts.querySelector('[data-state="partial"]')).toBeTruthy();
    },
  );

  /** Registration is a second, independently measured axis: a component whose
   * files are current can still be unregistered with its host. */
  it('reports registration degradation on a component that is itself current', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      host_evidence: {
        state: 'partial',
        reason: 'host receipt evidence includes an unregistered component',
        value: { items: [hostComponent({ state: 'current', registration: 'missing' })] },
      },
    }));

    await screen.findByText('Host integrations');
    const row = screen.getByRole('group', { name: 'codex · core' });
    expect(statesIn(row)).toEqual(['ready', 'unavailable']);
    expect(within(row).getByText(/component current/)).toBeTruthy();
    expect(within(row).getByText(/registration missing/)).toBeTruthy();
    expect(within(stage('Host integrations')).getByText(/1 component row · 1 degraded/))
      .toBeTruthy();
  });

  it('counts a component state it cannot read apart from a degraded one', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      host_evidence: {
        state: 'ready',
        value: { items: [hostComponent({ state: 'quarantined' })] },
      },
    }));

    await screen.findByText('Host integrations');
    const row = screen.getByRole('group', { name: 'codex · core' });
    expect(statesIn(row)).toEqual(['unsupported_schema', 'ready']);
    expect(
      within(stage('Host integrations')).getByText(
        /1 component row · 0 degraded · 2 artifacts · 1 in a state this build cannot read/,
      ),
    ).toBeTruthy();
  });

  it('renders unavailable host evidence without implying zero coverage', async () => {
    renderDelivery();

    expect(await screen.findByText('Host integrations')).toBeTruthy();
    const hosts = stage('Host integrations');
    expect(within(hosts).getByText(/host evidence authority is not mounted/)).toBeTruthy();
    expect(screen.queryByRole('list', { name: 'Host integration components' })).toBeNull();
    expect(within(hosts).queryByText(/component rows/)).toBeNull();
    expect(screen.queryByText('0 host integrations')).toBeNull();
  });

  /**
   * The release walk is bounded and its projection carries no `truncated` flag,
   * so the count cannot be presented as a total. Until the contract grows the
   * flag, the page says the statement is missing rather than implying there is
   * nothing beyond what it drew.
   */
  it('does not present a bounded release count as a total', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      releases: {
        state: 'ready',
        value: {
          items: [
            { tag: 'v1.0.0', commit: 'a'.repeat(40), title: 'one', prerelease: false, published_at_micros: 1 },
            { tag: 'v0.9.0', commit: 'b'.repeat(40), title: 'zero nine', prerelease: false, published_at_micros: 0 },
          ],
        },
      },
    }));

    await screen.findByText('Releases');
    const releases = stage('Releases');
    expect(statesIn(releases)).toEqual(['ready']);
    expect(
      within(releases).getByText(/2 releases · no truncation stated, so this is a floor/),
    ).toBeTruthy();
  });

  it('discloses when the commit timeline is truncated', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      commits: {
        ...DELIVERY_OVERVIEW.payload.commits,
        value: { ...DELIVERY_OVERVIEW.payload.commits.value, truncated: true },
      },
    }));

    expect(
      await screen.findByText(/2 commits shown · more commits not shown/),
    ).toBeTruthy();
  });

  it('reports a behind index generation as stale even when its read completed', async () => {
    renderDelivery(PROJECTS, 200, deliveryOverview({
      generation_freshness: {
        state: 'ready',
        value: {
          comparison: 'behind',
          head_commit: 'c'.repeat(40),
          indexed_commit: 'a'.repeat(40),
        },
      },
    }));

    await screen.findByText('Index freshness');
    const freshness = stage('Index freshness');
    expect(statesIn(freshness)).toEqual(['stale']);
    expect(
      within(freshness).getByText(/behind · HEAD cccccccc · indexed aaaaaaaa/),
    ).toBeTruthy();
    expect(within(freshness).getByText('unknown')).toBeTruthy();
  });

  it('reports a current index generation as measured evidence', async () => {
    renderDelivery();

    await screen.findByText('Index freshness');
    const freshness = stage('Index freshness');
    expect(statesIn(freshness)).toEqual(['ready']);
    expect(within(freshness).getByText(/HEAD aaaaaaaa · indexed aaaaaaaa/)).toBeTruthy();
    expect(within(freshness).getByText('measured')).toBeTruthy();
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
