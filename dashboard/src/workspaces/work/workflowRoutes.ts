import {
  TaskHandoffGrantSchema,
  TaskHandoffIssueRequestSchema,
  TaskHandoffRedeemRequestSchema,
  TaskHandoffRedeemedSchema,
  WorkflowActivationSchema,
  WorkflowDefinitionActivateRequestSchema,
  WorkflowDefinitionRegisterRequestSchema,
  WorkflowDefinitionSchema,
  WorkflowFanOutRequestSchema,
  WorkflowRunProjectionSchema,
} from '../../contracts/index.ts';
import type { ApplicationRoute } from './workApi.ts';

/**
 * Dashboard bindings for the canonical Workflow application surface.
 *
 * These paths enter the same application router as CLI, MCP, and both SDKs.
 * The dashboard owns no workflow scheduler, request defaults, or wire DTOs.
 */
export const WORKFLOW_REGISTER_DEFINITION_ROUTE = {
  operation: 'operation.workflow.register_definition',
  path: '/api/application/workflow/register-definition',
  request: WorkflowDefinitionRegisterRequestSchema,
  response: WorkflowDefinitionSchema,
} as const satisfies ApplicationRoute<unknown, unknown>;

export const WORKFLOW_ACTIVATE_DEFINITION_ROUTE = {
  operation: 'operation.workflow.activate_definition',
  path: '/api/application/workflow/activate-definition',
  request: WorkflowDefinitionActivateRequestSchema,
  response: WorkflowActivationSchema,
} as const satisfies ApplicationRoute<unknown, unknown>;

export const WORKFLOW_EXECUTE_FAN_OUT_ROUTE = {
  operation: 'operation.workflow.execute_fan_out',
  path: '/api/application/workflow/execute-fan-out',
  request: WorkflowFanOutRequestSchema,
  response: WorkflowRunProjectionSchema,
} as const satisfies ApplicationRoute<unknown, unknown>;

export const WORKFLOW_HANDOFF_ISSUE_ROUTE = {
  operation: 'operation.workflow.handoff_issue',
  path: '/api/application/workflow/handoff-issue',
  request: TaskHandoffIssueRequestSchema,
  response: TaskHandoffGrantSchema,
} as const satisfies ApplicationRoute<unknown, unknown>;

export const WORKFLOW_HANDOFF_REDEEM_ROUTE = {
  operation: 'operation.workflow.handoff_redeem',
  path: '/api/application/workflow/handoff-redeem',
  request: TaskHandoffRedeemRequestSchema,
  response: TaskHandoffRedeemedSchema,
} as const satisfies ApplicationRoute<unknown, unknown>;

export const WORKFLOW_ROUTES = [
  WORKFLOW_REGISTER_DEFINITION_ROUTE,
  WORKFLOW_ACTIVATE_DEFINITION_ROUTE,
  WORKFLOW_EXECUTE_FAN_OUT_ROUTE,
  WORKFLOW_HANDOFF_ISSUE_ROUTE,
  WORKFLOW_HANDOFF_REDEEM_ROUTE,
] as const;
