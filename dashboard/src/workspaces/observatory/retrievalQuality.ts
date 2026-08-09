/** Plan 26 retrieval quality from the canonical Observatory projection. */
import type { ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import { readMetric, type PlanDimension, type ReadAnchors } from './planDimension.ts';

/** Why no per-retriever budget, candidate, rank, or contribution figure reaches
 * this surface. */
const NO_RETRIEVER_PROJECTION =
  'the canonical Observatory projection has no retriever evidence for this horizon';

/** Why no planner, fan-out, or synthesis span reaches this surface. */
const NO_SPAN_PROJECTION =
  'the canonical Observatory projection has no retrieval-span evidence for this horizon';

/** Why context precision is not read off the feedback relevance ratio. */
const NO_CONTEXT_PRECISION =
  'context precision is not recorded; feedback relevance uses a different population and is not substituted';

/** Why no task-outcome linkage figure reaches this surface. */
const NO_OUTCOME_LINKAGE =
  'the canonical Observatory projection has no task-outcome evidence for this horizon';

/** Why no ablation comparison reaches this surface. */
const NO_ABLATION_PROJECTION =
  'the canonical Observatory projection has no compatible equal-budget ablation evidence';

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
export function retrieverDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  const projected = (
    id: string,
    label: string,
    requirement: string,
    metric: string,
  ): PlanDimension => ({
    id,
    label,
    requirement,
    reading: readMetric(model.metrics, metric, NO_RETRIEVER_PROJECTION),
  });
  return [
    projected(
      'retriever_budget',
      'per-retriever budget',
      'requested against consumed candidate budget, per retriever lane and profile revision',
      'retriever_consumed_candidates',
    ),
    projected(
      'candidate_counts',
      'candidate counts',
      'eligible against returned candidates over the same retriever population',
      'retriever_returned_candidates',
    ),
    projected(
      'candidate_rank',
      'candidate rank',
      'rank position of contributing candidates within each retriever lane',
      'retriever_candidate_rank',
    ),
    projected(
      'unique_contribution',
      'unique contribution',
      'candidates a lane contributed that no other lane returned',
      'retriever_unique_contributions',
    ),
  ];
}

/** Planner, fan-out, and synthesis spans, named individually for the same
 * reason the retriever band is split. */
export function spanDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  const span = (id: string, label: string, requirement: string, metric: string): PlanDimension => ({
    id,
    label,
    requirement,
    reading: readMetric(model.metrics, metric, NO_SPAN_PROJECTION),
  });
  return [
    span(
      'planner_span',
      'planner span',
      'time to decide requested and admitted lanes, with the planner revision that decided them',
      'retrieval_planner_span_p95',
    ),
    span(
      'fanout_span',
      'fan-out span',
      'time across the admitted retriever lanes running in parallel',
      'retrieval_fanout_span_p95',
    ),
    span(
      'synthesis_span',
      'synthesis span',
      'time to reduce candidates to admitted context, and whether the step abstained',
      'retrieval_synthesis_span_p95',
    ),
  ];
}

/** Precision, outcome linkage, and equal-budget ablations. */
export function judgementDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  return [
    {
      id: 'context_precision',
      label: 'context precision',
      requirement: 'share of admitted context items that contributed to the answer',
      reading: readMetric(model.metrics, 'retrieval_context_precision', NO_CONTEXT_PRECISION),
    },
    {
      id: 'task_outcome_linkage',
      label: 'task-outcome linkage',
      requirement:
        'retrieval linked to an independently observed task outcome, with censored links kept separate',
      reading: readMetric(model.metrics, 'retrieval_task_outcome_linkage', NO_OUTCOME_LINKAGE),
    },
    {
      id: 'equal_budget_ablation',
      label: 'equal-budget ablation',
      requirement:
        'baseline against candidate at equal candidate, context, and token budget, in a declared unit',
      reading: readMetric(model.metrics, 'retrieval_equal_budget_ablation', NO_ABLATION_PROJECTION),
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
      dimensions: retrieverDimensions(model),
    },
    { marker: 'spans', label: 'Planner, fan-out, and synthesis spans', dimensions: spanDimensions(model) },
    {
      marker: 'judgement',
      label: 'Precision, outcome linkage, and ablations',
      dimensions: judgementDimensions(model),
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
