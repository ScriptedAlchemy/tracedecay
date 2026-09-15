/**
 * The instrument plate above the field.
 *
 * Every figure on it is counted by `readoutCells` from the same `TraceModel`
 * the canvas draws, so the plate cannot say one thing while the picture says
 * another. What is left for this module is the layout that makes seven readings
 * legible at every width — which is a presentation problem, not a measurement
 * one, and is the whole reason it is separate from the arithmetic.
 */
import { readoutCells } from '../../viz/trace/readout.ts';
import type { TraceModel } from '../../viz/trace/types.ts';
import { TraceReading } from './TraceReading.tsx';

/**
 * The seven-cell plate above the field.
 *
 * Laid out as a description list because that is what it is: seven measured
 * terms and their readings. The hairline grid is drawn with a one-pixel gap
 * over the edge colour rather than per-cell borders, so cells that wrap onto a
 * second row at a narrow width still meet on a single rule.
 */
export function TraceReadoutStrip({
  model,
  expanding,
}: {
  model: TraceModel;
  expanding: boolean;
}) {
  return (
    <div className="border-b border-edge-subtle" data-testid="trace-readout">
      {/* Seven cells divide evenly into none of these column counts, and the
        * leftover space over a lit container renders as an eighth cell with
        * nothing in it — which on a plate whose whole subject is absence reads
        * as a reading that failed to arrive. The last cell is widened at each
        * breakpoint by exactly the shortfall, so every row is full and the
        * hairline grid stays a grid. */}
      <dl className="grid grid-cols-2 gap-px bg-edge-subtle sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-7">
        {readoutCells(model).map((cell) => (
          <div
            key={cell.label}
            className="flex min-w-0 flex-col gap-1 bg-surface-0 px-2.5 py-1.5 last:col-span-2 sm:last:col-span-3 lg:last:col-span-2 xl:last:col-span-1"
          >
            {/* `td-legend` is `nowrap` by default, which is right for a chip
              * and wrong for a plate: at seven columns it clipped `Callers ≤ 2
              * hops` to an ellipsis. A label that wraps to two lines is still
              * the label; one that truncates is a different word. */}
            <dt className="flex items-center gap-1.5">
              <span className="td-legend whitespace-normal">{cell.label}</span>
              <span aria-hidden className="td-rule" />
            </dt>
            <dd className="flex min-w-0 flex-col gap-0.5">
              <TraceReading value={cell.value} size="text-xs" />
              {cell.qualifier === null ? null : (
                <span className="text-3xs leading-snug text-text-secondary">{cell.qualifier}</span>
              )}
            </dd>
          </div>
        ))}
      </dl>
      {expanding ? (
        <p className="border-t border-edge-subtle px-2.5 py-1 text-3xs text-state-loading">
          still expanding hop 2 — every reading above is for what has arrived so far
        </p>
      ) : null}
    </div>
  );
}
