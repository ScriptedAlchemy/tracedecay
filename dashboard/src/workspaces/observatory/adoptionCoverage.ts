/**
 * `adoption-coverage` — the Plan 26 required view (§"Required product views":
 * "`adoption-coverage` shows eligible versus observed, late/dropped/capped,
 * suppression, and denominator failures").
 *
 * This is the view that says whether any other adoption number may be believed,
 * so it is the one view that must not flatter itself.
 *
 * WHAT IS BOUND
 *
 *   `observability_events` and `telemetry_drops_lower_bound` on
 *   `GET /api/observatory` are real canonical measurements: how many envelopes
 *   the projection admitted, and the *proved lower bound* on how many were
 *   dropped. The second is bound with its bound-ness intact — a lower bound
 *   printed as "dropped" would understate a loss and read as a total.
 *
 *   `by_event_kind` on `GET /api/plugins/analytics/diagnostics` gives per-family
 *   record counts, which is eligible-versus-observed at producer granularity:
 *   which of the canonical families wrote anything at all.
 *
 * THE DENOMINATOR PROBLEM, STATED RATHER THAN HIDDEN
 *
 * `observatory_read_model` sets `coverage.eligible = observed` whenever the read
 * is complete. The eligible population of the metric is therefore the observed
 * count itself, and any "observed over eligible" ratio built from it is 1 by
 * construction, not by measurement. `denominatorIntegrity` detects that and the
 * view reports it as a denominator failure instead of printing a reassuring
 * 100%. It is the difference between "we saw everything" and "we counted what
 * we saw twice".
 *
 * Every other eligible denominator on this surface is simply absent:
 * `AdoptionEligibilityObservedV1.eligible` is recorded and unprojected. So no
 * adoption rate exists here, and the Plan 26 rate floor (20 eligible units and
 * 90% coverage) is stated as the bar that is not cleared rather than quietly
 * skipped.
 */
import type { MetricValueV1, ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import { readMetric, type PlanDimension, type ReadAnchors } from './planDimension.ts';
import {
  RATE_MIN_COVERAGE,
  RATE_MIN_ELIGIBLE,
  eligibleVersusObserved,
  type EligibleVersusObserved,
} from './observedFamilies.ts';

/** Why no eligible adoption denominator reaches this surface. */
const NO_ELIGIBLE_PROJECTION =
  'AdoptionEligibilityObservedV1 records the eligible population per capability, but no landed ' +
  'read route projects it and the diagnostics projector counts records without opening the envelope';

/** Why no arrival lateness reaches this surface. */
const NO_LATENESS_PROJECTION =
  'OperationResourceObservedV1 records scheduled-arrival evidence and the observability envelope ' +
  'carries a producer sequence per process boot, but no landed read route projects late or ' +
  'out-of-order arrival';

/**
 * Whether a measurement's eligible denominator is independent of its own
 * observed count.
 *
 * `self_referential` is not a defect in the read — an event projection that has
 * seen every event it knows about is genuinely complete over that set. It is a
 * defect in any *rate* built from it, which is why it is detected here rather
 * than left for a reader to notice that a coverage figure is always 100%.
 */
export type DenominatorIntegrity =
  | { kind: 'independent'; eligible: number; observed: number }
  | { kind: 'self_referential'; count: number; reason: string }
  | { kind: 'missing'; reason: string };

export function denominatorIntegrity(metric: MetricValueV1 | undefined): DenominatorIntegrity {
  if (metric === undefined) {
    return { kind: 'missing', reason: NO_ELIGIBLE_PROJECTION };
  }
  const { eligible, observed } = metric.coverage;
  if (eligible == null) {
    return {
      kind: 'missing',
      reason:
        metric.unavailable_reason ??
        'the projector published no eligible population for this measurement',
    };
  }
  if (eligible === observed) {
    return {
      kind: 'self_referential',
      count: observed,
      reason:
        `the eligible population is the observed count itself (${observed.toLocaleString()}), so ` +
        'a share of it would be 1 by construction rather than by measurement',
    };
  }
  return { kind: 'independent', eligible, observed };
}

/**
 * Eligible versus observed for the event population, resolved through the
 * integrity check first.
 *
 * A self-referential or missing denominator never reaches the arithmetic: the
 * ratio is withheld with the reason, exactly as it would be if the denominator
 * were absent, because a denominator that cannot disagree with its numerator is
 * not a denominator.
 */
export function eventCoverageReading(model: ObservatoryReadModelV1): {
  integrity: DenominatorIntegrity;
  reading: EligibleVersusObserved | null;
} {
  const metric = model.metrics.find((candidate) => candidate.metric === 'observability_events');
  const integrity = denominatorIntegrity(metric);
  if (integrity.kind !== 'independent') return { integrity, reading: null };
  return {
    integrity,
    reading: eligibleVersusObserved(integrity.observed, integrity.eligible),
  };
}

/** Eligible versus observed for the adoption population itself, which no route
 * publishes at all. Written through the same function so that a future
 * projection changes an argument, not a branch. */
export function adoptionCoverageReading(): EligibleVersusObserved {
  return eligibleVersusObserved(null, null);
}

/** One band of the view. */
export interface CoverageBand {
  marker: string;
  label: string;
  dimensions: PlanDimension[];
}

/** Eligible against observed, as dimension cards. */
export function populationDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  return [
    {
      id: 'eligible_units',
      label: 'eligible units',
      requirement: 'the eligible population every adoption numerator is taken over',
      reading: { kind: 'unpublished', reason: NO_ELIGIBLE_PROJECTION },
    },
    {
      id: 'observed_events',
      label: 'observed events',
      requirement: 'observability envelopes the projection admitted over the read horizon',
      reading: readMetric(model.metrics, 'observability_events', NO_ELIGIBLE_PROJECTION),
    },
  ];
}

/** Late, dropped, and the failure count that keeps the two apart. */
export function arrivalDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  return [
    {
      id: 'late_arrivals',
      label: 'late arrivals',
      requirement: 'records that arrived after the window they belong to had been read',
      reading: { kind: 'unpublished', reason: NO_LATENESS_PROJECTION },
    },
    {
      id: 'dropped_lower_bound',
      label: 'dropped (lower bound)',
      requirement:
        'proved drop lower bound from producer-sequence gaps — a floor on the loss, never a total',
      reading: readMetric(model.metrics, 'telemetry_drops_lower_bound', NO_LATENESS_PROJECTION),
    },
    {
      id: 'terminal_failures',
      label: 'terminal failures',
      requirement:
        'admitted envelopes whose terminal result was failed or timed out, kept apart from drops',
      reading: readMetric(model.metrics, 'observability_failures', NO_LATENESS_PROJECTION),
    },
  ];
}

export function adoptionCoverageBands(model: ObservatoryReadModelV1): CoverageBand[] {
  return [
    { marker: 'population', label: 'Eligible versus observed', dimensions: populationDimensions(model) },
    { marker: 'arrival', label: 'Late, dropped, and failed', dimensions: arrivalDimensions(model) },
  ];
}

export function coverageAnchors(model: ObservatoryReadModelV1): ReadAnchors {
  return {
    authorizedScopeRef: model.authorized_scope_ref,
    watermark: model.watermark,
    horizon: model.horizon,
  };
}

/**
 * How many of a read model's measurements have no denominator that could
 * contradict them.
 *
 * A denominator failure is either an absent eligible population or one equal to
 * the observed count. Both mean the same thing for a reader: no rate may be
 * taken. Counting them is a statement about this read, derived from this read,
 * and it is the one number on this view that could not be wrong without the
 * payload being wrong.
 */
export function denominatorFailures(model: ObservatoryReadModelV1): {
  failed: number;
  total: number;
  missing: number;
  selfReferential: number;
} {
  let missing = 0;
  let selfReferential = 0;
  for (const metric of model.metrics) {
    const integrity = denominatorIntegrity(metric);
    if (integrity.kind === 'missing') missing += 1;
    if (integrity.kind === 'self_referential') selfReferential += 1;
  }
  return {
    failed: missing + selfReferential,
    total: model.metrics.length,
    missing,
    selfReferential,
  };
}

/** The Plan 26 floors, stated as the bar rather than applied silently. */
export const DECLARED_FLOORS: readonly { id: string; label: string; declared: string }[] = [
  {
    id: 'local_suppression',
    label: 'local cell suppression',
    declared: 'cells below five eligible units are suppressed',
  },
  {
    id: 'rate_floor',
    label: 'rate publication',
    declared: `a rate requires ${RATE_MIN_ELIGIBLE} eligible units and ${Math.round(
      RATE_MIN_COVERAGE * 100,
    )}% coverage`,
  },
  {
    id: 'comparison_floor',
    label: 'route/model comparison',
    declared:
      'a comparison requires 30 eligible outcomes, 90% coverage, at most 10% censoring, and no unresolved cohort shift',
  },
  {
    id: 'share_floor',
    label: 'shared cell',
    declared:
      'a shared cell requires 100 contribution windows, at most four dimensions, and one contribution per installation, capability, outcome, and day',
  },
];

/**
 * Every canonical observation family, as `ObservabilityPayloadV1::event_kind`
 * publishes it.
 *
 * The whole list is rendered, not only the families that answered, because that
 * is the coverage question: a producer that wrote nothing is the thing this
 * view exists to make visible, and it can only be visible if the row exists
 * whether or not the count does.
 */
export const CANONICAL_FAMILIES: readonly { eventKind: string; label: string }[] = [
  { eventKind: 'retrieval.query.completed.v1', label: 'retrieval query' },
  { eventKind: 'retrieval.planner.decided.v1', label: 'retrieval planner' },
  { eventKind: 'retrieval.retriever.completed.v1', label: 'retriever' },
  { eventKind: 'retrieval.synthesis.completed.v1', label: 'retrieval synthesis' },
  { eventKind: 'retrieval.source.observed.v1', label: 'retrieval source' },
  { eventKind: 'retrieval.context.outcome_linked.v1', label: 'context outcome' },
  { eventKind: 'retrieval.ablation.measured.v1', label: 'retrieval ablation' },
  { eventKind: 'adoption.eligibility_observed.v1', label: 'adoption eligibility' },
  { eventKind: 'adoption.outcome.linked.v1', label: 'adoption outcome' },
  { eventKind: 'analytics.consent.changed.v1', label: 'analytics consent' },
  { eventKind: 'operation.resource.completed.v1', label: 'operation resource' },
  { eventKind: 'operation.no_progress.terminal.v1', label: 'no progress' },
  { eventKind: 'operation.latency.observed.v1', label: 'operation latency' },
  { eventKind: 'operation.deadline.observed.v1', label: 'operation deadline' },
  { eventKind: 'storage.measurement.observed.v1', label: 'storage measurement' },
  { eventKind: 'index.measurement.observed.v1', label: 'index measurement' },
  { eventKind: 'work.execution_topology.sampled.v1', label: 'execution topology' },
  { eventKind: 'work.conflict_prediction.observed.v1', label: 'conflict prediction' },
  { eventKind: 'work.conflict_outcome.linked.v1', label: 'conflict outcome' },
  { eventKind: 'work.integration.transition.observed.v1', label: 'integration transition' },
  { eventKind: 'work.stack_drift.observed.v1', label: 'stack drift' },
  { eventKind: 'work.github_stack_capability.observed.v1', label: 'github stack capability' },
  { eventKind: 'work.duplicate_effort.observed.v1', label: 'duplicate effort' },
  { eventKind: 'work.blocked_interval.observed.v1', label: 'blocked interval' },
  { eventKind: 'work.rerun.observed.v1', label: 'work rerun' },
  { eventKind: 'work.execution_leak.observed.v1', label: 'execution leak' },
  { eventKind: 'work.delivery_fanout.observed.v1', label: 'delivery fan-out' },
  { eventKind: 'telemetry.drop.observed.v1', label: 'telemetry drop' },
  { eventKind: 'health.snapshot.observed.v1', label: 'health snapshot' },
  { eventKind: 'activity.observed.v1', label: 'activity' },
  { eventKind: 'mcp.dispatch.observed.v1', label: 'mcp dispatch' },
];

export function coverageTotals(bands: readonly CoverageBand[]): {
  measured: number;
  required: number;
  unprojected: number;
} {
  const dimensions = bands.flatMap((band) => band.dimensions);
  return {
    measured: dimensions.filter((dimension) => dimension.reading.kind === 'measured').length,
    required: dimensions.length,
    unprojected: dimensions.filter((dimension) => dimension.reading.kind === 'unpublished').length,
  };
}
