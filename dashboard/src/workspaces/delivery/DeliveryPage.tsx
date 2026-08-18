import { useQuery } from '@tanstack/react-query';
import { useMemo, useState } from 'react';
import type { LucideIcon } from 'lucide-react';
import {
  FolderGit2,
  GitBranch,
  GitCommitHorizontal,
  GitFork,
  GitPullRequest,
  Package,
  ScrollText,
  Server,
} from 'lucide-react';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { fetchEnvelope } from '../../data/query/envelope.ts';
import { ReadSection, type ReadState } from '../../ui/ReadSection.tsx';
import { FreshnessMeter } from '../../ui/OpsLayout.tsx';
import { StateChip } from '../../ui/StateChip';
import { EvidencePattern } from '../../ui/EvidencePattern.tsx';
import {
  Fact,
  Legend,
  Meter,
  Panel,
  Readout,
  ReadoutBar,
  WorkspaceHeader,
} from '../../ui/instrument.tsx';
import { cn } from '../../ui/cn';
import { formatCount } from '../../ui/format.ts';
import { freshnessTier, relativeAge } from '../../ui/time.ts';
import {
  useProjectRegistry,
  type ProjectRegistryResult,
} from '../../data/query/projectRegistry.ts';
import { DeliveryFieldPlot } from './DeliveryField.tsx';
import { composeDeliveryField, type DeliveryBody, type DeliveryField } from './field.ts';
import {
  type DeliveryCiCheckV1,
  type DeliveryCiTimelineV1,
  type DeliveryCommitTimelineV1,
  type DeliveryFailureLocalizationTimelineV1,
  type DeliveryGenerationFreshnessV1,
  type DeliveryGitHubOperationSnapshotV1,
  type DeliveryGitStatusV1,
  type DeliveryOverviewV1,
  DeliveryOverviewV1Schema,
  type DeliveryPullRequestIdentityV1,
  type DeliveryPullRequestTimelineV1,
  type DeliveryReleaseTimelineV1,
  type DeliveryReviewItemV1,
  type DeliveryReviewObservationV1,
  type DeliveryReviewTimelineV1,
  type ProjectRepoGroup,
  type ProjectsPayloadV1,
} from '../../contracts/generated.ts';

/**
 * Delivery — the daemon's git surface, read as a field rather than scrolled as
 * a list.
 *
 * `/api/projects` provides the repository field. `/api/delivery/overview`
 * provides independently typed local Git, provider, CI, localization, release,
 * and generation projections. Retained evidence remains visible when its
 * source is partial, stale, rate-limited, failed, or unavailable; none
 * of those states are converted into an empty timeline.
 *
 * The one word that has to stay exact everywhere on this page: `last_seen_at`
 * is when TraceDecay last INDEXED the checkout, not when anyone last committed
 * to it. Every caption says so, because "recency" on a delivery surface will
 * otherwise be read as commit recency, which would be a fabrication.
 */
export function DeliveryPage() {
  // The shared registry read, not a private copy of it. Under its own key this
  // page fetched the same listing a second time and, because the SSE
  // `project_registry_changed` invalidation names the registry root, was the
  // one reader a rename never reached.
  const projects = useProjectRegistry();
  const overview = useQuery({
    queryKey: ['delivery', 'overview'],
    queryFn: () => fetchEnvelope('/api/delivery/overview', DeliveryOverviewV1Schema),
  });
  const [selectedId, setSelectedId] = useState<string | null>(null);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <WorkspaceHeader
        path="delivery"
        title="Delivery"
        note="repositories, changes and commit history · external authorities explicit"
      />
      <ReadSection
        title="Delivery"
        chrome="centered"
        state={projectListingReadState(projects.isPending, projects.data)}
      >
        {(data) => {
          if (data.status === 'missing_registry') {
            return (
              <div className="flex min-h-0 flex-1 items-center justify-center p-8">
                <div className="flex max-w-sm flex-col items-center gap-3 text-center">
                  <StateChip kind="unknown" detail="no project registry available" />
                  <p className="text-xs leading-relaxed text-text-muted">
                    The daemon answered without a project registry, so there is
                    no repository to place on the field.{' '}
                    <span className="text-text-secondary">
                      This is the registry reporting itself absent, not an empty
                      workspace.
                    </span>
                  </p>
                </div>
              </div>
            );
          }
          // Every other non-ok status — `registry_unavailable` today, whatever
          // `projects.rs` adds tomorrow (`status` is a plain string on the
          // wire) — is a failed read. It used to fall through to
          // `project_tree ?? []` and draw the same pixels as a measured empty
          // registry; a refused read must say so instead.
          if (data.status !== 'ok') {
            return (
              <div className="flex min-h-0 flex-1 items-center justify-center p-8">
                <div className="flex max-w-sm flex-col items-center gap-3 text-center">
                  <StateChip
                    kind={data.status === 'registry_unavailable' ? 'unavailable' : 'unknown'}
                    detail={
                      data.status === 'registry_unavailable'
                        ? 'project registry read failed'
                        : `unrecognised registry status: ${data.status}`
                    }
                  />
                  {data.error ? (
                    <p className="text-xs leading-relaxed text-text-muted">{data.error}</p>
                  ) : null}
                </div>
              </div>
            );
          }
          // `ok` sends the tree, but the field is nullable because the failure
          // responses send it as an explicit null. A null here contradicts the
          // status and is not an empty workspace.
          if (!data.project_tree) {
            return (
              <div className="flex min-h-0 flex-1 items-center justify-center p-8">
                <StateChip kind="partial" detail="registry response is inconsistent" />
              </div>
            );
          }
          return (
            <DeliveryBody_
              tree={data.project_tree}
              truncated={data.truncated === true}
              selectedId={selectedId}
              onSelect={setSelectedId}
              overviewPending={overview.isPending}
              overview={overview.data}
            />
          );
        }}
      </ReadSection>
    </div>
  );
}

function projectListingReadState(
  pending: boolean,
  result: ProjectRegistryResult<ProjectsPayloadV1> | undefined,
): ReadState<ProjectsPayloadV1> {
  if (pending) {
    return { kind: 'blocked', state: 'loading', detail: 'reading the project registry' };
  }
  if (!result) {
    return { kind: 'blocked', state: 'unknown', detail: 'no response recorded' };
  }
  if (result.outcome === 'transport') {
    return {
      kind: 'blocked',
      state: result.state,
      detail: result.detail ?? 'the project registry could not be read',
    };
  }
  return { kind: 'ready', value: result.envelope.payload };
}

function DeliveryBody_({
  tree,
  truncated,
  selectedId,
  onSelect,
  overviewPending,
  overview,
}: {
  tree: readonly ProjectRepoGroup[];
  truncated: boolean;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  overviewPending: boolean;
  overview: EnvelopeResult<DeliveryOverviewV1> | undefined;
}) {
  // Keyed on identity: the payload is fetched once and does not churn, and the
  // field's clock only matters at recency-column boundaries.
  const nowSecs = useMemo(() => Math.floor(Date.now() / 1000), [tree]);
  const field = useMemo(() => composeDeliveryField(tree, nowSecs), [tree, nowSecs]);
  const selected =
    field.bodies.find((body) => body.id === selectedId) ?? null;
  const selectedGroup = tree.find(
    (group) => (group.git_common_dir ?? group.label) === selectedId,
  );
  const indexedToday = field.columns[0]?.count ?? 0;

  if (tree.length === 0) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center p-8">
        <p className="max-w-sm text-center text-xs leading-relaxed text-text-muted">
          The registry answered and holds no repositories in this workspace.{' '}
          <span className="text-text-secondary">
            Repositories appear here once TraceDecay has indexed one.
          </span>
        </p>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ReadoutBar
        label="Registry readings"
        elevation="raised"
        items={[
          { label: 'repositories', value: field.bodies.length },
          {
            label: 'branches',
            value: formatCount(field.totalBranches),
            note: `across ${field.bodies.length - field.unknownBranchCount} git repos`,
          },
          { label: 'checkouts', value: field.totalCheckouts },
          {
            label: 'worktrees',
            value: field.totalWorktrees,
            note: field.totalWorktrees === 0 ? 'none registered' : undefined,
          },
          {
            label: 'indexed today',
            value: indexedToday,
            fraction:
              field.bodies.length > 0 ? indexedToday / field.bodies.length : null,
            note: 'of all repositories',
          },
          {
            label: 'busiest repo',
            value: formatCount(field.branchCeiling),
            note: 'branches',
          },
        ]}
      />

      {/* The scrolling body takes a name and the tab stop: wide field plots
        * and tables can overflow it with nothing focusable in view
        * (axe: scrollable-region-focusable). */}
      <div
        role="region"
        aria-label="Delivery content"
        tabIndex={0}
        className="flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-3 [scrollbar-gutter:stable] xl:flex-row"
      >
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          <DeliveryFieldPlot
            field={field}
            selectedId={selectedId}
            onSelect={onSelect}
            ariaLabel={fieldDescription(field)}
          />
          <FieldAxis field={field} />
          <RepoTable
            field={field}
            selectedId={selectedId}
            onSelect={onSelect}
            truncated={truncated}
            nowSecs={nowSecs}
          />
        </div>

        <aside className="flex w-full shrink-0 flex-col gap-3 xl:w-[22rem]">
          <Panel legend="Pipeline">
            <PipelineOverview pending={overviewPending} result={overview} />
          </Panel>

          <RepoDetail body={selected} group={selectedGroup} nowSecs={nowSecs} />
        </aside>
      </div>
    </div>
  );
}

function PipelineOverview({
  pending,
  result,
}: {
  pending: boolean;
  result: EnvelopeResult<DeliveryOverviewV1> | undefined;
}) {
  if (pending) {
    return <StateChip kind="loading" detail="reading delivery projections" />;
  }
  if (!result) {
    return <StateChip kind="error" detail="delivery overview read failed" />;
  }
  if (result.outcome === 'transport') {
    return (
      <StateChip
        kind={result.state}
        detail={result.detail ?? `delivery overview ${result.state.replaceAll('_', ' ')}`}
      />
    );
  }
  const { authorization, domain_state: domainState, payload } = result.envelope;
  if (authorization.outcome !== 'authorized') {
    return (
      <StateChip
        kind={authorization.outcome}
        detail="delivery evidence was not disclosed"
      />
    );
  }
  if (domainState === 'denied' || domainState === 'unauthorized' || domainState === 'redacted') {
    return <StateChip kind={domainState} detail="delivery evidence was not disclosed" />;
  }

  return (
    <div className="flex flex-col gap-2.5">
      <div className="flex flex-wrap items-center gap-1.5">
        <StateChip kind={domainState} detail="delivery overview" />
        <EvidencePattern
          quality={domainState === 'ready' ? 'measured' : 'unknown'}
        />
      </div>
      <p className="text-2xs leading-relaxed text-text-muted">
        Active-checkout Git and retained provider evidence are independent.
        Each source keeps its reported state; retained rows remain inspectable
        through partial, stale, failed, rate-limited, and unavailable reads.
      </p>
      <PipelineStage icon={GitCommitHorizontal} label="Changes & commits">
        <ProjectionSource
          label="Working tree changes"
          projection={payload.changes}
          renderValue={renderGitStatus}
        />
        <ProjectionSource
          label="Commit history"
          projection={payload.commits}
          renderValue={renderCommitTimeline}
        />
      </PipelineStage>
      <PipelineStage icon={GitPullRequest} label="Pull requests & review">
        <ProjectionSource
          label="Pull requests"
          projection={payload.pull_requests}
          renderValue={renderPullRequests}
        />
        <ProjectionSource
          label="Review observations"
          projection={payload.review_comments}
          renderValue={renderReviews}
        />
      </PipelineStage>
      <PipelineStage icon={Server} label="Continuous integration">
        <ProjectionSource
          label="CI checks"
          projection={payload.ci_checks}
          renderValue={renderCiChecks}
        />
        <ProjectionSource
          label="Failure localization"
          projection={payload.failure_localization}
          renderValue={renderFailureLocalization}
        />
      </PipelineStage>
      <PipelineStage icon={Package} label="Releases">
        <ProjectionSource
          label="Release history"
          projection={payload.releases}
          renderValue={renderReleases}
        />
      </PipelineStage>
      <PipelineStage icon={ScrollText} label="Index freshness">
        <ProjectionSource
          label="Generation comparison"
          projection={payload.generation_freshness}
          renderValue={renderGenerationFreshness}
        />
      </PipelineStage>
    </div>
  );
}

type DeliveryProjection = DeliveryOverviewV1[keyof DeliveryOverviewV1];
type DeliveryProjectionValue<P extends DeliveryProjection> = P extends {
  value: infer V;
}
  ? NonNullable<V>
  : never;

function ProjectionSource<P extends DeliveryProjection>({
  label,
  projection,
  renderValue,
}: {
  label: string;
  projection: P;
  renderValue: (value: DeliveryProjectionValue<P>) => React.ReactNode;
}) {
  const value = projectionValue(projection);
  return (
    <section
      aria-label={label}
      data-projection-state={projection.state}
      className="flex min-w-0 flex-col gap-1.5 border-l border-edge-subtle pl-2"
    >
      <span className="td-legend text-text-secondary">{label}</span>
      <ProjectionState projection={projection} />
      {value == null ? null : renderValue(value)}
    </section>
  );
}

function projectionValue<P extends DeliveryProjection>(
  projection: P,
): DeliveryProjectionValue<P> | null {
  switch (projection.state) {
    case 'ready':
    case 'partial':
    case 'stale':
    case 'failed':
    case 'empty_measured':
      return projection.value as DeliveryProjectionValue<P>;
    case 'rate_limited':
    case 'unavailable':
      return projection.value as DeliveryProjectionValue<P> | null;
    case 'denied':
    case 'not_published':
      return null;
  }
}

function ProjectionState({ projection }: { projection: DeliveryProjection }) {
  switch (projection.state) {
    case 'ready':
      return <StateChip kind="ready" detail="ready" />;
    case 'partial':
      return <StateChip kind="partial" detail="partial · retained evidence shown" />;
    case 'stale':
      return <StateChip kind="stale" detail="stale · retained evidence shown" />;
    case 'failed':
      return <StateChip kind="error" detail="failed · retained evidence shown" />;
    case 'denied':
      return <StateChip kind="denied" detail="denied · evidence suppressed" />;
    case 'not_published':
      return (
        <StateChip
          kind="unsupported"
          detail={`not published · ${projection.reason} · requires ${projection.required_authority}`}
        />
      );
    case 'empty_measured':
      return <StateChip kind="complete_zero_findings" detail="measured empty" />;
    case 'unavailable':
      return (
        <StateChip
          kind="unavailable"
          detail={`unavailable · ${projection.reason} · requires ${projection.required_authority}`}
        />
      );
    case 'rate_limited': {
      const checkpoint = projection.checkpoint
        ? `${projection.checkpoint.remaining}/${projection.checkpoint.limit} remaining · reset ${projection.checkpoint.reset_at_micros} µs`
        : null;
      const retry = projection.retry_at_micros == null
        ? null
        : `retry ${projection.retry_at_micros} µs`;
      const parts = [checkpoint, retry].filter((part): part is string => part !== null);
      return (
        <StateChip
          kind="rate_limited"
          detail={parts.length > 0 ? parts.join(' · ') : undefined}
        />
      );
    }
  }
}

function renderGitStatus(status: DeliveryGitStatusV1) {
  return (
    <div className="flex flex-col gap-1 text-3xs text-text-muted">
      <span>
        {countLabel(status.changed_paths.length, 'changed path')} · {status.staged} staged ·{' '}
        {status.unstaged} unstaged · {status.conflicted} conflicted
      </span>
      {status.changed_paths.length > 0 ? (
        <ul aria-label="Changed paths" className="max-h-24 overflow-auto border border-edge-subtle">
          {status.changed_paths.map((path) => (
            <li key={path} className="truncate border-b border-edge-subtle px-1.5 py-0.5 last:border-b-0">
              {path}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function renderCommitTimeline(timeline: DeliveryCommitTimelineV1) {
  return (
    <TimelineItems
      count={timeline.items.length}
      singular="commit"
      truncated={timeline.truncated}
    >
      {timeline.items.map((commit) => (
        <li key={commit.commit} className="border-b border-edge-subtle px-1.5 py-1 last:border-b-0">
          <span className="block truncate text-text-secondary">{commit.subject}</span>
          <span className="td-value text-text-muted">{shortOid(commit.commit)}</span>
        </li>
      ))}
    </TimelineItems>
  );
}

function renderPullRequests(timeline: DeliveryPullRequestTimelineV1) {
  return (
    <div className="flex flex-col gap-1.5">
      <HeadComparison
        expected={timeline.expected_head_commit}
        retained={timeline.retained_head_commit}
      />
      <TimelineItems
        count={timeline.items.length}
        total={timeline.total_retained}
        singular="pull request"
        truncated={timeline.truncated}
      >
        {timeline.items.map((pullRequest) => (
          <li
            key={`${pullRequest.provider}:${pullRequest.pull_request_id}`}
            className="flex flex-col gap-1.5 border-b border-edge-subtle px-1.5 py-1 last:border-b-0"
          >
            <span className="font-medium text-text-secondary">
              {pullRequest.provider} · PR {pullRequest.pull_request_id}
              {pullRequest.identity ? ` — ${pullRequest.identity.title}` : ''}
            </span>
            {pullRequest.identity ? (
              <PullRequestIdentity identity={pullRequest.identity} />
            ) : (
              <span className="text-3xs text-text-muted">
                no retained PR identity read yet — title, state and diff shape
                appear after the next provider refresh
              </span>
            )}
            {pullRequest.operations.map((operation) => (
              <div key={operation.operation} className="flex flex-col gap-1 border-l border-edge-subtle pl-1.5">
                <span className="td-legend text-text-muted">{humanize(operation.operation)}</span>
                {operation.latest_attempt ? (
                  <OperationSnapshot label="latest attempt" snapshot={operation.latest_attempt} />
                ) : null}
                {operation.last_complete ? (
                  <OperationSnapshot label="last complete" snapshot={operation.last_complete} />
                ) : null}
              </div>
            ))}
          </li>
        ))}
      </TimelineItems>
    </div>
  );
}

/** PR identity and diff shape from the retained `RestGetPullRequest` read:
 * state, draftness and diff stats without a per-file listing. */
function PullRequestIdentity({ identity }: { identity: DeliveryPullRequestIdentityV1 }) {
  return (
    <span className="text-3xs text-text-muted">
      {humanize(identity.state)}
      {identity.draft ? ' · draft' : ''} · +{identity.additions} −{identity.deletions} ·{' '}
      {countLabel(identity.changed_files, 'file')} changed
    </span>
  );
}

function OperationSnapshot({
  label,
  snapshot,
}: {
  label: string;
  snapshot: DeliveryGitHubOperationSnapshotV1;
}) {
  return (
    <dl aria-label={label} className="grid grid-cols-2 gap-x-2 text-3xs text-text-muted">
      <Fact label={label} value={`${humanize(snapshot.outcome)} · ${humanize(snapshot.coverage)}`} />
      <Fact label="provider head" value={shortOid(snapshot.provider_head_commit_id)} />
      <Fact label="provider base" value={shortOid(snapshot.provider_base_commit_id)} />
      <Fact label="merge base" value={shortOid(snapshot.merge_base_commit_id)} />
      <Fact label="fetched at" value={`${snapshot.fetched_at_micros} µs`} />
    </dl>
  );
}

/** One review comment with the observation this page treats as current: the
 * most recently observed one, preferring the live latest attempt on ties. */
type ReviewComment = {
  item: DeliveryReviewItemV1;
  current: DeliveryReviewObservationV1;
};

type ReviewThread = {
  key: string;
  threadId: string | null;
  path: string;
  line: number | null;
  originalLine: number | null;
  lifecycle: DeliveryReviewObservationV1['lifecycle'];
  comments: ReviewComment[];
};

function currentObservation(
  item: DeliveryReviewItemV1,
): DeliveryReviewObservationV1 | null {
  let current: DeliveryReviewObservationV1 | null = null;
  for (const observation of item.observations) {
    if (
      current === null ||
      observation.observed_at_micros > current.observed_at_micros ||
      (observation.observed_at_micros === current.observed_at_micros &&
        observation.kind === 'latest_attempt' &&
        current.kind !== 'latest_attempt')
    ) {
      current = observation;
    }
  }
  return current;
}

/** Groups retained review comments into their provider threads. Comments
 * without a thread identity stand alone under their comment id. */
function groupReviewThreads(items: readonly DeliveryReviewItemV1[]): ReviewThread[] {
  const threads = new Map<string, ReviewThread>();
  for (const item of items) {
    const current = currentObservation(item);
    if (!current) continue;
    const key = current.thread_id
      ? `${item.provider}:${item.pull_request_id}:thread:${current.thread_id}`
      : `${item.provider}:${item.pull_request_id}:comment:${item.comment_id}`;
    const existing = threads.get(key);
    if (existing) {
      existing.comments.push({ item, current });
    } else {
      threads.set(key, {
        key,
        threadId: current.thread_id,
        path: current.path,
        line: current.line,
        originalLine: current.original_line,
        lifecycle: current.lifecycle,
        comments: [{ item, current }],
      });
    }
  }
  for (const thread of threads.values()) {
    thread.comments.sort((left, right) => {
      // Root comment first, then provider observation order.
      const leftRoot = left.current.reply_to_comment_id === null ? 0 : 1;
      const rightRoot = right.current.reply_to_comment_id === null ? 0 : 1;
      if (leftRoot !== rightRoot) return leftRoot - rightRoot;
      return left.current.observed_at_micros - right.current.observed_at_micros;
    });
  }
  return [...threads.values()];
}

function threadLocation(thread: ReviewThread): string {
  if (thread.line != null) return `${thread.path}:${thread.line}`;
  if (thread.originalLine != null) {
    return `${thread.path}:${thread.originalLine} (original diff)`;
  }
  return thread.path;
}

function renderReviews(timeline: DeliveryReviewTimelineV1) {
  const threads = groupReviewThreads(timeline.items);
  return (
    <div className="flex flex-col gap-1.5">
      <HeadComparison
        expected={timeline.expected_head_commit}
        retained={timeline.retained_head_commit}
      />
      <TimelineItems
        count={timeline.items.length}
        total={timeline.total_retained}
        singular="review comment"
        truncated={timeline.truncated}
      >
        {threads.map((thread) => (
          <li
            key={thread.key}
            className="flex flex-col gap-1 border-b border-edge-subtle px-1.5 py-1 last:border-b-0"
          >
            <div className="flex flex-wrap items-center gap-1.5">
              <span className="td-value font-medium text-text-secondary">
                {threadLocation(thread)}
              </span>
              <span className="td-legend text-text-muted">{humanize(thread.lifecycle)}</span>
              {thread.threadId ? (
                <span className="td-legend text-text-muted">thread {thread.threadId}</span>
              ) : null}
            </div>
            {thread.comments.map((comment) => (
              <ReviewCommentRow
                key={comment.item.comment_id}
                comment={comment}
              />
            ))}
          </li>
        ))}
      </TimelineItems>
    </div>
  );
}

function ReviewCommentRow({ comment }: { comment: ReviewComment }) {
  const { item, current } = comment;
  return (
    <div className="flex flex-col gap-0.5 border-l border-edge-subtle pl-1.5">
      <span className="text-3xs text-text-muted">
        {humanize(current.author_class)} · {humanize(current.review_state)} ·{' '}
        {humanize(current.lifecycle)}
        {current.reply_to_comment_id ? ` · reply to ${current.reply_to_comment_id}` : ''}
        {' · '}comment {item.comment_id}
      </span>
      {current.body_preview ? (
        <p className="whitespace-pre-wrap text-2xs leading-relaxed text-text-secondary">
          {current.body_preview.text}
          {current.body_preview.truncated ? '…' : ''}
        </p>
      ) : (
        <span className="text-3xs text-text-muted">
          body retained but not expanded for this read
        </span>
      )}
      <span className="text-3xs text-text-muted">
        {item.observations.length === 1
          ? `1 observation · ${humanize(current.operation)}`
          : `${item.observations.length} observations · latest ${humanize(current.operation)} (${humanize(current.kind)})`}
        {' · '}observed at {current.observed_at_micros} µs
      </span>
    </div>
  );
}

function renderCiChecks(timeline: DeliveryCiTimelineV1) {
  return (
    <div className="flex flex-col gap-1.5">
      <HeadComparison
        expected={timeline.expected_head_commit}
        retained={timeline.retained_head_commit}
      />
      <TimelineItems
        count={timeline.items.length}
        total={timeline.total_retained}
        singular="CI check"
        truncated={timeline.truncated}
      >
        {timeline.items.map((check) => (
          <li
            key={check.observation_id}
            className="flex flex-col gap-1 border-b border-edge-subtle px-1.5 py-1 last:border-b-0"
          >
            <span className="truncate font-medium text-text-secondary">{check.workflow_path}</span>
            <dl className="grid grid-cols-2 gap-x-2 text-3xs text-text-muted">
              <Fact label="workflow" value={statusAndConclusion(check.workflow_status, check.workflow_conclusion)} />
              <Fact label="job" value={statusAndConclusion(check.job_status, check.job_conclusion)} />
              <Fact label="check" value={statusAndConclusion(check.check_status, check.check_conclusion)} />
              <Fact label="failure kind" value={humanize(check.failure_kind)} />
              <Fact label="observation" value={check.observation_id} />
              <Fact label="provider head" value={shortOid(check.provider_head_commit)} />
              <Fact label="workflow ID" value={check.run.workflow_id} />
              <Fact label="run ID" value={check.run.run_id} />
              <Fact label="attempt ID" value={check.run.attempt_id} />
              <Fact label="suite ID" value={check.run.check_suite_id} />
              <Fact label="job ID" value={check.run.job_id} />
              <Fact label="check run ID" value={check.run.check_run_id} />
              {check.failed_step ? <Fact label="failed step" value={check.failed_step} /> : null}
              <Fact label="observed at" value={`${check.observed_at_micros} µs`} />
            </dl>
            <CiAnnotations check={check} />
          </li>
        ))}
      </TimelineItems>
    </div>
  );
}

/** Retained annotation summaries. The provider count stays authoritative, so
 * a bounded listing says how many rows it is showing of that count. */
function CiAnnotations({ check }: { check: DeliveryCiCheckV1 }) {
  if (check.annotation_count === 0) {
    return <span className="text-3xs text-text-muted">no annotations reported</span>;
  }
  const shown = check.annotations.length;
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-3xs text-text-muted">
        {shown < check.annotation_count
          ? `${shown} of ${check.annotation_count} annotations shown`
          : countLabel(shown, 'annotation')}
      </span>
      {shown > 0 ? (
        <ul
          aria-label={`Annotations for ${check.workflow_path}`}
          className="border-l border-edge-subtle pl-1.5"
        >
          {check.annotations.map((annotation, index) => (
            <li
              key={`${annotation.path}:${annotation.start_line}:${index}`}
              className="text-3xs text-text-muted"
            >
              <span className="td-value text-text-secondary">
                {annotation.path}:{annotation.start_line}
                {annotation.end_line !== annotation.start_line ? `-${annotation.end_line}` : ''}
              </span>{' '}
              · {humanize(annotation.level)}
              {annotation.title ? ` · ${annotation.title}` : ''}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function renderFailureLocalization(timeline: DeliveryFailureLocalizationTimelineV1) {
  return (
    <TimelineItems count={timeline.items.length} singular="localized failure" truncated={timeline.truncated}>
      {timeline.items.map((item) => (
        <li key={item.id} className="border-b border-edge-subtle px-1.5 py-1 last:border-b-0">
          <span className="text-text-secondary">{item.label}</span>{' '}
          <span className="td-value text-text-muted">{item.id}</span>
        </li>
      ))}
    </TimelineItems>
  );
}

function renderReleases(timeline: DeliveryReleaseTimelineV1) {
  return (
    <TimelineItems count={timeline.items.length} singular="release" truncated={timeline.truncated}>
      {timeline.items.map((release) => (
        <li key={release.id} className="flex flex-col gap-1 border-b border-edge-subtle px-1.5 py-1 last:border-b-0">
          <span className="font-medium text-text-secondary">
            {release.tag}{release.name ? ` · ${release.name}` : ''}
          </span>
          <span className="text-3xs text-text-muted">
            release {release.release_id} · {release.draft ? 'draft' : 'not draft'} ·{' '}
            {release.prerelease ? 'prerelease' : 'not prerelease'} · created{' '}
            {release.created_at_micros} µs
          </span>
          {release.published_at_micros == null ? null : (
            <span className="text-3xs text-text-muted">published {release.published_at_micros} µs</span>
          )}
          {release.assets.length > 0 ? (
            <ul aria-label={`Assets for ${release.tag}`} className="border-l border-edge-subtle pl-1.5">
              {release.assets.map((asset) => (
                <li key={asset.asset_id} className="text-3xs text-text-muted">
                  <span className="text-text-secondary">{asset.name}</span> · {asset.content_type} ·{' '}
                  {asset.size_bytes} bytes · {asset.download_count} downloads
                  {asset.digest ? ` · ${shortOpaque(asset.digest)}` : ''} · created{' '}
                  {asset.created_at_micros} µs · updated {asset.updated_at_micros} µs
                </li>
              ))}
            </ul>
          ) : null}
        </li>
      ))}
    </TimelineItems>
  );
}

function renderGenerationFreshness(value: DeliveryGenerationFreshnessV1) {
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <StateChip
        kind={value.comparison === 'current' ? 'ready' : 'stale'}
        detail={`${humanize(value.comparison)} · HEAD ${shortOid(value.head_commit)} · indexed ${shortOid(value.indexed_commit)}`}
      />
      <EvidencePattern quality={value.comparison === 'current' ? 'measured' : 'unknown'} />
    </div>
  );
}

function HeadComparison({ expected, retained }: { expected: string; retained: string }) {
  return (
    <div className="grid grid-cols-2 gap-x-2 text-3xs text-text-muted">
      <Fact label="live head" value={shortOid(expected)} />
      <Fact label="retained head" value={shortOid(retained)} />
    </div>
  );
}

function TimelineItems({
  count,
  total,
  singular,
  truncated,
  children,
}: {
  count: number;
  /** Retained rows known to the source, shown or not; renders "N of M". */
  total?: number;
  singular: string;
  truncated: boolean;
  children: React.ReactNode;
}) {
  const counted =
    total != null && total > count
      ? `${count} of ${total} ${singular}${total === 1 ? '' : 's'}`
      : countLabel(count, singular);
  return (
    <div className="flex flex-col gap-1">
      <span className="text-3xs text-text-muted">
        {counted}{truncated ? ' shown · more evidence not shown' : ''}
      </span>
      {count > 0 ? (
        <ul className="max-h-52 overflow-auto border border-edge-subtle">{children}</ul>
      ) : null}
    </div>
  );
}

function statusAndConclusion(status: string, conclusion: string | null): string {
  return conclusion ? `${humanize(status)} · ${humanize(conclusion)}` : humanize(status);
}

function humanize(value: string): string {
  return value.replaceAll('_', ' ');
}

function shortOpaque(value: string): string {
  return value.length > 16 ? `${value.slice(0, 16)}…` : value;
}

function shortOid(value: string): string {
  return value.slice(0, 8);
}

function countLabel(count: number, singular: string): string {
  return `${count} ${singular}${count === 1 ? '' : 's'}`;
}

/** The axes, printed. Both of them are easy to misread — one looks like commit
 * recency and is not, the other is logarithmic — so both are stated. */
function FieldAxis({ field }: { field: DeliveryField }) {
  const busiest = field.columns.reduce((max, column) => Math.max(max, column.count), 0);
  return (
    <div className="flex flex-col gap-1.5">
      <Legend>last indexed across · branches up</Legend>
      <div className="flex flex-wrap border-y border-edge-subtle bg-surface-1">
        {field.columns.map((column) => (
          <div
            key={column.id}
            className="min-w-0 flex-1 basis-24 border-l border-edge-subtle px-2.5 py-1.5 first:border-l-0"
          >
            <Readout
              label={column.label}
              value={column.count}
              unit={column.bound}
              fraction={busiest > 0 ? column.count / busiest : null}
              size="sm"
            />
          </div>
        ))}
      </div>
      <p className="text-2xs leading-relaxed text-text-muted">
        Each body is one repository: column = when TraceDecay last indexed it —{' '}
        <span className="text-text-secondary">
          not when it was last committed to; commit history is shown separately
          for the active checkout
        </span>{' '}
        — height = how many branches it has ({field.branchFloor} to{' '}
        {field.branchCeiling}, log scale), size = how many checkouts map to it,
        brightness = the same index recency. A ring marks the active project.{' '}
        {field.unknownBranchCount > 0
          ? `${field.unknownBranchCount} ${field.unknownBranchCount === 1 ? 'entry sits' : 'entries sit'} in the fenced band below the plot: they have no git directory, so their branch count is unknown rather than zero. `
          : ''}
        {field.multiCheckoutCount === 0
          ? 'Every repository here has exactly one checkout, so every body is the same size — the size channel is live but this registry has nothing to spend it on.'
          : `${field.multiCheckoutCount} ${field.multiCheckoutCount === 1 ? 'repository has' : 'repositories have'} more than one checkout and ${field.multiCheckoutCount === 1 ? 'is' : 'are'} drawn larger.`}
      </p>
    </div>
  );
}

/** The field's accessible equivalent, and the scanning surface: one line per
 * repository instead of the header-plus-row pair the flat list used to spend
 * on each one. */
function RepoTable({
  field,
  selectedId,
  onSelect,
  truncated,
  nowSecs,
}: {
  field: DeliveryField;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  truncated: boolean;
  nowSecs: number;
}) {
  return (
    <section aria-label="Repositories" className="flex min-w-0 flex-col">
      <Legend>repositories · most recently indexed first</Legend>
      <div
        role="region"
        aria-label="Repositories table"
        // Table rows hold no focusable elements, so the scroll region itself
        // must be keyboard-operable.
        tabIndex={0}
        className="mt-1.5 max-h-80 overflow-auto border border-edge-subtle"
      >
        <table className="w-full border-collapse text-2xs">
          <caption className="sr-only">
            Every repository on the field, most recently indexed first, with its
            branch count, checkouts and default branch.
          </caption>
          <thead className="sticky top-0 bg-surface-2">
            <tr className="text-left text-text-secondary">
              <th scope="col" className="px-2 py-1 font-medium">Repository</th>
              <th scope="col" className="px-2 py-1 font-medium">Default branch</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Branches</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Checkouts</th>
              <th scope="col" className="px-2 py-1 text-right font-medium">Indexed</th>
            </tr>
          </thead>
          <tbody>
            {field.bodies.map((body) => (
              <tr
                key={body.id}
                className={cn(
                  'border-t border-edge-subtle',
                  selectedId === body.id && 'bg-accent/10',
                )}
              >
                <td className="max-w-0 px-2 py-1">
                  <button
                    type="button"
                    onClick={() => onSelect(selectedId === body.id ? null : body.id)}
                    aria-pressed={selectedId === body.id}
                    className="flex min-h-[var(--touch-target-min)] w-full min-w-0 items-center gap-1.5 text-left"
                  >
                    {body.branches == null ? (
                      <FolderGit2 aria-hidden size={11} className="shrink-0 text-text-muted" />
                    ) : (
                      <GitBranch aria-hidden size={11} className="shrink-0 text-text-muted" />
                    )}
                    <span className="truncate text-text-primary">{body.label}</span>
                    {body.active ? (
                      <span className="td-legend shrink-0 bg-accent/15 px-1 text-text-primary">
                        active
                      </span>
                    ) : null}
                  </button>
                </td>
                <td className="max-w-0 truncate px-2 py-1 text-text-secondary">
                  {body.defaultBranch ?? '—'}
                </td>
                <td className="px-2 py-1 text-right" data-cell="numeric">
                  {body.branches == null ? (
                    <span className="text-text-muted">unknown</span>
                  ) : (
                    <span className="inline-flex items-center gap-1.5">
                      <Meter
                        fraction={
                          field.branchCeiling > 0
                            ? body.branches / field.branchCeiling
                            : null
                        }
                        className="w-10 shrink-0"
                      />
                      <span className="tabular-nums text-text-primary">
                        {body.branches}
                      </span>
                    </span>
                  )}
                </td>
                <td
                  className="px-2 py-1 text-right text-text-secondary tabular-nums"
                  data-cell="numeric"
                >
                  {body.checkouts}
                  {body.worktrees > 0 ? (
                    <span className="text-text-muted"> ({body.worktrees} wt)</span>
                  ) : null}
                </td>
                <td
                  className="px-2 py-1 text-right text-text-muted tabular-nums"
                  data-cell="numeric"
                >
                  <FreshnessMeter
                    tier={freshnessTier(Math.max(0, nowSecs - body.lastSeenAt))}
                    label={relativeAge(body.lastSeenAt, nowSecs) ?? 'not recorded'}
                  />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {truncated ? (
        <p className="pt-1 text-2xs text-text-muted">
          Result truncated by the registry limit — more repositories exist than
          are drawn.
        </p>
      ) : null}
    </section>
  );
}

/** The selected repository, expanded: its checkouts and the branch names the
 * registry actually holds. Branch names are all there is — no branch here
 * carries a time, an author or a tip commit. */
function RepoDetail({
  body,
  group,
  nowSecs,
}: {
  body: DeliveryBody | null;
  group: ProjectRepoGroup | undefined;
  nowSecs: number;
}) {
  if (!body || !group) {
    return (
      <Panel legend="Repository">
        <p className="text-2xs leading-relaxed text-text-muted">
          Select a repository — on the field or in the table — to see its
          checkouts and the branch names the registry holds for it.
        </p>
      </Panel>
    );
  }
  return (
    <Panel legend="Repository">
      <div className="flex flex-col gap-3">
        <div className="flex flex-col gap-1">
          <span className="text-xs font-medium leading-snug text-text-primary">
            {body.label}
          </span>
          <span className="td-value truncate text-3xs text-text-muted">
            {group.git_common_dir ?? 'no git directory'}
          </span>
        </div>

        <dl className="grid grid-cols-2 gap-x-3 gap-y-1.5 text-2xs">
          <Fact
            label="branches"
            value={body.branches == null ? 'unknown' : String(body.branches)}
            muted={body.branches == null}
          />
          <Fact label="checkouts" value={String(body.checkouts)} />
          <Fact label="worktrees" value={String(body.worktrees)} />
          <Fact
            label="last indexed"
            value={relativeAge(body.lastSeenAt, nowSecs) ?? 'not recorded'}
          />
        </dl>

        <div className="flex flex-col gap-1.5">
          <Legend>checkouts</Legend>
          <ul className="flex flex-col border border-edge-subtle">
            {group.projects.map((project) => (
              <li
                key={project.project_id}
                className="flex items-center gap-1.5 border-b border-edge-subtle px-2 py-1 last:border-b-0"
              >
                {project.kind === 'worktree' ? (
                  <GitFork aria-hidden size={11} className="shrink-0 text-text-muted" />
                ) : (
                  <FolderGit2 aria-hidden size={11} className="shrink-0 text-text-muted" />
                )}
                <span className="td-value min-w-0 flex-1 truncate text-3xs text-text-secondary">
                  {project.project_root}
                </span>
                <span className="td-legend shrink-0 text-text-muted">
                  {project.kind}
                </span>
              </li>
            ))}
          </ul>
        </div>

        <div className="flex flex-col gap-1.5">
          <Legend
            trailing={
              <span
                className="td-value shrink-0 text-3xs text-text-muted"
                data-cell="numeric"
              >
                {group.branches.length}
              </span>
            }
          >
            branch names
          </Legend>
          {group.branches.length === 0 ? (
            <StateChip
              kind={body.branches == null ? 'unsupported' : 'complete_zero_findings'}
              detail={
                body.branches == null
                  ? 'not a git checkout'
                  : 'registry holds no branch names'
              }
            />
          ) : (
            <>
              <ul
                aria-label="Branch names"
                tabIndex={0}
                className="max-h-40 overflow-auto border border-edge-subtle"
              >
                {group.branches.map((branch) => (
                  <li
                    key={branch}
                    className="td-value truncate border-b border-edge-subtle px-2 py-0.5 text-3xs text-text-secondary last:border-b-0"
                  >
                    {branch}
                  </li>
                ))}
              </ul>
              <span className="text-3xs leading-relaxed text-text-muted">
                Names only. The registry records no tip commit, author or time
                for any of these.
              </span>
            </>
          )}
        </div>
      </div>
    </Panel>
  );
}

function fieldDescription(field: DeliveryField): string {
  const occupied = field.columns
    .filter((column) => column.count > 0)
    .map((column) => `${column.count} ${column.label}`)
    .join(', ');
  return `Delivery field: ${field.bodies.length} repositories placed by when TraceDecay last indexed them (${occupied || 'none'}) and by branch count, ${field.branchFloor} to ${field.branchCeiling}. ${field.unknownBranchCount} entries have no git directory and no branch measurement. The repository table below is the accessible equivalent.`;
}

function PipelineStage({
  icon: Icon,
  label,
  children,
}: {
  icon: LucideIcon;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <span className="flex items-center gap-1.5 text-2xs font-medium text-text-secondary">
        <Icon aria-hidden size={12} className="text-text-muted" />
        {label}
      </span>
      {children}
    </div>
  );
}
