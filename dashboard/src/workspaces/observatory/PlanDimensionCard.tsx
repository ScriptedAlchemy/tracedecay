/**
 * One Plan 26 required dimension, drawn as an instrument card.
 *
 * Deliberately the same weight as `MetricPlate`, and for the same reason: an
 * unavailable dimension has to occupy the space a measured one would, or the
 * eye reads a page of two measurements as a page with two things on it rather
 * than as a page with thirteen requirements and two answers.
 *
 * Every row Plan 26 names — support, eligible denominator, censoring/unknowns,
 * interval coverage, horizon, descriptor revision, safe anchors — is rendered
 * on every card, including the cards whose answer is "not published". The rows
 * are a definition list so a screen reader gets the same term/value pairing the
 * grid gives the eye, and the state is a `StateChip` (icon + label +
 * `data-state`) rather than a hue.
 */
import { StateChip } from '../../ui/StateChip';
import { cn } from '../../ui/cn';
import type { PlanDimensionPresentation } from './planDimension.ts';

export function PlanDimensionCard({ dimension }: { dimension: PlanDimensionPresentation }) {
  return (
    <li
      className="flex min-w-0 flex-col gap-2 border border-edge-subtle bg-surface-1 px-3 py-2.5"
      data-dimension={dimension.id}
      data-dimension-available={dimension.available ? 'true' : 'false'}
      data-dimension-state={dimension.state}
    >
      <div className="flex min-w-0 flex-col gap-1">
        <span className="td-legend truncate" title={dimension.id}>
          {dimension.label}
        </span>
        <span className="flex min-w-0 flex-wrap items-baseline gap-1.5">
          <span
            className={cn(
              'td-value text-xl',
              dimension.available ? 'text-text-primary' : 'text-text-muted',
            )}
            data-cell="numeric"
          >
            {dimension.figure}
          </span>
          {dimension.unit != null ? (
            <span className="td-unit shrink-0 leading-none">{dimension.unit}</span>
          ) : null}
          {dimension.exact != null ? (
            <span className="td-unit shrink-0 leading-none text-text-muted">
              ({dimension.exact})
            </span>
          ) : null}
        </span>
      </div>

      {/* The reason IS the reading when there is no figure, so it sits where the
        * value would and carries the chip. */}
      {dimension.reason != null ? (
        <div className="flex flex-wrap items-center gap-2">
          <StateChip kind={dimension.state} />
          <span className="min-w-0 text-2xs text-text-secondary">{dimension.reason}</span>
        </div>
      ) : null}

      <p className="text-3xs leading-snug text-text-muted">{dimension.requirement}</p>

      <dl className="flex flex-col gap-1 text-3xs leading-snug text-text-muted">
        <Row term="support" value={dimension.support} />
        <Row term="denominator" value={dimension.denominator} />
        <Row term="censoring" value={dimension.censoring} />
        <Row term="interval" value={dimension.interval} />
        <Row term="horizon" value={dimension.horizon} />
        <Row term="descriptor revision" value={dimension.descriptorRevision} />
        <Row term="anchors" value={dimension.anchors} />
      </dl>
    </li>
  );
}

function Row({ term, value }: { term: string; value: string }) {
  return (
    <div className="flex min-w-0 gap-1.5">
      <dt className="shrink-0 uppercase tracking-[0.08em]">{term}</dt>
      <dd className="min-w-0 break-words text-text-secondary">{value}</dd>
    </div>
  );
}

/** A titled grid of dimension cards with a header stating how many of them a
 * reader is actually being shown a figure for. */
export function PlanDimensionGrid({
  label,
  dimensions,
  marker,
}: {
  label: string;
  dimensions: readonly PlanDimensionPresentation[];
  /** `data-dimension-group` marker, so a test can address one band. */
  marker: string;
}) {
  const measured = dimensions.filter((dimension) => dimension.available).length;
  return (
    <section
      className="flex min-w-0 flex-col gap-2"
      aria-label={`${label} dimensions`}
      data-dimension-group={marker}
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate">{label}</h3>
        <span aria-hidden className="td-rule" />
        <span className="shrink-0 text-3xs text-text-muted tabular">
          {measured} of {dimensions.length} measured
        </span>
      </div>
      <ul className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
        {dimensions.map((dimension) => (
          <PlanDimensionCard key={dimension.id} dimension={dimension} />
        ))}
      </ul>
    </section>
  );
}
