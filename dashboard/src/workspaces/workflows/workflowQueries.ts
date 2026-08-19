import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type {
  WorkflowDefinition,
  WorkflowDefinitionDisposition,
  WorkflowRunProjection,
} from '../../contracts/index.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { callWork, type WorkResult } from '../work/workApi.ts';
import {
  WORKFLOW_ACTIVATE_DEFINITION_ROUTE,
  WORKFLOW_GET_RUN_ROUTE,
  WORKFLOW_LIST_DEFINITIONS_ROUTE,
  WORKFLOW_REJECT_DEFINITION_ROUTE,
  WORKFLOW_RETIRE_DEFINITION_ROUTE,
} from './workflowRoutes.ts';

/**
 * The reads and lifecycle commands behind the Workflows workspace.
 *
 * Every call goes through the same application envelope walker the Work
 * surface uses (`callWork`), so a Workflow refusal is reported in exactly the
 * words every other application refusal uses and an absent value can never
 * render as an empty success. Scoping matches Work: `scopedUrl` rewrites to
 * the project gateway when the scope bar names a project.
 */

function workflowQueryKey(scope: string, ...parts: readonly (string | number)[]) {
  return ['workflow', scope, ...parts] as const;
}

export function useWorkflowDefinitions() {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<WorkflowDefinition[]>>({
    queryKey: workflowQueryKey(key, 'list-definitions'),
    queryFn: () =>
      callWork(
        WORKFLOW_LIST_DEFINITIONS_ROUTE,
        {},
        scopedUrl(scope, WORKFLOW_LIST_DEFINITIONS_ROUTE.path),
      ),
  });
}

/** One run's projection, read on demand for an operator-named run id. The
 * query is disabled until a run id is named, and a disabled query has no data
 * — which the view reports as unasked, not as a run that does not exist. */
export function useWorkflowRun(runId: string | null) {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<WorkflowRunProjection>>({
    queryKey: workflowQueryKey(key, 'get-run', runId ?? ''),
    enabled: runId !== null,
    queryFn: () =>
      callWork(
        WORKFLOW_GET_RUN_ROUTE,
        { run_id: runId ?? '' },
        scopedUrl(scope, WORKFLOW_GET_RUN_ROUTE.path),
      ),
  });
}

export type WorkflowLifecycleAction = 'activate' | 'retire' | 'reject';

export interface WorkflowLifecycleCommand {
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

/**
 * One compare-and-swap lifecycle transition. The mutation resolves to the
 * daemon's own `WorkResult` — the returned disposition on success, or the
 * typed refusal (conflict, denial, catalog admission) verbatim — and the
 * definitions list is re-read afterwards so the panel never carries a state
 * the daemon did not answer.
 */
export function useWorkflowLifecycle() {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  const client = useQueryClient();
  return useMutation<WorkResult<WorkflowDefinitionDisposition>, never, WorkflowLifecycleCommand>({
    mutationFn: (command) => {
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
    onSettled: () => {
      void client.invalidateQueries({ queryKey: workflowQueryKey(key, 'list-definitions') });
    },
  });
}
