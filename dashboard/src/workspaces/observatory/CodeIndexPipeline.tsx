import { useEffect, useState } from 'react';
import type {
  CodeIndexBuildProgressV1,
  CodeIndexFreshnessPayloadV1,
  CodeIndexMountFailureV1,
  CodeIndexWorktreeFreshnessV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { formatCount } from '../../ui/format.ts';
import { StateChip } from '../../ui/StateChip';

/** The exact code-index pipeline read: per-worktree readiness and every live
 * build's progress, retained across polls so a moving build never blinks out
 * between two reads that disagree about which incarnation is current. */
export function CodeIndexPipeline({
  result,
  pending,
  scopeKey,
}: {
  result: EnvelopeResult<CodeIndexFreshnessPayloadV1> | undefined;
  pending: boolean;
  scopeKey: string;
}) {
  const progress = useLatestCodeIndexProgress(result, scopeKey);
  const payload = result?.outcome === 'envelope' ? result.envelope.payload : undefined;
  const worktrees = payload?.worktrees ?? [];
  const observedAtMicros =
    result?.outcome === 'envelope' ? result.envelope.time.observation_time_micros : null;
  if (pending) {
    return (
      <section className="mx-4 mt-3" aria-label="Code-index pipeline">
        <p className="text-body text-text-muted">reading code-index pipeline…</p>
      </section>
    );
  }
  if (result?.outcome === 'transport') {
    return (
      <section className="mx-4 mt-3" aria-label="Code-index pipeline">
        <StateChip kind={result.state} detail={result.detail ?? 'code-index progress unavailable'} />
      </section>
    );
  }
  return (
    <section
      className="mx-4 mt-3 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 p-3"
      aria-label="Code-index pipeline"
    >
      <h2 className="td-legend">Code-index pipeline</h2>
      {payload?.mount_failure ? (
        <CodeIndexMountFailureNotice className="mt-2" failure={payload.mount_failure} />
      ) : (
        <CodeIndexReadinessList worktrees={worktrees} observedAtMicros={observedAtMicros} />
      )}
      {worktrees.some((worktree) => worktree.restore_progress != null) ? (
        <div className="mt-2 flex flex-col gap-2">
          {worktrees.flatMap((worktree) =>
            worktree.restore_progress ? (
              <article
                key={`${worktree.worktree_root}:restore`}
                className="rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 p-2.5"
              >
                <p className="font-medium text-text-secondary">restoring lexical artifact</p>
                <progress
                  aria-label={`Artifact restore for ${worktree.restore_progress.generation_id}`}
                  className="mt-1.5 h-1.5 w-full accent-accent"
                  max={worktree.restore_progress.authenticated_total}
                  value={worktree.restore_progress.authenticated_completed}
                />
                <p className="mt-1 text-sm text-text-muted">
                  {worktree.restore_progress.authenticated_remaining} authentication checks remaining
                </p>
              </article>
            ) : [],
          )}
        </div>
      ) : null}
      {progress.length === 0 ? (
        worktrees.some(hasLiveWorktreeProgress) ? null : (
          <p className="mt-2 text-body text-text-muted">
            {worktrees.some((worktree) => worktree.rebuild_in_flight)
              ? 'code-index build in progress; progress not yet reported'
              : 'no active code-index build'}
          </p>
        )
      ) : (
        <div className="mt-2 flex flex-col gap-2">
          {progress.map(([worktreeRoot, build]) => (
            <CodeIndexBuildCard key={worktreeRoot} worktreeRoot={worktreeRoot} progress={build} />
          ))}
        </div>
      )}
    </section>
  );
}

/** The daemon's safe sentence for a failed mount and the command that retries it. */
export function CodeIndexMountFailureNotice({
  failure,
  className,
}: {
  failure: CodeIndexMountFailureV1;
  className?: string;
}) {
  return (
    <div className={className} data-code-index-mount-failure>
      <p className="text-body text-state-error">{failure.message}</p>
      <p className="text-sm text-text-secondary">{failure.remediation}</p>
    </div>
  );
}

function CodeIndexReadinessList({
  worktrees,
  observedAtMicros,
}: {
  worktrees: CodeIndexWorktreeFreshnessV1[];
  observedAtMicros: number | null;
}) {
  if (worktrees.length === 0) {
    return <p className="mt-2 text-body text-text-muted">no mounted code-index worktree</p>;
  }
  return (
    <ul className="mt-2 flex flex-col gap-2">
      {worktrees.map((worktree) => (
        <li
          key={worktree.worktree_root}
          className="rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 p-2.5"
        >
          <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-sm">
            <dt className="text-text-muted">Lexical readiness</dt>
            <dd className="text-right text-text-secondary">
              {lexicalReadinessLabel(worktree)}
            </dd>
            <dt className="text-text-muted">Graph serving</dt>
            <dd className="text-right text-text-secondary">
              {graphServingLabel(worktree.code_graph_serving)}
            </dd>
          </dl>
          <CodeIndexAttemptReading worktree={worktree} observedAtMicros={observedAtMicros} />
          {worktree.parked ? (
            <p className="mt-2 text-sm text-state-warning">
              parked: {worktree.parked.reason} · {worktree.parked.remediation}
            </p>
          ) : null}
        </li>
      ))}
    </ul>
  );
}

export function CodeIndexAttemptReading({
  worktree,
  observedAtMicros,
}: {
  worktree: CodeIndexWorktreeFreshnessV1;
  observedAtMicros: number | null;
}) {
  const attempt = worktree.preseal_attempt;
  const graph = worktree.native_graph_progress;
  // Retiring the source observer at durable handoff must not erase its last
  // authentic work. Show the retired reading only while the rebuild remains
  // in flight; it does not represent an active writer or renew its work clock.
  const showPreseal = attempt != null && (
    attempt.active || (worktree.rebuild_in_flight && attempt.last_progress_micros != null)
  );
  // Only the daemon's observation time and an actual producer work stamp
  // can establish inactivity. Reading status is not a progress heartbeat.
  const presealQuiet = attempt?.active
    ? observedWorkQuietDuration(attempt.last_progress_micros, observedAtMicros)
    : null;
  const graphQuiet = graph?.active
    ? observedWorkQuietDuration(graph.last_progress_micros, observedAtMicros)
    : null;
  // An inactive writer is no longer live, but its committed milestones still
  // belong to this generation. Encoding alone is not durable completion.
  const showGraph = graph != null && (
    graph.active || (
      graph.generation_id === worktree.latest_generation_id &&
      (graph.committed_stage_pages > 0 || graph.committed_compact_segments > 0)
    )
  );
  if (!showPreseal && !showGraph) return null;
  return (
    <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-sm">
      {showPreseal && attempt && (
        <>
          <dt className="text-text-muted">
            {attempt.active ? 'Before sealing' : 'Last observed preseal work'}
          </dt>
          <dd className="text-right text-text-secondary">
            {attempt.last_progress_micros == null ? (
              'awaiting first observed preseal work'
            ) : (
              <>
                {attempt.phase.replaceAll('_', ' ')} · {formatCount(attempt.completed_items)}
                {attempt.total_items != null && ` / ${formatCount(attempt.total_items)}`} items
              </>
            )}
          </dd>
          {presealQuiet && (
            <>
              <dt className="text-state-warning">Preseal progress</dt>
              <dd className="text-right text-state-warning">
                no observed preseal work for {presealQuiet}
              </dd>
            </>
          )}
        </>
      )}
      {showGraph && graph && (
        <>
          <dt className="text-text-muted">
            {graph.active ? 'Graph writer' : 'Graph milestones'}
          </dt>
          <dd className="text-right text-text-secondary">
            {graph.last_progress_micros == null ? (
              'awaiting first observed native graph work'
            ) : (
              <>
                {graph.phase?.replaceAll('_', ' ') ?? 'work phase not reported'} ·{' '}
                {formatCount(graph.encoded_entities)} entities encoded ·{' '}
                {formatCount(graph.encoded_relations)} relations encoded ·{' '}
                {formatCount(graph.committed_stage_pages)} pages committed ·{' '}
                {formatCount(graph.committed_compact_segments)} segments committed
              </>
            )}
          </dd>
          {graphQuiet && (
            <>
              <dt className="text-state-warning">Graph progress</dt>
              <dd className="text-right text-state-warning">
                no observed native graph work for {graphQuiet}
              </dd>
            </>
          )}
        </>
      )}
    </dl>
  );
}

function observedWorkQuietDuration(
  lastProgressMicros: number | null,
  observedAtMicros: number | null,
): string | null {
  if (lastProgressMicros == null || observedAtMicros == null) return null;
  const quietMicros = observedAtMicros - lastProgressMicros;
  // No work stamp, or a stamp after the read, is not evidence of a stall.
  return quietMicros >= 30_000_000 ? formatDurationMicros(quietMicros) : null;
}

export function hasLiveWorktreeProgress(worktree: CodeIndexWorktreeFreshnessV1): boolean {
  return (
    worktree.restore_progress != null ||
    worktree.preseal_attempt?.active === true ||
    worktree.native_graph_progress?.active === true
  );
}

function lexicalReadinessLabel(worktree: CodeIndexWorktreeFreshnessV1): string {
  if (!worktree.latest_generation_id) return 'unavailable';
  return worktree.staleness_state ?? 'unknown';
}

export function graphServingLabel(
  graph: CodeIndexWorktreeFreshnessV1['code_graph_serving'],
): string {
  if (!graph) return 'unknown';
  switch (graph.state) {
    case 'pending':
      return 'pending';
    case 'ready':
      return 'ready';
    case 'warming':
      return `warming · ${graph.reason}`;
    case 'refused':
      return `refused · ${graph.reason}`;
    case 'unavailable':
      return `unavailable · ${graph.reason}`;
    default: {
      const unhandled: never = graph;
      return unhandled;
    }
  }
}

function CodeIndexBuildCard({
  progress,
  worktreeRoot,
}: {
  progress: CodeIndexBuildProgressV1;
  worktreeRoot: string;
}) {
  const percentage = codeIndexProgressPercentage(progress);
  const hasRate =
    progress.files_per_second != null && progress.lexical_units_per_second != null;
  return (
    <article
      className="rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 p-2.5"
      data-code-index-generation={progress.generation_id}
    >
      <p className="flex flex-wrap items-baseline justify-between gap-x-2 text-body">
        <span className="font-medium text-text-secondary">
          {`${codeIndexPhaseLabel(progress.phase)} · ${percentage.toFixed(1)}%`}
        </span>
      </p>
      <progress
        aria-label={`Code progress for ${progress.generation_id}`}
        className="mt-1.5 h-1.5 w-full accent-accent"
        max={100}
        value={percentage}
      />
      <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-sm leading-snug">
        <dt className="text-text-muted">generation</dt>
        <dd className="truncate text-right font-mono text-text-secondary" title={progress.generation_id}>
          {progress.generation_id}
        </dd>
        <dt className="text-text-muted">worktree</dt>
        <dd className="truncate text-right font-mono text-text-secondary" title={worktreeRoot}>
          {worktreeRoot}
        </dd>
        <dt className="text-text-muted">files</dt>
        <dd className="text-right text-text-secondary">
          {formatCount(progress.completed_files)} / {formatCount(progress.total_files)} files
        </dd>
        <dt className="text-text-muted">throughput</dt>
        <dd className="text-right text-text-secondary">
          {hasRate
            ? `${formatCount(progress.files_per_second)} files/s · ${formatCount(progress.lexical_units_per_second)} lexical units/s`
            : 'throughput unavailable'}
        </dd>
        <dt className="text-text-muted">elapsed</dt>
        <dd className="text-right text-text-secondary">
          elapsed {formatDurationMicros(progress.elapsed_micros)}
        </dd>
        <dt className="text-text-muted">last commit</dt>
        <dd className="text-right text-text-secondary">
          last commit{' '}
          {progress.last_commit_latency_micros != null
            ? formatDurationMicros(progress.last_commit_latency_micros)
            : 'not reported'}
        </dd>
      </dl>
      {progress.blocked_reason ? (
        <p className="mt-1.5 text-sm text-state-warning">
          blocked: {codeIndexBlockedReasonLabel(progress.blocked_reason)}
        </p>
      ) : null}
    </article>
  );
}

// Serving readiness can describe the previous generation while its successor
// is still capturing sources, or while a lexical artifact is being restored.
// Neither operation is required to have a committed build-progress report yet.
/** One worktree's actual or admitted pending work, shared by both dashboard
 * views. An inactive terminal park is not a one-second polling obligation. */
export function worktreeHasActiveCodeIndexBuild(worktree: CodeIndexWorktreeFreshnessV1): boolean {
  // A real successor can already be working while the retained generation
  // still carries an old terminal refusal. An active attempt owns its work;
  // only an idle, refused generation should suppress live polling.
  if (worktree.preseal_attempt?.active || worktree.native_graph_progress?.active) return true;
  if (worktree.parked != null && !worktree.parked.retries_on_wake) return false;
  if (worktree.progress?.blocked_reason === 'publication_authority_corrupt') return false;
  return (
    worktree.rebuild_in_flight ||
    hasLiveWorktreeProgress(worktree) ||
    worktree.staleness_state === 'verifying' ||
    worktree.staleness_state === 'restoring' ||
    (worktree.progress != null && worktree.progress.phase !== 'ready') ||
    (worktree.latest_generation_id == null && worktree.staleness_state === 'indexing')
  );
}

export function hasActiveCodeIndexBuild(
  result: EnvelopeResult<CodeIndexFreshnessPayloadV1> | undefined,
): boolean {
  if (result?.outcome !== 'envelope') return false;
  const { domain_state, payload } = result.envelope;
  if (payload.mount_failure) return false;
  // A loading scope may not have enrolled its first worktree yet.
  if (payload.worktrees.length === 0) return domain_state === 'loading';
  return payload.worktrees.some(worktreeHasActiveCodeIndexBuild);
}

function useLatestCodeIndexProgress(
  result: EnvelopeResult<CodeIndexFreshnessPayloadV1> | undefined,
  currentScopeKey: string,
): ReadonlyArray<readonly [string, CodeIndexBuildProgressV1]> {
  const [latestProgress, setLatestProgress] = useState<ScopedCodeIndexProgress>(() => ({
    scopeKey: currentScopeKey,
    byWorktree: new Map(),
  }));
  useEffect(() => {
    if (result?.outcome !== 'envelope') {
      // Disconnect invalidates the old observation even if a later response
      // names the same generation without receipted work.
      setLatestProgress((rendered) =>
        rendered.scopeKey === currentScopeKey && rendered.byWorktree.size === 0
          ? rendered
          : { scopeKey: currentScopeKey, byWorktree: new Map() },
      );
      return;
    }
    setLatestProgress((rendered) => {
      const current =
        rendered.scopeKey === currentScopeKey
          ? rendered.byWorktree
          : new Map<string, CodeIndexBuildProgressV1>();
      const next = new Map<string, CodeIndexBuildProgressV1>();
      for (const worktree of result.envelope.payload.worktrees) {
        const incoming = worktree.progress;
        const renderedProgress = current.get(worktree.worktree_root);
        if (!incoming) {
          // A confirmed replacement seal makes the old text cursor stale.
          // Retain only a transient missing sample for a still-sealed
          // generation. No sealed generation means no text cursor to retain.
          if (
            renderedProgress &&
            worktree.latest_generation_id === renderedProgress.generation_id &&
            !(
              result.envelope.domain_state === 'ready' &&
              worktree.latest_generation_id === renderedProgress.generation_id
            )
          ) {
            next.set(worktree.worktree_root, renderedProgress);
          }
        } else if (
          !renderedProgress ||
          isCurrentOrNewerCodeIndexProgress(incoming, renderedProgress)
        ) {
          next.set(worktree.worktree_root, incoming);
        } else {
          next.set(worktree.worktree_root, renderedProgress);
        }
      }
      return rendered.scopeKey === currentScopeKey &&
        sameCodeIndexProgressMap(next, rendered.byWorktree)
        ? rendered
        : { scopeKey: currentScopeKey, byWorktree: next };
    });
  }, [currentScopeKey, result]);
  return latestProgress.scopeKey === currentScopeKey ? [...latestProgress.byWorktree] : [];
}

interface ScopedCodeIndexProgress {
  scopeKey: string;
  byWorktree: ReadonlyMap<string, CodeIndexBuildProgressV1>;
}

function sameCodeIndexProgressMap(
  left: ReadonlyMap<string, CodeIndexBuildProgressV1>,
  right: ReadonlyMap<string, CodeIndexBuildProgressV1>,
): boolean {
  if (left.size !== right.size) return false;
  for (const [worktreeRoot, progress] of left) {
    if (right.get(worktreeRoot) !== progress) return false;
  }
  return true;
}

export function isCurrentOrNewerCodeIndexProgress(
  incoming: CodeIndexBuildProgressV1,
  rendered: CodeIndexBuildProgressV1,
): boolean {
  if (incoming.daemon_incarnation !== rendered.daemon_incarnation) {
    return incoming.daemon_incarnation > rendered.daemon_incarnation;
  }
  if (incoming.producer_incarnation !== rendered.producer_incarnation) {
    return incoming.producer_incarnation > rendered.producer_incarnation;
  }
  return incoming.progress_epoch >= rendered.progress_epoch;
}

export function codeIndexProgressPercentage(progress: CodeIndexBuildProgressV1): number {
  const completed =
    progress.total_lexical_units > 0
      ? progress.completed_lexical_units / progress.total_lexical_units
      : progress.phase === 'ready'
        ? 1
        : 0;
  return Math.min(100, Math.max(0, completed * 100));
}

export function codeIndexPhaseLabel(phase: CodeIndexBuildProgressV1['phase']): string {
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
    case 'graph_publication':
      return 'graph publication';
    case 'ready':
      return 'ready';
  }
}

export function codeIndexBlockedReasonLabel(
  reason: CodeIndexBuildProgressV1['blocked_reason'],
): string {
  switch (reason) {
    case 'resident_memory':
      return 'resident memory';
    case 'source_unavailable':
      return 'source unavailable';
    case 'artifact_store_unavailable':
      return 'artifact store unavailable';
    case 'retry_backoff':
      return 'retry backoff';
    case 'publication_authority_corrupt':
      return 'publication authority corrupt';
    case null:
      return 'not blocked';
  }
}

export function formatDurationMicros(micros: number): string {
  if (micros < 1_000) return `${micros}µs`;
  if (micros < 1_000_000) return `${Math.round(micros / 1_000)}ms`;
  const seconds = micros / 1_000_000;
  if (seconds < 90) return `${Math.round(seconds)}s`;
  if (seconds < 5_400) return `${Math.round(seconds / 60)}m`;
  const hours = Math.floor(seconds / 3_600);
  const minutes = Math.round((seconds % 3_600) / 60);
  return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
}
