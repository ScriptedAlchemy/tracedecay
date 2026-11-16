/** Explorer stream state and source progress beside the submitted query. */
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import { cn } from '../../ui/cn';
import { Meter } from '../../ui/instrument.tsx';
import type { DashboardDomainStateV1 } from '../../contracts/generated.ts';
import type { ScopeWritability } from '../../data/scope/store.ts';
import type { RunProgress } from './controller.ts';
import { runStateKind } from './laneModel.ts';
import type { ExplorerStreamSnapshot } from './laneModel.ts';

/** A refused scope or failed response before any source frame arrives. */
function noRunReading(
  writability: ScopeWritability,
  transport: { state: DashboardDomainStateV1; detail?: string } | undefined,
): { kind: DomainStateKind; detail: string } {
  switch (writability.state) {
    case 'read_only':
      return { kind: 'locked', detail: writability.reason };
    case 'unknown':
      return { kind: 'unknown', detail: writability.reason };
    case 'writable':
      return transport
        ? { kind: transport.state, detail: transport.detail ?? 'coordinator response unavailable' }
        : { kind: 'loading', detail: 'reading sources' };
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
  transport,
  run,
  writability,
  progress,
  cancelling,
  onCancel,
  className,
}: {
  transport: { state: DashboardDomainStateV1; detail?: string } | undefined;
  run: ExplorerStreamSnapshot | undefined;
  writability: ScopeWritability;
  progress: RunProgress | null;
  cancelling: boolean;
  onCancel: (() => void) | undefined;
  className?: string;
}) {
  if (!run) {
    const blocked = noRunReading(writability, transport);
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
      <StateChip kind={transport?.state ?? runStateKind(run.state)} detail={transport?.detail ?? run.state} />
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
