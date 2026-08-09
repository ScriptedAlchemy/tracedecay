/** Plan 26 adoption coverage over the canonical Observatory projection. */
import type {
  CoverageStateV1,
  MetricValueV1,
  ObservatoryReadModelV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import { readMetric, type PlanDimension, type ReadAnchors } from './planDimension.ts';
import {
  RATE_MIN_COVERAGE,
  RATE_MIN_ELIGIBLE,
  eligibleVersusObserved,
  type EligibleVersusObserved,
} from './observedFamilies.ts';

const NO_ELIGIBLE_PROJECTION =
  'the canonical Observatory projection did not publish an eligible population for this horizon';

const NO_LATENESS_PROJECTION =
  'the canonical Observatory projection did not publish arrival evidence for this horizon';

/**
 * The coverage state belongs to the metric, not to the enclosing snapshot.
 *
 * A current snapshot can faithfully report stale, sampled, partial, or unknown
 * metric evidence. Mapping `current` to Ready would turn those source states
 * into a claim the source did not make, so only the wire's `known` state is
 * ready here. A missing metric is unknown rather than a clean zero.
 */
export interface CoverageWindowTruth {
  readonly metricState: CoverageStateV1 | 'missing';
  readonly presentation: DomainStateKind;
}

export function coverageWindowTruth(model: ObservatoryReadModelV1): CoverageWindowTruth {
  const metricState = model.metrics.find((metric) => metric.metric === 'observability_events')
    ?.coverage.state;
  switch (metricState) {
    case 'known':
      return { metricState, presentation: 'ready' };
    case 'capped':
    case 'partial':
    case 'sampled':
      return { metricState, presentation: 'partial' };
    case 'stale':
      return { metricState, presentation: 'stale' };
    case 'unknown':
      return { metricState, presentation: 'unknown' };
    case undefined:
      return { metricState: 'missing', presentation: 'unknown' };
    default: {
      const unhandled: never = metricState;
      return unhandled;
    }
  }
}

export type DenominatorIntegrity =
  | { kind: 'independent'; eligible: number; observed: number }
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
  return { kind: 'independent', eligible, observed };
}

/**
 * Eligible versus observed for the event population, resolved through the
 * integrity check first.
 *
 * A missing denominator never reaches the arithmetic. Equal eligible and
 * observed counts remain a valid measured pair because denominator provenance
 * is decided by the server, not guessed from coincidental numeric equality.
 */
export function eventCoverageReading(model: ObservatoryReadModelV1): {
  integrity: DenominatorIntegrity;
  reading: EligibleVersusObserved | null;
  coverage: CoverageStateV1 | 'missing';
} {
  const metric = model.metrics.find((candidate) => candidate.metric === 'observability_events');
  const integrity = denominatorIntegrity(metric);
  const coverage = metric?.coverage.state ?? 'missing';
  // Numeric fields in a capped, sampled, stale, partial, or unknown metric are
  // source facts, not a complete eligible/observed measurement. Do not let a
  // pair-shaped value promote its source state to `measured` in the UI.
  if (integrity.kind !== 'independent' || coverage !== 'known') {
    return { integrity, reading: null, coverage };
  }
  return {
    integrity,
    reading: eligibleVersusObserved(integrity.observed, integrity.eligible),
    coverage,
  };
}

/** Eligible versus invoked for the adoption population. */
export function adoptionCoverageReading(model: ObservatoryReadModelV1): EligibleVersusObserved {
  const eligible = model.metrics.find((metric) => metric.metric === 'adoption_eligible');
  const observed = model.metrics.find((metric) => metric.metric === 'adoption_invoked');
  if (
    eligible?.value == null ||
    observed?.value == null ||
    eligible.coverage.state !== 'known' ||
    observed.coverage.state !== 'known'
  ) {
    return eligibleVersusObserved(null, null);
  }
  return eligibleVersusObserved(observed.value, eligible.value);
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
      reading: readMetric(model.metrics, 'adoption_eligible', NO_ELIGIBLE_PROJECTION),
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
      reading: readMetric(model.metrics, 'observability_late_arrivals', NO_LATENESS_PROJECTION),
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
 * A denominator failure is an absent eligible population. Numeric equality is
 * not evidence that two independently projected counts share an authority.
 */
export function denominatorFailures(model: ObservatoryReadModelV1): {
  failed: number;
  total: number;
  missing: number;
} {
  let missing = 0;
  for (const metric of model.metrics) {
    const integrity = denominatorIntegrity(metric);
    if (integrity.kind === 'missing') missing += 1;
  }
  return {
    failed: missing,
    total: model.metrics.length,
    missing,
  };
}

/** A denominator audit requires at least one published metric. `0 of 0` says
 * the audit had no population, not that every denominator independently passed. */
export function denominatorFailureTruth(failures: ReturnType<typeof denominatorFailures>): {
  state: DomainStateKind;
  detail: string;
} {
  if (failures.total === 0) {
    return {
      state: 'unknown',
      detail: 'no metric carried an eligible denominator, so no denominator audit is available',
    };
  }
  if (failures.failed === 0) {
    return {
      state: 'ready',
      detail: 'every published metric carried an independent eligible denominator',
    };
  }
  return {
    state: 'unknown',
    detail: `${failures.failed} published measurement${failures.failed === 1 ? '' : 's'} lacks an eligible denominator`,
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
