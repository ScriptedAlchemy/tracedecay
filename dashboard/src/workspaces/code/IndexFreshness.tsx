/**
 * INDEX FRESHNESS — `GET /api/code-index/freshness`.
 *
 * The branch-aware answer to "is the graph beside this panel current, and
 * current *for what*". Every symbol, edge and trace on this page was read from
 * one sealed code-index generation, and that generation was sealed against one
 * exact source reference — so a spine drawn from a generation sealed on
 * `refs/heads/main` while the checkout sits on a feature branch is stale in a
 * way no node count reveals.
 *
 * The route is honest by construction and this surface keeps it that way:
 *
 *   unsupported  the dashboard is not attached to a daemon-owned scheduler
 *                registry at all. There is no generation to report, and the
 *                panel says so instead of drawing a "fresh" badge.
 *   unknown      a registry is attached but has no mounted scheduler for this
 *                project, or has one that has not sealed a generation.
 *   loading      a mount exists and is indexing — a generation is coming.
 *   partial      a mount and a generation exist but the scheduler's own
 *                coverage of them is incomplete.
 *   ready        a complete, fresh generation with complete coverage.
 *
 * Those five are the server's states, not this component's: the envelope's
 * `domain_state` is rendered directly, and nothing here infers freshness from
 * the payload fields.
 */
import { useQuery } from '@tanstack/react-query';
import { useEffect, useState } from 'react';
import {
  CodeIndexFreshnessPayloadV1Schema,
  type CodeIndexFreshnessPayloadV1,
  type CodeIndexWorktreeFreshnessV1,
} from '../../contracts/generated.ts';
import { fetchEnvelope, type EnvelopeResult } from '../../data/query/envelope.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { authorizationState } from '../../ui/EnvelopeTruth.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { elideStart, formatCount, formatMicrosUtc, splitBytes } from '../../ui/format.ts';

type CodeIndexBuildProgress = NonNullable<CodeIndexWorktreeFreshnessV1['progress']>;

export function IndexFreshness() {
  const scope = useScope((s) => s.scope);
  const freshness = useQuery({
    queryKey: ['code-index', 'freshness', scopeKey(scope)],
    queryFn: () =>
      fetchEnvelope(scopedUrl(scope, '/api/code-index/freshness'), CodeIndexFreshnessPayloadV1Schema),
    refetchInterval: (query) => (hasActiveBuild(query.state.data) ? 1_000 : 30_000),
  });

  return (
    <section className="flex flex-col gap-1.5" aria-label="Code index freshness">
      <div className="flex items-center gap-1.5">
        <h3 className="td-legend">index freshness</h3>
        <span aria-hidden className="td-rule" />
      </div>
      {freshness.isPending ? (
        <p className="text-2xs text-state-loading">reading scheduler state…</p>
      ) : freshness.data === undefined ? (
        <p className="text-2xs text-state-unknown">no response recorded</p>
      ) : (
        <FreshnessReading result={freshness.data} />
      )}
    </section>
  );
}

function FreshnessReading({ result }: { result: EnvelopeResult<CodeIndexFreshnessPayloadV1> }) {
  const latestProgress = useLatestBuildProgress(result);
  if (result.outcome === 'transport') {
    return (
      <div className="flex flex-col gap-1.5">
        <StateChip kind={result.state} detail={result.detail ?? 'daemon unreachable'} />
      </div>
    );
  }
  const { envelope } = result;
  const { worktrees, note } = envelope.payload;
  // Authorization is an independent axis from the read's own state: a mount can
  // be `ready` and separately `redacted` for the identity asking. Folding them
  // together loses which one the reader is actually blocked by.
  const authorization = authorizationState(envelope.authorization);
  return (
    <div className="flex flex-col gap-2" data-index-freshness={envelope.domain_state}>
      <StateChip kind={envelope.domain_state} />
      {authorization ? (
        <StateChip kind={authorization} detail="read authorization" />
      ) : null}
      {worktrees.map((worktree) => (
        <WorktreeReading
          key={worktree.worktree_root}
          progress={latestProgress.get(worktree.worktree_root)}
          worktree={worktree}
        />
      ))}
      {/* The route's own sentence for why the list is the length it is. It is
        * the only thing distinguishing "no scheduler is attached" from "a
        * scheduler is attached and this project is not mounted", and both
        * arrive as an empty array. */}
      <p className="text-3xs leading-snug text-text-muted">{note}</p>
    </div>
  );
}

/**
 * One mounted worktree.
 *
 * The source reference leads because it is the branch-aware part: it names what
 * the sealed generation is a picture *of*. Everything below it is identity and
 * timing for that same generation.
 */
function WorktreeReading({
  worktree,
  progress,
}: {
  worktree: CodeIndexWorktreeFreshnessV1;
  progress: CodeIndexBuildProgress | undefined;
}) {
  return (
    <div
      className="td-raised flex flex-col gap-1.5 border border-edge-subtle px-2.5 py-2"
      data-worktree-staleness={worktree.staleness_state ?? 'unreported'}
    >
      <div className="flex min-w-0 flex-col gap-0.5">
        <span className="td-legend">source reference</span>
        <span
          className="td-value min-w-0 truncate text-2xs text-text-primary"
          title={worktree.source_reference ?? undefined}
        >
          {worktree.source_reference ?? 'not reported by the scheduler'}
        </span>
      </div>
      {progress ? <BuildProgressReading progress={progress} /> : null}
      <dl className="flex flex-col gap-1 text-3xs leading-snug">
        <Row label="staleness">{worktree.staleness_state ?? 'not reported'}</Row>
        <Row label="coverage">{worktree.coverage}</Row>
        <Row label="generation" mono>
          {worktree.latest_generation_id ?? 'no sealed generation yet'}
        </Row>
        {worktree.snapshot_content_identity ? (
          <Row label="snapshot" mono>
            {worktree.snapshot_content_identity}
          </Row>
        ) : null}
        <Row label="sealed">{formatMicros(worktree.sealed_at_micros)}</Row>
        <Row label="reconciled">{formatMicros(worktree.last_reconcile_micros)}</Row>
        {/* A pending-hint count of zero is a real reading — the scheduler
          * counted and found none — so unlike the identity fields above it is
          * printed whenever the server sent a number, and omitted only when it
          * sent none. */}
        {worktree.hook_hint_count != null ? (
          <Row label="pending hints">{worktree.hook_hint_count.toLocaleString()}</Row>
        ) : null}
        {worktree.repository_id ? (
          <Row label="repository" mono>
            {worktree.repository_id}
          </Row>
        ) : null}
      </dl>
      <p
        className="td-value truncate text-3xs text-text-muted"
        title={worktree.worktree_root}
      >
        {elideStart(worktree.worktree_root, 40)}
      </p>
    </div>
  );
}

function BuildProgressReading({
  progress,
}: {
  progress: CodeIndexBuildProgress;
}) {
  const percentage = progressPercentage(progress);
  const hasRate =
    progress.files_per_second != null && progress.lexical_bytes_per_second != null;
  return (
    <div
      className="flex flex-col gap-1.5 border-b border-edge-subtle pb-1.5 text-3xs leading-snug"
      data-code-index-progress={progress.generation_id}
    >
      <div className="flex min-w-0 items-baseline justify-between gap-2">
        <span className="td-legend">Code progress</span>
        <span className="td-value shrink-0 text-text-primary">
          {phaseLabel(progress.phase)} · {percentage.toFixed(1)}%
        </span>
      </div>
      <progress
        aria-label="Code progress"
        className="h-1.5 w-full accent-accent"
        max={100}
        value={percentage}
      />
      <dl className="flex flex-col gap-1">
        <Row label="generation" mono>
          {progress.generation_id}
        </Row>
        <Row label="files">
          {`${formatCount(progress.completed_files)} / ${formatCount(progress.total_files)} files`}
        </Row>
        <Row label="pages">{`${formatCount(progress.committed_pages)} pages committed`}</Row>
        <Row label="chunks">{`${formatCount(progress.committed_chunks)} chunks committed`}</Row>
        <Row label="imports">{`${formatCount(progress.committed_imports)} imports committed`}</Row>
        <Row label="payload">{`${formatBytes(progress.committed_payload_bytes)} payload committed`}</Row>
        <Row label="batch">
          {`${formatCount(progress.current_batch_pages)} pages · ${formatBytes(progress.current_batch_payload_bytes)}`}
        </Row>
        <Row label="throughput">
          {progress.files_per_second != null && progress.lexical_bytes_per_second != null
            ? `${formatCount(progress.files_per_second)} files/s · ${formatBytes(progress.lexical_bytes_per_second)} lexical bytes/s`
            : 'throughput unavailable'}
        </Row>
        <Row label="ETA">
          {hasRate && progress.estimated_remaining_seconds != null
            ? `ETA ${formatDurationSeconds(progress.estimated_remaining_seconds)}`
            : 'ETA unavailable'}
        </Row>
        <Row label="last progress">{formatMicros(progress.last_progress_micros)}</Row>
        <Row label="last commit">
          {progress.last_commit_latency_micros != null
            ? formatDurationMicros(progress.last_commit_latency_micros)
            : 'not reported'}
        </Row>
      </dl>
      {progress.blocked_reason ? (
        <p className="text-state-warning">blocked: {blockedReasonLabel(progress.blocked_reason)}</p>
      ) : null}
    </div>
  );
}

function hasActiveBuild(result: EnvelopeResult<CodeIndexFreshnessPayloadV1> | undefined): boolean {
  return (
    result?.outcome === 'envelope' &&
    (result.envelope.domain_state !== 'ready' ||
      result.envelope.payload.worktrees.some(
        (worktree) => worktree.progress != null && worktree.progress.phase !== 'ready',
      ))
  );
}

function useLatestBuildProgress(
  result: EnvelopeResult<CodeIndexFreshnessPayloadV1>,
): ReadonlyMap<string, CodeIndexBuildProgress> {
  const [latestProgress, setLatestProgress] = useState<ReadonlyMap<string, CodeIndexBuildProgress>>(
    () => new Map(),
  );
  useEffect(() => {
    if (result.outcome !== 'envelope') return;
    setLatestProgress((rendered) => {
      const next = new Map(rendered);
      let changed = false;
      for (const worktree of result.envelope.payload.worktrees) {
        const incoming = worktree.progress;
        const current = next.get(worktree.worktree_root);
        if (!incoming) {
          if (
            current &&
            result.envelope.domain_state === 'ready' &&
            worktree.latest_generation_id === current.generation_id
          ) {
            next.delete(worktree.worktree_root);
            changed = true;
          }
        } else if (!current || isCurrentOrNewerProgress(incoming, current)) {
          next.set(worktree.worktree_root, incoming);
          changed = true;
        }
      }
      return changed ? next : rendered;
    });
  }, [result]);
  return latestProgress;
}

function isCurrentOrNewerProgress(
  incoming: CodeIndexBuildProgress,
  rendered: CodeIndexBuildProgress,
): boolean {
  if (incoming.daemon_incarnation !== rendered.daemon_incarnation) {
    return incoming.daemon_incarnation > rendered.daemon_incarnation;
  }
  if (incoming.producer_incarnation !== rendered.producer_incarnation) {
    return incoming.producer_incarnation > rendered.producer_incarnation;
  }
  return incoming.progress_epoch >= rendered.progress_epoch;
}

function progressPercentage(progress: CodeIndexBuildProgress): number {
  const completed =
    progress.total_lexical_bytes > 0
      ? progress.completed_lexical_bytes / progress.total_lexical_bytes
      : progress.phase === 'ready'
        ? 1
        : 0;
  return Math.min(100, Math.max(0, completed * 100));
}

function phaseLabel(phase: CodeIndexBuildProgress['phase']): string {
  switch (phase) {
    case 'source_scan':
      return 'source scan';
    case 'relational_preparation':
      return 'relational preparation';
    case 'bulk_commit':
      return 'bulk commit';
    case 'index_build':
      return 'index build';
    case 'verification':
      return 'verification';
    case 'ready':
      return 'ready';
  }
}

function blockedReasonLabel(reason: NonNullable<CodeIndexBuildProgress['blocked_reason']>): string {
  switch (reason) {
    case 'resident_memory':
      return 'resident memory';
    case 'source_unavailable':
      return 'source unavailable';
    case 'artifact_store_unavailable':
      return 'artifact store unavailable';
    case 'retry_backoff':
      return 'retry backoff';
  }
}

function formatBytes(bytes: number): string {
  const { value, unit } = splitBytes(bytes);
  return unit ? `${value} ${unit}` : value;
}

function formatDurationSeconds(seconds: number): string {
  if (seconds < 90) return `${Math.round(seconds)}s`;
  if (seconds < 5_400) return `${Math.round(seconds / 60)}m`;
  const hours = Math.floor(seconds / 3_600);
  const minutes = Math.round((seconds % 3_600) / 60);
  return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
}

function formatDurationMicros(micros: number): string {
  if (micros < 1_000) return `${micros}µs`;
  if (micros < 1_000_000) return `${Math.round(micros / 1_000)}ms`;
  return formatDurationSeconds(micros / 1_000_000);
}

function Row({
  label,
  children,
  mono,
}: {
  label: string;
  children: string;
  mono?: boolean;
}) {
  return (
    <div className="flex min-w-0 items-baseline gap-1.5">
      <dt className="shrink-0 uppercase tracking-[0.08em] text-text-muted">{label}</dt>
      <dd
        className={`min-w-0 flex-1 truncate text-right text-text-secondary${mono ? ' td-value' : ''}`}
        title={children}
      >
        {children}
      </dd>
    </div>
  );
}

/** A stamp the scheduler did not report stays unreported. An absent time is not
 * the epoch, and printing `1970-01-01` for one would read as a real, very old
 * observation. */
function formatMicros(micros: number | null): string {
  return formatMicrosUtc(micros, { nullAs: 'not reported' });
}
