/**
 * `performance-comparisons` — the Plan 26 required view (§"Required product
 * views": "`performance-comparisons` shows baseline/candidate evidence and
 * promote/reject/insufficient-evidence disposition").
 *
 * THE DISPOSITION IS THE WHOLE POINT
 *
 * Plan 26 states the comparison record twice, and both statements end the same
 * way: a comparison retains "the exact baseline and candidate build, workload,
 * corpus, environment, oracle, configuration, platform, coverage, paired
 * outcomes, resource results, and one disposition: `promote | reject |
 * insufficient_evidence`", and the compact evaluation read models record
 * "per-stratum support/results; intervals; calibration and risk/coverage;
 * flaky/indeterminate evidence; deviations; and exactly one `promote | reject |
 * insufficient_evidence` disposition".
 *
 * `insufficient_evidence` is a peer of the other two, not a soft `reject`. The
 * plan's own list of conditions that reach it — "missing lineage, dirty or
 * incompatible subjects, post-result threshold changes, coordinated omission,
 * insufficient support, hidden regressions, or incomplete coverage yield
 * `reject` or `insufficient_evidence` directly" — is precisely the set a naive
 * implementation would collapse into "did not pass". A run that could not be
 * judged has not failed, and rendering it as a failure would be a fabricated
 * negative result exactly as much as rendering it as a pass would be a
 * fabricated positive one.
 *
 * {@link decideDisposition} is therefore total, returns exactly one
 * disposition, and can only reach `reject` from complete evidence. Absent,
 * unpinned, or under-floor evidence reaches `insufficient_evidence` and cannot
 * reach `reject` by any path.
 *
 * WHAT IS BEHIND THIS SURFACE TODAY
 *
 * No landed read route publishes a comparison record. The plan is explicit
 * that these views "reuse canonical events and anchors and do not form a
 * benchmark service or separate database", so this file does not invent one:
 * it binds to `GET /api/observatory` for the authorized scope, watermark, and
 * horizon that anchor a comparison read, and reports the comparison evidence
 * itself as unpublished. With no baseline and no candidate build pinned, the
 * disposition the plan's own rules produce is `insufficient_evidence`, and that
 * is what renders.
 */
import type { ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { PlanDimension, ReadAnchors } from './planDimension.ts';

/** The closed disposition set. Exactly one of these is ever attached to a
 * comparison. */
export type ComparisonDispositionV1 = 'promote' | 'reject' | 'insufficient_evidence';

export const COMPARISON_DISPOSITIONS: readonly ComparisonDispositionV1[] = [
  'promote',
  'reject',
  'insufficient_evidence',
];

/**
 * Eligible support floor for a comparison.
 *
 * Plan 26 §"Adoption analytics and retention" sets the comparison floor at 30
 * eligible outcomes with 90% coverage and at most 10% censoring. Below it, a
 * comparison is not a weak result — it is not a result.
 */
export const COMPARISON_SUPPORT_FLOOR = 30;

/**
 * The evidence a comparison is judged from. Every field is nullable because
 * "not published" is a state this view has to be able to represent without
 * inventing a zero: `pairedOutcomes: null` is not `pairedOutcomes: 0`.
 */
export interface ComparisonEvidence {
  /** The exact baseline build the comparison pins. */
  baselineBuild: string | null;
  /** The exact candidate build the comparison pins. */
  candidateBuild: string | null;
  /** Whether the baseline is a reproducible accepted baseline with intact
   * lineage and a pinned prior rollback profile. */
  lineageComplete: boolean | null;
  /** Eligible outcomes in the comparison population. */
  eligible: number | null;
  /** Paired outcomes actually observed across baseline and candidate. */
  pairedOutcomes: number | null;
  /** Whether the resource/latency results contain a regression. */
  regressionObserved: boolean | null;
}

/** The state of this view today: a comparison read model that does not exist,
 * stated as absent rather than as an empty result set. */
export const UNPUBLISHED_COMPARISON_EVIDENCE: ComparisonEvidence = {
  baselineBuild: null,
  candidateBuild: null,
  lineageComplete: null,
  eligible: null,
  pairedOutcomes: null,
  regressionObserved: null,
};

export interface DispositionDecision {
  disposition: ComparisonDispositionV1;
  /** Why this disposition and not another, in the plan's own terms. */
  reason: string;
}

/**
 * The one disposition, decided from the evidence.
 *
 * Ordering matters and is the plan's: a comparison "may promote only from a
 * reproducible accepted baseline and pins the prior rollback profile", so every
 * missing precondition is checked before any result is looked at. A regression
 * found in evidence that was never complete enough to judge is not a `reject`;
 * the evidence simply does not classify the outcome.
 */
export function decideDisposition(evidence: ComparisonEvidence): DispositionDecision {
  if (evidence.baselineBuild == null || evidence.candidateBuild == null) {
    return {
      disposition: 'insufficient_evidence',
      reason:
        'the exact baseline and candidate build are not published, so no comparison subject is pinned',
    };
  }
  if (evidence.lineageComplete !== true) {
    return {
      disposition: 'insufficient_evidence',
      reason:
        'lineage is missing or unverified; a comparison may promote only from a reproducible accepted baseline with a pinned prior rollback profile',
    };
  }
  if (evidence.eligible == null || evidence.pairedOutcomes == null) {
    return {
      disposition: 'insufficient_evidence',
      reason: 'the eligible population or paired-outcome count is unknown, so support is unknown',
    };
  }
  if (evidence.pairedOutcomes < COMPARISON_SUPPORT_FLOOR) {
    return {
      disposition: 'insufficient_evidence',
      reason: `support is ${evidence.pairedOutcomes.toLocaleString()} paired outcomes, below the ${COMPARISON_SUPPORT_FLOOR}-outcome comparison floor`,
    };
  }
  if (evidence.regressionObserved == null) {
    return {
      disposition: 'insufficient_evidence',
      reason: 'the resource and latency results carry no regression finding to judge',
    };
  }
  if (evidence.regressionObserved) {
    return {
      disposition: 'reject',
      reason: 'complete evidence over sufficient support recorded a regression',
    };
  }
  return {
    disposition: 'promote',
    reason:
      'complete lineage and sufficient paired support recorded no regression against a reproducible accepted baseline',
  };
}

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

/** Why no comparison record reaches this surface. */
const NO_COMPARISON_PROJECTION =
  'no landed read route publishes a comparison record; Plan 26 reuses canonical events and anchors here rather than a benchmark service or separate database';

/** The subject evidence a comparison pins, one dimension each because the plan
 * requires the exact build on both sides and a single "subjects" row could not
 * say which of the two is missing. */
export function subjectDimensions(): PlanDimension[] {
  const subject = (id: string, label: string, requirement: string): PlanDimension => ({
    id,
    label,
    requirement,
    reading: { kind: 'unpublished', reason: NO_COMPARISON_PROJECTION },
  });
  return [
    subject('baseline_build', 'baseline build', 'the exact baseline build the comparison pins'),
    subject('candidate_build', 'candidate build', 'the exact candidate build the comparison pins'),
    subject(
      'workload_and_corpus',
      'workload and corpus',
      'the workload and corpus both sides were run against',
    ),
    subject(
      'environment_and_platform',
      'environment and platform',
      'the environment, configuration, and platform both sides ran on',
    ),
    subject('oracle', 'oracle', 'the oracle and its revision'),
    subject(
      'rollback_profile',
      'prior rollback profile',
      'the prior rollback profile a promotion pins',
    ),
  ];
}

/** The result evidence a compact evaluation read model records. Each is its own
 * dimension because Plan 26 keeps correctness, safety, latency, resources,
 * tokens, cost, autonomy, and effects as separate axes and refuses one reward
 * score. */
export function resultDimensions(): PlanDimension[] {
  const result = (id: string, label: string, requirement: string): PlanDimension => ({
    id,
    label,
    requirement,
    reading: { kind: 'unpublished', reason: NO_COMPARISON_PROJECTION },
  });
  return [
    result(
      'outcome_counts',
      'outcome counts',
      'eligible, attempted, answered, abstained, denied, unknown, excluded, and censored counts, separately',
    ),
    result('stratum_support', 'per-stratum support', 'support and results for each stratum'),
    result('intervals', 'intervals', 'interval coverage for each reported result'),
    result(
      'calibration',
      'calibration',
      'predicted band, observed value, error/coverage, horizon, and estimator revision',
    ),
    result('risk_coverage', 'risk and coverage', 'the risk/coverage curve and its AURC'),
    result(
      'flaky_indeterminate',
      'flaky and indeterminate evidence',
      'flaky and indeterminate results, kept apart from failures',
    ),
    result(
      'deviations',
      'deviations',
      'deviations from the frozen plan, including post-result threshold changes',
    ),
    result(
      'paired_outcomes',
      'paired outcomes',
      'outcomes paired across baseline and candidate, with resource results',
    ),
  ];
}

export interface ComparisonBand {
  marker: string;
  label: string;
  dimensions: PlanDimension[];
}

export function performanceComparisonBands(): ComparisonBand[] {
  return [
    { marker: 'subjects', label: 'Baseline and candidate evidence', dimensions: subjectDimensions() },
    { marker: 'results', label: 'Evaluation results', dimensions: resultDimensions() },
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
