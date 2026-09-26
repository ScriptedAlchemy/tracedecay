import { useMemo } from 'react';
import type {
  DeliveryInboxPullRequestV1,
  DeliveryOverviewV1,
  DeliveryReviewLifecycleV1,
} from '../../contracts/generated.ts';
import { ReadSection } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { ReadoutBar } from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn.ts';
import { useProjectOverview, type DeliveryContext } from './deliveryContext.ts';
import {
  ControlLink,
  GradeMark,
  IdentityRow,
  ReadOnlyProviderBadge,
  UnavailableControl,
  microsToIso,
  shortSha,
} from './deliveryChrome.tsx';
import { projectFor } from './inboxFilter.ts';
import { ProjectionLedger, overviewReadState } from './ProjectionLedger.tsx';
import {
  BranchPanel,
  CheckMatrixPanel,
  CommitsPanel,
  ExactDiffPanel,
  FailureLocalizationPanel,
  IdentityPanel,
  ProjectionGap,
  ReviewThreadsPanel,
  WorkingTreePanel,
  buildReviewModel,
  threadAnchor,
  type ReviewHead,
  type ReviewModel,
} from './ReviewPanels.tsx';
import {
  REVIEW_LIFECYCLES,
  compareHref,
  lifecycleKind,
  type CheckRow,
  type ReviewThread,
} from './review.ts';
import { pullRequestNumberLabel } from './deliveryReading.ts';

/**
 * The exact review workspace: review threads by lifecycle and path, the check
 * matrix, CI failure localization, commits and the provider's own identity
 * for the selected pull request, laid out as instrument panels over the
 * project overview. Every provider object is read here and never written;
 * the outward pivots are the provider's `source_url` and Code's Compare view.
 */
export function ReviewWorkspace({
  context,
  row,
}: {
  context: DeliveryContext;
  row: DeliveryInboxPullRequestV1;
}) {
  const overview = useProjectOverview(row.project_id);
  return (
    <ReadSection
      title="PR review"
      chrome="centered"
      state={overviewReadState(overview.isPending, overview.data)}
    >
      {(value) => <ReviewBody context={context} row={row} overview={value} />}
    </ReadSection>
  );
}

function laneRank(lifecycle: DeliveryReviewLifecycleV1): number {
  switch (lifecycle) {
    case 'current':
      return 0;
    case 'outdated':
      return 1;
    case 'resolved':
      return 2;
    case 'edited':
      return 3;
    case 'deleted':
      return 4;
    default: {
      const unhandled: never = lifecycle;
      return unhandled;
    }
  }
}

const LANE_ORDER = [...REVIEW_LIFECYCLES].sort((left, right) => laneRank(left) - laneRank(right));

function ReviewBody({
  context,
  row,
  overview,
}: {
  context: DeliveryContext;
  row: DeliveryInboxPullRequestV1;
  overview: DeliveryOverviewV1;
}) {
  const { location, navigate } = context;
  const model = useMemo(() => buildReviewModel(overview, row, location.lane), [overview, row, location.lane]);
  const selectedThread = model.allThreads.find((thread) => thread.id === location.thread) ?? null;
  const selectedCheck =
    selectedThread === null
      ? (model.checkRows?.find((candidate) => candidate.check.id === location.check) ?? null)
      : null;

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-auto">
      <ReviewHeader context={context} row={row} />
      <ReviewReadouts model={model} row={row} />
      <ReviewLanesGroup model={model} context={context} />
      <div className="grid grid-cols-1 gap-3 p-3 xl:grid-cols-[18rem_minmax(0,1fr)_22rem]">
        <div className="flex min-w-0 flex-col gap-3">
          <IdentityPanel model={model} row={row} />
          <BranchPanel row={row} />
          <CommitsPanel commits={model.commits} />
          <WorkingTreePanel changes={model.changes} head={model.head} />
        </div>
        <div className="flex min-w-0 flex-col gap-3">
          <ReviewThreadsPanel model={model} location={location} navigate={navigate} />
          <ExactDiffPanel
            href={compareHref({ ...model.head, file: selectedThread?.latest.path ?? null })}
            revision={model.head.revision}
          />
          <CheckMatrixPanel model={model} location={location} navigate={navigate} />
          <FailureLocalizationPanel localization={model.localization} />
        </div>
        <div className="flex min-w-0 flex-col gap-3">
          <SelectedRail model={model} thread={selectedThread} check={selectedCheck} />
          <ProjectionLedger overview={overview} />
        </div>
      </div>
      <ThreadsTable model={model} />
    </div>
  );
}

function ReviewHeader({ context, row }: { context: DeliveryContext; row: DeliveryInboxPullRequestV1 }) {
  const { inbox, navigate } = context;
  const project = projectFor(inbox, row.project_id);
  const button =
    'inline-flex min-h-[var(--touch-target-min)] items-center border border-edge-strong px-3 text-xs text-text-primary hover:bg-surface-2';
  return (
    <header className="flex flex-wrap items-center gap-3 border-b border-edge-subtle bg-surface-1 px-3 py-2">
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 text-xs">
        <span className="text-text-muted">{project?.label ?? row.project_id}</span>
        <span aria-hidden className="text-text-muted">·</span>
        <span className="font-mono text-text-secondary">Pull request {pullRequestNumberLabel(row.pull_request)}</span>
        <span aria-hidden className="text-text-muted">·</span>
        <span className="font-medium text-text-primary">
          {row.pull_request.identity?.title ?? row.pull_request.label}
        </span>
      </div>
      <div className="ml-auto flex flex-wrap items-center gap-2">
        <button type="button" className={button} onClick={() => navigate({ mode: 'inbox' })}>
          Back to inbox
        </button>
        <button type="button" className={button} onClick={() => navigate({ mode: 'journey' })}>
          Open journey
        </button>
        <ReadOnlyProviderBadge />
      </div>
    </header>
  );
}

function ReviewReadouts({ model, row }: { model: ReviewModel; row: DeliveryInboxPullRequestV1 }) {
  const identity = (model.pullRequestItem ?? row.pull_request).identity;
  const counts = model.lanes?.counts ?? null;
  const total = counts === null ? null : Object.values(counts).reduce((sum, count) => sum + count, 0);
  const failed = model.checkRows?.filter((candidate) => candidate.status === 'error').length ?? null;
  return (
    <ReadoutBar
      label="Review readings"
      elevation="raised"
      items={[
        { label: 'changed files', value: identity?.changed_files ?? '—' },
        {
          label: 'commits',
          value: model.commits.value?.items.length ?? '—',
          note: model.commits.state.kind === 'served' ? undefined : model.commits.state.kind,
        },
        {
          label: 'review threads',
          value: total ?? '—',
          note:
            counts === null
              ? model.reviews.state.kind
              : `${counts.current} current · ${counts.outdated} outdated · ${counts.resolved} resolved`,
        },
        {
          label: 'checks',
          value: model.checkRows?.length ?? '—',
          note: failed === null ? model.checks.state.kind : `${failed} failed`,
        },
        {
          label: 'index freshness',
          value: model.freshness.value?.comparison ?? model.freshness.state.kind,
          note: model.freshness.value === null ? undefined : `indexed ${shortSha(model.freshness.value.indexed_commit)}`,
        },
      ]}
    />
  );
}

function ReviewLanesGroup({ model, context }: { model: ReviewModel; context: DeliveryContext }) {
  const { location, navigate } = context;
  const counts = model.lanes?.counts ?? null;
  const unobserved = model.lanes?.unobserved ?? 0;
  return (
    <div className="flex flex-wrap items-center gap-3 border-b border-edge-subtle bg-surface-1 px-3 py-2">
      <div role="group" aria-label="Review lanes" className="flex flex-wrap border border-edge-subtle">
        {LANE_ORDER.map((lifecycle) => {
          const pressed = location.lane === lifecycle;
          return (
            <button
              key={lifecycle}
              type="button"
              aria-pressed={pressed}
              className={cn(
                'td-hit gap-2 border-r border-edge-subtle px-3 last:border-r-0',
                pressed ? 'bg-surface-2' : 'hover:bg-surface-2',
              )}
              onClick={() => navigate({ lane: pressed ? null : lifecycle })}
            >
              <span className={cn('text-3xs uppercase tracking-[0.12em]', pressed ? 'text-text-primary' : 'text-text-muted')}>
                {lifecycle}
              </span>{' '}
              <span className="td-value text-xs" data-cell="numeric">
                {counts === null ? '—' : counts[lifecycle]}
              </span>{' '}
              <StateChip kind={lifecycleKind(lifecycle)} />
            </button>
          );
        })}
      </div>
      {unobserved > 0 ? (
        <p className="text-3xs text-text-muted">
          {unobserved} item{unobserved === 1 ? '' : 's'} without an observation
        </p>
      ) : null}
      <p className="ml-auto text-3xs text-text-muted">
        counts are the provider's lifecycle words at the indexed head · no composite score
      </p>
    </div>
  );
}

function SelectedRail({ model, thread, check }: { model: ReviewModel; thread: ReviewThread | null; check: CheckRow | null }) {
  return (
    <section aria-label="Selected review thread" className="relative flex min-w-0 flex-col border border-edge-subtle bg-surface-1">
      <header className="flex h-8 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5">
        <h2 className="td-title truncate">
          {thread !== null ? 'Selected review thread' : check !== null ? 'Selected check' : 'Selection'}
        </h2>
        <span aria-hidden className="td-rule" />
      </header>
      <div className="p-3">
        {thread !== null ? (
          <SelectedThread thread={thread} />
        ) : check !== null ? (
          <SelectedCheck row={check} head={model.head} />
        ) : (
          <p className="text-xs text-text-muted">
            Select a review thread or a check to read its exact identity, observation time and provider anchors.
          </p>
        )}
      </div>
    </section>
  );
}

function SelectedThread({ thread }: { thread: ReviewThread }) {
  const { item, latest } = thread;
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <StateChip kind={lifecycleKind(latest.lifecycle)} detail={latest.lifecycle} />
        <GradeMark grade={thread.grade} source="review" />
      </div>
      <div>
        <IdentityRow label="comment" value={item.comment_id} />
        <IdentityRow label="provider" value={item.provider} />
        <IdentityRow label="thread" value={latest.thread_id ?? '—'} />
        <IdentityRow label="review" value={latest.review_id ?? '—'} />
        <IdentityRow label="author class" value={latest.author_class.replaceAll('_', ' ')} />
        <IdentityRow label="review state" value={latest.review_state.replaceAll('_', ' ')} />
        <IdentityRow label="observed (UTC)" value={microsToIso(latest.observed_at_micros)} />
        <IdentityRow label="anchor" value={threadAnchor(latest)} />
        <IdentityRow label="original line" value={latest.original_line === null ? '—' : String(latest.original_line)} />
        <IdentityRow label="version digest" value={latest.version_digest} />
      </div>
      <div>
        <p className="td-legend">body preview</p>
        {latest.body_preview === null ? (
          <p className="mt-1 text-xs text-text-muted">body not served</p>
        ) : (
          <p className="mt-1 whitespace-pre-wrap text-xs leading-relaxed text-text-primary">
            {latest.body_preview.text}
            {latest.body_preview.truncated ? <span className="text-text-muted"> (truncated)</span> : null}
          </p>
        )}
      </div>
      <div className="flex flex-wrap gap-2">
        {latest.source_url === null ? (
          <UnavailableControl label="Open in provider" reason="no source_url served" />
        ) : (
          <ControlLink href={latest.source_url} external>
            Open in provider
          </ControlLink>
        )}
        <ControlLink href={thread.compareHref}>Compare in Code</ControlLink>
      </div>
      <div className="flex flex-wrap items-center gap-2 border-t border-edge-subtle pt-3">
        <span className="td-legend">provider review actions</span>
        <ReadOnlyProviderBadge />
        <span className="text-3xs text-text-muted">observed data, not a disposition</span>
      </div>
    </div>
  );
}

function SelectedCheck({ row, head }: { row: CheckRow; head: ReviewHead }) {
  const { check } = row;
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        <StateChip kind={row.status} detail={check.check_conclusion ?? check.check_status} />
        <GradeMark grade="exact" source="check_result" />
      </div>
      <div>
        <IdentityRow label="check" value={check.label} />
        <IdentityRow label="workflow" value={check.workflow_path} />
        <IdentityRow label="workflow id" value={check.run.workflow_id} />
        <IdentityRow label="run / attempt" value={`${check.run.run_id} / ${check.run.attempt_id}`} />
        <IdentityRow label="job" value={check.run.job_id} />
        <IdentityRow label="check run" value={check.run.check_run_id} />
        <IdentityRow label="check suite" value={check.run.check_suite_id} />
        <IdentityRow label="workflow status" value={`${check.workflow_status} · ${check.workflow_conclusion ?? '—'}`} />
        <IdentityRow label="job status" value={`${check.job_status} · ${check.job_conclusion ?? '—'}`} />
        <IdentityRow label="check status" value={`${check.check_status} · ${check.check_conclusion ?? '—'}`} />
        <IdentityRow label="failure" value={`${check.failure_kind.replaceAll('_', ' ')}${check.failed_step === null ? '' : ` · ${check.failed_step}`}`} />
        <IdentityRow label="observed (UTC)" value={microsToIso(check.observed_at_micros)} />
        <IdentityRow label="provider head" value={shortSha(check.provider_head_commit)} />
      </div>
      <div>
        <p className="td-legend">annotations · {check.annotation_count}</p>
        {check.annotations.length === 0 ? (
          <p className="mt-1 text-xs text-text-muted">no annotation served</p>
        ) : (
          <ul className="mt-1 space-y-1">
            {check.annotations.map((annotation, index) => (
              <li key={`${annotation.path}:${annotation.start_line}:${index}`} className="flex items-baseline justify-between gap-2">
                <span className="min-w-0 break-all font-mono text-3xs text-text-secondary">
                  {annotation.path}:{annotation.start_line}-{annotation.end_line} · {annotation.level} ·{' '}
                  {annotation.title ?? '—'}
                </span>
                <a href={compareHref({ ...head, file: annotation.path })} className="shrink-0 font-mono text-3xs text-accent hover:underline">
                  Compare
                </a>
              </li>
            ))}
          </ul>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-2 border-t border-edge-subtle pt-3">
        <span className="td-legend">provider check actions</span>
        <ReadOnlyProviderBadge />
        <span className="text-3xs text-text-muted">observed data, not a disposition</span>
      </div>
    </div>
  );
}

const THREAD_COLUMNS = ['Path', 'Line', 'Lifecycle', 'Review state', 'Author', 'Grade', 'Observed (UTC)', 'Provider'] as const;

function ThreadsTable({ model }: { model: ReviewModel }) {
  const { reviews, allThreads } = model;
  return (
    <section aria-label="Review threads table" className="border-t border-edge-subtle">
      <header className="flex h-8 items-center gap-2.5 border-b border-edge-subtle bg-surface-1 px-3">
        <h2 className="td-title truncate">
          Review threads · exact table · {reviews.value === null ? '—' : allThreads.length}
        </h2>
        <span aria-hidden className="td-rule" />
        <span className="text-3xs text-text-muted">every observed thread, regardless of the lane filter</span>
      </header>
      {reviews.value === null ? (
        <ProjectionGap state={reviews.state} className="p-3" />
      ) : (
        <div className="overflow-auto">
          <table className="w-full border-collapse text-xs" aria-label="Review threads table">
            <thead className="text-left">
              <tr className="border-b border-edge-subtle">
                {THREAD_COLUMNS.map((heading) => (
                  <th key={heading} scope="col" className="td-legend px-3 py-2 font-normal">
                    {heading}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {allThreads.map((thread) => (
                <tr key={thread.id} className="border-b border-edge-subtle align-top">
                  <td className="px-3 py-2 font-mono text-3xs text-text-primary">{thread.latest.path}</td>
                  <td className="px-3 py-2 font-mono text-3xs text-text-secondary" data-cell="numeric">
                    {thread.latest.line ?? '—'}
                  </td>
                  <td className="px-3 py-2">
                    <StateChip kind={lifecycleKind(thread.latest.lifecycle)} detail={thread.latest.lifecycle} />
                  </td>
                  <td className="px-3 py-2 text-text-secondary">{thread.latest.review_state.replaceAll('_', ' ')}</td>
                  <td className="px-3 py-2 text-text-secondary">{thread.latest.author_class.replaceAll('_', ' ')}</td>
                  <td className="px-3 py-2">
                    <GradeMark grade={thread.grade} source="review" />
                  </td>
                  <td className="px-3 py-2 font-mono text-3xs text-text-muted">
                    {microsToIso(thread.latest.observed_at_micros)}
                  </td>
                  <td className="px-3 py-2">
                    {thread.latest.source_url === null ? (
                      <span className="text-text-muted">—</span>
                    ) : (
                      <a
                        href={thread.latest.source_url}
                        target="_blank"
                        rel="noreferrer noopener"
                        className="font-mono text-3xs text-accent hover:underline"
                      >
                        provider
                      </a>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
