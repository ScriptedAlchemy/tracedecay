/**
 * PERFORMANCE BUDGETS — `GET /api/observatory` (Plan 26 canonical measurements).
 *
 * Reads the same `ObservatoryReadModelV1` bytes the CLI and MCP serve, and
 * states every budget dimension Plan 26 requires: the two that projection
 * actually carries, and the eleven that no landed read route projects yet. See
 * `performanceBudgets.ts` for which is which and why.
 *
 * Nothing here derives a percentile, sums a span, or grades a budget. The one
 * thing this surface computes is how many of its own requirements the wire
 * answered, and it prints that beside the requirement count so a reader cannot
 * mistake a mostly-unavailable view for a healthy one.
 */
import type { ReactNode } from 'react';
import {
  ObservatoryReadModelV1Schema,
  type ObservatoryReadModelV1,
} from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { EnvelopeTruth, OmissionReasons } from '../../ui/EnvelopeTruth.tsx';
import { EnvelopeSection } from '../../ui/ReadSection.tsx';
import { Field } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { StateChip } from '../../ui/StateChip';
import { PlanDimensionGrid } from './PlanDimensionCard.tsx';
import { planDimensionPresentation } from './planDimension.ts';
import { budgetAnchors, budgetCoverage, performanceBudgetBands } from './performanceBudgets.ts';

export function PerformanceBudgets() {
  const read = useEnvelope(
    ['observatory', 'performance-budgets'],
    '/api/observatory',
    ObservatoryReadModelV1Schema,
    { staleTime: 30_000 },
  );

  return (
    <EnvelopeSection
      title="Performance budgets"
      blurb={
        'p50/p95/p99 with support and intervals, queue/lock/provider spans, RSS/CPU/I/O,' +
        ' no-progress outcomes, and the accepted budget revision — from the Plan 26 canonical' +
        ' read model'
      }
      result={read.data}
      pending={read.isPending}
      loadingDetail="requesting canonical performance measurements"
      transportDetail="canonical performance measurements could not be read"
    >
      {(envelope) => (
        <BudgetReadModel
          model={envelope.payload}
          truth={
            <>
              <EnvelopeTruth
                envelope={envelope}
                refreshing={read.isFetching}
                onRefresh={() => void read.refetch()}
              />
              <OmissionReasons coverage={envelope.coverage} />
            </>
          }
        />
      )}
    </EnvelopeSection>
  );
}

function BudgetReadModel({
  model,
  truth,
}: {
  model: ObservatoryReadModelV1;
  truth: ReactNode;
}) {
  const bands = performanceBudgetBands(model);
  const anchors = budgetAnchors(model);
  const coverage = budgetCoverage(bands);
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });

  return (
    <>
      {truth}
      <dl
        className="mx-4 mt-3 grid gap-x-4 gap-y-1 border border-edge-subtle bg-surface-1 px-3 py-2 text-3xs sm:grid-cols-2 xl:grid-cols-4"
        data-budgets-current={model.current ? 'true' : 'false'}
        data-budgets-measured={coverage.measured}
        data-budgets-required={coverage.required}
      >
        <Field label="horizon">
          {stamp(model.horizon.since_micros)} → {stamp(model.horizon.until_micros)}
        </Field>
        <Field label="observed at">{stamp(model.observed_at_micros)}</Field>
        <Field label="authorized scope">{model.authorized_scope_ref}</Field>
        <Field label="frontier">
          {model.current ? 'current' : 'not current'} · watermark {model.watermark}
        </Field>
      </dl>

      <div className="flex flex-col gap-4 px-4 py-3">
        <p className="text-2xs leading-relaxed text-text-secondary" data-budgets-summary="">
          {coverage.measured} of {coverage.required} required budget dimensions carry a figure.{' '}
          {coverage.unprojected} are recorded server-side but projected by no landed read route,
          and each states its own reason below rather than reading as zero.
        </p>

        {bands.map((band) => (
          <PlanDimensionGrid
            key={band.marker}
            marker={band.marker}
            label={band.label}
            dimensions={band.dimensions.map((dimension) =>
              planDimensionPresentation(dimension, anchors),
            )}
          />
        ))}

        <section
          className="flex flex-col gap-1.5 border border-edge-subtle bg-surface-1 px-3 py-2.5"
          aria-label="Budget projection gap"
          data-budgets-gap="unprojected"
        >
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="td-legend truncate">why most cards are unavailable</h3>
            <span aria-hidden className="td-rule" />
          </div>
          <StateChip kind="unsupported" detail="no read route projects these families" />
          <p className="text-3xs leading-snug text-text-muted">
            The producing families are landed and recording:{' '}
            <span className="td-value">OperationResourceObservedV1</span> carries the percentile-
            eligible latencies, the closed span set, RSS/PSS, CPU, and I/O amplification, and{' '}
            <span className="td-value">NoProgressObservedV1</span> carries the stalled frontier and
            escalation. What does not exist is a read model that projects them, so this dashboard
            has nothing to bind to. Deriving these in the browser from event counts would
            fabricate a measurement the daemon never took.
          </p>
        </section>
      </div>
    </>
  );
}
