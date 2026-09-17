import type {
  FeedbackProximityEncounterV1,
  FeedbackProximityParticipantV1,
  FeedbackProximityReadResultV1,
  ObservationSourceIdentityV1,
  WorkAttemptIdentityV1,
  WorkEvidenceRetrievalV1,
  WorkExecutionHistoryV1,
  WorkExecutionSpanV1,
} from '../../contracts/index.ts';
import type { WorkResult } from './workApi.ts';

export interface ConcurrentAttemptRow {
  readonly encounter: FeedbackProximityEncounterV1;
  readonly left: JoinedAttempt;
  readonly right: JoinedAttempt;
}

export interface JoinedAttempt {
  readonly identity: WorkAttemptIdentityV1;
  readonly participant: FeedbackProximityParticipantV1;
  readonly span: WorkExecutionSpanV1;
}

export type ConcurrentAttemptsReading =
  | { readonly state: 'complete_zero'; readonly rows: readonly [] }
  | { readonly state: 'ready'; readonly rows: readonly ConcurrentAttemptRow[] }
  | {
      readonly state: 'partial';
      readonly rows: readonly ConcurrentAttemptRow[];
      readonly missingJoins: number;
    }
  | { readonly state: 'unavailable'; readonly detail: string };

export function concurrentAttemptsReading(
  proximity: WorkResult<FeedbackProximityReadResultV1> | undefined,
  history: WorkResult<WorkExecutionHistoryV1> | undefined,
  evidence: WorkResult<WorkEvidenceRetrievalV1> | undefined,
): ConcurrentAttemptsReading {
  if (proximity === undefined || history === undefined || evidence === undefined) {
    return { state: 'unavailable', detail: 'concurrent-attempt authorities are loading' };
  }
  if (proximity.outcome === 'refused') {
    return { state: 'unavailable', detail: proximity.detail };
  }
  if (history.outcome === 'refused') {
    return { state: 'unavailable', detail: history.detail };
  }
  if (evidence.outcome === 'refused') {
    return { state: 'unavailable', detail: evidence.detail };
  }
  const proximityValue = proximity.value;
  const historyValue = history.value;
  const evidenceValue = evidence.value;
  const encounters = proximityEncounters(proximityValue);
  if (encounters === null) {
    return {
      state: 'unavailable',
      detail:
        proximityValue.state === 'denied'
          ? 'participant disclosure was denied'
          : 'proximity evidence is unavailable',
    };
  }
  if (historyValue.state === 'absent') {
    return encounters.length === 0
      ? { state: 'complete_zero', rows: [] }
      : { state: 'partial', rows: [], missingJoins: encounters.length * 2 };
  }
  const receipts = attemptReceipts(evidenceValue);
  const spans = new Map(
    historyValue.spans.map((span) => [attemptKey(span.identity), span]),
  );
  const rows: ConcurrentAttemptRow[] = [];
  let missingJoins = 0;
  for (const encounter of encounters) {
    const joined = joinConcurrentEncounter(encounter, receipts, spans);
    if (joined.row === null) {
      missingJoins += joined.missingJoins;
      continue;
    }
    rows.push(joined.row);
  }
  const sourcePartial =
    proximityValue.state === 'partial' ||
    proximityValue.state === 'stale' ||
    historyValue.timing_coverage.coverage === 'partial' ||
    evidenceValue.coverage.state !== 'complete';
  if (missingJoins > 0 || sourcePartial) {
    return { state: 'partial', rows, missingJoins };
  }
  return rows.length === 0
    ? { state: 'complete_zero', rows: [] }
    : { state: 'ready', rows };
}

export function joinConcurrentEncounter(
  encounter: FeedbackProximityEncounterV1,
  receipts: ReadonlyMap<string, WorkAttemptIdentityV1>,
  spans: ReadonlyMap<string, WorkExecutionSpanV1>,
): { readonly row: ConcurrentAttemptRow | null; readonly missingJoins: number } {
  const join = (participant: FeedbackProximityParticipantV1 | undefined) => {
    if (participant === undefined) return null;
    const identity = receipts.get(sourceKey(participant.source));
    const span = identity === undefined ? undefined : spans.get(attemptKey(identity));
    return identity === undefined || span === undefined
      ? null
      : { identity, participant, span };
  };
  const left = join(encounter.participants[0]);
  const right = join(encounter.participants[1]);
  return left === null || right === null
    ? {
        row: null,
        missingJoins: Number(left === null) + Number(right === null),
      }
    : { row: { encounter, left, right }, missingJoins: 0 };
}

function proximityEncounters(
  result: FeedbackProximityReadResultV1,
): readonly FeedbackProximityEncounterV1[] | null {
  switch (result.state) {
    case 'complete':
    case 'complete_zero':
    case 'partial':
    case 'stale':
      return result.page.encounters;
    case 'denied':
    case 'unavailable':
      return null;
    default: {
      const unhandled: never = result;
      return unhandled;
    }
  }
}

function attemptReceipts(
  evidence: WorkEvidenceRetrievalV1,
): ReadonlyMap<string, WorkAttemptIdentityV1> {
  const receipts = new Map<string, WorkAttemptIdentityV1>();
  for (const source of evidence.sources) {
    switch (source.kind) {
      case 'attempt_receipt': {
        const providerSession = source.receipt.evidence?.provider_session;
        if (providerSession !== undefined && providerSession !== null) {
          receipts.set(sourceKey(providerSession), source.receipt.identity);
        }
        break;
      }
      case 'anchor':
      case 'task_session':
        break;
      default: {
        const unhandled: never = source;
        return unhandled;
      }
    }
  }
  return receipts;
}

function sourceKey(source: ObservationSourceIdentityV1): string {
  return JSON.stringify([source.provider, source.session_id]);
}

function attemptKey(identity: WorkAttemptIdentityV1): string {
  return JSON.stringify([identity.task_id, identity.run_id, identity.attempt_id]);
}
