import type {
  DeliveryCiTimelineV1,
  DeliveryCommitTimelineV1,
  DeliveryFailureLocalizationTimelineV1,
  DeliveryGenerationFreshnessV1,
  DeliveryGitHeadV1,
  DeliveryGitStatusV1,
  DeliveryInboxPullRequestV1,
  DeliveryOverviewV1,
  DeliveryPullRequestTimelineV1,
  DeliveryPullRequestV1,
  DeliveryReviewLifecycleV1,
  DeliveryReviewObservationV1,
  DeliveryReviewTimelineV1,
} from '../../contracts/generated.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import { ControlLink, GradeMark, IdentityRow, microsToIso, shortSha } from './deliveryChrome.tsx';
import type { DeliveryLocation, DeliveryLocationPatch } from './deliveryLocation.ts';
import { providerOutcomeKind } from './evidence.ts';
import { pullRequestNumberLabel } from './deliveryReading.ts';
import { laneServes, laneStateKind, projectionLaneState, type LaneState } from './journey.ts';
import { laneStateDetail } from './ProjectionLedger.tsx';
import {
  buildCheckRows,
  buildReviewLanes,
  compareHref,
  lifecycleKind,
  providerIdentity,
  type CheckRow,
  type PullRequestIdentity,
  type ReviewLanes,
  type ReviewThread,
} from './review.ts';

/**
 * The review workspace's instrument panels over the project overview. Each
 * panel renders one projection under its own typed state: a projection whose
 * authority did not serve prints the daemon's reason, never an empty list.
 */

export interface ReviewHead {
  readonly branch: string;
  readonly revision: string;
}

export interface Served<T> {
  readonly state: LaneState;
  readonly value: T | null;
}

type Projection = DeliveryOverviewV1[keyof DeliveryOverviewV1];

function readProjection<T>(
  projection: Projection & { readonly value?: T | null },
  label: string,
): Served<T> {
  const state = projectionLaneState(projection, label);
  return { state, value: laneServes(state) ? (projection.value ?? null) : null };
}

export interface ReviewModel {
  readonly head: ReviewHead;
  readonly changes: Served<DeliveryGitStatusV1>;
  readonly commits: Served<DeliveryCommitTimelineV1>;
  readonly pullRequests: Served<DeliveryPullRequestTimelineV1>;
  readonly reviews: Served<DeliveryReviewTimelineV1>;
  readonly checks: Served<DeliveryCiTimelineV1>;
  readonly localization: Served<DeliveryFailureLocalizationTimelineV1>;
  readonly freshness: Served<DeliveryGenerationFreshnessV1>;
  readonly pullRequestItem: DeliveryPullRequestV1 | null;
  readonly identity: PullRequestIdentity;
  /** Threads admitted by the lane filter, grouped by path; `null` when reviews did not serve. */
  readonly lanes: ReviewLanes | null;
  /** Every observed thread regardless of the lane filter. */
  readonly allThreads: readonly ReviewThread[];
  readonly checkRows: readonly CheckRow[] | null;
}

export function buildReviewModel(
  overview: DeliveryOverviewV1,
  row: DeliveryInboxPullRequestV1,
  filter: DeliveryReviewLifecycleV1 | null,
): ReviewModel {
  const head: ReviewHead = { branch: row.branch_ref, revision: row.indexed_head_commit_id };
  const pullRequests = readProjection<DeliveryPullRequestTimelineV1>(overview.pull_requests, 'Pull requests');
  const reviews = readProjection<DeliveryReviewTimelineV1>(overview.review_comments, 'Reviews');
  const checks = readProjection<DeliveryCiTimelineV1>(overview.ci_checks, 'CI checks');
  const pullRequestItem =
    pullRequests.value?.items.find(
      (item) =>
        item.pull_request_id === row.pull_request.pull_request_id &&
        item.provider === row.pull_request.provider,
    ) ?? null;
  return {
    head,
    changes: readProjection<DeliveryGitStatusV1>(overview.changes, 'Changes'),
    commits: readProjection<DeliveryCommitTimelineV1>(overview.commits, 'Commits'),
    pullRequests,
    reviews,
    checks,
    localization: readProjection<DeliveryFailureLocalizationTimelineV1>(
      overview.failure_localization,
      'Failure localization',
    ),
    freshness: readProjection<DeliveryGenerationFreshnessV1>(overview.generation_freshness, 'Index freshness'),
    pullRequestItem,
    identity: providerIdentity(pullRequestItem),
    lanes: reviews.value === null ? null : buildReviewLanes(reviews.value.items, head, filter),
    allThreads: reviews.value === null ? [] : buildReviewLanes(reviews.value.items, head, null).threads,
    checkRows: checks.value === null ? null : buildCheckRows(checks.value.items, head),
  };
}

export function threadAnchor(observation: DeliveryReviewObservationV1): string {
  const anchor = observation.line === null ? observation.path : `${observation.path}:${observation.line}`;
  return observation.original_line !== null && observation.original_line !== observation.line
    ? `${anchor} (original line ${observation.original_line})`
    : anchor;
}

/** The state word under the chip. The chip's detail already carries the
 * daemon's sentence for every state except `not_published`, whose chip names
 * only the required authority, so only that arm repeats the reason. */
function gapSentence(state: LaneState): string {
  switch (state.kind) {
    case 'served':
    case 'served_empty':
    case 'stale':
    case 'partial':
    case 'rate_limited':
    case 'failed':
    case 'denied':
    case 'unavailable':
      return state.kind;
    case 'not_published':
      return `${state.kind} · ${state.detail}`;
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

/** A projection's typed state printed as chip plus the daemon's sentence. */
export function ProjectionGap({ state, className }: { state: LaneState; className?: string }) {
  return (
    <div className={cn('flex flex-col gap-1.5', className)}>
      <StateChip kind={laneStateKind(state)} detail={laneStateDetail(state)} />
      <p className="font-mono text-3xs text-text-muted">{gapSentence(state)}</p>
    </div>
  );
}

function notServed(sha: string | null): string {
  return sha === null ? '— not served' : shortSha(sha);
}

export function IdentityPanel({ model, row }: { model: ReviewModel; row: DeliveryInboxPullRequestV1 }) {
  const { identity, pullRequestItem, pullRequests } = model;
  return (
    <Panel legend="Identity (exact)" bodyClassName="p-3">
      <div className="mb-2 flex flex-wrap items-center gap-2">
        {identity.outcome === null ? (
          <StateChip kind="unknown" detail="no complete pull_request read" />
        ) : (
          <StateChip kind={providerOutcomeKind(identity.outcome)} detail={`pull_request read · ${identity.outcome}`} />
        )}
        <GradeMark grade={pullRequestItem === null ? 'unavailable' : 'exact'} source="pull_request" />
      </div>
      <IdentityRow label="base" value={notServed(identity.base)} />
      <IdentityRow label="head (indexed)" value={shortSha(row.indexed_head_commit_id)} />
      <IdentityRow label="provider head" value={notServed(identity.head)} />
      <IdentityRow label="merge-base" value={notServed(identity.mergeBase)} />
      <IdentityRow
        label="fetched (UTC)"
        value={identity.fetchedAtMicros === null ? '— not served' : microsToIso(identity.fetchedAtMicros)}
      />
      {pullRequestItem !== null ? null : pullRequests.value === null ? (
        <ProjectionGap state={pullRequests.state} className="mt-2" />
      ) : (
        <p className="mt-2 text-3xs text-text-muted">
          Pull request {pullRequestNumberLabel(row.pull_request)} is not among the {pullRequests.value.items.length}{' '}
          head-bound provider items.
        </p>
      )}
    </Panel>
  );
}

export function BranchPanel({ row }: { row: DeliveryInboxPullRequestV1 }) {
  return (
    <Panel legend="Branch / worktree" bodyClassName="p-3">
      <IdentityRow label="branch" value={row.branch_ref} />
      <IdentityRow label="repository" value={row.repository_id} />
      <IdentityRow label="worktree" value={row.worktree_id} />
      <IdentityRow label="generation" value={row.indexed_generation} />
    </Panel>
  );
}

export function CommitsPanel({ commits }: { commits: Served<DeliveryCommitTimelineV1> }) {
  const items = commits.value?.items ?? null;
  return (
    <Panel legend={`Commits (${items === null ? '—' : items.length})`} bodyClassName="p-0">
      {commits.state.kind === 'served' ? null : <ProjectionGap state={commits.state} className="p-3" />}
      {items === null ? null : (
        <ol className="divide-y divide-edge-subtle">
          {items.map((commit) => (
            <li key={commit.commit} className="px-3 py-2">
              <div className="flex items-baseline gap-2">
                <span className="td-value shrink-0 text-3xs text-accent">{shortSha(commit.commit, 7)}</span>
                <span className="min-w-0 flex-1 truncate text-xs text-text-primary">{commit.subject}</span>
              </div>
              <p className="mt-0.5 font-mono text-3xs text-text-muted">
                {commit.author_name} · {microsToIso(commit.committer_at_micros)}
              </p>
            </li>
          ))}
          {commits.value?.truncated ? (
            <li className="px-3 py-2 font-mono text-3xs text-text-muted">list truncated by the daemon</li>
          ) : null}
        </ol>
      )}
    </Panel>
  );
}

function gitHeadRows(head: DeliveryGitHeadV1): readonly (readonly [string, string])[] {
  switch (head.state) {
    case 'attached':
      return [['head', `${head.state} · ${head.branch}`], ['head commit', shortSha(head.commit)]];
    case 'detached':
      return [['head', `${head.state} · ${shortSha(head.commit)}`]];
    case 'unborn':
      return [['head', `${head.state} · ${head.branch}`]];
    default: {
      const unhandled: never = head;
      return unhandled;
    }
  }
}

export function WorkingTreePanel({ changes, head }: { changes: Served<DeliveryGitStatusV1>; head: ReviewHead }) {
  const status = changes.value;
  return (
    <Panel legend="Working tree" bodyClassName="p-3">
      {changes.state.kind === 'served' ? null : <ProjectionGap state={changes.state} className="mb-2" />}
      {status === null ? null : (
        <>
          {gitHeadRows(status.head).map(([label, value]) => (
            <IdentityRow key={label} label={label} value={value} />
          ))}
          <IdentityRow label="operation" value={status.operation} />
          <IdentityRow label="staged / unstaged" value={`${status.staged} / ${status.unstaged}`} />
          <IdentityRow label="untracked / conflicted" value={`${status.untracked} / ${status.conflicted}`} />
          <p className="td-legend mt-3">changed paths · {status.changed_paths.length}</p>
          {status.changed_paths.length === 0 ? (
            <p className="mt-1 text-3xs text-text-muted">no changed path reported</p>
          ) : (
            <ul className="mt-1 space-y-1">
              {status.changed_paths.map((path) => (
                <li key={path} className="flex items-center justify-between gap-2">
                  <span className="min-w-0 truncate font-mono text-3xs text-text-secondary" title={path}>
                    {path}
                  </span>
                  <a href={compareHref({ ...head, file: path })} className="shrink-0 font-mono text-3xs text-accent hover:underline">
                    Compare
                  </a>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </Panel>
  );
}

export function ReviewThreadsPanel({
  model,
  location,
  navigate,
}: {
  model: ReviewModel;
  location: DeliveryLocation;
  navigate: (patch: DeliveryLocationPatch) => void;
}) {
  const { lanes, reviews, head } = model;
  return (
    <Panel legend="Review threads" bodyClassName="p-3">
      {reviews.state.kind === 'served' ? null : <ProjectionGap state={reviews.state} className="mb-3" />}
      {lanes === null ? null : lanes.files.length === 0 ? (
        <p className="text-3xs text-text-muted">
          {location.lane === null
            ? 'No review thread carries an observation.'
            : `No review thread is ${location.lane} at this head.`}
        </p>
      ) : (
        <div className="space-y-4">
          {lanes.files.map((file) => (
            <div key={file.path}>
              <div className="mb-1.5 flex items-center justify-between gap-2 border-b border-edge-subtle pb-1">
                <h3 className="min-w-0 truncate font-mono text-3xs text-text-primary" title={file.path}>
                  {file.path}
                </h3>
                <ControlLink href={compareHref({ ...head, file: file.path })} className="min-h-7 px-2 text-3xs">
                  Compare
                </ControlLink>
              </div>
              <ul className="space-y-1">
                {file.threads.map((thread) => (
                  <li key={thread.id}>
                    <ThreadButton
                      thread={thread}
                      pressed={location.thread === thread.id}
                      onSelect={() => navigate({ thread: thread.id, check: null })}
                    />
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
      )}
    </Panel>
  );
}

function ThreadButton({ thread, pressed, onSelect }: { thread: ReviewThread; pressed: boolean; onSelect: () => void }) {
  const { latest } = thread;
  return (
    <button
      type="button"
      aria-pressed={pressed}
      className={cn(
        'relative flex w-full flex-col gap-1 border border-edge-subtle px-2.5 py-2 text-left hover:bg-surface-2',
        pressed && 'border-accent bg-surface-2',
      )}
      onClick={onSelect}
    >
      {pressed ? <span aria-hidden className="absolute inset-y-0 left-0 w-[2px] bg-accent" /> : null}
      <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="font-mono text-3xs text-text-primary">{threadAnchor(latest)}</span>{' '}
        <span className="text-3xs text-text-secondary">{latest.review_state.replaceAll('_', ' ')}</span>{' '}
        <StateChip kind={lifecycleKind(latest.lifecycle)} detail={latest.lifecycle} />{' '}
        <span className="text-3xs text-text-muted">{latest.author_class.replaceAll('_', ' ')}</span>{' '}
        <GradeMark grade={thread.grade} source="review" />
      </span>
      <span className="text-xs text-text-secondary">
        {latest.body_preview === null ? (
          <span className="text-text-muted">body not served</span>
        ) : (
          <>
            {latest.body_preview.text}
            {latest.body_preview.truncated ? <span className="text-text-muted"> (truncated)</span> : null}
          </>
        )}
      </span>
    </button>
  );
}

export function ExactDiffPanel({ href, revision }: { href: string; revision: string }) {
  return (
    <Panel legend="Exact diff" bodyClassName="p-3">
      <StateChip
        kind="unavailable"
        detail="Delivery's read authority serves review anchors and check annotations, not diff hunks"
      />
      <p className="mt-2 text-xs leading-relaxed text-text-muted">
        No hunk body is served on this route, so no code pane is drawn here. The Code workspace's
        Compare view renders the indexed revision {shortSha(revision)} with the selected file prefilled.
      </p>
      <ControlLink href={href} className="mt-3">
        Open exact revision in Code · Compare
      </ControlLink>
    </Panel>
  );
}

const CHECK_COLUMNS = ['Workflow', 'Job', 'Check', 'Status', 'Conclusion', 'Observed (UTC)', 'Failure', 'Code'] as const;

function failureCell(row: CheckRow): string {
  const kind = row.check.failure_kind.replaceAll('_', ' ');
  return row.check.failed_step === null ? kind : `${kind} · ${row.check.failed_step}`;
}

export function CheckMatrixPanel({
  model,
  location,
  navigate,
}: {
  model: ReviewModel;
  location: DeliveryLocation;
  navigate: (patch: DeliveryLocationPatch) => void;
}) {
  const { checks, checkRows } = model;
  const cell = 'px-3 py-2 font-mono text-3xs';
  return (
    <Panel legend="Check matrix" bodyClassName="p-0">
      {checks.state.kind === 'served' ? null : <ProjectionGap state={checks.state} className="p-3" />}
      {checkRows === null ? null : checkRows.length === 0 ? (
        <p className="p-3 text-3xs text-text-muted">No check observation is retained at this head.</p>
      ) : (
        <div className="overflow-auto">
          <table className="w-full border-collapse text-xs" aria-label="Check matrix">
            <thead className="text-left">
              <tr className="border-b border-edge-subtle">
                {CHECK_COLUMNS.map((heading) => (
                  <th key={heading} scope="col" className="td-legend px-3 py-2 font-normal">
                    {heading}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {checkRows.map((row) => {
                const pressed = location.check === row.check.id;
                const { check } = row;
                return (
                  <tr key={check.id} className={cn('border-b border-edge-subtle align-top', pressed && 'bg-surface-2')}>
                    <td className={cn(cell, 'text-text-secondary')}>
                      <span className="block">{check.workflow_path}</span>
                      <span className="block text-text-muted">
                        {check.workflow_status} · {check.workflow_conclusion ?? '—'}
                      </span>
                    </td>
                    <td className={cn(cell, 'text-text-secondary')}>
                      <span className="block">{check.run.job_id}</span>
                      <span className="block text-text-muted">
                        {check.job_status} · {check.job_conclusion ?? '—'}
                      </span>
                    </td>
                    <td className="px-3 py-2">
                      <button
                        type="button"
                        aria-pressed={pressed}
                        className={cn('text-left text-text-primary hover:underline', pressed && 'text-accent')}
                        onClick={() => navigate({ check: check.id, thread: null })}
                      >
                        {check.label}
                      </button>
                    </td>
                    <td className="px-3 py-2">
                      <StateChip kind={row.status} detail={check.check_conclusion ?? check.check_status} />
                    </td>
                    <td className={cn(cell, 'text-text-secondary')}>{check.check_conclusion ?? '—'}</td>
                    <td className={cn(cell, 'text-text-muted')}>{microsToIso(check.observed_at_micros)}</td>
                    <td className={cn(cell, 'text-text-secondary')}>{failureCell(row)}</td>
                    <td className="px-3 py-2">
                      {row.compareHref === null ? (
                        <span className="text-text-muted">—</span>
                      ) : (
                        <a href={row.compareHref} className="font-mono text-3xs text-accent hover:underline">
                          Compare
                        </a>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </Panel>
  );
}

export function FailureLocalizationPanel({
  localization,
}: {
  localization: Served<DeliveryFailureLocalizationTimelineV1>;
}) {
  const items = localization.value?.items ?? null;
  return (
    <Panel legend="CI failure localization" bodyClassName="p-3">
      {localization.state.kind === 'served' ? null : <ProjectionGap state={localization.state} />}
      {items === null ? null : items.length === 0 ? (
        <p className="text-3xs text-text-muted">No failure localization is retained for this head.</p>
      ) : (
        <ul className="divide-y divide-edge-subtle">
          {items.map((item) => (
            <li key={item.id} className="flex items-baseline justify-between gap-2 py-1">
              <span className="text-xs text-text-primary">{item.label}</span>
              <span className="font-mono text-3xs text-text-muted">{item.id}</span>
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}
