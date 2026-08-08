/**
 * `retrieval-quality` — the Plan 26 required view (§"Required product views":
 * "`retrieval-quality` shows per-retriever budgets, candidate/rank/contribution,
 * source freshness/coverage/denial, planner/fan-out/synthesis spans, context
 * precision, task-outcome linkage, and equal-budget ablations").
 *
 * WHAT IS ACTUALLY BEHIND THIS SURFACE
 *
 * Two landed reads, answering two different kinds of question.
 *
 *   `GET /api/observatory` → `ObservatoryReadModelV1` carries the Plan 37
 *   feedback-system quality measurements. Four of them answer parts of this
 *   view's sentence directly and are bound as canonical measurements, with the
 *   wire's own denominator, coverage, descriptor revision, and watermark:
 *   `feedback_coverage`, `feedback_denial_rate`, `feedback_staleness_rate`,
 *   `feedback_diversity`, and `feedback_omission_rate`.
 *
 *   `GET /api/plugins/analytics/diagnostics` → `by_event_kind` carries the
 *   number of records each retrieval observation family produced. That is a
 *   support count, not a measurement, and it is rendered in its own ledger for
 *   the reasons in `observedFamilies.ts`.
 *
 * WHAT IS NOT
 *
 * The seven `retrieval.*` families are landed and recording — `RetrieverObservedV1`
 * carries requested/consumed/eligible/returned candidates and unique
 * contributions per retriever lane, `RetrievalPlannerObservedV1` carries the
 * requested and admitted lanes, `RetrievalSynthesisObservedV1` carries
 * candidate/context counts and tokens, `ContextOutcomeObservedV1` carries the
 * closed outcome vocabulary with its independently-observed and censored flags,
 * and `RetrievalAblationObservedV1` carries baseline/candidate values with a
 * declared unit and coverage. None of it is projected into a read model. The
 * diagnostics projector counts rows and never opens the envelope's JSON, so
 * every figure inside those records is unreachable from this dashboard.
 *
 * THE SUBSTITUTION THIS FILE REFUSES
 *
 * `feedback_relevance` is a ratio over `relevance_labels` — how much of the
 * labelled material was judged relevant. Context precision is a ratio over the
 * context a synthesis step actually admitted. Different numerator, different
 * denominator, different population. Printing the first under the second's
 * heading is the single most tempting error available on this surface, so
 * context precision is stated as unpublished and the reason names the metric
 * that was not substituted for it.
 */
import type { ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import { readMetric, type PlanDimension, type ReadAnchors } from './planDimension.ts';

/** Why no per-retriever budget, candidate, rank, or contribution figure reaches
 * this surface. */
const NO_RETRIEVER_PROJECTION =
  'RetrieverObservedV1 records requested/consumed/eligible/returned candidates and unique ' +
  'contributions per retriever lane, but no landed read route projects them and the diagnostics ' +
  'projector counts records without opening the envelope';

/** Why no planner, fan-out, or synthesis span reaches this surface. */
const NO_SPAN_PROJECTION =
  'RetrievalPlannerObservedV1, RetrieverObservedV1, and RetrievalSynthesisObservedV1 are ' +
  'recorded per stage, but no landed read route projects a stage duration';

/** Why context precision is not read off the feedback relevance ratio. */
const NO_CONTEXT_PRECISION =
  'no landed read route projects context precision; feedback_relevance is a ratio over ' +
  'relevance_labels, a different population from the admitted context set, and is not ' +
  'substituted for it here';

/** Why no task-outcome linkage figure reaches this surface. */
const NO_OUTCOME_LINKAGE =
  'ContextOutcomeObservedV1 records the closed outcome vocabulary with independently-observed ' +
  'and censored flags, but no landed read route projects a linkage rate';

/** Why no ablation comparison reaches this surface. */
const NO_ABLATION_PROJECTION =
  'RetrievalAblationObservedV1 records baseline and candidate values with a declared unit and ' +
  'coverage, but no landed read route projects the comparison';

/** One band of the view, in the order Plan 26 names them. */
export interface RetrievalBand {
  marker: string;
  label: string;
  dimensions: PlanDimension[];
}

/**
 * Source freshness, coverage, and denial — the band that is genuinely measured.
 *
 * Each of these may still arrive with `value: null` and the projector's own
 * reason, which is an unmeasured metric and a different state from an
 * unprojected one. `readMetric` keeps the two apart.
 */
export function sourceDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  return [
    {
      id: 'source_coverage',
      label: 'source coverage',
      requirement: 'share of eligible retrieval observations a source actually answered',
      reading: readMetric(model.metrics, 'feedback_coverage', NO_RETRIEVER_PROJECTION),
    },
    {
      id: 'source_denial',
      label: 'source denial',
      requirement: 'share of outcome observations in which a source refused to answer',
      reading: readMetric(model.metrics, 'feedback_denial_rate', NO_RETRIEVER_PROJECTION),
    },
    {
      id: 'source_freshness',
      label: 'source freshness',
      requirement: 'share of outcome observations served from stale evidence',
      reading: readMetric(model.metrics, 'feedback_staleness_rate', NO_RETRIEVER_PROJECTION),
    },
    {
      id: 'source_family_diversity',
      label: 'source family diversity',
      requirement: 'share of eligible source families represented in a result',
      reading: readMetric(model.metrics, 'feedback_diversity', NO_RETRIEVER_PROJECTION),
    },
    {
      id: 'source_omission',
      label: 'source omission',
      requirement: 'share of returned-and-omitted items withheld from a result',
      reading: readMetric(model.metrics, 'feedback_omission_rate', NO_RETRIEVER_PROJECTION),
    },
  ];
}

/**
 * Per-retriever budgets and the candidate/rank/contribution chain.
 *
 * Four dimensions rather than one, because the plan names four and a single
 * "retriever evidence" card would hide which of them is missing. Rank is its
 * own dimension even though `RetrieverObservedV1` carries no rank field: the
 * plan requires rank, so the card states the requirement and says nothing
 * records it, which is a stronger statement than omitting the row.
 */
export function retrieverDimensions(): PlanDimension[] {
  const unpublished = (id: string, label: string, requirement: string): PlanDimension => ({
    id,
    label,
    requirement,
    reading: { kind: 'unpublished', reason: NO_RETRIEVER_PROJECTION },
  });
  return [
    unpublished(
      'retriever_budget',
      'per-retriever budget',
      'requested against consumed candidate budget, per retriever lane and profile revision',
    ),
    unpublished(
      'candidate_counts',
      'candidate counts',
      'eligible against returned candidates over the same retriever population',
    ),
    unpublished(
      'candidate_rank',
      'candidate rank',
      'rank position of contributing candidates within each retriever lane',
    ),
    unpublished(
      'unique_contribution',
      'unique contribution',
      'candidates a lane contributed that no other lane returned',
    ),
  ];
}

/** Planner, fan-out, and synthesis spans, named individually for the same
 * reason the retriever band is split. */
export function spanDimensions(): PlanDimension[] {
  const span = (id: string, label: string, requirement: string): PlanDimension => ({
    id,
    label,
    requirement,
    reading: { kind: 'unpublished', reason: NO_SPAN_PROJECTION },
  });
  return [
    span(
      'planner_span',
      'planner span',
      'time to decide requested and admitted lanes, with the planner revision that decided them',
    ),
    span(
      'fanout_span',
      'fan-out span',
      'time across the admitted retriever lanes running in parallel',
    ),
    span(
      'synthesis_span',
      'synthesis span',
      'time to reduce candidates to admitted context, and whether the step abstained',
    ),
  ];
}

/** Precision, outcome linkage, and equal-budget ablations. */
export function judgementDimensions(): PlanDimension[] {
  return [
    {
      id: 'context_precision',
      label: 'context precision',
      requirement: 'share of admitted context items that contributed to the answer',
      reading: { kind: 'unpublished', reason: NO_CONTEXT_PRECISION },
    },
    {
      id: 'task_outcome_linkage',
      label: 'task-outcome linkage',
      requirement:
        'retrieval linked to an independently observed task outcome, with censored links kept separate',
      reading: { kind: 'unpublished', reason: NO_OUTCOME_LINKAGE },
    },
    {
      id: 'equal_budget_ablation',
      label: 'equal-budget ablation',
      requirement:
        'baseline against candidate at equal candidate, context, and token budget, in a declared unit',
      reading: { kind: 'unpublished', reason: NO_ABLATION_PROJECTION },
    },
  ];
}

export function retrievalQualityBands(model: ObservatoryReadModelV1): RetrievalBand[] {
  return [
    {
      marker: 'sources',
      label: 'Source freshness, coverage, and denial',
      dimensions: sourceDimensions(model),
    },
    {
      marker: 'retrievers',
      label: 'Per-retriever budgets and contribution',
      dimensions: retrieverDimensions(),
    },
    { marker: 'spans', label: 'Planner, fan-out, and synthesis spans', dimensions: spanDimensions() },
    {
      marker: 'judgement',
      label: 'Precision, outcome linkage, and ablations',
      dimensions: judgementDimensions(),
    },
  ];
}

/** The anchors every card falls back to when it has no metric of its own. */
export function retrievalAnchors(model: ObservatoryReadModelV1): ReadAnchors {
  return {
    authorizedScopeRef: model.authorized_scope_ref,
    watermark: model.watermark,
    horizon: model.horizon,
  };
}

/**
 * The seven canonical retrieval observation families, in pipeline order.
 *
 * Identifiers are the wire's own `ObservabilityPayloadV1::event_kind` strings
 * and are printed verbatim on every row: a label can be reworded, an event kind
 * is what a reader would have to grep for.
 */
export const RETRIEVAL_FAMILIES: readonly { eventKind: string; label: string }[] = [
  { eventKind: 'retrieval.query.completed.v1', label: 'query completed' },
  { eventKind: 'retrieval.planner.decided.v1', label: 'planner decided' },
  { eventKind: 'retrieval.retriever.completed.v1', label: 'retriever completed' },
  { eventKind: 'retrieval.synthesis.completed.v1', label: 'synthesis completed' },
  { eventKind: 'retrieval.source.observed.v1', label: 'source observed' },
  { eventKind: 'retrieval.context.outcome_linked.v1', label: 'context outcome linked' },
  { eventKind: 'retrieval.ablation.measured.v1', label: 'ablation measured' },
];

/** Totals for the view header. Both numbers print, because "5 measured" alone
 * does not say out of how many requirements. */
export function retrievalCoverage(bands: readonly RetrievalBand[]): {
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
