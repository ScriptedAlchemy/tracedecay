/** Plan 26 adoption outcomes from the canonical Observatory projection. */
import type { ObservatoryReadModelV1 } from '../../contracts/generated.ts';
import { readMetric, type PlanDimension } from './planDimension.ts';
import { ADOPTION_FUNNEL_STAGES, type FunnelStageCount } from './observedFamilies.ts';

/** Why no eligibility-side stage count reaches this surface. */
const NO_ELIGIBILITY_PROJECTION =
  'the canonical Observatory projection has no eligibility evidence for this horizon';

/** Why no outcome-side stage count reaches this surface. */
const NO_OUTCOME_PROJECTION =
  'the canonical Observatory projection has no outcome evidence for this horizon';

/** Why correct abstention is not read off any landed route. */
const NO_ABSTENTION_PROJECTION =
  'a correct abstention needs the abstention and the independently observed absence of a right ' +
  'answer; RetrievalSynthesisObservedV1 records the abstention flag and ContextOutcomeObservedV1 ' +
  'records the outcome vocabulary, but no correct-abstention observation is available';

/** Why the terminal stage is not taken from the diagnostics outcome tally. */
export const OUTCOME_TALLY_NOT_TERMINAL =
  'the diagnostics by_outcome tally mixes hook results, tool results, and observability terminal ' +
  'results from every provider with no column to separate them, so it is not read as an adoption ' +
  'terminal count';

/** One band of the view. */
export interface OutcomeBand {
  marker: string;
  label: string;
  dimensions: PlanDimension[];
}

/** Which recording family would carry each funnel stage, and the field name on
 * it. Named per stage so a card's reason points at one field rather than at a
 * family. */
const STAGE_SOURCE: Record<(typeof ADOPTION_FUNNEL_STAGES)[number], { field: string; reason: string }> =
  {
    Eligible: { field: 'AdoptionEligibilityObservedV1.eligible', reason: NO_ELIGIBILITY_PROJECTION },
    Enabled: { field: 'AdoptionEligibilityObservedV1.enabled', reason: NO_ELIGIBILITY_PROJECTION },
    Available: { field: 'AdoptionEligibilityObservedV1.available', reason: NO_ELIGIBILITY_PROJECTION },
    Invoked: { field: 'AdoptionOutcomeLinkedV1.invoked', reason: NO_OUTCOME_PROJECTION },
    Terminal: { field: 'AdoptionOutcomeLinkedV1.terminal', reason: NO_OUTCOME_PROJECTION },
    IndependentlyUseful: {
      field: 'AdoptionOutcomeLinkedV1.independently_useful',
      reason: NO_OUTCOME_PROJECTION,
    },
    RepeatUseful: { field: 'AdoptionOutcomeLinkedV1.repeat_useful', reason: NO_OUTCOME_PROJECTION },
  };

/**
 * The seven funnel stages as dimension cards, in the plan's order.
 *
 * Each stage binds directly to its server metric. Missing evidence stays
 * unknown and never becomes a browser-derived zero.
 */
const STAGE_METRIC: Record<(typeof ADOPTION_FUNNEL_STAGES)[number], string> = {
  Eligible: 'adoption_eligible',
  Enabled: 'adoption_enabled',
  Available: 'adoption_available',
  Invoked: 'adoption_invoked',
  Terminal: 'adoption_terminal',
  IndependentlyUseful: 'adoption_independently_useful',
  RepeatUseful: 'adoption_repeat_useful',
};

export function funnelDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  return ADOPTION_FUNNEL_STAGES.map((stage, index) => ({
    id: `funnel_${stage.toLowerCase()}`,
    label: `${index + 1}. ${stage}`,
    requirement: `${stage} count with its explicit denominator, unknown/censored counts, and interval — from ${STAGE_SOURCE[stage].field}`,
    reading: readMetric(model.metrics, STAGE_METRIC[stage], STAGE_SOURCE[stage].reason),
  }));
}

/** The stage counts as the consistency check sees them. */
export function funnelStageCounts(model: ObservatoryReadModelV1): FunnelStageCount[] {
  return ADOPTION_FUNNEL_STAGES.map((stage) => {
    const reading = readMetric(model.metrics, STAGE_METRIC[stage], STAGE_SOURCE[stage].reason);
    return { stage, count: reading.kind === 'measured' ? reading.metric.value : null };
  });
}

/**
 * Correct abstention, independent usefulness, retained use, and the censored
 * and unknown counts that keep the funnel honest.
 *
 * Censored and unknown are dimensions of their own rather than a footnote:
 * Plan 26 requires unknown/censored counts alongside every funnel denominator,
 * and a funnel that reported six stages and no censoring would let a reader
 * assume there was none.
 */
export function outcomeQualityDimensions(model: ObservatoryReadModelV1): PlanDimension[] {
  return [
    {
      id: 'correct_abstention',
      label: 'correct abstention',
      requirement:
        'abstentions that were right to abstain, judged against an independently observed absence of a correct answer',
      reading: readMetric(model.metrics, 'adoption_correct_abstention', NO_ABSTENTION_PROJECTION),
    },
    {
      id: 'independently_useful',
      label: 'independently useful',
      requirement:
        'use whose usefulness was observed independently of the surface that produced it — never acceptance or a self-report',
      reading: readMetric(model.metrics, 'adoption_independently_useful', NO_OUTCOME_PROJECTION),
    },
    {
      id: 'repeat_useful',
      label: 'retained use',
      requirement: 'independently useful outcomes that recurred, over the same eligible population',
      reading: readMetric(model.metrics, 'adoption_repeat_useful', NO_OUTCOME_PROJECTION),
    },
    {
      id: 'censored_outcomes',
      label: 'censored outcomes',
      requirement: 'invoked units whose terminal outcome was censored, kept out of every numerator',
      reading: readMetric(model.metrics, 'adoption_censored_outcomes', NO_OUTCOME_PROJECTION),
    },
    {
      id: 'unknown_outcomes',
      label: 'unknown outcomes',
      requirement: 'invoked units with no observed terminal outcome, kept separate from censored',
      reading: readMetric(model.metrics, 'adoption_unknown_outcomes', NO_OUTCOME_PROJECTION),
    },
  ];
}

export function adoptionOutcomeBands(model: ObservatoryReadModelV1): OutcomeBand[] {
  return [
    { marker: 'funnel', label: 'Outcome funnel', dimensions: funnelDimensions(model) },
    {
      marker: 'quality',
      label: 'Abstention, independent usefulness, and retained use',
      dimensions: outcomeQualityDimensions(model),
    },
  ];
}

/** The two adoption observation families, with the wire's own event kinds. */
export const ADOPTION_FAMILIES: readonly { eventKind: string; label: string }[] = [
  { eventKind: 'adoption.eligibility_observed.v1', label: 'eligibility observed' },
  { eventKind: 'adoption.outcome.linked.v1', label: 'outcome linked' },
];

export function outcomeCoverage(bands: readonly OutcomeBand[]): {
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
