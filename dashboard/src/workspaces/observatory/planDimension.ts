/**
 * The reading rule shared by the Plan 26 accounting views.
 *
 * Plan 26 §"Required product views" ends every view description with the same
 * two sentences, and they are the whole reason this module exists rather than
 * three ad-hoc renderers:
 *
 *   "Every card exposes support, eligible denominator, censoring/unknowns,
 *    interval coverage, horizon, descriptor revision, and safe anchors;
 *    unsupported or under-floor metrics render unavailable rather than zero."
 *
 * A *dimension* is one thing a view is required to state. It resolves into
 * exactly one of three readings, and the three are not interchangeable:
 *
 *   measured     the projector published a value. The figure is printed and
 *                every accompanying fact is the wire's own.
 *   unmeasured   the projector published the metric but no value — a real
 *                accounting state carrying a denominator, coverage counts, a
 *                descriptor revision, and anchors. The reason is printed where
 *                the figure would be. It is never 0.
 *   unpublished  no landed read route projects this dimension at all. There is
 *                no denominator, no coverage, and no support to state, so the
 *                card says so in those words rather than borrowing the read's
 *                frame and implying a measurement was attempted.
 *
 * The distinction between the last two is load-bearing. `unmeasured` says the
 * daemon looked and could not answer; `unpublished` says nothing on the wire
 * answers this question yet. Collapsing them would turn a missing server
 * capability into an apparent measurement failure.
 *
 * A value of `0` is a fourth thing again, and it is `measured`: zero observed
 * events is a reading. Only a *missing* value renders as unavailable.
 */
import type { MetricValueV1, ObservabilityHorizonV1 } from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import { coverageSentence, denominatorSentence, metricFigure } from '../../ui/metricModel.ts';
import { formatMicrosUtc } from '../../ui/format.ts';

/** What is said in place of a figure that does not exist. Never `0`, never an
 * empty string — both read as a measurement. */
export const NO_FIGURE = '—';

/** Said where support, a denominator, or coverage would go for a dimension no
 * read route projects. */
export const NOT_PUBLISHED = 'not published';

export type DimensionReading =
  | { kind: 'measured'; metric: MetricValueV1 }
  | { kind: 'unmeasured'; metric: MetricValueV1; reason: string }
  | { kind: 'unpublished'; reason: string };

/** The anchors a card falls back to when it has no metric of its own: the
 * scope the read was authorized for, the watermark it was read at, and the
 * window it covers. All three come from the enclosing read model. */
export interface ReadAnchors {
  authorizedScopeRef: string;
  watermark: string;
  horizon: ObservabilityHorizonV1;
}

/** One thing a Plan 26 view is required to state, and what the wire had to say
 * about it. */
export interface PlanDimension {
  /** Wire-style identifier. Doubles as the `data-dimension` marker. */
  id: string;
  label: string;
  /** What Plan 26 requires this dimension to expose, in the plan's terms. */
  requirement: string;
  reading: DimensionReading;
}

/** One dimension reduced to the strings a card renders. Every field of the
 * plan's mandatory list is present on every card, including the cards that have
 * to answer "not published" — an omitted row would read as an oversight rather
 * than as a stated absence. */
export interface PlanDimensionPresentation {
  id: string;
  label: string;
  requirement: string;
  available: boolean;
  figure: string;
  unit: string | null;
  /** The unconverted server figure when the display figure converts it. */
  exact: string | null;
  /** Verbatim reason, never paraphrased into "no data". */
  reason: string | null;
  state: DomainStateKind;
  support: string;
  denominator: string;
  censoring: string;
  interval: string;
  horizon: string;
  descriptorRevision: string;
  anchors: string;
}

/**
 * Resolve one metric identifier against a read model's measurements.
 *
 * A metric absent from the payload is `unpublished` — the projector does not
 * emit it. A metric present with `value: null` is `unmeasured`, and the reason
 * is the projector's own; a projector that emitted no reason is reported as
 * having emitted none rather than being given one here.
 */
export function readMetric(
  metrics: readonly MetricValueV1[],
  identifier: string,
  unpublishedReason: string,
): DimensionReading {
  const metric = metrics.find((candidate) => candidate.metric === identifier);
  if (metric === undefined) return { kind: 'unpublished', reason: unpublishedReason };
  if (metric.value == null) {
    return {
      kind: 'unmeasured',
      metric,
      reason: metric.unavailable_reason ?? 'the projector published no reason',
    };
  }
  return { kind: 'measured', metric };
}

/** The state chip a reading carries. `unsupported` is the taxonomy's word for a
 * capability that does not exist here, which is exactly what `unpublished`
 * means; `unknown` is the word for a source that could not answer. */
export function dimensionState(reading: DimensionReading): DomainStateKind {
  switch (reading.kind) {
    case 'measured':
      return 'ready';
    case 'unmeasured':
      return 'unknown';
    case 'unpublished':
      return 'unsupported';
  }
}

/**
 * Support is the observed count behind a figure, kept separate from the
 * eligible denominator: a read can be eligible for 400 operations and have
 * observed 3 of them, and a card that printed only one of those two numbers
 * would let a three-sample percentile pass for a measured budget.
 */
export function supportSentence(reading: DimensionReading): string {
  if (reading.kind === 'unpublished') return NOT_PUBLISHED;
  return `${reading.metric.coverage.observed.toLocaleString()} observed`;
}

/** Censored, excluded, and unknown counts, always stated separately. Zero of
 * each is a real reading and prints as such — this line is the one place the
 * absence of censoring must be assertable rather than inferred from silence. */
export function censoringSentence(reading: DimensionReading): string {
  if (reading.kind === 'unpublished') return NOT_PUBLISHED;
  const { censored, excluded, unknown } = reading.metric.coverage;
  return `${censored.toLocaleString()} censored · ${excluded.toLocaleString()} excluded · ${unknown.toLocaleString()} unknown`;
}

/**
 * An interval only when there is one.
 *
 * The observability composer fills `lower`/`upper` with the point value itself
 * for every known value, so a bound identical to the estimate is reported as no
 * measured interval rather than dressed up as coverage. When the projector
 * supplied a reason for having no bounds, that reason is the reading.
 */
export function intervalSentence(reading: DimensionReading): string {
  if (reading.kind === 'unpublished') return NOT_PUBLISHED;
  const { lower, upper, reason } = reading.metric.uncertainty;
  if (lower == null || upper == null) return reason ?? 'no interval published';
  if (lower === upper && lower === reading.metric.value) return 'no measured interval';
  return `${lower.toLocaleString()} – ${upper.toLocaleString()} ${reading.metric.unit}`;
}

/** The window the figure was measured over. `since_micros: 0` is an open-ended
 * request, not January 1970. */
export function horizonSentence(horizon: ObservabilityHorizonV1): string {
  const stamp = (micros: number) => formatMicrosUtc(micros, { zeroAs: 'unbounded' });
  return `${stamp(horizon.since_micros)} → ${stamp(horizon.until_micros)}`;
}

/**
 * The safe anchors a reader may follow: the scope the read was authorized for
 * and the watermark it was taken at. Neither is a project path, a query, or a
 * payload — Plan 26 forbids those reaching a metric label, and nothing here
 * constructs one.
 */
export function anchorSentence(reading: DimensionReading, anchors: ReadAnchors): string {
  const watermark =
    reading.kind === 'unpublished' ? anchors.watermark : reading.metric.provenance.watermark;
  return `scope ${anchors.authorizedScopeRef} · watermark ${watermark}`;
}

/** The descriptor revision a changed definition would have to change, so a
 * redefinition cannot silently rewrite history. */
export function descriptorRevisionSentence(reading: DimensionReading): string {
  if (reading.kind === 'unpublished') return NOT_PUBLISHED;
  return reading.metric.descriptor_revision;
}

export function planDimensionPresentation(
  dimension: PlanDimension,
  anchors: ReadAnchors,
): PlanDimensionPresentation {
  const { reading } = dimension;
  const figure =
    reading.kind === 'measured'
      ? metricFigure(reading.metric)
      : { value: NO_FIGURE, unit: null, exact: null };
  return {
    id: dimension.id,
    label: dimension.label,
    requirement: dimension.requirement,
    available: reading.kind === 'measured',
    figure: figure.value,
    unit: figure.unit,
    exact: figure.exact,
    reason: reading.kind === 'measured' ? null : reading.reason,
    state: dimensionState(reading),
    support: supportSentence(reading),
    denominator:
      reading.kind === 'unpublished' ? NOT_PUBLISHED : denominatorSentence(reading.metric),
    censoring: censoringSentence(reading),
    interval: intervalSentence(reading),
    horizon: horizonSentence(
      reading.kind === 'unpublished' ? anchors.horizon : reading.metric.temporal.horizon,
    ),
    descriptorRevision: descriptorRevisionSentence(reading),
    anchors: anchorSentence(reading, anchors),
  };
}

/** How many dimensions of a set actually carry a figure. A header that said
 * "13 budgets" over eleven unavailable cards would be a claim the read cannot
 * support. */
export function measuredCount(dimensions: readonly PlanDimension[]): number {
  return dimensions.filter((dimension) => dimension.reading.kind === 'measured').length;
}

/** Re-exported so views state coverage in the same words the canonical metric
 * plates do. */
export { coverageSentence };
