/** Plan 26 performance budgets from `GET /api/observatory`. */
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
          {coverage.required - coverage.measured} carry an explicit unknown or partial state;
          none are inferred in the browser or rendered as zero.
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

      </div>
    </>
  );
}
