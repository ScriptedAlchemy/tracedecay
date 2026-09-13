import type {
  WorkAttemptListV1,
  WorkAttemptV1,
  WorkEffectStateV1,
  WorkRestartReasonV1,
} from '../../contracts/index.ts';
import type { WorkChannel } from './workChannel.ts';
import type {
  WorkAccountingAnchor,
  WorkAccountingFigure,
  WorkAccountingProvenance,
  WorkAttemptPageV1,
} from './workAccountingModel.ts';

/**
 * One walk of one attempt page, and the provenance every card drawn off that
 * page shares.
 *
 * The census exists so the cards read a tallied structure instead of the
 * attempts: three cards would otherwise each re-walk the same page, and a
 * fourth reading of the same field is a fourth chance to disagree with the
 * other three. Nothing here resolves a contradiction it finds — a page whose
 * typed `state` and typed `recovery` disagree is COUNTED as a disagreement,
 * because neither field is the authority over the other and the card's job is
 * to say so rather than to pick.
 *
 * The provenance built here is the honest one for a page read: the eligible
 * denominator is unavailable when the page is capped (the page can establish
 * only a floor, not its full eligible set), the censored count is the open
 * attempts, and the horizon says outright that a page is not an interval
 * because the read carries no time window.
 */

// --- The attempt page --------------------------------------------------------

/** What the attempt page proves about reruns, effects, and its own censoring.
 * One walk of the page; the cards below read this rather than the attempts. */
export interface AttemptCensus {
  readonly attempts: number;
  /** Attempts with no terminal evidence — right-censored, not failures. */
  readonly open: number;
  readonly recovery: {
    readonly fresh: number;
    readonly restarted: number;
    readonly resumed: number;
    readonly recoveryRequired: number;
  };
  readonly restartReasons: Readonly<Record<WorkRestartReasonV1, number>>;
  readonly effects: Readonly<Record<WorkEffectStateV1, number>>;
  /** Attempts whose typed `state` and typed `recovery` disagree about whether
   * recovery is required. */
  readonly recoveryDisagreements: number;
  /** Attempts carrying terminal evidence while still typed as in flight. */
  readonly terminalWhileRunning: number;
  readonly anchors: readonly WorkAccountingAnchor[];
}

export const RESTART_REASONS: readonly WorkRestartReasonV1[] = [
  'failure_observed',
  'lease_lost',
  'process_lost',
  'provider_unavailable',
];

export const EFFECT_STATES: readonly WorkEffectStateV1[] = [
  'observational',
  'intercepted',
  'compound_non_repeatable',
];

/** How many anchors a card offers before it stops listing them. The count is
 * still exact; only the listing is bounded. */
export const ANCHOR_CAP = 8;

export function attemptCensus(attempts: readonly WorkAttemptV1[]): AttemptCensus {
  const recovery = { fresh: 0, restarted: 0, resumed: 0, recoveryRequired: 0 };
  const restartReasons: Record<WorkRestartReasonV1, number> = {
    failure_observed: 0,
    lease_lost: 0,
    process_lost: 0,
    provider_unavailable: 0,
  };
  const effects: Record<WorkEffectStateV1, number> = {
    observational: 0,
    intercepted: 0,
    compound_non_repeatable: 0,
  };
  const anchors: WorkAccountingAnchor[] = [];
  let open = 0;
  let recoveryDisagreements = 0;
  let terminalWhileRunning = 0;

  for (const attempt of attempts) {
    if (attempt.terminal === null) open += 1;
    else if (attempt.state === 'running' || attempt.state === 'leased') {
      terminalWhileRunning += 1;
    }

    switch (attempt.recovery.state) {
      case 'fresh':
        recovery.fresh += 1;
        break;
      case 'restarted':
        recovery.restarted += 1;
        restartReasons[attempt.recovery.reason] += 1;
        break;
      case 'resumed':
        recovery.resumed += 1;
        break;
      case 'recovery_required':
        recovery.recoveryRequired += 1;
        restartReasons[attempt.recovery.reason] += 1;
        break;
      default: {
        const unhandled: never = attempt.recovery;
        return unhandled;
      }
    }

    // The two typed fields that can disagree about the same fact. Counted
    // rather than resolved: neither field is the authority over the other.
    if (
      (attempt.state === 'recovery_required') !==
      (attempt.recovery.state === 'recovery_required')
    ) {
      recoveryDisagreements += 1;
    }

    effects[attempt.execution.effect_state] += 1;

    anchors.push({
      kind: 'attempt',
      id: attempt.identity.attempt_id,
      taskId: attempt.identity.task_id,
    });
  }

  return {
    attempts: attempts.length,
    open,
    recovery,
    restartReasons,
    effects,
    recoveryDisagreements,
    terminalWhileRunning,
    anchors: anchors.slice(0, ANCHOR_CAP),
  };
}

export function coverageSentence(page: WorkAttemptListV1 & { state: 'listed' }): string {
  const coverage = page.coverage;
  switch (coverage.coverage) {
    case 'complete':
      return `complete over ${coverage.returned} ${coverage.returned === 1 ? 'attempt' : 'attempts'}`;
    case 'capped':
      return `capped attempt page: ${coverage.returned} returned and ${coverage.remaining} remaining — every count below is a floor`;
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}

function eligibleFromCoverage(
  page: WorkAttemptPageV1,
): WorkChannel<WorkAccountingFigure> {
  const coverage = page.coverage;
  switch (coverage.coverage) {
    case 'complete':
      return { available: true, value: { value: coverage.returned, unit: 'attempts' } };
    case 'capped':
      return {
        available: false,
        state: 'partial',
        detail:
          'the capped attempt page establishes only returned and remaining page facts, not a full eligible denominator; treating their sum as one would derive a total the contract did not publish',
      };
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}

/** The provenance shared by every card drawn off one attempt page. */
export function attemptProvenance(
  page: WorkAttemptPageV1,
  census: AttemptCensus,
  censoringNote: string,
): WorkAccountingProvenance {
  return {
    support: {
      available: true,
      value: {
        value: census.attempts,
        unit: 'attempts',
        note:
          page.coverage.coverage === 'capped'
            ? 'count on the capped attempt page — a floor, not a total'
            : undefined,
      },
    },
    eligible: eligibleFromCoverage(page),
    censoring: {
      available: true,
      value: { censored: census.open, unknown: 0, note: censoringNote },
    },
    intervalCoverage: { available: true, value: coverageSentence(page) },
    horizon: {
      available: true,
      value: `one attempt page under topology generation ${page.topology.generation} · the read carries no time window, so this is a page and not an interval`,
    },
    descriptorRevision: {
      available: true,
      value: {
        kind: 'source_read_pin',
        value: `topology generation ${page.topology.generation} · ${page.topology.task_count} tasks`,
      },
    },
    anchors: { available: true, value: census.anchors },
  };
}
