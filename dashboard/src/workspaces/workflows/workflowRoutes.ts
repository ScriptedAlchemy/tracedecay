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
 * The Workflow routes this dashboard calls: same operation ids and
 * `/application/workflow/<segment>` paths as the canonical `WorkflowOperation`
 * descriptor (`crates/tracedecay-api/src/workflow.rs`). Handoffs and run
 * control are deliberately undeclared — the browser never holds a bearer or
 * mints fences/command ids — and register/validate/get/diff stay undeclared
 * until an authoring journey exists.
 */

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

/** The three lifecycle compare-and-swaps; catalog admission gates activate
 * on the daemon before the transition is journaled. */
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

/** One run's projection, rebuilt from the run's own event journal. */
export const WORKFLOW_GET_RUN_ROUTE = {
  operation: 'operation.workflow.get_run',
  path: '/api/application/workflow/get-run',
  request: WorkflowRunGetRequestSchema,
  response: WorkflowRunProjectionSchema,
} as const satisfies WorkRoute<unknown, unknown>;
