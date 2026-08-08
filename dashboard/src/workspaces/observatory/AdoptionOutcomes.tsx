/**
 * ADOPTION OUTCOMES — the Plan 26 outcome funnel, correct abstention,
 * independently useful and retained use.
 *
 * Two independent reads, as on every accounting surface here:
 *
 *   `GET /api/observatory` supplies the safe anchors every card needs — the
 *   scope the read was authorized for, the watermark it was taken at, and the
 *   window it covers. It publishes no adoption measurement, and this view does
 *   not pretend otherwise: the anchors are labelled as the observatory read's
 *   own.
 *
 *   `GET /api/plugins/analytics/diagnostics` supplies how many records each
 *   adoption observation family produced.
 *
 * The whole funnel is unavailable, and that is the finding rather than a gap in
 * the page. Nothing here draws a funnel, infers a stage from a record count,
 * or reads the diagnostics outcome tally as a terminal count. See
 * `adoptionOutcomes.ts` for which field would have carried each stage.
 */
import type { ReactNode } from 'react';
import {
  AnalyticsDiagnosticsPayloadV1Schema,
  ObservatoryReadModelV1Schema,
  type AnalyticsDiagnosticsPayloadV1,
  type DashboardEnvelopeV1,
  type ObservatoryReadModelV1,
} from '../../contracts/generated.ts';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { EnvelopeTruth, OmissionReasons } from '../../ui/EnvelopeTruth.tsx';
import { EnvelopeSection, envelopeReadState, type ReadState } from '../../ui/ReadSection.tsx';
import { Field } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { StateChip } from '../../ui/StateChip';
import { PlanDimensionGrid } from './PlanDimensionCard.tsx';
import { planDimensionPresentation, type ReadAnchors } from './planDimension.ts';
import { BlockedFamilyLedger, ObservedFamilyLedger } from './ObservedFamilyLedger.tsx';
import {
  DIAGNOSTICS_WINDOW_ROWS,
  NOT_SUCCESS_OUTCOMES,
  SUPPRESSION_FLOOR,
  familyRowPresentation,
  funnelConsistency,
  readFamily,
  windowTruth,
} from './observedFamilies.ts';
import {
  ADOPTION_FAMILIES,
  OUTCOME_TALLY_NOT_TERMINAL,
  adoptionOutcomeBands,
  funnelStageCounts,
  outcomeCoverage,
} from './adoptionOutcomes.ts';

export function AdoptionOutcomes() {
  const read = useEnvelope(
    ['observatory', 'adoption-outcomes'],
    '/api/observatory',
    ObservatoryReadModelV1Schema,
    { staleTime: 60_000 },
  );
  const diagnostics = useEnvelope(
    ['observatory', 'adoption-outcomes', 'families'],
    '/api/plugins/analytics/diagnostics',
    AnalyticsDiagnosticsPayloadV1Schema,
    { staleTime: 60_000 },
  );
  const families = envelopeReadState(diagnostics.isPending, diagnostics.data, {
    loading: 'requesting adoption observation counts',
    transport: 'adoption observation counts could not be read',
  });

  return (
    <EnvelopeSection
      title="Adoption outcomes"
      blurb={
        'the Eligible → Enabled → Available → Invoked → Terminal → IndependentlyUseful →' +
        ' RepeatUseful funnel, correct abstention, independently useful and retained use'
      }
      result={read.data}
      pending={read.isPending}
      loadingDetail="requesting adoption read anchors"
      transportDetail="adoption read anchors could not be read"
    >
      {(envelope) => (
        <OutcomeReadModel
          model={envelope.payload}
          families={families}
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

function OutcomeReadModel({
  model,
  families,
  truth,
}: {
  model: ObservatoryReadModelV1;
  families: ReadState<DashboardEnvelopeV1<AnalyticsDiagnosticsPayloadV1>>;
  truth: ReactNode;
}) {
  const bands = adoptionOutcomeBands();
  const coverage = outcomeCoverage(bands);
  const consistency = funnelConsistency(funnelStageCounts());
  const anchors: ReadAnchors = {
    authorizedScopeRef: model.authorized_scope_ref,
    watermark: model.watermark,
    horizon: model.horizon,
  };
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });

  return (
    <>
      {truth}
      <dl
        className="mx-4 mt-3 grid gap-x-4 gap-y-1 border border-edge-subtle bg-surface-1 px-3 py-2 text-3xs sm:grid-cols-2 xl:grid-cols-4"
        data-outcomes-measured={coverage.measured}
        data-outcomes-required={coverage.required}
        data-outcomes-funnel={consistency.kind}
      >
        <Field label="anchor horizon">
          {stamp(model.horizon.since_micros)} → {stamp(model.horizon.until_micros)}
        </Field>
        <Field label="observed at">{stamp(model.observed_at_micros)}</Field>
        <Field label="authorized scope">{model.authorized_scope_ref}</Field>
        <Field label="anchor watermark">
          {model.current ? 'current' : 'not current'} · {model.watermark}
        </Field>
      </dl>

      <div className="flex flex-col gap-4 px-4 py-3">
        <p className="text-2xs leading-relaxed text-text-secondary" data-outcomes-summary="">
          {coverage.measured} of {coverage.required} required outcome dimensions carry a figure.{' '}
          {coverage.unprojected} are recorded server-side but projected by no landed read route.
          The anchors above are the observatory read&apos;s own — that read publishes no adoption
          measurement, and none is borrowed from it.
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
          aria-label="Funnel consistency"
          data-funnel-consistency={consistency.kind}
        >
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="td-legend truncate">funnel consistency</h3>
            <span aria-hidden className="td-rule" />
          </div>
          <StateChip
            kind={
              consistency.kind === 'consistent'
                ? 'ready'
                : consistency.kind === 'contradiction'
                  ? 'conflicting'
                  : 'unsupported'
            }
            detail={
              consistency.kind === 'consistent'
                ? `${consistency.measured} stages checked`
                : undefined
            }
          />
          <p className="text-3xs leading-snug text-text-muted">
            {consistency.kind === 'consistent'
              ? `Every measured stage admits no more units than the stage before it, across ${consistency.measured} stages.`
              : consistency.reason}
          </p>
        </section>

        <AdoptionFamilies families={families} />

        <section
          className="flex flex-col gap-1.5 border border-edge-subtle bg-surface-1 px-3 py-2.5"
          aria-label="Signals excluded from every outcome above"
          data-outcomes-excluded="not_success"
        >
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="td-legend truncate">not success outcomes</h3>
            <span aria-hidden className="td-rule" />
          </div>
          <p className="text-3xs leading-snug text-text-muted">
            Plan 26 refuses these nine as success outcomes. Two of them are readable from the
            diagnostics route right now — per-tool invocation counts and tool calls per message —
            which is why the refusal is printed rather than merely honoured. None contributes to
            any numerator on this page.
          </p>
          <ul
            className="flex flex-wrap gap-1.5"
            aria-label="Signals that are not success outcomes"
          >
            {NOT_SUCCESS_OUTCOMES.map((signal) => (
              <li
                key={signal}
                className="border border-edge-subtle px-1.5 py-0.5 text-3xs text-text-secondary"
                data-not-outcome={signal}
              >
                {signal}
              </li>
            ))}
          </ul>
          <p className="text-3xs leading-snug text-text-muted" data-outcomes-tally="unread">
            {OUTCOME_TALLY_NOT_TERMINAL}.
          </p>
        </section>
      </div>
    </>
  );
}

function AdoptionFamilies({
  families,
}: {
  families: ReadState<DashboardEnvelopeV1<AnalyticsDiagnosticsPayloadV1>>;
}) {
  if (families.kind === 'blocked') {
    return (
      <BlockedFamilyLedger
        label="adoption observation counts"
        marker="adoption"
        state={families.state}
        detail={families.detail}
      />
    );
  }
  const envelope = families.value;
  const payload = envelope.payload;
  const window = windowTruth(envelope.coverage.completeness, payload.available, payload.source);
  const rows = ADOPTION_FAMILIES.map((family) =>
    familyRowPresentation(
      family.eventKind,
      family.label,
      readFamily(payload.by_event_kind, family.eventKind, window),
    ),
  );

  return (
    <>
      <ObservedFamilyLedger
        marker="adoption"
        label="adoption observation counts"
        rows={rows}
        caption={
          `How many records each adoption family produced in a ${window.completeness} window ` +
          `bounded at ${DIAGNOSTICS_WINDOW_ROWS.toLocaleString()} rows, attributed to ` +
          `${payload.source}. A record count is not a funnel stage: one eligibility record can ` +
          `carry any eligible population, and cells below the ${SUPPRESSION_FLOOR}-unit local ` +
          'suppression floor are withheld rather than printed.'
        }
      />
      <p className="text-3xs leading-snug text-text-muted" data-outcomes-ledger-note="">
        These counts say how often the producers wrote, not how many units moved through any
        stage. No stage count above is derived from them.
      </p>
    </>
  );
}
