/**
 * ADOPTION COVERAGE — eligible versus observed, late/dropped/capped,
 * suppression, and denominator failures.
 *
 * Two independent reads: `GET /api/observatory` for the canonical event
 * measurements and the read's own anchors, and
 * `GET /api/plugins/analytics/diagnostics` for per-family record counts.
 *
 * The point of this surface is to state the conditions under which the rest of
 * the accounting views may be believed, so it prints its own failures first:
 * how many measurements have no denominator that could contradict them, which
 * window the counts were taken through, and how many cells were withheld by the
 * Plan 26 suppression floor. See `adoptionCoverage.ts` for why an "observed
 * over eligible" ratio is refused rather than shown as 100%.
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
  CANONICAL_FAMILIES,
  DECLARED_FLOORS,
  adoptionCoverageBands,
  adoptionCoverageReading,
  coverageAnchors,
  coverageTotals,
  denominatorFailures,
  eventCoverageReading,
} from './adoptionCoverage.ts';

export function AdoptionCoverage() {
  const read = useEnvelope(
    ['observatory', 'adoption-coverage'],
    '/api/observatory',
    ObservatoryReadModelV1Schema,
    { staleTime: 30_000 },
  );
  const diagnostics = useEnvelope(
    ['observatory', 'adoption-coverage', 'families'],
    '/api/plugins/analytics/diagnostics',
    AnalyticsDiagnosticsPayloadV1Schema,
    { staleTime: 30_000 },
  );
  const families = envelopeReadState(diagnostics.isPending, diagnostics.data, {
    loading: 'requesting per-family record counts',
    transport: 'per-family record counts could not be read',
  });

  return (
    <EnvelopeSection
      title="Adoption coverage"
      blurb={
        'eligible versus observed, late/dropped/capped, suppression, and denominator failures' +
        ' — the conditions under which the other accounting views may be read'
      }
      result={read.data}
      pending={read.isPending}
      loadingDetail="requesting canonical coverage measurements"
      transportDetail="canonical coverage measurements could not be read"
    >
      {(envelope) => (
        <CoverageReadModel
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

function CoverageReadModel({
  model,
  families,
  truth,
}: {
  model: ObservatoryReadModelV1;
  families: ReadState<DashboardEnvelopeV1<AnalyticsDiagnosticsPayloadV1>>;
  truth: ReactNode;
}) {
  const bands = adoptionCoverageBands(model);
  const anchors = coverageAnchors(model);
  const totals = coverageTotals(bands);
  const failures = denominatorFailures(model);
  const events = eventCoverageReading(model);
  const adoption = adoptionCoverageReading();
  // Whichever layer refused the ratio owns the sentence: the arithmetic when it
  // ran and declined, the integrity check when the arithmetic was never
  // reached. Never a generic "no data".
  const eventsReason =
    events.reading != null
      ? events.reading.kind === 'measured'
        ? null
        : events.reading.reason
      : events.integrity.kind === 'independent'
        ? null
        : events.integrity.reason;
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });

  return (
    <>
      {truth}
      <dl
        className="mx-4 mt-3 grid gap-x-4 gap-y-1 border border-edge-subtle bg-surface-1 px-3 py-2 text-3xs sm:grid-cols-2 xl:grid-cols-4"
        data-coverage-current={model.current ? 'true' : 'false'}
        data-coverage-measured={totals.measured}
        data-coverage-required={totals.required}
        data-coverage-denominator-failures={failures.failed}
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
        <p className="text-2xs leading-relaxed text-text-secondary" data-coverage-summary="">
          {totals.measured} of {totals.required} required coverage dimensions carry a figure, and{' '}
          {failures.failed} of {failures.total} measurements in this read have a denominator that
          cannot contradict them. No rate is published on this page.
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
          className="flex flex-col gap-2 border border-edge-subtle bg-surface-1 px-3 py-2.5"
          aria-label="Eligible versus observed"
          data-coverage-ratio={events.reading?.kind ?? events.integrity.kind}
        >
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="td-legend truncate">eligible versus observed</h3>
            <span aria-hidden className="td-rule" />
          </div>

          <div className="flex flex-col gap-1" data-coverage-population="events">
            <span className="td-legend">observability events</span>
            {events.reading?.kind === 'measured' ? (
              <>
                <span className="td-value text-xl text-text-primary" data-cell="numeric">
                  {events.reading.observed.toLocaleString()} of{' '}
                  {events.reading.eligible.toLocaleString()}
                </span>
                <span className="text-3xs text-text-muted">
                  {events.reading.remainder.toLocaleString()} eligible units not observed
                </span>
              </>
            ) : (
              <>
                <span className="td-value text-xl text-text-muted" data-cell="numeric">
                  —
                </span>
                <span className="flex flex-wrap items-center gap-2">
                  <StateChip kind="unsupported" />
                  <span className="min-w-0 text-2xs text-text-secondary">
                    {eventsReason ?? 'no reason published'}
                  </span>
                </span>
              </>
            )}
          </div>

          <div className="flex flex-col gap-1" data-coverage-population="adoption">
            <span className="td-legend">adoption units</span>
            <span className="td-value text-xl text-text-muted" data-cell="numeric">
              —
            </span>
            <span className="flex flex-wrap items-center gap-2">
              <StateChip kind="unsupported" />
              <span className="min-w-0 text-2xs text-text-secondary">
                {adoption.kind === 'measured'
                  ? `${adoption.observed.toLocaleString()} of ${adoption.eligible.toLocaleString()}`
                  : adoption.reason}
              </span>
            </span>
          </div>
        </section>

        <CoverageWindow model={model} families={families} />

        <section
          className="flex flex-col gap-1.5 border border-edge-subtle bg-surface-1 px-3 py-2.5"
          aria-label="Suppression and publication floors"
          data-coverage-floors="declared"
        >
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="td-legend truncate">suppression and publication floors</h3>
            <span aria-hidden className="td-rule" />
          </div>
          <dl className="flex flex-col gap-1 text-3xs leading-snug text-text-muted">
            {DECLARED_FLOORS.map((floor) => (
              <div key={floor.id} className="flex min-w-0 gap-1.5" data-coverage-floor={floor.id}>
                <dt className="shrink-0 uppercase tracking-[0.08em]">{floor.label}</dt>
                <dd className="min-w-0 break-words text-text-secondary">{floor.declared}</dd>
              </div>
            ))}
          </dl>
          <p className="text-3xs leading-snug text-text-muted">
            The first floor is enforced on every ledger cell on this page. The remaining three are
            not cleared by anything published here, because no eligible denominator exists to
            measure them against — which is a denominator failure, not a passing grade.
          </p>
        </section>

        <section
          className="flex flex-col gap-1.5 border border-edge-subtle bg-surface-1 px-3 py-2.5"
          aria-label="Denominator failures"
          data-coverage-failures={failures.failed}
        >
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="td-legend truncate">denominator failures</h3>
            <span aria-hidden className="td-rule" />
          </div>
          <StateChip
            kind={failures.failed === 0 ? 'ready' : 'conflicting'}
            detail={`${failures.failed} of ${failures.total} measurements`}
          />
          <p className="text-3xs leading-snug text-text-muted">
            {failures.missing} measurement{failures.missing === 1 ? '' : 's'} publish no eligible
            population at all, and {failures.selfReferential} report one equal to their own
            observed count. Neither can disagree with its numerator, so no share is taken from
            either.
          </p>
        </section>

        <CoverageFamilies families={families} />
      </div>
    </>
  );
}

/**
 * Whether the counts on this page were taken through a window that could have
 * contained the answer.
 *
 * `capped` is one of the three words Plan 26 requires this view to show, and it
 * is the one that changes how every other number reads: a capped window turns
 * an absent family from "produced nothing" into "cannot be told apart from
 * something outside the window".
 */
function CoverageWindow({
  model,
  families,
}: {
  model: ObservatoryReadModelV1;
  families: ReadState<DashboardEnvelopeV1<AnalyticsDiagnosticsPayloadV1>>;
}) {
  const eventMetric = model.metrics.find((metric) => metric.metric === 'observability_events');
  const metricState = eventMetric?.coverage.state ?? 'unknown';
  const diagnosticsCompleteness =
    families.kind === 'ready' ? families.value.coverage.completeness : null;
  const capped = metricState === 'capped' || diagnosticsCompleteness === 'partial';

  return (
    <section
      className="flex flex-col gap-1.5 border border-edge-subtle bg-surface-1 px-3 py-2.5"
      aria-label="Window truthfulness"
      data-coverage-window={capped ? 'capped' : metricState}
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate">window truthfulness</h3>
        <span aria-hidden className="td-rule" />
      </div>
      <StateChip kind={capped ? 'partial' : model.current ? 'ready' : 'stale'} />
      <dl className="flex flex-col gap-1 text-3xs leading-snug text-text-muted">
        <div className="flex min-w-0 gap-1.5">
          <dt className="shrink-0 uppercase tracking-[0.08em]">event coverage</dt>
          <dd className="min-w-0 break-words text-text-secondary">
            {metricState} · frontier {model.current ? 'current' : 'not current'}
          </dd>
        </div>
        <div className="flex min-w-0 gap-1.5">
          <dt className="shrink-0 uppercase tracking-[0.08em]">record window</dt>
          <dd className="min-w-0 break-words text-text-secondary">
            {diagnosticsCompleteness == null
              ? 'not read'
              : `${diagnosticsCompleteness} · bounded at ${DIAGNOSTICS_WINDOW_ROWS.toLocaleString()} rows`}
          </dd>
        </div>
      </dl>
    </section>
  );
}

function CoverageFamilies({
  families,
}: {
  families: ReadState<DashboardEnvelopeV1<AnalyticsDiagnosticsPayloadV1>>;
}) {
  if (families.kind === 'blocked') {
    return (
      <BlockedFamilyLedger
        label="canonical family coverage"
        marker="canonical"
        state={families.state}
        detail={families.detail}
      />
    );
  }
  const envelope = families.value;
  const payload = envelope.payload;
  const window = windowTruth(envelope.coverage.completeness, payload.available, payload.source);
  const rows = CANONICAL_FAMILIES.map((family) =>
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
        marker="canonical"
        label="canonical family coverage"
        rows={rows}
        caption={
          `Every canonical observation family, whether or not it answered, read through a ` +
          `${window.completeness} window bounded at ${DIAGNOSTICS_WINDOW_ROWS.toLocaleString()} ` +
          `rows and attributed to ${payload.source}. A family with no row here has not been ` +
          'shown to be silent; cells below the ' +
          `${SUPPRESSION_FLOOR}-unit local suppression floor are withheld rather than printed.`
        }
      />
      <p className="text-3xs leading-snug text-text-muted" data-coverage-withheld={withheld}>
        {withheld} of {rows.length} families are withheld above. That number is this view&apos;s own
        withholding, not a count of silent producers — the reading on each row says which it is.
      </p>
    </>
  );
}
