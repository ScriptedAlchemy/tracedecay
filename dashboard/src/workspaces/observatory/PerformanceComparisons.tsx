/** Performance comparison evidence and disposition from `GET /api/observatory`. */
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
import {
  COMPARISON_DISPOSITIONS,
  comparisonAnchors,
  dispositionPresentation,
  performanceComparisonBands,
} from './performanceComparisons.ts';
import type { ComparisonDispositionV1 } from '../../contracts/generated.ts';

export function PerformanceComparisons() {
  const read = useEnvelope(
    ['observatory', 'performance-comparisons'],
    '/api/observatory',
    ObservatoryReadModelV1Schema,
    { staleTime: 60_000 },
  );

  return (
    <EnvelopeSection
      title="Performance comparisons"
      blurb={
        'baseline and candidate evidence with exactly one promote / reject /' +
        ' insufficient-evidence disposition'
      }
      result={read.data}
      pending={read.isPending}
      loadingDetail="requesting comparison anchors"
      transportDetail="comparison anchors could not be read"
    >
      {(envelope) => (
        <ComparisonReadModel
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

function ComparisonReadModel({
  model,
  truth,
}: {
  model: ObservatoryReadModelV1;
  truth: ReactNode;
}) {
  const anchors = comparisonAnchors(model);
  const bands = performanceComparisonBands(model);
  const decision = model.comparison;
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });

  return (
    <>
      {truth}
      <dl
        className="mx-4 mt-3 grid gap-x-4 gap-y-1 border border-edge-subtle bg-surface-1 px-3 py-2 text-3xs sm:grid-cols-2 xl:grid-cols-4"
        data-comparisons-current={model.current ? 'true' : 'false'}
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
        <Disposition
          disposition={decision.disposition}
          reason={decision.unavailable_reason ?? 'comparison evidence is complete'}
        />

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

/**
 * The one disposition, plus the two it is not.
 *
 * The reached disposition is a single element with a single
 * `data-comparison-disposition` attribute; the others are listed as
 * not-reached with their own meanings so a reader can see the taxonomy is
 * three-valued and that this comparison landed on one specific value of it.
 */
function Disposition({
  disposition,
  reason,
}: {
  disposition: ComparisonDispositionV1;
  reason: string;
}) {
  const reached = dispositionPresentation(disposition);
  const others = COMPARISON_DISPOSITIONS.filter((candidate) => candidate !== disposition);
  return (
    <section
      className="flex flex-col gap-2 border border-edge-subtle bg-surface-1 px-3 py-2.5"
      aria-label="Comparison disposition"
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate">disposition</h3>
        <span aria-hidden className="td-rule" />
      </div>

      <div
        className="flex flex-col gap-1.5"
        data-comparison-disposition={reached.disposition}
        data-comparison-disposition-reached="true"
      >
        <div className="flex flex-wrap items-center gap-2">
          <StateChip kind={reached.state} detail={reached.label} />
        </div>
        <p className="text-2xs leading-relaxed text-text-secondary">{reason}</p>
        <p className="text-3xs leading-snug text-text-muted">{reached.meaning}</p>
      </div>

      <dl className="flex flex-col gap-1 border-t border-edge-subtle pt-2 text-3xs leading-snug text-text-muted">
        {others.map((candidate) => {
          const presentation = dispositionPresentation(candidate);
          return (
            <div
              key={candidate}
              className="flex min-w-0 gap-1.5"
              data-comparison-disposition-not-reached={candidate}
            >
              <dt className="shrink-0 uppercase tracking-[0.08em]">{presentation.label}</dt>
              <dd className="min-w-0 break-words text-text-secondary">
                not reached · {presentation.meaning}
              </dd>
            </div>
          );
        })}
      </dl>
    </section>
  );
}
