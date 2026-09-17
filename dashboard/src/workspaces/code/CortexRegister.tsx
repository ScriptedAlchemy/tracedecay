/**
 * The Cortex register: seven ruled cells across the top of the aperture that
 * state what the field beneath is a picture of.
 *
 * Nodes, edges and files are the overview's served totals; modules and density
 * are derived from them in `cortex.ts`; layout and rank name the rule the
 * renderer draws by. A cell whose figure the index cannot supply prints its
 * absence in place of a number, and a read that did not answer prints the
 * daemon's own state across the strip rather than seven dashes that would read
 * as an empty graph.
 */
import type { GraphOverviewPayloadV1 } from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';
import { envelopeReadState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { cn } from '../../ui/cn';
import { cortexRegister, type RegisterCell } from './cortex.ts';

export function CortexRegister({
  pending,
  result,
}: {
  pending: boolean;
  result: EnvelopeResult<GraphOverviewPayloadV1> | undefined;
}) {
  const state = envelopeReadState(pending, result, {
    loading: 'reading the code graph overview',
    transport: 'the code graph overview could not be read',
  });
  return (
    <section
      aria-label="Graph register"
      className="td-raised flex min-h-12 shrink-0 flex-wrap items-stretch border-b border-edge-subtle"
      data-graph-register={state.kind === 'ready' ? state.value.domain_state : state.state}
    >
      {state.kind === 'ready' ? (
        cortexRegister(state.value.payload).map((cell) => <Cell key={cell.label} cell={cell} />)
      ) : (
        <div className="flex min-w-0 items-center gap-3 px-3 py-2">
          <span className="td-legend shrink-0">graph</span>
          <StateChip kind={state.state} detail={state.detail} />
        </div>
      )}
    </section>
  );
}

function Cell({ cell }: { cell: RegisterCell }) {
  const { reading } = cell;
  return (
    <div
      className="flex min-w-0 flex-1 basis-28 flex-col justify-center gap-0.5 border-l border-edge-subtle px-3 py-1.5 first:border-l-0"
      data-register-cell={cell.label}
      data-reading={reading.kind}
    >
      <span className="td-legend truncate">{cell.label}</span>
      {reading.kind === 'measured' ? (
        <>
          <span className="flex min-w-0 items-baseline gap-1">
            <span
              className={cn(
                'truncate text-text-primary',
                /^[\d.,e+-]+$/.test(reading.value) ? 'td-value text-sm' : 'td-value text-xs',
              )}
              data-cell="numeric"
            >
              {reading.value}
            </span>
            {reading.unit ? <span className="td-unit shrink-0">{reading.unit}</span> : null}
          </span>
          <span className="truncate text-3xs text-text-muted" title={reading.note}>
            {reading.note}
          </span>
        </>
      ) : (
        <>
          <span className="td-value text-sm text-text-muted" aria-hidden>
            —
          </span>
          <span className="truncate text-3xs text-state-unknown" title={reading.reason}>
            {reading.reason}
          </span>
        </>
      )}
    </div>
  );
}
