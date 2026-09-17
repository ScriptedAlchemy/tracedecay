import { useMemo } from 'react';
import { useSearchParams } from 'react-router';
import type { WorkGraphReadV1 } from '../../contracts/index.ts';
import { useFeedbackProximity } from '../../viz/proximity/index.ts';
import { useWorkEvidence } from './workEvidenceQueries.ts';
import type { WorkResult } from './workApi.ts';
import {
  concurrentAttemptsReading,
} from './workConcurrentAttempts.ts';
import {
  graphRuntimeAttempts,
  type WorkGraphReading,
} from './workGraphModel.ts';
import { useWorkExecutionHistory } from './workViewsQueries.ts';
import {
  WORK_PROJECTIONS,
  type WorkProjectionKind,
} from './views/WorkProjectionSwitcher.tsx';

export function useWorkConcurrentAttempts(
  graph: WorkResult<WorkGraphReadV1> | undefined,
  graphReading: WorkGraphReading,
  selectedTaskId: string | null,
  requestedProjection: WorkProjectionKind,
) {
  const [params, setParams] = useSearchParams();
  const observedAttempts = useMemo(
    () =>
      selectedTaskId === null
        ? 0
        : graphRuntimeAttempts(graphReading).filter(
            (attempt) => attempt.identity.task_id === selectedTaskId,
          ).length,
    [graphReading, selectedTaskId],
  );
  const visible = observedAttempts > 1;
  const projections = useMemo(
    () =>
      WORK_PROJECTIONS.filter(
        (candidate) => candidate !== 'concurrent-attempts' || visible,
      ),
    [visible],
  );
  const active =
    requestedProjection === 'concurrent-attempts' && !visible
      ? 'board'
      : requestedProjection;
  const enabled = active === 'concurrent-attempts';
  const proximity = useFeedbackProximity(enabled);
  const executionHistory = useWorkExecutionHistory(enabled);
  const workEvidence = useWorkEvidence(
    graph,
    selectedTaskId,
    enabled ? { kind: 'current' } : null,
    null,
  );
  const reading = useMemo(
    () =>
      concurrentAttemptsReading(
        proximity.data,
        executionHistory.data,
        workEvidence.data,
      ),
    [executionHistory.data, proximity.data, workEvidence.data],
  );
  const selectEncounter = (encounterId: string | null) => {
    const next = new URLSearchParams(params);
    if (encounterId === null) next.delete('attemptEncounter');
    else next.set('attemptEncounter', encounterId);
    setParams(next, { replace: true });
  };
  return {
    active,
    projections,
    proximity: proximity.data,
    reading,
    selectedEncounterId: params.get('attemptEncounter'),
    selectEncounter,
  };
}
