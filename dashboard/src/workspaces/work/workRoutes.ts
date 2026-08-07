import {
  AcceptProposalCommandSchema,
  AcceptTaskCommandSchema,
  AdmitExecutionCommandSchema,
  AttachRuntimeEvidenceCommandSchema,
  CreateWorkCommandSchema,
  ReplanDependenciesCommandSchema,
  ReviewProposalRequestV1Schema,
  WorkAttemptListRequestV1Schema,
  WorkAttemptListV1Schema,
  WorkGraphReadRequestV1Schema,
  WorkGraphReadV1Schema,
  WorkProjectionDeltaRequestV1Schema,
  WorkProjectionDeltaV1Schema,
  WorkProjectionSchema,
  WorkProjectionSnapshotRequestV1Schema,
  WorkProjectionSnapshotV1Schema,
} from "../../contracts/index.ts";
import type { WorkRoute } from "./workApi.ts";

/**
 * The eleven Work routes this build can reach, and no others.
 *
 * Each one names a core operation of the canonical `WorkOperation` descriptor
 * (`crates/tracedecay-api/src/work.rs`), which is what the daemon mounts and
 * what `src/dashboard/work_api.rs` publishes as the dashboard route document:
 * same operation id, same path, same request and response contract. They are
 * written out rather than derived because there is no generated route table on
 * the dashboard side, and a route invented here would be a request the daemon
 * has never mounted.
 *
 * The descriptor mounts sixteen; these eleven are the ones the dashboard
 * declares. The five it leaves alone are `generate_proposal` and the four
 * attempt operations — start, the single-attempt status read, cancel, and
 * resume — which drive execution rather than read it, and belong to whichever
 * surface takes on running Work.
 *
 * Declared is not the same as called, and the difference is deliberate. Eight
 * of the eleven have a caller in this build; `review_proposal`,
 * `accept_proposal` and `attach_runtime_evidence` are declared and unbound.
 * They stay because this table is the dashboard's record of what the daemon
 * mounts and what contracts sit on either side of it — deleting a row would
 * not un-mount the route, it would only mean the next caller has to rediscover
 * its contract by hand.
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
 * The execution record behind the projections.
 *
 * Paged like the snapshot, but cursored on the verified topology generation the
 * page was read under rather than on a sequence: a cursor minted against a
 * superseded generation is refused (`work.topology_generation_superseded`)
 * instead of being continued across a topology that moved. Ordering is stable
 * on (task_id, run_id, attempt_id), which is what makes the resume point exact.
 */
export const WORK_LIST_ATTEMPTS_ROUTE = {
  operation: "operation.work.list_attempts",
  path: "/api/work/list-attempts",
  request: WorkAttemptListRequestV1Schema,
  response: WorkAttemptListV1Schema,
} as const satisfies WorkRoute<unknown, unknown>;

/**
 * The work-product graph, and every projection derived from one version of it.
 *
 * This is the read the four projections beside the board were waiting for. It
 * answers `WorkGraphReadV1`, tagged by the mode it was asked in: `current` and
 * `as_of` carry one `snapshot` entry, `evolution` and `forensic` carry a
 * `timeline` of entries plus the coverage that timeline was read under. Every
 * entry holds one immutable graph version AND the whole
 * `WorkProductProjectionBundleV1` derived from that same version at the
 * caller's own observation instant, so effort, gating edges, declared causal
 * candidates, timeline instants, workload and live runtime state are all read
 * off one consistent version rather than stitched from separate reads.
 *
 * `continuation` is a timeline cursor and is legal only on the two timeline
 * modes; `selection` names the relation scope, and a `relations` selection with
 * an empty scope set is an invalid request rather than an empty answer.
 */
export const WORK_VIEWS_ROUTE = {
  operation: "operation.work.views",
  path: "/api/work/views",
  request: WorkGraphReadRequestV1Schema,
  response: WorkGraphReadV1Schema,
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
