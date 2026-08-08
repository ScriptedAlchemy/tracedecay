/**
 * RETRIEVAL QUALITY — two independent reads, never merged.
 *
 *   `GET /api/observatory` → the Plan 26 canonical measurements. Five of this
 *   view's dimensions are real figures from it; the other ten state that no
 *   landed read route projects them.
 *
 *   `GET /api/plugins/analytics/diagnostics` → per-family record counts for the
 *   seven `retrieval.*` observation families.
 *
 * They are read independently and rendered independently. A failed diagnostics
 * read must not blank the measured source ratios, and a failed observatory read
 * must not blank the record ledger — each says what it could not read, in its
 * own frame, and neither stands in for the other.
 *
 * Nothing on this surface divides a record count by a measurement, sums a span,
 * or derives a precision. See `retrievalQuality.ts` for what is bound to what,
 * and for the one substitution this view exists to refuse.
 */
import type { ReactNode } from 'react';
import {
  type AnalyticsDiagnosticsPayloadV1,
  type DashboardEnvelopeV1,
  type ObservatoryReadModelV1,
} from '../../contracts/generated.ts';
import { EnvelopeTruth, OmissionReasons } from '../../ui/EnvelopeTruth.tsx';
import { EnvelopeSection, envelopeReadState, type ReadState } from '../../ui/ReadSection.tsx';
import { Field } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { PlanDimensionGrid } from './PlanDimensionCard.tsx';
import { planDimensionPresentation } from './planDimension.ts';
import { BlockedFamilyLedger, ObservedFamilyLedger } from './ObservedFamilyLedger.tsx';
import {
  DIAGNOSTICS_WINDOW_ROWS,
  SUPPRESSION_FLOOR,
  familyRowPresentation,
  readFamily,
  windowTruth,
  withheldCount,
} from './observedFamilies.ts';
import {
  RETRIEVAL_FAMILIES,
  retrievalAnchors,
  retrievalCoverage,
  retrievalQualityBands,
} from './retrievalQuality.ts';
import type { ObservatoryAccountingReads } from './accountingReads.ts';

export function RetrievalQuality({ reads }: { reads: ObservatoryAccountingReads }) {
  const { observatory: read, diagnostics } = reads;
  const families = envelopeReadState(diagnostics.pending, diagnostics.result, {
    loading: 'requesting retrieval observation counts',
    transport: 'retrieval observation counts could not be read',
  });

  return (
    <EnvelopeSection
      title="Retrieval quality"
      blurb={
        'per-retriever budgets, candidate/rank/contribution, source freshness/coverage/denial,' +
        ' planner/fan-out/synthesis spans, context precision, task-outcome linkage, and' +
        ' equal-budget ablations'
      }
      result={read.result}
      pending={read.pending}
      loadingDetail="requesting canonical retrieval measurements"
      transportDetail="canonical retrieval measurements could not be read"
    >
      {(envelope) => (
        <RetrievalReadModel
          model={envelope.payload}
          families={families}
          truth={
            <>
              <EnvelopeTruth
                envelope={envelope}
                refreshing={read.refreshing}
                onRefresh={read.refresh}
              />
              <OmissionReasons coverage={envelope.coverage} />
            </>
          }
        />
      )}
    </EnvelopeSection>
  );
}

function RetrievalReadModel({
  model,
  families,
  truth,
}: {
  model: ObservatoryReadModelV1;
  families: ReadState<DashboardEnvelopeV1<AnalyticsDiagnosticsPayloadV1>>;
  truth: ReactNode;
}) {
  const bands = retrievalQualityBands(model);
  const anchors = retrievalAnchors(model);
  const coverage = retrievalCoverage(bands);
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });

  return (
    <>
      {truth}
      <dl
        className="mx-4 mt-3 grid gap-x-4 gap-y-1 border border-edge-subtle bg-surface-1 px-3 py-2 text-3xs sm:grid-cols-2 xl:grid-cols-4"
        data-retrieval-current={model.current ? 'true' : 'false'}
        data-retrieval-measured={coverage.measured}
        data-retrieval-required={coverage.required}
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
        <p className="text-2xs leading-relaxed text-text-secondary" data-retrieval-summary="">
          {coverage.measured} of {coverage.required} required retrieval dimensions carry a figure.{' '}
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

        <RetrievalFamilies families={families} />
      </div>
    </>
  );
}

/**
 * The record ledger, resolved from its own read.
 *
 * A blocked diagnostics read renders the state and the daemon's own sentence
 * and *no ledger at all* — a table of seven em dashes would say the families
 * produced nothing, which is not what an unreachable projector reported.
 */
function RetrievalFamilies({
  families,
}: {
  families: ReadState<DashboardEnvelopeV1<AnalyticsDiagnosticsPayloadV1>>;
}) {
  if (families.kind === 'blocked') {
    return (
      <BlockedFamilyLedger
        label="retrieval observation counts"
        marker="retrieval"
        state={families.state}
        detail={families.detail}
      />
    );
  }

  const envelope = families.value;
  const payload = envelope.payload;
  const window = windowTruth(envelope.coverage.completeness, payload.available, payload.source);
  const rows = RETRIEVAL_FAMILIES.map((family) =>
    familyRowPresentation(
      family.eventKind,
      family.label,
      readFamily(payload.by_event_kind, family.eventKind, window),
    ),
  );
  const withheld = withheldCount(rows);

  return (
    <>
      <ObservedFamilyLedger
        marker="retrieval"
        label="retrieval observation counts"
        rows={rows}
        caption={
          `How many records each retrieval family produced in a ${window.completeness} window ` +
          `bounded at ${DIAGNOSTICS_WINDOW_ROWS.toLocaleString()} rows, attributed to ` +
          `${payload.source}. These are record counts, not measurements: they carry no descriptor ` +
          `revision, no eligible denominator, and no interval, and cells below the ` +
          `${SUPPRESSION_FLOOR}-unit local suppression floor are withheld rather than printed.`
        }
      />
      <p className="text-3xs leading-snug text-text-muted" data-retrieval-withheld={withheld}>
        {withheld} of {rows.length} families are withheld above. A withheld cell is not a zero: the
        reading beside it says whether the count was below the suppression floor or whether the
        window could not prove the family was silent.
      </p>
    </>
  );
}
