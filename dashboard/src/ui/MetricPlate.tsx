import type { MetricValueV1 } from '../contracts/generated.ts';
import { cn } from './cn';
import { EvidencePattern } from './EvidencePattern.tsx';
import { StateChip } from './StateChip';
import {
  availableCount,
  groupBySource,
  metricPresentation,
  type MetricGroup,
} from './metricModel.ts';

/**
 * One Plan 26 canonical measurement, drawn as an instrument plate.
 *
 * The plate is deliberately taller than a stat tile because a bare number is
 * exactly what this contract exists to prevent: the denominator, the coverage
 * counts, the evidence class, and the producing source are all part of the
 * reading, not footnotes to it. A measurement with no value keeps the same
 * plate and prints the server's own reason where the figure would go, so an
 * unavailable metric occupies the same visual weight as an available one and
 * cannot be mistaken for an absent row.
 */
export function MetricPlate({ metric }: { metric: MetricValueV1 }) {
  const presentation = metricPresentation(metric);
  return (
    <li
      className="relative flex min-w-0 flex-col gap-2 border border-edge-subtle bg-surface-1 px-3 py-2.5"
      data-metric={metric.metric}
      data-metric-available={presentation.available ? 'true' : 'false'}
      data-metric-coverage={metric.coverage.state}
      data-metric-evidence={presentation.evidenceClass}
    >
      <div className="flex min-w-0 flex-col gap-1">
        <span className="td-legend truncate" title={metric.metric}>
          {presentation.label}
        </span>
        <span className="flex min-w-0 flex-wrap items-baseline gap-1.5">
          <span
            className={cn(
              'td-value text-xl',
              presentation.available ? 'text-text-primary' : 'text-text-muted',
            )}
            data-cell="numeric"
          >
            {presentation.figure}
          </span>
          {presentation.unit ? (
            <span className="td-unit shrink-0 leading-none">{presentation.unit}</span>
          ) : null}
          {presentation.exact ? (
            <span className="td-unit shrink-0 leading-none text-text-muted">
              ({presentation.exact})
            </span>
          ) : null}
        </span>
      </div>

      {/* The reason is the reading when there is no figure, so it sits where a
        * value would and carries the state chip rather than hiding in a
        * footnote under the coverage line. */}
      {!presentation.available ? (
        <div className="flex flex-wrap items-center gap-2">
          <StateChip kind="unknown" />
          <span className="min-w-0 text-2xs text-text-secondary">
            {presentation.unavailableReason ?? 'the daemon reported no reason'}
          </span>
        </div>
      ) : null}

      <dl className="flex flex-col gap-1 text-3xs leading-snug text-text-muted">
        <div className="flex min-w-0 gap-1.5">
          <dt className="shrink-0 uppercase tracking-[0.08em]">denominator</dt>
          <dd className="min-w-0 break-words text-text-secondary">{presentation.denominator}</dd>
        </div>
        <div className="flex min-w-0 gap-1.5">
          <dt className="shrink-0 uppercase tracking-[0.08em]">coverage</dt>
          <dd className="min-w-0 break-words text-text-secondary">{presentation.coverage}</dd>
        </div>
        {presentation.interval ? (
          <div className="flex min-w-0 gap-1.5">
            <dt className="shrink-0 uppercase tracking-[0.08em]">interval</dt>
            <dd className="min-w-0 break-words text-text-secondary">{presentation.interval}</dd>
          </div>
        ) : null}
        {presentation.delta ? (
          <div className="flex min-w-0 gap-1.5">
            <dt className="shrink-0 uppercase tracking-[0.08em]">delta</dt>
            <dd className="min-w-0 break-words text-text-secondary">{presentation.delta}</dd>
          </div>
        ) : null}
        {presentation.calibration ? (
          <div className="flex min-w-0 gap-1.5">
            <dt className="shrink-0 uppercase tracking-[0.08em]">calibration</dt>
            <dd className="min-w-0 break-words text-text-secondary">
              {presentation.calibration}
            </dd>
          </div>
        ) : null}
        <div className="flex min-w-0 gap-1.5">
          <dt className="shrink-0 uppercase tracking-[0.08em]">source</dt>
          <dd className="min-w-0 break-words">{presentation.provenance}</dd>
        </div>
      </dl>

      <EvidencePattern quality={presentation.evidenceQuality} />
    </li>
  );
}

/**
 * Every measurement in a read model, grouped by its producing source.
 *
 * Each group header states how many of its plates actually carry a value, so a
 * source whose store was unreachable cannot read as a section of zeroes. The
 * grouping itself is the wire's own `provenance.source` attribution rather than
 * a taxonomy invented here.
 */
export function MetricGroups({
  metrics,
  emptyLabel,
}: {
  metrics: MetricValueV1[];
  emptyLabel: string;
}) {
  const groups = groupBySource(metrics);
  if (groups.length === 0) {
    return (
      <p className="px-3 py-4 text-2xs text-text-secondary" data-metric-groups="empty">
        {emptyLabel}
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-4">
      {groups.map((group) => (
        <MetricSourceGroup key={group.source} group={group} />
      ))}
    </div>
  );
}

function MetricSourceGroup({ group }: { group: MetricGroup }) {
  const available = availableCount(group.metrics);
  return (
    <section
      className="flex min-w-0 flex-col gap-2"
      aria-label={`${group.label} measurements`}
      data-metric-source={group.source}
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate">{group.label}</h3>
        <span aria-hidden className="td-rule" />
        <span className="shrink-0 text-3xs text-text-muted tabular">
          {available} of {group.metrics.length} measured
        </span>
      </div>
      <ul className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
        {group.metrics.map((metric) => (
          <MetricPlate key={`${metric.provenance.source}:${metric.metric}`} metric={metric} />
        ))}
      </ul>
    </section>
  );
}
