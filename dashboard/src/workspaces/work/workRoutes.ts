import {
  AcceptProposalCommandSchema,
  AcceptTaskCommandSchema,
  AdmitExecutionCommandSchema,
  AttachRuntimeEvidenceCommandSchema,
  CreateWorkCommandSchema,
  ReplanDependenciesCommandSchema,
  ReviewProposalRequestV1Schema,
  WorkProjectionDeltaRequestV1Schema,
  WorkProjectionDeltaV1Schema,
  WorkProjectionSchema,
  WorkProjectionSnapshotRequestV1Schema,
  WorkProjectionSnapshotV1Schema,
} from "../../contracts/index.ts";
import type { WorkRoute } from "./workApi.ts";

/**
 * The nine Work routes this build can reach, and no others.
 *
 * Each one names a core operation of the canonical `WorkOperation` descriptor
 * (`crates/tracedecay-api/src/work.rs`), which is what the daemon mounts and
 * what `src/dashboard/work_api.rs` publishes as the dashboard route document:
 * same operation id, same path, same request and response contract. They are
 * written out rather than derived because there is no generated route table on
 * the dashboard side, and a route invented here would be a request the daemon
 * has never mounted. Execution is owned by the Workflow runtime and has its
 * own routes; the Work descriptor carries no execution operations.
 */

export const WORK_SNAPSHOT_ROUTE = {
  operation: "operation.work.snapshot",
  path: "/api/work/snapshot",
  request: WorkProjectionSnapshotRequestV1Schema,
  response: WorkProjectionSnapshotV1Schema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORK_DELTA_ROUTE = {
  operation: "operation.work.delta",
  path: "/api/work/delta",
  request: WorkProjectionDeltaRequestV1Schema,
  response: WorkProjectionDeltaV1Schema,
} as const satisfies WorkRoute<unknown, unknown>;

/**
 * The seven commands, each answering with the projection it produced.
 *
 * Six of the seven carry `expected_version` and are therefore compare-and-swap:
 * the daemon answers 409 when the task moved underneath the caller. `create` is
 * the exception, because a task that does not exist yet has no version to
 * compare against.
 */
export const WORK_CREATE_ROUTE = {
  operation: "operation.work.create",
  path: "/api/work/create",
  request: CreateWorkCommandSchema,
  response: WorkProjectionSchema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORK_REPLAN_DEPENDENCIES_ROUTE = {
  operation: "operation.work.replan_dependencies",
  path: "/api/work/replan-dependencies",
  request: ReplanDependenciesCommandSchema,
  response: WorkProjectionSchema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORK_REVIEW_PROPOSAL_ROUTE = {
  operation: "operation.work.review_proposal",
  path: "/api/work/review-proposal",
  request: ReviewProposalRequestV1Schema,
  response: WorkProjectionSchema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORK_ACCEPT_PROPOSAL_ROUTE = {
  operation: "operation.work.accept_proposal",
  path: "/api/work/accept-proposal",
  request: AcceptProposalCommandSchema,
  response: WorkProjectionSchema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORK_ADMIT_EXECUTION_ROUTE = {
  operation: "operation.work.admit_execution",
  path: "/api/work/admit-execution",
  request: AdmitExecutionCommandSchema,
  response: WorkProjectionSchema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORK_ATTACH_RUNTIME_EVIDENCE_ROUTE = {
  operation: "operation.work.attach_runtime_evidence",
  path: "/api/work/attach-runtime-evidence",
  request: AttachRuntimeEvidenceCommandSchema,
  response: WorkProjectionSchema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORK_ACCEPT_TASK_ROUTE = {
  operation: "operation.work.accept_task",
  path: "/api/work/accept-task",
  request: AcceptTaskCommandSchema,
  response: WorkProjectionSchema,
} as const satisfies WorkRoute<unknown, unknown>;
