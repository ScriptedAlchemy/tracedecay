import type { ReactNode } from 'react';
import { StateChip } from '../../../ui/StateChip.tsx';
import { cn } from '../../../ui/cn.ts';
import type { WorkChannel } from '../workViewsModel.ts';

/**
 * The shared vocabulary of the four plan 11c Work projections.
 *
 * Three marks, repeated in every view, so the projections read as one
 * instrument seen from four positions rather than as four pages:
 *
 *   ChannelAbsence  a measurement the view was asked to encode and cannot
 *   ViewCaption     the population, window and cap a projection ran under
 *   EmptyReading    a projection whose input was genuinely empty
 *
 * `ChannelAbsence` is the one that matters. 11c requires absence to be drawn,
 * and every one of these views is missing at least one channel until the
 * product-graph read is mounted. Drawing that gap in the projection's own body
 * — rather than in a footnote — is what keeps a hollow weave from reading as a
 * weave of nothing.
 */

/** A measurement the projection states it could not take, with the reason it
 * could not take it. Never rendered as a zero, an empty axis, or a blank. */
export function ChannelAbsence({
  measure,
  channel,
  className,
}: {
  /** What the channel would have encoded, in the plan's own words. */
  measure: string;
  channel: WorkChannel<unknown>;
  className?: string;
}) {
  if (channel.available) return null;
  return (
    <div
      className={cn('flex min-w-0 flex-col gap-1', className)}
      data-work-channel="absent"
      data-work-measure={measure}
    >
      <StateChip kind={channel.state} detail={measure} />
      <p className="text-3xs leading-snug text-text-muted">{channel.detail}</p>
    </div>
  );
}

/** Several absences at once, as one ledger rather than a stack of chips. A
 * projection missing three channels is making one statement about its inputs. */
export function ChannelLedger({
  legend,
  channels,
}: {
  legend: string;
  channels: readonly { readonly measure: string; readonly channel: WorkChannel<unknown> }[];
}) {
  const absent = channels.filter((entry) => !entry.channel.available);
  if (absent.length === 0) return null;
  return (
    <section
      aria-label={legend}
      className="flex min-w-0 flex-col gap-2 border border-edge-subtle bg-surface-2 p-2.5"
      data-work-channel-ledger={absent.length}
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate text-text-secondary">{legend}</h3>
        <span aria-hidden className="td-rule" />
      </div>
      <ul className="flex min-w-0 flex-col gap-2">
        {absent.map((entry) => (
          <li key={entry.measure} className="min-w-0">
            <ChannelAbsence measure={entry.measure} channel={entry.channel} />
          </li>
        ))}
      </ul>
    </section>
  );
}

/**
 * What a projection is drawn over, printed on the projection.
 *
 * The Agents-page lesson in 11c: a view that does not state its population and
 * window cannot be told apart from a complete one. `population` is the
 * projection's own sentence — "18 of 312 tasks, capped at 100" — and the
 * daemon's coverage reading is its only source.
 */
export function ViewCaption({
  population,
  note,
  children,
}: {
  population: string;
  note?: string;
  children?: ReactNode;
}) {
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-3xs text-text-muted">
      <span className="td-value" data-cell="numeric">
        {population}
      </span>
      {note ? <span className="min-w-0">{note}</span> : null}
      {children}
    </div>
  );
}

/** A projection whose input really was empty, as distinct from one whose input
 * could not be read. The caller only reaches this after a successful read. */
export function EmptyReading({ children }: { children: ReactNode }) {
  return (
    <p className="text-2xs leading-relaxed text-text-muted" data-work-reading="empty">
      {children}
    </p>
  );
}
