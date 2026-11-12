/** Explorer's query register chrome: the search's own state, drawn beside the
 * query it answers, with the one explicit cancel control. Every value is the
 * search's; the lanes below render each source's condition. */
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import { cn } from '../../ui/cn';
import { Meter } from '../../ui/instrument.tsx';
import type { ScopeWritability } from '../../data/scope/store.ts';
import type { SearchProgress } from './controller.ts';
import { searchStateKind, type ExplorerSearch, type ExplorerSearchResult } from './laneModel.ts';

/** What the register says while no search exists: the scope's refusal when the
 * scope authority declined to open one, otherwise the search being opened or
 * the transport failure that answered instead. */
function noSearchReading(
  writability: ScopeWritability,
  result: ExplorerSearchResult | undefined,
): { kind: DomainStateKind; detail: string } {
  switch (writability.state) {
    case 'read_only':
      return { kind: 'locked', detail: 'no search opened: the scope is read-only' };
    case 'unknown':
      return { kind: 'unknown', detail: 'no search opened until the scope is checked' };
    case 'writable':
      return result?.outcome === 'transport'
        ? { kind: result.state, detail: result.detail ?? 'search response unavailable' }
        : { kind: 'loading', detail: 'opening the search' };
    default: {
      const exhaustive: never = writability;
      return exhaustive;
    }
  }
}

/**
 * The search readout. Progress is counted in sources concluded, because that is
 * the only figure with a real denominator: sources report incommensurable
 * units, and a blended percentage would be a number nobody measured.
 *
 * Cancel is mounted only while a search is streaming. There is no disabled
 * cancel for a search that has already concluded; a control that cannot act is
 * not shown pretending it might.
 */
export function RunRegister({
  result,
  search,
  writability,
  progress,
  onCancel,
  className,
}: {
  result: ExplorerSearchResult | undefined;
  search: ExplorerSearch | undefined;
  writability: ScopeWritability;
  progress: SearchProgress | null;
  onCancel: (() => void) | undefined;
  className?: string;
}) {
  if (!search) {
    const blocked = noSearchReading(writability, result);
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
      data-run-state={search.status}
    >
      <span className="td-legend">Run</span>
      <StateChip kind={searchStateKind(search.status)} />
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
      {search.elapsedMicros !== null ? (
        <span className="td-value text-xs text-text-muted" data-cell="numeric">
          {Math.round(search.elapsedMicros / 1_000).toLocaleString()} ms
        </span>
      ) : null}
      {onCancel ? (
        <button
          type="button"
          onClick={onCancel}
          className="td-hit ml-auto border border-edge-subtle px-3 text-2xs uppercase tracking-[0.1em] text-text-secondary hover:border-accent hover:text-text-primary disabled:opacity-50"
        >
          Cancel
        </button>
      ) : null}
    </div>
  );
}
