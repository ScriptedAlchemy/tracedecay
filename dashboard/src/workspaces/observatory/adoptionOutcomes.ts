/**
 * `adoption-outcomes` — the Plan 26 required view (§"Required product views":
 * "`adoption-outcomes` shows the outcome funnel, correct abstention,
 * independently useful and retained use").
 *
 * THE FUNNEL
 *
 * Plan 26 fixes it verbatim: `Eligible -> Enabled -> Available -> Invoked ->
 * Terminal -> IndependentlyUseful -> RepeatUseful`, "with explicit
 * denominators, unknown/censored counts, watermarks, horizons, coverage, and
 * intervals". The seven stages are split across two recording families:
 * `AdoptionEligibilityObservedV1` carries `eligible`, `enabled`, and
 * `available` per capability; `AdoptionOutcomeLinkedV1` carries `invoked`,
 * `terminal`, `independently_useful`, `repeat_useful`, `censored`, and
 * `unknown`.
 *
 * Both families are landed and both validate their own monotonicity —
 * `AdoptionOutcomeLinkedV1::validate` refuses a record where `terminal >
 * invoked` or `repeat_useful > independently_useful`. Neither is projected into
 * a read model. `GET /api/plugins/analytics/diagnostics` counts the *records*
 * each family produced and never opens the envelope JSON, so no stage count
 * reaches this dashboard. Every stage therefore renders unavailable with the
 * reason naming the field that would have carried it.
 *
 * WHY NO FUNNEL IS DRAWN
 *
 * A funnel chart of seven unmeasured stages is seven equal bars or seven empty
 * ones, and both assert a shape the wire never published. The stages render as
 * dimension cards in funnel order, and `funnelConsistency` reports that fewer
 * than two of them carry a count, so no ordering is claimed either.
 *
 * WHAT IS NOT AN OUTCOME
 *
 * Plan 26: "Display, click, invocation, process completion, self-report, cards
 * closed, tests run, token volume, and subjective trust do not become success
 * outcomes." Two of those are readable right now —
 * `AnalyticsDiagnosticsPayloadV1` publishes `by_tool` invocation counts and
 * `ratios.tool_calls_per_message` — which is precisely why the exclusion is
 * printed on the surface instead of merely honoured in the code. A reader
 * cannot see a metric that was declined.
 *
 * WHY `by_outcome` IS NOT THE TERMINAL STAGE
 *
 * The diagnostics payload does publish `by_outcome`. It is the `outcome` column
 * of every analytics row from every provider — hook results, tool results, and
 * the lowercased `terminal_result` of any observability envelope, with no
 * column left to tell them apart. Reading `Terminal` off it would count a
 * successful `PostToolUse` hook as an adoption terminal. It is left unread and
 * this file says so.
 */
import type { PlanDimension } from './planDimension.ts';
import { ADOPTION_FUNNEL_STAGES, type FunnelStageCount } from './observedFamilies.ts';

/** Why no eligibility-side stage count reaches this surface. */
const NO_ELIGIBILITY_PROJECTION =
  'AdoptionEligibilityObservedV1 records eligible, enabled, and available per capability, but no ' +
  'landed read route projects them and the diagnostics projector counts records without opening ' +
  'the envelope';

/** Why no outcome-side stage count reaches this surface. */
const NO_OUTCOME_PROJECTION =
  'AdoptionOutcomeLinkedV1 records invoked, terminal, independently_useful, repeat_useful, ' +
  'censored, and unknown, but no landed read route projects them and the diagnostics projector ' +
  'counts records without opening the envelope';

/** Why correct abstention is not read off any landed route. */
const NO_ABSTENTION_PROJECTION =
  'a correct abstention needs the abstention and the independently observed absence of a right ' +
  'answer; RetrievalSynthesisObservedV1 records the abstention flag and ContextOutcomeObservedV1 ' +
  'records the outcome vocabulary, but no landed read route joins or projects them';

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
 * `readings` lets a caller supply a stage count if a future read route
 * publishes one; today nothing does, and the default is the whole funnel
 * unavailable. The parameter exists so that wiring a real projection changes a
 * call site rather than this file's structure.
 */
export function funnelDimensions(): PlanDimension[] {
  return ADOPTION_FUNNEL_STAGES.map((stage, index) => ({
    id: `funnel_${stage.toLowerCase()}`,
    label: `${index + 1}. ${stage}`,
    requirement: `${stage} count with its explicit denominator, unknown/censored counts, and interval — from ${STAGE_SOURCE[stage].field}`,
    reading: { kind: 'unpublished', reason: STAGE_SOURCE[stage].reason },
  }));
}

/** The stage counts as the consistency check sees them. Every stage is `null`
 * today; the shape is what a projection would fill. */
export function funnelStageCounts(): FunnelStageCount[] {
  return ADOPTION_FUNNEL_STAGES.map((stage) => ({ stage, count: null }));
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
export function outcomeQualityDimensions(): PlanDimension[] {
  return [
    {
      id: 'correct_abstention',
      label: 'correct abstention',
      requirement:
        'abstentions that were right to abstain, judged against an independently observed absence of a correct answer',
      reading: { kind: 'unpublished', reason: NO_ABSTENTION_PROJECTION },
    },
    {
      id: 'independently_useful',
      label: 'independently useful',
      requirement:
        'use whose usefulness was observed independently of the surface that produced it — never acceptance or a self-report',
      reading: { kind: 'unpublished', reason: NO_OUTCOME_PROJECTION },
    },
    {
      id: 'repeat_useful',
      label: 'retained use',
      requirement: 'independently useful outcomes that recurred, over the same eligible population',
      reading: { kind: 'unpublished', reason: NO_OUTCOME_PROJECTION },
    },
    {
      id: 'censored_outcomes',
      label: 'censored outcomes',
      requirement: 'invoked units whose terminal outcome was censored, kept out of every numerator',
      reading: { kind: 'unpublished', reason: NO_OUTCOME_PROJECTION },
    },
    {
      id: 'unknown_outcomes',
      label: 'unknown outcomes',
      requirement: 'invoked units with no observed terminal outcome, kept separate from censored',
      reading: { kind: 'unpublished', reason: NO_OUTCOME_PROJECTION },
    },
  ];
}

export function adoptionOutcomeBands(): OutcomeBand[] {
  return [
    { marker: 'funnel', label: 'Outcome funnel', dimensions: funnelDimensions() },
    {
      marker: 'quality',
      label: 'Abstention, independent usefulness, and retained use',
      dimensions: outcomeQualityDimensions(),
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
