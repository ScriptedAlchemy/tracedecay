import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type {
  WorkflowDefinition,
  WorkflowDefinitionDisposition,
  WorkflowRunProjection,
} from '../../contracts/generated.ts';
import { scopeKey, scopedUrl, scopeWritable, useScope } from '../../data/scope/store.ts';
import { callWork, workResultInvalidatesReads, type WorkResult } from '../work/workApi.ts';
import type { WorkflowLifecycleAction } from './workflowLedger.ts';
import {
  WORKFLOW_ACTIVATE_DEFINITION_ROUTE,
  WORKFLOW_DEFINITION_HISTORY_ROUTE,
  WORKFLOW_GET_RUN_ROUTE,
  WORKFLOW_LIST_DEFINITIONS_ROUTE,
  WORKFLOW_REJECT_DEFINITION_ROUTE,
  WORKFLOW_RETIRE_DEFINITION_ROUTE,
} from './workflowRoutes.ts';

function workflowQueryKey(scope: string, ...parts: readonly (string | number)[]) {
  return ['workflow', scope, ...parts] as const;
}

export function useWorkflowDefinitions() {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<WorkflowDefinition[]>>({
    queryKey: workflowQueryKey(key, 'list-definitions'),
    queryFn: ({ signal }) =>
      callWork(
        WORKFLOW_LIST_DEFINITIONS_ROUTE,
        {},
        scopedUrl(scope, WORKFLOW_LIST_DEFINITIONS_ROUTE.path),
        signal,
      ),
  });
}

/** The version track of one definition identity; disabled until one is
 * selected. */
export function useWorkflowDefinitionHistory(definitionId: string | null) {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<WorkflowDefinition[]>>({
    queryKey: workflowQueryKey(key, 'definition-history', definitionId ?? ''),
    enabled: definitionId !== null,
    queryFn: ({ signal }) =>
      callWork(
        WORKFLOW_DEFINITION_HISTORY_ROUTE,
        { definition_id: definitionId ?? '' },
        scopedUrl(scope, WORKFLOW_DEFINITION_HISTORY_ROUTE.path),
        signal,
      ),
  });
}

/** One run's projection, read on demand; disabled until a run id is named. */
export function useWorkflowRun(runId: string | null) {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<WorkflowRunProjection>>({
    queryKey: workflowQueryKey(key, 'get-run', runId ?? ''),
    enabled: runId !== null,
    queryFn: ({ signal }) =>
      callWork(
        WORKFLOW_GET_RUN_ROUTE,
        { run_id: runId ?? '' },
        scopedUrl(scope, WORKFLOW_GET_RUN_ROUTE.path),
        signal,
      ),
  });
}

interface WorkflowLifecycleCommand {
  readonly action: WorkflowLifecycleAction;
  readonly definitionId: string;
  readonly definitionVersion: number;
  readonly expectedRevision: number;
}

function lifecycleRoute(action: WorkflowLifecycleAction) {
  switch (action) {
    case 'activate':
      return WORKFLOW_ACTIVATE_DEFINITION_ROUTE;
    case 'retire':
      return WORKFLOW_RETIRE_DEFINITION_ROUTE;
    case 'reject':
      return WORKFLOW_REJECT_DEFINITION_ROUTE;
    default: {
      const unhandled: never = action;
      return unhandled;
    }
  }
}

/** The refusal a lifecycle command outside the writable scope reports without
 * issuing a request, the same `locked` reading Work commands answer, because
 * the gateway rule it repeats is the same one (`scopeWritable`). */
function notWritable(reason: string): WorkResult<WorkflowDefinitionDisposition> {
  return { outcome: 'refused', state: 'locked', detail: reason };
}

export function useWorkflowLifecycle() {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  const client = useQueryClient();
  const writability = scopeWritable(scope);
  return useMutation<WorkResult<WorkflowDefinitionDisposition>, never, WorkflowLifecycleCommand>({
    mutationFn: (command) => {
      if (writability.state !== 'writable') {
        return Promise.resolve(notWritable(writability.reason));
      }
      const route = lifecycleRoute(command.action);
      return callWork(
        route,
        {
          definition_id: command.definitionId,
          definition_version: command.definitionVersion,
          expected_revision: command.expectedRevision,
        },
        scopedUrl(scope, route.path),
      );
    },
    onSuccess: (result, command) => {
      if (!workResultInvalidatesReads(result)) return;
      void client.invalidateQueries({ queryKey: workflowQueryKey(key, 'list-definitions') });
      void client.invalidateQueries({
        queryKey: workflowQueryKey(key, 'definition-history', command.definitionId),
      });
    },
  });
}
