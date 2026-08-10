import { useQuery } from '@tanstack/react-query';
import type {
  WorkEvidenceContinuationV1,
  WorkEvidenceRetrieveRequestV1,
  WorkGraphReadV1,
} from '../../contracts/index.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { workQueryKey } from '../../data/query/work.ts';
import { callWork, type WorkResult } from './workApi.ts';
import { WORK_RETRIEVE_EVIDENCE_ROUTE } from './workRoutes.ts';

export const WORK_EVIDENCE_PAGE_SIZE = 25;

/** The identity that separates evidence caches and continuation state. */
export function workEvidenceAuthorityKey(
  graph: WorkResult<WorkGraphReadV1> | undefined,
  taskId: string | null,
): string | null {
  if (taskId === null || graph?.outcome !== 'value' || graph.value.mode !== 'current') {
    return null;
  }
  return JSON.stringify({
    selection: graph.value.authorized_scope.selection,
    task_id: taskId,
    verified_version: graph.value.snapshot.verified_version,
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
  continuation: WorkEvidenceContinuationV1 | null = null,
  observedAt: number = Date.now() * 1_000,
): WorkEvidenceRetrieveRequestV1 | undefined {
  if (taskId === null || graph?.outcome !== 'value' || graph.value.mode !== 'current') {
    return undefined;
  }
  return {
    selection: graph.value.authorized_scope.selection,
    task_id: taskId,
    verified_version: graph.value.snapshot.verified_version,
    temporal: { kind: 'forensic' },
    page_size: WORK_EVIDENCE_PAGE_SIZE,
    expansion: expansionFor(continuation),
    continuation,
    observed_at: observedAt,
  };
}

export function useWorkEvidence(
  graph: WorkResult<WorkGraphReadV1> | undefined,
  taskId: string | null,
  continuation: WorkEvidenceContinuationV1 | null,
) {
  const scope = useScope((state) => state.scope);
  const request = workEvidenceRequest(graph, taskId, continuation);
  const authorityKey = workEvidenceAuthorityKey(graph, taskId);
  return useQuery({
    queryKey: workQueryKey(
      scopeKey(scope),
      'retrieve-evidence',
      authorityKey,
      continuation === null ? null : JSON.stringify(continuation),
    ),
    enabled: request !== undefined,
    queryFn: () =>
      callWork(
        WORK_RETRIEVE_EVIDENCE_ROUTE,
        request as WorkEvidenceRetrieveRequestV1,
        scopedUrl(scope, WORK_RETRIEVE_EVIDENCE_ROUTE.path),
      ),
  });
}
