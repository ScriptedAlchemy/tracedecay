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
  type AnalyticsDiagnosticsPayloadV1,
  type CoverageStateV1,
  type DashboardEnvelopeV1,
  type ObservatoryReadModelV1,
} from '../../contracts/generated.ts';
import { EnvelopeTruth, OmissionReasons } from '../../ui/EnvelopeTruth.tsx';
import { EnvelopeSection, envelopeReadState, type ReadState } from '../../ui/ReadSection.tsx';
import { Field } from '../../ui/instrument.tsx';
import { formatMicrosUtc } from '../../ui/format.ts';
import { StateChip, type DomainStateKind } from '../../ui/StateChip';
import { PlanDimensionGrid } from './PlanDimensionCard.tsx';
import { planDimensionPresentation } from './planDimension.ts';
import { BlockedFamilyLedger, ObservedFamilyLedger } from './ObservedFamilyLedger.tsx';
import {
  DIAGNOSTICS_WINDOW_ROWS,
  SUPPRESSION_FLOOR,
  familyRowPresentation,
  readFamily,
  type EligibleVersusObserved,
  windowTruth,
  withheldCount,
} from './observedFamilies.ts';
import {
  CANONICAL_FAMILIES,
  DECLARED_FLOORS,
  adoptionCoverageBands,
  adoptionCoverageReading,
  coverageWindowTruth,
  coverageAnchors,
  coverageTotals,
  type DenominatorIntegrity,
  denominatorFailures,
  denominatorFailureTruth,
  eventCoverageReading,
} from './adoptionCoverage.ts';
import type { ObservatoryAccountingReads } from './accountingReads.ts';

export function AdoptionCoverage({ reads }: { reads: ObservatoryAccountingReads }) {
  const { observatory: read, diagnostics } = reads;
  const families = envelopeReadState(diagnostics.pending, diagnostics.result, {
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
      result={read.result}
      pending={read.pending}
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
  const denominatorTruth = denominatorFailureTruth(failures);
  const events = eventCoverageReading(model);
  const adoption = adoptionCoverageReading();
  const eventDisplay = eligibleDisplay(events.reading, events.integrity, events.coverage);
  const adoptionDisplay = eligibleDisplay(adoption);
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
        data-coverage-denominator-state={denominatorTruth.state}
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
          {failures.total === 0
            ? 'No metric published a denominator to audit, so no rate is published on this page.'
            : `${failures.failed} of ${failures.total} measurements in this read have a denominator that cannot contradict them. No rate is published on this page.`}
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
          data-coverage-ratio={eventDisplay.kind}
        >
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="td-legend truncate">eligible versus observed</h3>
            <span aria-hidden className="td-rule" />
          </div>

          <div className="flex flex-col gap-1" data-coverage-population="events">
            <span className="td-legend">observability events</span>
            {eventDisplay.pair === undefined ? (
              <span className="flex flex-wrap items-center gap-2">
                <StateChip kind={eventDisplay.state} />
                <span className="min-w-0 text-2xs text-text-secondary">{eventDisplay.detail}</span>
              </span>
            ) : (
              <>
                <span className="flex flex-wrap items-center gap-2">
                  <StateChip kind={eventDisplay.state} />
                  <span className="td-value text-xl text-text-primary" data-cell="numeric">
                    {eventDisplay.pair.observed.toLocaleString()} observed of{' '}
                    {eventDisplay.pair.eligible.toLocaleString()} eligible
                  </span>
                </span>
                <span className="text-3xs text-text-muted">
                  {eventDisplay.detail}
                </span>
              </>
            )}
          </div>

          <div className="flex flex-col gap-1" data-coverage-population="adoption">
            <span className="td-legend">adoption units</span>
            {adoptionDisplay.pair === undefined ? (
              <span className="flex flex-wrap items-center gap-2">
                <StateChip kind={adoptionDisplay.state} />
                <span className="min-w-0 text-2xs text-text-secondary">
                  {adoptionDisplay.detail}
                </span>
              </span>
            ) : (
              <span className="td-value text-xl text-text-primary" data-cell="numeric">
                {adoptionDisplay.pair.observed.toLocaleString()} observed of{' '}
                {adoptionDisplay.pair.eligible.toLocaleString()} eligible
              </span>
            )}
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
            kind={denominatorTruth.state}
            detail={denominatorTruth.detail}
          />
          <p className="text-3xs leading-snug text-text-muted">
            {failures.total === 0
              ? 'No metric payload reached this read, so an empty audit is unknown rather than a passing denominator check.'
              : `${failures.missing} measurement${failures.missing === 1 ? '' : 's'} publish no eligible population at all, and ${failures.selfReferential} report one equal to their own observed count. Neither can disagree with its numerator, so no share is taken from either.`}
          </p>
        </section>

        <CoverageFamilies families={families} />
      </div>
    </>
  );
}

interface EligibleDisplay {
  readonly kind: EligibleVersusObserved['kind'] | DenominatorIntegrity['kind'] | 'coverage_limited';
  readonly state: DomainStateKind;
  readonly detail: string;
  readonly pair?: { readonly observed: number; readonly eligible: number };
}

/**
 * The eligible/observed pair is rendered in the exact state the source gave
 * it. A rate floor, a contradiction, and a missing denominator are different
 * facts; reducing all three to an Unsupported chip would erase the only
 * explanation the reader has for why no rate is shown.
 */
function eligibleDisplay(
  reading: EligibleVersusObserved | null,
  integrity?: DenominatorIntegrity,
  coverage: CoverageStateV1 | 'missing' = 'known',
): EligibleDisplay {
  if (reading === null) {
    if (integrity?.kind === 'self_referential') {
      return {
        kind: integrity.kind,
        state: 'conflicting',
        detail: `non-independent denominator: ${integrity.reason}`,
      };
    }
    if (integrity === undefined || integrity.kind === 'independent') {
      const coverageDisplay = limitedCoverageDisplay(coverage);
      if (coverageDisplay !== null) return coverageDisplay;
      return {
        kind: 'missing',
        state: 'unknown',
        detail: 'the source did not carry an eligible-versus-observed reading',
      };
    }
    return {
      kind: integrity.kind,
      state: 'unknown',
      detail: integrity.reason,
    };
  }

  switch (reading.kind) {
    case 'measured':
      return {
        kind: reading.kind,
        state: 'ready',
        detail: 'the source published both counts; no rate or remainder is derived in the dashboard',
        pair: { observed: reading.observed, eligible: reading.eligible },
      };
    case 'under_rate_floor':
      return { kind: reading.kind, state: 'partial', detail: reading.reason };
    case 'contradiction':
      return { kind: reading.kind, state: 'conflicting', detail: reading.reason };
    case 'denominator_missing':
    case 'observed_missing':
      return { kind: reading.kind, state: 'unknown', detail: reading.reason };
    default: {
      const unhandled: never = reading;
      return unhandled;
    }
  }
}

function limitedCoverageDisplay(
  coverage: CoverageStateV1 | 'missing',
): EligibleDisplay | null {
  switch (coverage) {
    case 'known':
      return null;
    case 'capped':
    case 'partial':
    case 'sampled':
      return {
        kind: 'coverage_limited',
        state: 'partial',
        detail: `the metric's eligible/observed counts are ${coverage}, so they are not rendered as a complete pair`,
      };
    case 'stale':
      return {
        kind: 'coverage_limited',
        state: 'stale',
        detail: 'the metric coverage is stale, so its numeric counts are not rendered as a current pair',
      };
    case 'unknown':
    case 'missing':
      return {
        kind: 'coverage_limited',
        state: 'unknown',
        detail: `the metric coverage is ${coverage}, so its numeric counts are not rendered as a measured pair`,
      };
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
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
  const metric = coverageWindowTruth(model);
  const diagnosticsCompleteness =
    families.kind === 'ready' ? families.value.coverage.completeness : null;

  return (
    <section
      className="flex flex-col gap-1.5 border border-edge-subtle bg-surface-1 px-3 py-2.5"
      aria-label="Window truthfulness"
      data-coverage-window={metric.metricState}
    >
      <div className="flex min-w-0 items-center gap-2">
        <h3 className="td-legend truncate">window truthfulness</h3>
        <span aria-hidden className="td-rule" />
      </div>
      <StateChip kind={metric.presentation} />
      <dl className="flex flex-col gap-1 text-3xs leading-snug text-text-muted">
        <div className="flex min-w-0 gap-1.5">
          <dt className="shrink-0 uppercase tracking-[0.08em]">event coverage</dt>
          <dd className="min-w-0 break-words text-text-secondary">
            {metric.metricState} · frontier {model.current ? 'current' : 'not current'}
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
