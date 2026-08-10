import type {
  WorkAttemptListCoverageV1,
  WorkAttemptListV1,
  WorkAttemptStateV1,
  WorkAttemptTopologyBindingV1,
  WorkAttemptV1,
  WorkRestartReasonV1,
  WorkTerminalEvidenceV1,
} from '../../contracts/index.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkResult } from './workApi.ts';

/**
 * One page of the attempt list, read into the facts the Work projections need.
 *
 * The retired snapshot projection exposed only a run id and a terminal flag.
 * That was not an execution record: a run is not an executor, a second row was
 * not provably a retry, and a reference was not an event. The attempt list is
 * the execution record, so everything this module derives is read off
 * `WorkAttemptV1` rather than inferred from incidence.
 *
 * Three readings come out of it, and they are exactly the three the timeline
 * previously drew as named absences:
 *
 *   executors   who actually ran the attempt (`actual_route`), against who was
 *               asked (`requested_route`) — a fallback that took over is a
 *               divergence between the two and is counted as one
 *   lineages    the retry chain, followed through `recovery.source_attempt_id`
 *               rather than counted from repeated references
 *   ladder      the typed cancellation rungs, including attempts whose recorded
 *               state claims a cancellation their cancellation record does not
 *
 * What does NOT come out of it is a span. `WorkLeaseFenceV1` is
 * `{epoch, lease_id}` and `WorkAttemptProgressV1` is `{completed, total}`;
 * neither is a clock. A terminated attempt carries `terminal.observed_at`, so
 * this build can state the order things were observed to finish in — and still
 * cannot state how long any of them took, because nothing records a start.
 * `terminalOrder` is that first fact; the wall-clock absence survives.
 *
 * Paging is a truth boundary, not a detail. Under `capped` coverage every count
 * here is a floor: a lineage whose root sits on an earlier page is marked
 * `truncated`, and `partial` says the page itself was capped, so no caller can
 * read a windowed tally as a total.
 */

/** What the attempt list said, or the reason it said nothing. */
export type WorkAttemptReading =
  | { readonly state: 'pending' }
  | { readonly state: 'refused'; readonly chip: DomainStateKind; readonly detail: string }
  /** The daemon's own `absent`: no Work in scope. Policy makes this
   * indistinguishable from a denial, and it is reported as the one typed state
   * it arrived as rather than resolved into a guess. */
  | { readonly state: 'absent' }
  | { readonly state: 'listed'; readonly page: WorkAttemptPage };

export interface WorkAttemptPage {
  readonly topology: WorkAttemptTopologyBindingV1;
  readonly coverage: WorkAttemptListCoverageV1;
  readonly attemptCount: number;
  /** The page was capped, so every count derived from it is a floor. */
  readonly partial: boolean;
  readonly executors: readonly WorkExecutorReading[];
  readonly lineages: readonly WorkAttemptLineage[];
  readonly ladder: WorkCancellationLadder;
  readonly terminalOrder: readonly WorkTerminalObservation[];
}

/**
 * One provider route, and how the attempts on it got there.
 *
 * Rows are keyed by the route that actually ran — `actual_route` when the
 * daemon observed one, and the requested route when it has not. Those two cases
 * are counted apart (`diverted`, `unobserved`) so an attributed row can never
 * hide how much of its attribution is observation and how much is the request.
 */
export interface WorkExecutorReading {
  readonly providerId: string;
  readonly routeId: string;
  readonly attempts: number;
  /** Attempts requested on another route that actually ran here — the fallback
   * topology took over. */
  readonly diverted: number;
  /** Attempts requested here whose actual route this read has not observed. */
  readonly unobserved: number;
}

export type WorkAttemptOrigin = 'fresh' | 'restarted' | 'resumed' | 'recovery_required';

export type WorkTerminalOutcome = WorkTerminalEvidenceV1['outcome'];

export interface WorkAttemptOutcomeReading {
  readonly outcome: WorkTerminalOutcome;
  readonly observedAt: number;
}

export interface WorkAttemptLink {
  readonly attemptId: string;
  readonly state: WorkAttemptStateV1;
  readonly origin: WorkAttemptOrigin;
  readonly sourceAttemptId: string | null;
  readonly reason: WorkRestartReasonV1 | null;
  readonly outcome: WorkAttemptOutcomeReading | null;
}

/** Every attempt one run made at one task, in the order they descend from each
 * other. */
export interface WorkAttemptLineage {
  readonly taskId: string;
  readonly runId: string;
  readonly links: readonly WorkAttemptLink[];
  /** Attempts after the first. A floor when `truncated`. */
  readonly restarts: number;
  /** The latest attempt carries no terminal evidence. */
  readonly open: boolean;
  /** A link descends from an attempt this page does not carry, so the chain
   * began before the window. */
  readonly truncated: boolean;
}

export interface WorkCancellationLadder {
  readonly requested: number;
  readonly acknowledged: number;
  readonly escalated: number;
  /** Attempts whose recorded state is a cancellation rung while their
   * cancellation record is `none`. The two disagree; neither is preferred. */
  readonly unrecorded: number;
}

export interface WorkTerminalObservation {
  readonly attemptId: string;
  readonly taskId: string;
  readonly runId: string;
  readonly outcome: WorkTerminalOutcome;
  readonly observedAt: number;
}

/** Where an attempt came from, and why, read off the recovery record. */
function originOf(attempt: WorkAttemptV1): {
  origin: WorkAttemptOrigin;
  source: string | null;
  reason: WorkRestartReasonV1 | null;
} {
  const recovery = attempt.recovery;
  switch (recovery.state) {
    case 'fresh':
      return { origin: 'fresh', source: null, reason: null };
    case 'recovery_required':
      return {
        origin: 'recovery_required',
        source: recovery.source_attempt_id,
        reason: recovery.reason,
      };
    case 'restarted':
      return { origin: 'restarted', source: recovery.source_attempt_id, reason: recovery.reason };
    case 'resumed':
      return { origin: 'resumed', source: recovery.source_attempt_id, reason: null };
    default: {
      const unhandled: never = recovery;
      return unhandled;
    }
  }
}

function outcomeOf(terminal: WorkTerminalEvidenceV1 | null): WorkAttemptOutcomeReading | null {
  if (terminal === null) return null;
  return { outcome: terminal.outcome, observedAt: terminal.observed_at };
}

/** Whether a recorded attempt state is itself a claim about cancellation. */
function claimsCancellation(state: WorkAttemptStateV1): boolean {
  switch (state) {
    case 'cancellation_requested':
    case 'cancellation_acknowledged':
    case 'cancellation_escalated':
    case 'cancelled':
      return true;
    case 'failed':
    case 'leased':
    case 'recovery_required':
    case 'running':
    case 'succeeded':
    case 'timed_out':
      return false;
    default: {
      const unhandled: never = state;
      return unhandled;
    }
  }
}

function executorReadings(attempts: readonly WorkAttemptV1[]): WorkExecutorReading[] {
  const rows = new Map<
    string,
    { providerId: string; routeId: string; attempts: number; diverted: number; unobserved: number }
  >();

  for (const attempt of attempts) {
    const requested = attempt.requested_route;
    const actual = attempt.actual_route;
    const effective = actual ?? requested;
    const key = `${effective.provider_id}\u0000${effective.route_id}`;
    const row = rows.get(key) ?? {
      providerId: effective.provider_id,
      routeId: effective.route_id,
      attempts: 0,
      diverted: 0,
      unobserved: 0,
    };
    rows.set(key, row);
    row.attempts += 1;
    if (actual === null) row.unobserved += 1;
    else if (
      actual.provider_id !== requested.provider_id ||
      actual.route_id !== requested.route_id
    ) {
      row.diverted += 1;
    }
  }

  return [...rows.values()].sort(
    (a, b) =>
      b.attempts - a.attempts ||
      a.providerId.localeCompare(b.providerId) ||
      a.routeId.localeCompare(b.routeId),
  );
}

/**
 * The retry chains, followed rather than counted.
 *
 * Attempts are grouped by the (task, run) they belong to and then ordered by
 * descent: each root is an attempt that is `fresh` or whose source is off this
 * page, and the chain walks forward through the attempts that name it. An
 * attempt whose source is missing marks the lineage `truncated`, because the
 * chain provably started earlier than the window. Attempts left unreached — a
 * source cycle the store should never produce — are appended in the stable
 * order they arrived in rather than dropped.
 */
function lineageReadings(attempts: readonly WorkAttemptV1[]): WorkAttemptLineage[] {
  const groups = new Map<string, WorkAttemptV1[]>();
  for (const attempt of attempts) {
    const key = `${attempt.identity.task_id}\u0000${attempt.identity.run_id}`;
    const group = groups.get(key) ?? [];
    groups.set(key, group);
    group.push(attempt);
  }

  const lineages: WorkAttemptLineage[] = [];
  for (const group of groups.values()) {
    const present = new Set(group.map((attempt) => attempt.identity.attempt_id));
    const successor = new Map<string, WorkAttemptV1>();
    const roots: WorkAttemptV1[] = [];
    let truncated = false;

    for (const attempt of group) {
      const { source } = originOf(attempt);
      if (source === null) roots.push(attempt);
      else if (present.has(source)) successor.set(source, attempt);
      else {
        roots.push(attempt);
        truncated = true;
      }
    }

    const ordered: WorkAttemptV1[] = [];
    const walked = new Set<string>();
    roots.sort((a, b) => a.identity.attempt_id.localeCompare(b.identity.attempt_id));
    for (const root of roots) {
      let cursor: WorkAttemptV1 | undefined = root;
      while (cursor !== undefined && !walked.has(cursor.identity.attempt_id)) {
        walked.add(cursor.identity.attempt_id);
        ordered.push(cursor);
        cursor = successor.get(cursor.identity.attempt_id);
      }
    }
    for (const attempt of group) {
      if (walked.has(attempt.identity.attempt_id)) continue;
      walked.add(attempt.identity.attempt_id);
      ordered.push(attempt);
    }

    const first = ordered[0];
    const last = ordered[ordered.length - 1];
    if (first === undefined || last === undefined) continue;

    lineages.push({
      taskId: first.identity.task_id,
      runId: first.identity.run_id,
      links: ordered.map((attempt) => {
        const { origin, source, reason } = originOf(attempt);
        return {
          attemptId: attempt.identity.attempt_id,
          state: attempt.state,
          origin,
          sourceAttemptId: source,
          reason,
          outcome: outcomeOf(attempt.terminal),
        };
      }),
      restarts: ordered.length - 1,
      open: last.terminal === null,
      truncated,
    });
  }

  return lineages.sort(
    (a, b) =>
      b.restarts - a.restarts || a.taskId.localeCompare(b.taskId) || a.runId.localeCompare(b.runId),
  );
}

function ladderReading(attempts: readonly WorkAttemptV1[]): WorkCancellationLadder {
  let requested = 0;
  let acknowledged = 0;
  let escalated = 0;
  let unrecorded = 0;

  for (const attempt of attempts) {
    const cancellation = attempt.cancellation;
    switch (cancellation.state) {
      case 'none':
        if (claimsCancellation(attempt.state)) unrecorded += 1;
        break;
      case 'requested':
        requested += 1;
        break;
      case 'acknowledged':
        acknowledged += 1;
        break;
      case 'escalated':
        escalated += 1;
        break;
      default: {
        const unhandled: never = cancellation;
        return unhandled;
      }
    }
  }

  return { requested, acknowledged, escalated, unrecorded };
}

/** Attempts that reached a terminal, in the order the daemon observed them
 * reach it. The one ordering on this page that is a measurement rather than a
 * collation of identifiers. */
function terminalOrder(attempts: readonly WorkAttemptV1[]): WorkTerminalObservation[] {
  const observed: WorkTerminalObservation[] = [];
  for (const attempt of attempts) {
    const outcome = outcomeOf(attempt.terminal);
    if (outcome === null) continue;
    observed.push({
      attemptId: attempt.identity.attempt_id,
      taskId: attempt.identity.task_id,
      runId: attempt.identity.run_id,
      outcome: outcome.outcome,
      observedAt: outcome.observedAt,
    });
  }
  return observed.sort(
    (a, b) => a.observedAt - b.observedAt || a.attemptId.localeCompare(b.attemptId),
  );
}

function attemptPage(list: Extract<WorkAttemptListV1, { state: 'listed' }>): WorkAttemptPage {
  return {
    topology: list.topology,
    coverage: list.coverage,
    attemptCount: list.attempts.length,
    partial: list.coverage.coverage === 'capped',
    executors: executorReadings(list.attempts),
    lineages: lineageReadings(list.attempts),
    ladder: ladderReading(list.attempts),
    terminalOrder: terminalOrder(list.attempts),
  };
}

/**
 * The attempt list as a reading.
 *
 * `undefined` is the request still being in flight, which is distinct from
 * every answer the daemon can give — including `absent`, which is an answer.
 */
export function workAttemptReading(
  result: WorkResult<WorkAttemptListV1> | undefined,
): WorkAttemptReading {
  if (result === undefined) return { state: 'pending' };
  if (result.outcome === 'refused') {
    return { state: 'refused', chip: result.state, detail: result.detail };
  }
  const list = result.value;
  switch (list.state) {
    case 'absent':
      return { state: 'absent' };
    case 'listed':
      return { state: 'listed', page: attemptPage(list) };
    default: {
      const unhandled: never = list;
      return unhandled;
    }
  }
}

/** The page a reading carries, or `null` for the three states that carry none.
 * Callers that only need the facts use this instead of restating the switch. */
export function attemptPageOf(reading: WorkAttemptReading): WorkAttemptPage | null {
  return reading.state === 'listed' ? reading.page : null;
}
