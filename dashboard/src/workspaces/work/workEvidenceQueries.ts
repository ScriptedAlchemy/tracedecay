import { skipToken, useQuery } from '@tanstack/react-query';
import type {
  TemporalModeV1,
  WorkEvidenceContinuationV1,
  WorkEvidenceRetrieveRequestV1,
  WorkGraphReadV1,
} from '../../contracts/index.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { workQueryKey } from '../../data/query/work.ts';
import { callWork, type WorkResult } from './workApi.ts';
import { WORK_RETRIEVE_EVIDENCE_ROUTE } from './workRoutes.ts';

export const WORK_EVIDENCE_PAGE_SIZE = 25;
export type WorkEvidenceTemporalKind = TemporalModeV1['kind'];

/** Translate the browser's explicitly UTC-labelled cutoff into the temporal
 * kernel's microsecond clock. Empty and invalid as-of values stay absent; the
 * browser must not fabricate a cutoff on the operator's behalf. */
export function workEvidenceTemporalMode(
  kind: WorkEvidenceTemporalKind,
  cutoffUtc: string,
): TemporalModeV1 | undefined {
  switch (kind) {
    case 'current':
    case 'evolution':
    case 'forensic':
      return { kind };
    case 'as_of': {
      const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,3}))?)?$/.exec(
        cutoffUtc,
      );
      if (match === null) return undefined;
      const [, yearText, monthText, dayText, hourText, minuteText, secondText, millisText] =
        match;
      const year = Number(yearText);
      const month = Number(monthText) - 1;
      const day = Number(dayText);
      const hour = Number(hourText);
      const minute = Number(minuteText);
      const second = Number(secondText ?? '0');
      const millis = Number((millisText ?? '').padEnd(3, '0'));
      const cutoff = new Date(0);
      cutoff.setUTCHours(0, 0, 0, 0);
      cutoff.setUTCFullYear(year, month, day);
      cutoff.setUTCHours(hour, minute, second, millis);
      if (
        cutoff.getUTCFullYear() !== year ||
        cutoff.getUTCMonth() !== month ||
        cutoff.getUTCDate() !== day ||
        cutoff.getUTCHours() !== hour ||
        cutoff.getUTCMinutes() !== minute ||
        cutoff.getUTCSeconds() !== second ||
        cutoff.getUTCMilliseconds() !== millis
      ) {
        return undefined;
      }
      const cutoffMicros = cutoff.getTime() * 1_000;
      return Number.isSafeInteger(cutoffMicros)
        ? { kind: 'as_of', cutoff: cutoffMicros }
        : undefined;
    }
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

/** The identity that separates evidence caches and continuation state. */
export function workEvidenceAuthorityKey(
  graph: WorkResult<WorkGraphReadV1> | undefined,
  taskId: string | null,
  temporal: TemporalModeV1 | null,
): string | null {
  const request = workEvidenceRequest(graph, taskId, temporal);
  if (request === undefined) return null;
  return JSON.stringify({
    selection: request.selection,
    task_id: request.task_id,
    temporal: request.temporal,
    verified_version: request.verified_version,
  });
}

function expansionFor(
  continuation: WorkEvidenceContinuationV1 | null,
): WorkEvidenceRetrieveRequestV1['expansion'] {
  if (continuation === null) return null;
  switch (continuation.kind) {
    case 'anchor':
      return { kind: 'anchor', link_id: continuation.link_id };
    case 'task_session':
      return { kind: 'task_session', attempt: continuation.continuation.attempt };
    default: {
      const unhandled: never = continuation;
      return unhandled;
    }
  }
}

/**
 * Bind an evidence request to the exact current Work graph response. The
 * browser never reconstructs a graph identity, accepted attempt, or provider
 * session. A continuation is paired with its owning expansion relation because
 * the backend rejects a free-floating cursor even when its bytes are valid.
 */
export function workEvidenceRequest(
  graph: WorkResult<WorkGraphReadV1> | undefined,
  taskId: string | null,
  temporal: TemporalModeV1 | null,
  continuation: WorkEvidenceContinuationV1 | null = null,
  observedAt: number = Date.now() * 1_000,
): WorkEvidenceRetrieveRequestV1 | undefined {
  if (
    taskId === null ||
    temporal === null ||
    graph?.outcome !== 'value' ||
    graph.value.mode !== 'current'
  ) {
    return undefined;
  }
  return {
    selection: graph.value.authorized_scope.selection,
    task_id: taskId,
    verified_version: graph.value.snapshot.verified_version,
    temporal,
    page_size: WORK_EVIDENCE_PAGE_SIZE,
    expansion: expansionFor(continuation),
    continuation,
    observed_at: observedAt,
  };
}

export function useWorkEvidence(
  graph: WorkResult<WorkGraphReadV1> | undefined,
  taskId: string | null,
  temporal: TemporalModeV1 | null,
  continuation: WorkEvidenceContinuationV1 | null,
) {
  const scope = useScope((state) => state.scope);
  const request = workEvidenceRequest(graph, taskId, temporal, continuation);
  const authorityKey = workEvidenceAuthorityKey(graph, taskId, temporal);
  return useQuery({
    queryKey: workQueryKey(
      scopeKey(scope),
      'retrieve-evidence',
      authorityKey,
      continuation === null ? null : JSON.stringify(continuation),
    ),
    queryFn:
      request === undefined
        ? skipToken
        : () =>
            callWork(
              WORK_RETRIEVE_EVIDENCE_ROUTE,
              request,
              scopedUrl(scope, WORK_RETRIEVE_EVIDENCE_ROUTE.path),
            ),
  });
}
