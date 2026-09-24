/** Explorer's query register chrome: the coordinator run's own state, drawn
 * beside the query it answers, with the one explicit cancel control. Every
 * value is the run's; the lanes below render each source's condition. */
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import { cn } from '../../ui/cn';
import { Meter } from '../../ui/instrument.tsx';
import type { ExplorerQueryRunV1 } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import type { ScopeWritability } from '../../data/scope/store.ts';
import type { RunProgress } from './controller.ts';
import { runStateKind } from './laneModel.ts';

/** What the register says while no run exists: the scope's refusal when the
 * scope authority declined to dispatch one, otherwise the plan being admitted
 * or the transport failure that answered instead. */
function noRunReading(
  writability: ScopeWritability,
  result: EnvelopeResult<ExplorerQueryRunV1> | undefined,
): { kind: DomainStateKind; detail: string } {
  switch (writability.state) {
    case 'read_only':
      return { kind: 'locked', detail: 'no run created: the scope is read-only' };
    case 'unknown':
      return { kind: 'unknown', detail: 'no run created until the scope is checked' };
    case 'writable':
      return result?.outcome === 'transport'
        ? { kind: result.state, detail: result.detail ?? 'coordinator response unavailable' }
        : { kind: 'loading', detail: 'admitting the source plan' };
    default: {
      const exhaustive: never = writability;
      return exhaustive;
    }
  }
}

/**
 * The run readout. Progress is counted in sources concluded, because that is
 * the only figure with a real denominator: sources report incommensurable
 * units, and a blended percentage would be a number nobody measured.
 *
 * Cancel is mounted only while a cancellable run is in flight. There is no
 * disabled cancel for a run that has already concluded; a control that cannot
 * act is not shown pretending it might.
 */
export function RunRegister({
  result,
  run,
  writability,
  progress,
  cancelling,
  onCancel,
  className,
}: {
  result: EnvelopeResult<ExplorerQueryRunV1> | undefined;
  run: ExplorerQueryRunV1 | undefined;
  writability: ScopeWritability;
  progress: RunProgress | null;
  cancelling: boolean;
  onCancel: (() => void) | undefined;
  className?: string;
}) {
  if (!run) {
    const blocked = noRunReading(writability, result);
    return (
      <div
        className={cn('flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1', className)}
        aria-live="polite"
        data-run-register
        data-run-state={blocked.kind}
      >
        <span className="td-legend">Run</span>
        <StateChip kind={blocked.kind} detail={blocked.detail} />
      </div>
    );
  }
  const fraction = progress === null || progress.total === 0 ? null : progress.concluded / progress.total;
  return (
    <div
      className={cn('flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1', className)}
      aria-live="polite"
      data-run-register
      data-run-state={run.state}
    >
      <span className="td-legend">Run</span>
      <StateChip kind={runStateKind(run.state)} detail={run.finality} />
      {progress !== null ? (
        <span className="flex items-center gap-2">
          <Meter
            fraction={fraction}
            className="w-24"
            tone="bg-accent"
            ariaLabel={`${progress.concluded} of ${progress.total} sources concluded`}
          />
          <span className="td-value text-sm text-text-secondary" data-cell="numeric">
            {progress.concluded}/{progress.total}
          </span>
          <span className="text-sm text-text-muted">sources concluded</span>
        </span>
      ) : null}
      <span className="td-value text-xs text-text-muted" data-cell="numeric">
        {Math.round(run.elapsed_micros / 1_000).toLocaleString()} ms
      </span>
      <span className="hidden min-w-0 truncate font-mono text-xs text-text-muted xl:inline" title={run.run_id}>
        {run.run_id}
      </span>
      <span className="hidden text-sm text-text-muted 2xl:inline">{run.ordering_policy}</span>
      {onCancel ? (
        <button
          type="button"
          onClick={onCancel}
          disabled={cancelling}
          className="td-hit ml-auto border border-edge-subtle px-3 text-2xs uppercase tracking-[0.1em] text-text-secondary hover:border-accent hover:text-text-primary disabled:opacity-50"
        >
          {cancelling ? 'Cancelling…' : 'Cancel'}
        </button>
      ) : null}
    </div>
  );
}
