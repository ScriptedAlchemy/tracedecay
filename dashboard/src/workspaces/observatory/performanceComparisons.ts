/** Plan 26 comparison presentation over the canonical Observatory projection. */
import type {
  ComparisonDispositionV1,
  ObservatoryReadModelV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import { readMetric, type PlanDimension, type ReadAnchors } from './planDimension.ts';

export const COMPARISON_DISPOSITIONS: readonly ComparisonDispositionV1[] = [
  'promote',
  'reject',
  'insufficient_evidence',
];

export interface DispositionPresentation {
  disposition: ComparisonDispositionV1;
  label: string;
  state: DomainStateKind;
  /** What this disposition asserts — and, for `insufficient_evidence`, what it
   * explicitly does not. */
  meaning: string;
}

/**
 * Each disposition gets its own word, its own state, and its own sentence.
 *
 * `insufficient_evidence` maps to `unknown`, never to `denied`. The two are
 * different chips with different icons and different `data-state` values, so a
 * comparison that could not be judged is distinguishable from one that was
 * judged and refused by markup alone, not only by prose.
 */
export function dispositionPresentation(
  disposition: ComparisonDispositionV1,
): DispositionPresentation {
  switch (disposition) {
    case 'promote':
      return {
        disposition,
        label: 'Promote',
        state: 'ready',
        meaning:
          'the candidate is accepted against a reproducible accepted baseline with its prior rollback profile pinned',
      };
    case 'reject':
      return {
        disposition,
        label: 'Reject',
        state: 'denied',
        meaning: 'the comparison was judged on complete evidence and the candidate was refused',
      };
    case 'insufficient_evidence':
      return {
        disposition,
        label: 'Insufficient evidence',
        state: 'unknown',
        meaning:
          'the available evidence cannot classify this comparison. This is not a rejection: nothing was judged and nothing failed',
      };
  }
}

const NO_COMPARISON_PROJECTION =
  'the canonical Observatory projection has no comparison evidence for this horizon';

/** The subject evidence a comparison pins, one dimension each because the plan
 * requires the exact build on both sides and a single "subjects" row could not
 * say which of the two is missing. */
export function subjectDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  const subject = (
    id: string,
    label: string,
    requirement: string,
    metric: string,
  ): PlanDimension => ({
    id,
    label,
    requirement,
    reading: readMetric(model.metrics, metric, NO_COMPARISON_PROJECTION),
  });
  return [
    subject(
      'baseline_build',
      'baseline build',
      'the exact baseline build the comparison pins',
      'comparison_baseline_build',
    ),
    subject(
      'candidate_build',
      'candidate build',
      'the exact candidate build the comparison pins',
      'comparison_candidate_build',
    ),
    subject(
      'workload_and_corpus',
      'workload and corpus',
      'the workload and corpus both sides were run against',
      'comparison_workload_corpus',
    ),
    subject(
      'environment_and_platform',
      'environment and platform',
      'the environment, configuration, and platform both sides ran on',
      'comparison_environment_platform',
    ),
    subject('oracle', 'oracle', 'the oracle and its revision', 'comparison_oracle'),
    subject(
      'rollback_profile',
      'prior rollback profile',
      'the prior rollback profile a promotion pins',
      'comparison_rollback_profile',
    ),
  ];
}

/** The result evidence a compact evaluation read model records. Each is its own
 * dimension because Plan 26 keeps correctness, safety, latency, resources,
 * tokens, cost, autonomy, and effects as separate axes and refuses one reward
 * score. */
export function resultDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  const result = (
    id: string,
    label: string,
    requirement: string,
    metric: string,
  ): PlanDimension => ({
    id,
    label,
    requirement,
    reading: readMetric(model.metrics, metric, NO_COMPARISON_PROJECTION),
  });
  return [
    result(
      'outcome_counts',
      'outcome counts',
      'eligible, attempted, answered, abstained, denied, unknown, excluded, and censored counts, separately',
      'comparison_outcome_counts',
    ),
    result(
      'stratum_support',
      'per-stratum support',
      'support and results for each stratum',
      'comparison_stratum_support',
    ),
    result(
      'intervals',
      'intervals',
      'interval coverage for each reported result',
      'comparison_intervals',
    ),
    result(
      'calibration',
      'calibration',
      'predicted band, observed value, error/coverage, horizon, and estimator revision',
      'comparison_calibration',
    ),
    result(
      'risk_coverage',
      'risk and coverage',
      'the risk/coverage curve and its AURC',
      'comparison_risk_coverage',
    ),
    result(
      'flaky_indeterminate',
      'flaky and indeterminate evidence',
      'flaky and indeterminate results, kept apart from failures',
      'comparison_flaky_indeterminate',
    ),
    result(
      'deviations',
      'deviations',
      'deviations from the frozen plan, including post-result threshold changes',
      'comparison_deviations',
    ),
    result(
      'paired_outcomes',
      'paired outcomes',
      'outcomes paired across baseline and candidate, with resource results',
      'comparison_paired_outcomes',
    ),
  ];
}

export interface ComparisonBand {
  marker: string;
  label: string;
  dimensions: PlanDimension[];
}

export function performanceComparisonBands(model: ObservatoryReadModelV1): ComparisonBand[] {
  return [
    { marker: 'subjects', label: 'Baseline and candidate evidence', dimensions: subjectDimensions(model) },
    { marker: 'results', label: 'Evaluation results', dimensions: resultDimensions(model) },
  ];
}

/** The anchors a comparison read is taken against. Plan 26 requires these to be
 * safe anchors; a scope reference and a watermark are, and a path or a query
 * would not be. */
export function comparisonAnchors(model: ObservatoryReadModelV1): ReadAnchors {
  return {
    authorizedScopeRef: model.authorized_scope_ref,
    watermark: model.watermark,
    horizon: model.horizon,
  };
}
