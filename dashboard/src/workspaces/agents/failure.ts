import type { WorkAttemptStateV1, WorkGraphReadV1 } from '../../contracts/index.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkResult } from '../work/workApi.ts';
import { latestGraphEntry } from './handoff.ts';
import { type CountRow, rankedCounts } from './activity.ts';

/**
 * Failure context, from the two authorities that record it.
 *
 * The analytics diagnostics fold records how the window's events came out
 * (`by_outcome`) and carries a short tape of the latest events with their own
 * outcomes (`recent_events`). The work-product graph read carries the runtime
 * projection — every attempt the daemon could observe, with the state it is in
 * (`WorkAttemptStateV1`, whose members include `failed`, `timed_out`,
 * `cancelled` and `recovery_required`).
 *
 * The hard part is the outcome vocabulary. `outcome` is a free string on the
 * wire, so classifying it is this build's reading and not the daemon's. Two
 * rules keep that reading honest:
 *
 *   1. The two lists below are exhaustive of what this build claims to
 *      recognize. Nothing else is guessed at from substrings.
 *   2. An outcome word in neither list is UNCLASSIFIED and is reported as such.
 *      Folding an unrecognized word into "not a failure" is how a new failure
 *      mode would land on this page as a clean bill of health.
 */

/** Outcome words this build reads as a failed call. */
export const FAILED_OUTCOMES: readonly string[] = [
  'error',
  'failed',
  'failure',
  'timeout',
  'timed_out',
  'denied',
  'blocked',
  'refused',
  'rejected',
  'cancelled',
  'canceled',
];

/** Outcome words this build reads as a call that did not fail. `observed` is
 * here because hook-routing events carry it and they are not tool calls at
 * all — they are events the fold saw go past. */
export const SETTLED_OUTCOMES: readonly string[] = [
  'success',
  'succeeded',
  'ok',
  'completed',
  'complete',
  'allow',
  'allowed',
  'observed',
];

export type OutcomeClass = 'failed' | 'settled' | 'unclassified';

export function classifyOutcome(outcome: string): OutcomeClass {
  const normalized = outcome.trim().toLowerCase();
  if (FAILED_OUTCOMES.includes(normalized)) return 'failed';
  if (SETTLED_OUTCOMES.includes(normalized)) return 'settled';
  return 'unclassified';
}

export interface OutcomeReading {
  readonly failed: readonly { label: string; count: number }[];
  readonly settled: readonly { label: string; count: number }[];
  readonly unclassified: readonly { label: string; count: number }[];
  readonly failedTotal: number;
  readonly unclassifiedTotal: number;
  readonly counted: number;
}

/** `by_outcome`, split three ways. `counted` is the sum of everything the row
 * set carried — the only denominator these shares may be taken against, because
 * the window's own event count includes events this array never described. */
export function readOutcomes(rows: readonly CountRow[]): OutcomeReading {
  const ranked = rankedCounts(rows, 'outcome');
  const failed = ranked.filter((row) => classifyOutcome(row.label) === 'failed');
  const settled = ranked.filter((row) => classifyOutcome(row.label) === 'settled');
  const unclassified = ranked.filter((row) => classifyOutcome(row.label) === 'unclassified');
  const sum = (rowsIn: readonly { count: number }[]) =>
    rowsIn.reduce((total, row) => total + row.count, 0);
  return {
    failed,
    settled,
    unclassified,
    failedTotal: sum(failed),
    unclassifiedTotal: sum(unclassified),
    counted: sum(ranked),
  };
}

export interface FailedEvent {
  readonly timestamp: number;
  readonly tool: string;
  readonly kind: string;
  readonly outcome: string;
}

/**
 * The failures on the served tape, and how long the tape was.
 *
 * `served` is reported alongside them because "none of the twenty events served
 * failed" and "no tape was served" are different readings, and only the first
 * one is evidence of anything.
 */
export function failedEvents(rows: readonly CountRow[]): {
  readonly events: readonly FailedEvent[];
  readonly served: number;
} {
  const parsed = rows
    .map((row) => ({
      timestamp: Number(row['timestamp'] ?? Number.NaN),
      tool: String(row['tool_name'] ?? ''),
      kind: String(row['event_kind'] ?? ''),
      outcome: String(row['outcome'] ?? ''),
    }))
    .filter((row) => Number.isFinite(row.timestamp) && row.timestamp > 0);
  return {
    events: parsed
      .filter((row) => classifyOutcome(row.outcome) === 'failed')
      .sort((a, b) => b.timestamp - a.timestamp),
    served: parsed.length,
  };
}

/** The attempt states this build reads as an attempt that did not come out
 * clean. Taken from the generated `WorkAttemptStateV1` union rather than from
 * free text, so a new member of that union is a type error here and not a
 * silent omission on the page. */
const UNCLEAN_ATTEMPT_STATES: ReadonlySet<WorkAttemptStateV1> = new Set<WorkAttemptStateV1>([
  'failed',
  'timed_out',
  'cancelled',
  'recovery_required',
  'cancellation_requested',
  'cancellation_acknowledged',
  'cancellation_escalated',
]);

export interface AttemptFailure {
  readonly attemptId: string;
  readonly taskId: string;
  readonly runId: string;
  readonly state: WorkAttemptStateV1;
}

export type AttemptFailureReading =
  | { readonly state: 'pending' }
  | { readonly state: 'refused'; readonly chip: DomainStateKind; readonly detail: string }
  | {
      readonly state: 'read';
      readonly failures: readonly AttemptFailure[];
      readonly byState: readonly { label: string; count: number }[];
      readonly attempts: number;
      /** The runtime projection's own coverage. `unavailable` means the daemon
       * could observe NO attempt, which is not the same as observing none to
       * have failed — the surface must not print a zero for it. */
      readonly coverage: 'complete' | 'partial' | 'unavailable';
      /** Attempts the daemon named as unobservable under `partial` coverage. */
      readonly unobserved: number;
      readonly graphVersion: number;
    };

/**
 * Attempt failures, from the runtime projection on the work-product graph read.
 *
 * This is the same read the handoff frontier is derived from, deliberately: the
 * frontier and the failures on this page then describe one graph version rather
 * than two, and a reader comparing them is comparing things that coexisted.
 */
export function readAttemptFailures(
  result: WorkResult<WorkGraphReadV1> | undefined,
): AttemptFailureReading {
  if (result === undefined) return { state: 'pending' };
  if (result.outcome === 'refused') {
    return { state: 'refused', chip: result.state, detail: result.detail };
  }
  const latest = latestGraphEntry(result.value);
  if (latest === null) {
    return {
      state: 'refused',
      chip: 'unavailable',
      detail: 'the graph read answered with a timeline holding no version to read attempts from',
    };
  }
  const runtime = latest.entry.runtime;
  const failures = runtime.attempts
    .filter((attempt) => UNCLEAN_ATTEMPT_STATES.has(attempt.state))
    .map((attempt) => ({
      attemptId: attempt.identity.attempt_id,
      taskId: attempt.identity.task_id,
      runId: attempt.identity.run_id,
      state: attempt.state,
    }))
    .sort((a, b) => a.state.localeCompare(b.state) || a.taskId.localeCompare(b.taskId));
  const tally = new Map<string, number>();
  for (const failure of failures) tally.set(failure.state, (tally.get(failure.state) ?? 0) + 1);
  return {
    state: 'read',
    failures,
    byState: [...tally.entries()]
      .map(([label, count]) => ({ label, count }))
      .sort((a, b) => b.count - a.count || a.label.localeCompare(b.label)),
    attempts: runtime.attempts.length,
    coverage: runtime.coverage.coverage,
    unobserved:
      runtime.coverage.coverage === 'partial' ? runtime.coverage.unavailable_attempts.length : 0,
    graphVersion: latest.entry.graph.version,
  };
}
