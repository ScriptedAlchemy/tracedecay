import type {
  DeliveryInboxPullRequestV1,
  DeliveryInboxV1,
  DeliveryOverviewV1,
} from '../contracts/generated.ts';

/**
 * Delivery test fixtures shaped to the generated contracts. Two registered
 * projects, three admitted pull requests, and membership edges that include
 * the correlating bases the daemon *may* serve — so umbrella grouping,
 * cross-project rails and journey lanes can be exercised without inventing a
 * wire shape. Production inboxes today serve only the branch reference; the
 * DOM tests that assert the unavailable-correlation state use `INBOX_BRANCH_ONLY`.
 */

export const HEAD_ALPHA = 'a'.repeat(40);
export const HEAD_BETA = 'b'.repeat(40);
const BASE_ALPHA = 'c'.repeat(40);
const MERGE_BASE_ALPHA = 'd'.repeat(40);

/** 2026-05-09T14:00:00Z in microseconds. */
export const T0 = 1_778_335_200_000_000;
const HOUR = 3_600_000_000;

function pullRequestRow(
  overrides: Partial<DeliveryInboxPullRequestV1> & {
    project_id: string;
    number: string;
    title: string;
    head: string;
    branch_ref: string;
  },
): DeliveryInboxPullRequestV1 {
  const { number, title, head, ...rest } = overrides;
  return {
    id: `${rest.project_id}:github:${number}`,
    repository_id: `repository.${rest.project_id}`,
    worktree_id: `worktree.${rest.project_id}`,
    indexed_head_commit_id: head,
    indexed_generation: `generation.${rest.project_id}.1`,
    state: 'current',
    pull_request: {
      id: `github:${number}`,
      label: `Pull request #${number} — ${title}`,
      provider: 'github',
      pull_request_id: number,
      identity: {
        title,
        state: 'open',
        draft: false,
        additions: 120,
        deletions: 35,
        changed_files: 8,
      },
      operations: [
        {
          operation: 'pull_request',
          last_complete: {
            coverage: 'complete',
            fetched_at_micros: T0 + 6 * HOUR,
            merge_base_commit_id: MERGE_BASE_ALPHA,
            outcome: 'complete',
            provider_base_commit_id: BASE_ALPHA,
            provider_head_commit_id: head,
          },
          latest_attempt: null,
        },
      ],
    },
    attention: [],
    shared_code: [
      {
        kind: 'shared_code',
        state: 'requires_selection',
        href: '/code?view=shared-code',
        source_generation: `generation.${rest.project_id}.1`,
      },
      {
        kind: 'compare',
        state: 'requires_selection',
        href: '/code?view=compare',
        source_generation: `generation.${rest.project_id}.1`,
      },
    ],
    ...rest,
  };
}

export const INBOX: DeliveryInboxV1 = {
  registry_state: 'ready',
  projects: [
    {
      project_id: 'project.alpha',
      label: 'alpha',
      project_root: '/src/alpha',
      git_common_dir: '/src/alpha/.git',
      repository_id: 'repository.project.alpha',
      worktree_id: 'worktree.project.alpha',
      branch_ref: 'refs/heads/feature/delivery',
      indexed_head_commit_id: HEAD_ALPHA,
      indexed_generation: 'generation.project.alpha.1',
      provider_state: 'ready',
    },
    {
      project_id: 'project.beta',
      label: 'beta',
      project_root: '/src/beta',
      git_common_dir: '/src/beta/.git',
      repository_id: 'repository.project.beta',
      worktree_id: 'worktree.project.beta',
      branch_ref: 'refs/heads/feature/retry',
      indexed_head_commit_id: HEAD_BETA,
      indexed_generation: 'generation.project.beta.1',
      provider_state: 'stale',
    },
  ],
  pull_requests: [
    pullRequestRow({
      project_id: 'project.alpha',
      number: '42',
      title: 'Admit delivery inbox',
      head: HEAD_ALPHA,
      branch_ref: 'refs/heads/feature/delivery',
      attention: [
        {
          id: 'project.alpha:42:ci_failure',
          project_id: 'project.alpha',
          pull_request_id: '42',
          source: 'ci_failure',
          state: 'active',
          evidence: [{ kind: 'ci_failure', failure_anchor: 'anchor.ci.42' }],
          coverage: 'complete',
          observed_at_micros: T0 + 7 * HOUR,
        },
        {
          id: 'project.alpha:42:unresolved_review',
          project_id: 'project.alpha',
          pull_request_id: '42',
          source: 'unresolved_review',
          state: 'active',
          evidence: [{ kind: 'review_comment', comment_id: 'R1', path: 'src/ingest/retry.ts' }],
          coverage: 'complete',
          observed_at_micros: T0 + 7 * HOUR,
        },
        {
          id: 'project.alpha:42:overlapping_edit',
          project_id: 'project.alpha',
          pull_request_id: '42',
          source: 'overlapping_edit',
          state: 'unavailable',
          evidence: [],
          coverage: 'unsupported',
          observed_at_micros: null,
        },
      ],
    }),
    pullRequestRow({
      project_id: 'project.alpha',
      number: '43',
      title: 'Persist retry backoff',
      head: HEAD_ALPHA,
      branch_ref: 'refs/heads/feature/delivery',
      pull_request: {
        id: 'github:43',
        label: 'Pull request #43 — Persist retry backoff',
        provider: 'github',
        pull_request_id: '43',
        identity: {
          title: 'Persist retry backoff',
          state: 'open',
          draft: true,
          additions: 12,
          deletions: 2,
          changed_files: 2,
        },
        operations: [],
      },
    }),
    pullRequestRow({
      project_id: 'project.beta',
      number: '8',
      title: 'Emit retry events',
      head: HEAD_BETA,
      branch_ref: 'refs/heads/feature/retry',
      state: 'stale',
    }),
  ],
  membership_edges: [
    {
      id: 'project.alpha:42:branch_pull_request_reference:0',
      project_id: 'project.alpha',
      pull_request_id: '42',
      basis: {
        kind: 'branch_pull_request_reference',
        branch_ref: 'refs/heads/feature/delivery',
        head_commit_id: HEAD_ALPHA,
      },
    },
    {
      id: 'project.alpha:42:shared_work_objective:1',
      project_id: 'project.alpha',
      pull_request_id: '42',
      basis: { kind: 'shared_work_objective', work_item_id: 'work.retry-backoff' },
    },
    {
      id: 'project.alpha:42:session_git_relation:2',
      project_id: 'project.alpha',
      pull_request_id: '42',
      basis: {
        kind: 'session_git_relation',
        session_id: 'session.alpha.1',
        commit_id: HEAD_ALPHA,
      },
    },
    {
      id: 'project.alpha:43:branch_pull_request_reference:0',
      project_id: 'project.alpha',
      pull_request_id: '43',
      basis: {
        kind: 'branch_pull_request_reference',
        branch_ref: 'refs/heads/feature/delivery',
        head_commit_id: HEAD_ALPHA,
      },
    },
    {
      id: 'project.beta:8:branch_pull_request_reference:0',
      project_id: 'project.beta',
      pull_request_id: '8',
      basis: {
        kind: 'branch_pull_request_reference',
        branch_ref: 'refs/heads/feature/retry',
        head_commit_id: HEAD_BETA,
      },
    },
    {
      id: 'project.beta:8:shared_work_objective:1',
      project_id: 'project.beta',
      pull_request_id: '8',
      basis: { kind: 'shared_work_objective', work_item_id: 'work.retry-backoff' },
    },
    {
      id: 'project.beta:8:shared_agent:2',
      project_id: 'project.beta',
      pull_request_id: '8',
      basis: { kind: 'shared_agent', agent_id: 'agent.claude-code' },
    },
    {
      id: 'project.alpha:43:shared_agent:1',
      project_id: 'project.alpha',
      pull_request_id: '43',
      basis: { kind: 'shared_agent', agent_id: 'agent.claude-code' },
    },
  ],
  omitted_projects: 0,
  excluded_pull_requests: 1,
};

/** What production serves today: the branch reference only. */
export const INBOX_BRANCH_ONLY: DeliveryInboxV1 = {
  ...INBOX,
  membership_edges: INBOX.membership_edges.filter(
    (edge) => edge.basis.kind === 'branch_pull_request_reference',
  ),
};

export const OVERVIEW_ALPHA: DeliveryOverviewV1 = {
  changes: {
    state: 'ready',
    value: {
      schema_version: 'delivery.git-status.v1',
      repository: '/src/alpha',
      head: { state: 'attached', branch: 'feature/delivery', commit: HEAD_ALPHA },
      operation: 'none',
      staged: 1,
      unstaged: 2,
      untracked: 0,
      conflicted: 0,
      ignored: 4,
      changed_paths: ['src/ingest/retry.ts', 'src/ingest/config.ts'],
    },
  },
  commits: {
    state: 'ready',
    value: {
      truncated: false,
      items: [
        {
          commit: HEAD_ALPHA,
          subject: 'feat(ingest): add retry backoff',
          author_name: 'octocat',
          author_email: 'octocat@example.com',
          author_at_micros: T0 + 3 * HOUR,
          committer_at_micros: T0 + 3 * HOUR,
        },
        {
          commit: 'e'.repeat(40),
          subject: 'test(ingest): cover backoff jitter',
          author_name: 'octocat',
          author_email: 'octocat@example.com',
          author_at_micros: T0 + 1 * HOUR,
          committer_at_micros: T0 + 1 * HOUR,
        },
      ],
    },
  },
  pull_requests: {
    state: 'ready',
    value: {
      expected_head_commit: HEAD_ALPHA,
      retained_head_commit: HEAD_ALPHA,
      total_retained: 1,
      truncated: false,
      items: [INBOX.pull_requests[0]!.pull_request],
    },
  },
  review_comments: {
    state: 'ready',
    value: {
      expected_head_commit: HEAD_ALPHA,
      retained_head_commit: HEAD_ALPHA,
      total_retained: 2,
      truncated: false,
      items: [
        {
          id: 'review.R1',
          label: 'Review comment R1',
          provider: 'github',
          pull_request_id: '42',
          comment_id: 'R1',
          observations: [
            {
              kind: 'last_complete',
              operation: 'review_comments',
              observed_at_micros: T0 + 7 * HOUR,
              provider_outcome: 'complete',
              repository_id: 'repository.project.alpha',
              review_id: 'review.1',
              thread_id: 'thread.1',
              reply_to_comment_id: null,
              author_class: 'maintainer',
              review_state: 'changes_requested',
              lifecycle: 'current',
              path: 'src/ingest/retry.ts',
              line: 142,
              original_line: 142,
              body_preview: { text: 'Should shouldRetry accept attempt param?', truncated: false },
              source_url: 'https://github.com/example/alpha/pull/42#discussion_r1',
              version_digest: 'digest.1',
            },
          ],
        },
        {
          id: 'review.R2',
          label: 'Review comment R2',
          provider: 'github',
          pull_request_id: '42',
          comment_id: 'R2',
          observations: [
            {
              kind: 'last_complete',
              operation: 'review_comments',
              observed_at_micros: T0 + 7 * HOUR,
              provider_outcome: 'complete',
              repository_id: 'repository.project.alpha',
              review_id: 'review.1',
              thread_id: 'thread.2',
              reply_to_comment_id: null,
              author_class: 'bot',
              review_state: 'commented',
              lifecycle: 'outdated',
              path: 'src/ingest/config.ts',
              line: null,
              original_line: 12,
              body_preview: null,
              source_url: null,
              version_digest: 'digest.2',
            },
          ],
        },
      ],
    },
  },
  ci_checks: {
    state: 'ready',
    value: {
      expected_head_commit: HEAD_ALPHA,
      retained_head_commit: HEAD_ALPHA,
      total_retained: 2,
      truncated: false,
      items: [
        {
          id: 'check.unit',
          label: 'Unit tests',
          observation_id: 'obs.1',
          observed_at_micros: T0 + 5 * HOUR,
          provider_head_commit: HEAD_ALPHA,
          workflow_path: '.github/workflows/ci.yml',
          workflow_status: 'completed',
          workflow_conclusion: 'success',
          job_status: 'completed',
          job_conclusion: 'success',
          check_status: 'completed',
          check_conclusion: 'success',
          failure_kind: 'unknown',
          failed_step: null,
          annotation_count: 0,
          annotations: [],
          run: {
            attempt_id: '1',
            check_run_id: 'cr.1',
            check_suite_id: 'cs.1',
            job_id: 'job.1',
            run_id: 'run.1',
            workflow_id: 'wf.1',
          },
        },
        {
          id: 'check.integration',
          label: 'Integration tests',
          observation_id: 'obs.2',
          observed_at_micros: T0 + 5 * HOUR + 600_000_000,
          provider_head_commit: HEAD_ALPHA,
          workflow_path: '.github/workflows/ci.yml',
          workflow_status: 'completed',
          workflow_conclusion: 'failure',
          job_status: 'completed',
          job_conclusion: 'failure',
          check_status: 'completed',
          check_conclusion: 'failure',
          failure_kind: 'test_failure',
          failed_step: 'cargo test',
          annotation_count: 1,
          annotations: [
            {
              path: 'src/ingest/retry.ts',
              start_line: 140,
              end_line: 146,
              level: 'failure',
              title: 'retry exhausted',
            },
          ],
          run: {
            attempt_id: '1',
            check_run_id: 'cr.2',
            check_suite_id: 'cs.1',
            job_id: 'job.2',
            run_id: 'run.1',
            workflow_id: 'wf.1',
          },
        },
      ],
    },
  },
  failure_localization: {
    state: 'unavailable',
    reason: 'no CI localization owner is mounted for this project',
    required_authority: 'retained CI localization state and exact-evidence authority',
    value: null,
  },
  releases: {
    state: 'not_published',
    reason: 'no landed read route serves this projection without github_read_authority',
    required_authority: 'github_read_authority',
  },
  generation_freshness: {
    state: 'ready',
    value: { comparison: 'current', head_commit: HEAD_ALPHA, indexed_commit: HEAD_ALPHA },
  },
};

/** A project whose provider authority is absent: local Git remains useful. */
export const OVERVIEW_LOCAL_ONLY: DeliveryOverviewV1 = {
  ...OVERVIEW_ALPHA,
  pull_requests: {
    state: 'not_published',
    reason: 'no landed read route serves this projection without github_read_authority',
    required_authority: 'github_read_authority',
  },
  review_comments: {
    state: 'not_published',
    reason: 'no landed read route serves this projection without github_read_authority',
    required_authority: 'github_read_authority',
  },
  ci_checks: {
    state: 'not_published',
    reason: 'no landed read route serves this projection without ci_provider_read_authority',
    required_authority: 'ci_provider_read_authority',
  },
};
