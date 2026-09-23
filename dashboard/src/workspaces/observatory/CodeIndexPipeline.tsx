import { useEffect, useState } from 'react';
import type {
  CodeIndexBuildProgressV1,
  CodeIndexFreshnessPayloadV1,
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
  const worktrees = result?.outcome === 'envelope' ? result.envelope.payload.worktrees : [];
  if (pending) {
    return (
      <section className="mx-4 mt-3" aria-label="Code-index pipeline">
        <p className="text-2xs text-text-muted">reading code-index pipeline…</p>
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
      <CodeIndexReadinessList worktrees={worktrees} />
      {progress.length === 0 ? (
        <p className="mt-2 text-2xs text-text-muted">no active code-index build</p>
      ) : (
        <div className="mt-2 flex flex-col gap-2">
          {progress.map((build) => (
            <CodeIndexBuildCard key={build.generation_id} progress={build} />
          ))}
        </div>
      )}
    </section>
  );
}

function CodeIndexReadinessList({
  worktrees,
}: {
  worktrees: CodeIndexWorktreeFreshnessV1[];
}) {
  if (worktrees.length === 0) {
    return <p className="mt-2 text-2xs text-text-muted">no mounted code-index worktree</p>;
  }
  return (
    <ul className="mt-2 flex flex-col gap-2">
      {worktrees.map((worktree) => (
        <li
          key={worktree.worktree_root}
          className="rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 p-2.5"
        >
          <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-3xs">
            <dt className="text-text-muted">Lexical readiness</dt>
            <dd className="text-right text-text-secondary">
              {lexicalReadinessLabel(worktree)}
            </dd>
            <dt className="text-text-muted">Graph serving</dt>
            <dd className="text-right text-text-secondary">
              {graphServingLabel(worktree.code_graph_serving)}
            </dd>
          </dl>
        </li>
      ))}
    </ul>
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

function CodeIndexBuildCard({ progress }: { progress: CodeIndexBuildProgressV1 }) {
  const percentage = codeIndexProgressPercentage(progress);
  const hasRate =
    progress.files_per_second != null && progress.lexical_units_per_second != null;
  return (
    <article
      className="rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-2 p-2.5"
      data-code-index-generation={progress.generation_id}
    >
      <p className="flex flex-wrap items-baseline justify-between gap-x-2 text-2xs">
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
      <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-3xs leading-snug">
        <dt className="text-text-muted">generation</dt>
        <dd className="truncate text-right font-mono text-text-secondary" title={progress.generation_id}>
          {progress.generation_id}
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
        <p className="mt-1.5 text-3xs text-state-warning">
          blocked: {codeIndexBlockedReasonLabel(progress.blocked_reason)}
        </p>
      ) : null}
    </article>
  );
}

export function hasActiveCodeIndexBuild(
  result: EnvelopeResult<CodeIndexFreshnessPayloadV1> | undefined,
): boolean {
  return (
    result?.outcome === 'envelope' &&
    (result.envelope.domain_state !== 'ready' ||
      result.envelope.payload.worktrees.some(
        (worktree) => worktree.progress != null && worktree.progress.phase !== 'ready',
      ))
  );
}

function useLatestCodeIndexProgress(
  result: EnvelopeResult<CodeIndexFreshnessPayloadV1> | undefined,
  currentScopeKey: string,
): readonly CodeIndexBuildProgressV1[] {
  const [latestProgress, setLatestProgress] = useState<ScopedCodeIndexProgress>(() => ({
    scopeKey: currentScopeKey,
    byWorktree: new Map(),
  }));
  useEffect(() => {
    if (result?.outcome !== 'envelope') {
      setLatestProgress((rendered) =>
        rendered.scopeKey === currentScopeKey
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
          if (
            renderedProgress &&
            result.envelope.domain_state === 'ready' &&
            worktree.latest_generation_id === renderedProgress.generation_id
          ) {
            continue;
          }
          if (renderedProgress) next.set(worktree.worktree_root, renderedProgress);
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
  return latestProgress.scopeKey === currentScopeKey ? [...latestProgress.byWorktree.values()] : [];
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
