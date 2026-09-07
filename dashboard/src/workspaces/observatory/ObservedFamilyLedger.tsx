/**
 * A ledger of observation-family record counts.
 *
 * Deliberately a table and not a grid of `PlanDimensionCard`s. The cards are
 * for canonical `MetricValueV1` measurements, which carry a descriptor
 * revision, an eligible denominator, coverage counts, and a projector
 * attribution. A row here has none of those — it is the number of records the
 * diagnostics projector counted in one family — and dressing it in the same
 * chrome would let a record count pass for a measurement.
 *
 * Every row therefore states the two things it lacks, on the row and not in a
 * footnote: `denominator` is `not published` on all of them, and a cell with no
 * figure prints the daemon's own reason where the number would be.
 *
 * The visual state of a row is a `StateChip` — icon plus label plus
 * `data-state` — beside the reason, never a hue on its own, and the count
 * column carries its state in `data-family-state` so an assertion does not have
 * to read colour either.
 */
import { useRef } from 'react';
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import { cn } from '../../ui/cn';
import { useScrollTabStop } from '../../ui/useScrollTabStop.ts';
import type { FamilyRowPresentation } from './observedFamilies.ts';

/**
 * What stands where a ledger would be when the read that supplies it did not
 * resolve.
 *
 * A table of em dashes would say the families produced nothing, which is not
 * what an unreachable projector reported. So there is no table: the state, the
 * daemon's own sentence, and an explicit statement that no count is being
 * shown.
 */
export function BlockedFamilyLedger({
  label,
  marker,
  state,
  detail,
}: {
  label: string;
  marker: string;
  state: DomainStateKind;
  detail?: string | undefined;
}) {
  return (
    <section
      className="flex flex-col gap-1.5 border border-edge-subtle bg-surface-1 px-3 py-2.5"
      aria-label={`${label} record counts`}
      data-family-ledger={marker}
      data-family-ledger-blocked={state}
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate">{label}</h3>
        <span aria-hidden className="td-rule" />
      </div>
      <StateChip kind={state} detail={detail} />
      <p className="text-3xs leading-snug text-text-muted">
        No record count is shown. The read that would have supplied them did not resolve, and a
        family with no reported count is not a family that produced nothing.
      </p>
    </section>
  );
}

export function ObservedFamilyLedger({
  label,
  caption,
  rows,
  marker,
}: {
  label: string;
  /** What these counts are and are not, in the surface's own words. */
  caption: string;
  rows: readonly FamilyRowPresentation[];
  /** `data-family-ledger` marker so a test can address one ledger. */
  marker: string;
}) {
  const tableViewportRef = useRef<HTMLDivElement>(null);
  const tableViewportTabStop = useScrollTabStop(tableViewportRef, 'horizontal');
  const observed = rows.filter((row) => row.available).length;
  return (
    <section
      className="flex min-w-0 flex-col gap-2"
      aria-label={`${label} record counts`}
      data-family-ledger={marker}
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate">{label}</h3>
        <span aria-hidden className="td-rule" />
        <span className="shrink-0 text-3xs text-text-muted tabular">
          {observed} of {rows.length} publishable
        </span>
      </div>
      <p className="text-3xs leading-snug text-text-muted" data-family-caption={marker}>
        {caption}
      </p>
      <div
        ref={tableViewportRef}
        className="min-w-0 overflow-x-auto sm:overflow-x-visible"
        role="region"
        aria-label={`${label} table`}
        tabIndex={tableViewportTabStop}
      >
        <table className="w-full min-w-[34rem] border-collapse text-2xs">
          <caption className="sr-only">{caption}</caption>
          <thead>
            <tr className="border-b border-edge-subtle text-3xs uppercase tracking-[0.08em] text-text-muted">
              <th scope="col" className="py-1 pr-3 text-left font-normal">
                observation family
              </th>
              <th scope="col" className="py-1 pr-3 text-right font-normal">
                records observed
              </th>
              <th scope="col" className="py-1 pr-3 text-left font-normal">
                eligible denominator
              </th>
              <th scope="col" className="py-1 text-left font-normal">
                reading
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr
                key={row.eventKind}
                className="border-b border-edge-subtle/60 align-top"
                data-family={row.eventKind}
                data-family-available={row.available ? 'true' : 'false'}
                data-family-state={row.state}
              >
                <th scope="row" className="min-w-0 py-1.5 pr-3 text-left font-normal">
                  <span className="td-value block truncate" title={row.eventKind}>
                    {row.label}
                  </span>
                  <span className="block truncate text-3xs text-text-muted">{row.eventKind}</span>
                </th>
                <td
                  className={cn(
                    'py-1.5 pr-3 text-right tabular',
                    row.available ? 'text-text-primary' : 'text-text-muted',
                  )}
                  data-cell="numeric"
                >
                  {row.figure}
                </td>
                <td className="py-1.5 pr-3 text-text-muted">{row.denominator}</td>
                <td className="min-w-0 py-1.5">
                  {row.reason == null ? (
                    <StateChip kind={row.state} detail="records counted" />
                  ) : (
                    <span className="flex min-w-0 flex-wrap items-center gap-1.5">
                      <StateChip kind={row.state} />
                      <span className="min-w-0 break-words text-text-secondary">{row.reason}</span>
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}
