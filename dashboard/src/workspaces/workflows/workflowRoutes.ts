import { z } from 'zod';
import {
  WorkflowDefinitionActivateRequestSchema,
  WorkflowDefinitionDispositionSchema,
  WorkflowDefinitionHistoryRequestSchema,
  WorkflowDefinitionListRequestSchema,
  WorkflowDefinitionRejectRequestSchema,
  WorkflowDefinitionRetireRequestSchema,
  WorkflowDefinitionSchema,
  WorkflowRunGetRequestSchema,
  WorkflowRunProjectionSchema,
} from '../../contracts/index.ts';
import type { WorkRoute } from '../work/workApi.ts';

/**
 * The canonical Workflow routes this dashboard calls or documents.
 *
 * Each one names an operation of the canonical `WorkflowOperation` descriptor
 * (`crates/tracedecay-api/src/workflow.rs`): same operation id, same
 * `/application/workflow/<segment>` path the catalog advertises
 * (`workflow_executable_binding_registry`), reached through the dashboard's
 * `/api/application` nest. They are written out rather than derived because
 * there is no generated route table on the dashboard side, and a route
 * invented here would be a request the daemon has never mounted.
 *
 * Declared is not the same as mounted-for-the-browser. Six of the sixteen
 * Workflow operations are deliberately NOT declared here:
 *
 *   handoff-issue / handoff-redeem     the dashboard never holds a bearer
 *                                      token and must not grow a client that
 *                                      could redeem one.
 *   start-run / pause-run / resume-run the browser must not mint execution
 *   / cancel-run                       fences, command ids, or provider
 *                                      admissions; runs are started and
 *                                      controlled by their owning surfaces
 *                                      and observed here through `get-run`.
 *
 * Register, validate, get, and diff stay undeclared until the workspace grows
 * a definition-authoring journey; a declared-but-uncalled route would be
 * advertising the dashboard does not back.
 */

/** Every registered definition version, newest data straight off the durable
 * authority. The response is the daemon's own list; an empty array is a real
 * empty registry, never a substitute for a refusal. */
export const WORKFLOW_LIST_DEFINITIONS_ROUTE = {
  operation: 'operation.workflow.list_definitions',
  path: '/api/application/workflow/list-definitions',
  request: WorkflowDefinitionListRequestSchema,
  response: z.array(WorkflowDefinitionSchema),
} as const satisfies WorkRoute<unknown, unknown>;

/** Every immutable version of one definition identity, oldest first. */
export const WORKFLOW_DEFINITION_HISTORY_ROUTE = {
  operation: 'operation.workflow.definition_history',
  path: '/api/application/workflow/definition-history',
  request: WorkflowDefinitionHistoryRequestSchema,
  response: z.array(WorkflowDefinitionSchema),
} as const satisfies WorkRoute<unknown, unknown>;

/**
 * The three lifecycle transitions, each a compare-and-swap against the
 * disposition revision the caller last saw. A stale revision is a typed
 * conflict, never a silent overwrite; catalog admission gates activate on the
 * daemon before the transition is journaled.
 */
export const WORKFLOW_ACTIVATE_DEFINITION_ROUTE = {
  operation: 'operation.workflow.activate_definition',
  path: '/api/application/workflow/activate-definition',
  request: WorkflowDefinitionActivateRequestSchema,
  response: WorkflowDefinitionDispositionSchema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORKFLOW_RETIRE_DEFINITION_ROUTE = {
  operation: 'operation.workflow.retire_definition',
  path: '/api/application/workflow/retire-definition',
  request: WorkflowDefinitionRetireRequestSchema,
  response: WorkflowDefinitionDispositionSchema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORKFLOW_REJECT_DEFINITION_ROUTE = {
  operation: 'operation.workflow.reject_definition',
  path: '/api/application/workflow/reject-definition',
  request: WorkflowDefinitionRejectRequestSchema,
  response: WorkflowDefinitionDispositionSchema,
} as const satisfies WorkRoute<unknown, unknown>;

/** One run's projection: status, sequence, per-step states and receipts,
 * rebuilt from the run's own event journal. */
export const WORKFLOW_GET_RUN_ROUTE = {
  operation: 'operation.workflow.get_run',
  path: '/api/application/workflow/get-run',
  request: WorkflowRunGetRequestSchema,
  response: WorkflowRunProjectionSchema,
} as const satisfies WorkRoute<unknown, unknown>;
