/**
 * Observed observability-family record counts, and the Plan 26 floors that
 * decide whether one may be printed.
 *
 * WHAT IS ON THE WIRE
 *
 * Every canonical observation is written to `analytics_events` with its
 * `event_kind` set to the family identifier (`adoption.eligibility_observed.v1`,
 * `retrieval.retriever.completed.v1`, …) and the whole versioned envelope kept
 * as JSON. `GET /api/plugins/analytics/diagnostics` groups that table by
 * `event_kind` and publishes the per-kind row counts as `by_event_kind`. So the
 * *count of records in a family* is landed and readable. Nothing inside the
 * envelope is — the diagnostics projector never opens `metadata_json`, so the
 * eligible/enabled/available denominators on `AdoptionEligibilityObservedV1`,
 * the invoked/terminal/useful counts on `AdoptionOutcomeLinkedV1`, and every
 * retrieval budget, rank, and contribution figure stay unread.
 *
 * That is why this module deliberately produces something weaker than a
 * `MetricValueV1`. A record count has no descriptor revision, no eligible
 * denominator, no interval, and no projector attribution, and manufacturing
 * those fields in the browser so a count could be rendered as a canonical
 * measurement is exactly the fabrication Plan 26's truthful-aggregation section
 * forbids. Counts render as counts, in their own frame, saying what they lack.
 *
 * THE FLOORS
 *
 * Plan 26 §"Truthful aggregation": "Local cells below five eligible units are
 * suppressed. Rates require 20 eligible units and 90% coverage." Both are
 * applied here rather than at the point of drawing, because a floor that lives
 * in a component is a floor one component can forget.
 *
 * Suppression is applied against the *observed* count, which is the
 * conservative direction: a family with three observed rows cannot be shown to
 * have five eligible units, so it is withheld. A family absent from the window
 * is worse than three — under a capped or partial read it cannot even be told
 * apart from a family whose rows fell outside the window, so it is reported as
 * censored by the window rather than as zero.
 *
 * NOTHING HERE EVER RETURNS 0 AS A READING OF ABSENCE. Zero rows in a complete
 * window is a genuine observation and it is still under the suppression floor,
 * so it too renders withheld. The only number this module emits is a count of
 * five or more that the daemon actually reported.
 */
import type { AnalyticsEventKindCountV1 } from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import { NOT_PUBLISHED, NO_FIGURE } from './planDimension.ts';

/** Plan 26: "Local cells below five eligible units are suppressed." */
export const SUPPRESSION_FLOOR = 5;

/** Plan 26: "Rates require 20 eligible units and 90% coverage." */
export const RATE_MIN_ELIGIBLE = 20;

/** The coverage share a rate requires, as a fraction. */
export const RATE_MIN_COVERAGE = 0.9;

/**
 * The row ceiling the diagnostics projector reads under. Stated as a constant
 * because a reader is owed the size of the window a "capped" answer was capped
 * at; `ANALYTICS_EVENT_LIMIT` in `analytics_api.rs` is the same number.
 */
export const DIAGNOSTICS_WINDOW_ROWS = 10_000;

/**
 * What the wire had to say about one observation family.
 *
 * `suppressed` and `censored` are not the same absence and are never merged.
 * Suppressed means the daemon answered and the answer is below a floor that
 * exists to stop small cells identifying anyone. Censored means the window the
 * daemon read could not have contained the answer either way.
 */
export type FamilyReading =
  | { kind: 'observed'; count: number }
  | { kind: 'suppressed'; floor: number; reason: string }
  | { kind: 'censored'; reason: string }
  | { kind: 'unreadable'; reason: string };

/** What a diagnostics read said about its own completeness, carried forward so
 * an absent family can be read against it. */
export interface WindowTruth {
  /** The daemon's own completeness word, printed verbatim and never softened. */
  completeness: string;
  /** True only for `complete`. Any other word leaves absence unprovable. */
  complete: boolean;
  /** Whether the diagnostics payload reported a store at all. */
  available: boolean;
  /** The projector's own attribution for the rows it counted. */
  source: string;
}

export function windowTruth(
  completeness: string,
  available: boolean,
  source: string,
): WindowTruth {
  return { completeness, complete: completeness === 'complete', available, source };
}

/**
 * Resolve one canonical family identifier against a diagnostics read.
 *
 * The order of the ladder is the order the failures nest in: an unreadable
 * store cannot report a window, a window that is not complete cannot prove an
 * absence, and an absence that *is* proven is still a cell below the floor.
 */
export function readFamily(
  counts: readonly AnalyticsEventKindCountV1[],
  eventKind: string,
  window: WindowTruth,
): FamilyReading {
  if (!window.available) {
    return {
      kind: 'unreadable',
      reason: `the analytics diagnostics read reported no event store (source ${window.source})`,
    };
  }
  const entry = counts.find((candidate) => candidate.event_kind === eventKind);
  if (entry === undefined) {
    if (!window.complete) {
      return {
        kind: 'censored',
        reason:
          `no row of this family appears in a ${window.completeness} window bounded at ` +
          `${DIAGNOSTICS_WINDOW_ROWS.toLocaleString()} rows, which cannot tell a family that ` +
          'produced nothing apart from one whose rows fell outside the window',
      };
    }
    return {
      kind: 'suppressed',
      floor: SUPPRESSION_FLOOR,
      reason:
        'the window is complete and contains no row of this family; zero is below the ' +
        `${SUPPRESSION_FLOOR}-unit local suppression floor, so no cell is published`,
    };
  }
  if (entry.count < SUPPRESSION_FLOOR) {
    return {
      kind: 'suppressed',
      floor: SUPPRESSION_FLOOR,
      reason: `fewer than ${SUPPRESSION_FLOOR} units observed, below the local suppression floor`,
    };
  }
  return { kind: 'observed', count: entry.count };
}

/** The state chip a family reading carries. A withheld cell is not an error and
 * not an outage: the daemon answered and the answer may not be shown. */
export function familyState(reading: FamilyReading): DomainStateKind {
  switch (reading.kind) {
    case 'observed':
      return 'ready';
    case 'suppressed':
      return 'redacted';
    case 'censored':
      return 'partial';
    case 'unreadable':
      return 'unavailable';
  }
}

/**
 * Eligible versus observed, and the remainder between them.
 *
 * Plan 26 §"Required product views": `adoption-coverage` shows "eligible versus
 * observed … and denominator failures". Today no landed read route projects an
 * eligible denominator for any adoption family, so every call resolves to
 * `denominator_missing` — but the arithmetic is written out rather than
 * hard-coded to that answer, because the rule that matters is what happens when
 * a denominator *does* arrive: a remainder is withheld when either total is
 * missing, an eligible population below the rate floor produces no rate at all,
 * and an observed count larger than its denominator is reported as a
 * contradiction rather than clamped to a remainder of zero.
 */
export type EligibleVersusObserved =
  | { kind: 'measured'; observed: number; eligible: number; ratio: number; remainder: number }
  | { kind: 'denominator_missing'; observed: number | null; reason: string }
  | { kind: 'observed_missing'; eligible: number; reason: string }
  | { kind: 'under_rate_floor'; observed: number; eligible: number; reason: string }
  | { kind: 'contradiction'; observed: number; eligible: number; reason: string };

export function eligibleVersusObserved(
  observed: number | null,
  eligible: number | null,
): EligibleVersusObserved {
  if (eligible == null) {
    return {
      kind: 'denominator_missing',
      observed,
      reason:
        'no landed read route publishes the eligible denominator for this population, so the ' +
        'remainder against it is withheld rather than assumed',
    };
  }
  if (observed == null) {
    return {
      kind: 'observed_missing',
      eligible,
      reason: 'the observed count is not published, so the remainder against the denominator is withheld',
    };
  }
  if (observed > eligible) {
    return {
      kind: 'contradiction',
      observed,
      eligible,
      reason:
        `${observed.toLocaleString()} observed against ${eligible.toLocaleString()} eligible is ` +
        'impossible; the remainder is reported as a contradiction rather than clamped to zero',
    };
  }
  if (eligible < RATE_MIN_ELIGIBLE) {
    return {
      kind: 'under_rate_floor',
      observed,
      eligible,
      reason:
        `a rate requires ${RATE_MIN_ELIGIBLE} eligible units and ` +
        `${Math.round(RATE_MIN_COVERAGE * 100)}% coverage; this population has ` +
        `${eligible.toLocaleString()}`,
    };
  }
  return {
    kind: 'measured',
    observed,
    eligible,
    ratio: observed / eligible,
    remainder: eligible - observed,
  };
}

/** The canonical adoption funnel, verbatim from Plan 26: `Eligible -> Enabled
 * -> Available -> Invoked -> Terminal -> IndependentlyUseful -> RepeatUseful`. */
export const ADOPTION_FUNNEL_STAGES = [
  'Eligible',
  'Enabled',
  'Available',
  'Invoked',
  'Terminal',
  'IndependentlyUseful',
  'RepeatUseful',
] as const;

export type AdoptionFunnelStage = (typeof ADOPTION_FUNNEL_STAGES)[number];

export interface FunnelStageCount {
  stage: string;
  count: number | null;
}

/**
 * Whether a set of funnel stage counts can be believed as a funnel.
 *
 * A funnel is monotone by construction — `AdoptionOutcomeLinkedV1::validate`
 * refuses a record where `terminal > invoked` or `repeat_useful >
 * independently_useful` — so a projection that produced a rising stage is
 * reporting something other than the funnel it is labelled as. That is a
 * contradiction to state, not a bar to draw shorter.
 *
 * Unmeasured stages are skipped rather than treated as zero, and comparison is
 * against the last measured stage: monotonicity holds across the whole chain,
 * so two measured stages with an unmeasured one between them are still
 * comparable.
 */
export type FunnelConsistency =
  | { kind: 'not_evaluable'; measured: number; reason: string }
  | { kind: 'consistent'; measured: number }
  | { kind: 'contradiction'; earlier: string; later: string; reason: string };

export function funnelConsistency(stages: readonly FunnelStageCount[]): FunnelConsistency {
  let previous: FunnelStageCount | null = null;
  let measured = 0;
  for (const stage of stages) {
    if (stage.count == null) continue;
    measured += 1;
    if (previous != null && previous.count != null && stage.count > previous.count) {
      return {
        kind: 'contradiction',
        earlier: previous.stage,
        later: stage.stage,
        reason:
          `${stage.stage} (${stage.count.toLocaleString()}) exceeds ${previous.stage} ` +
          `(${previous.count.toLocaleString()}); a later funnel stage cannot admit more units ` +
          'than the stage it is drawn from',
      };
    }
    previous = stage;
  }
  if (measured < 2) {
    return {
      kind: 'not_evaluable',
      measured,
      reason:
        'fewer than two stages carry a count, so no ordering between stages can be checked and ' +
        'no funnel is drawn',
    };
  }
  return { kind: 'consistent', measured };
}

/**
 * Signals Plan 26 explicitly refuses as success outcomes: "Display, click,
 * invocation, process completion, self-report, cards closed, tests run, token
 * volume, and subjective trust do not become success outcomes."
 *
 * Listed on the surface rather than merely omitted from it. A reader cannot see
 * that a dashboard declined to count clicks; they can see a sentence saying so.
 */
export const NOT_SUCCESS_OUTCOMES = [
  'display',
  'click',
  'invocation',
  'process completion',
  'self-report',
  'cards closed',
  'tests run',
  'token volume',
  'subjective trust',
] as const;

/** One family row, reduced to the strings a ledger prints. */
export interface FamilyRowPresentation {
  eventKind: string;
  label: string;
  available: boolean;
  /** The count, or an em dash. Never `0`, and never an empty cell. */
  figure: string;
  reason: string | null;
  state: DomainStateKind;
  /** Always the same today, and stated on every row rather than in a footnote:
   * a record count has no eligible denominator on the wire. */
  denominator: string;
}

export function familyRowPresentation(
  eventKind: string,
  label: string,
  reading: FamilyReading,
): FamilyRowPresentation {
  return {
    eventKind,
    label,
    available: reading.kind === 'observed',
    figure: reading.kind === 'observed' ? reading.count.toLocaleString() : NO_FIGURE,
    reason: reading.kind === 'observed' ? null : reading.reason,
    state: familyState(reading),
    denominator: NOT_PUBLISHED,
  };
}

/** How many cells this view withheld, and why the number is worth printing: a
 * ledger of nine em dashes should say whether it is looking at a silent daemon
 * or at nine cells below a privacy floor. */
export function withheldCount(rows: readonly FamilyRowPresentation[]): number {
  return rows.filter((row) => !row.available).length;
}
