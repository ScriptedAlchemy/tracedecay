import {
  AcceptProposalCommandSchema,
  AcceptTaskCommandSchema,
  AdmitWorkExecutionRequestV1Schema,
  ExecutionTopologyMetricsRequestV1Schema,
  ExecutionTopologyMetricsV1Schema,
  ExecutionTopologyViewV1Schema,
  ReplanDependenciesCommandSchema,
  ReviewProposalRequestV1Schema,
  WorkAttemptListRequestV1Schema,
  WorkAttemptListV1Schema,
  WorkGraphReadRequestV1Schema,
  WorkGraphReadV1Schema,
  PrepareWorkProductMutationRequestV1Schema,
  WorkProductMutationReceiptV1Schema,
  WorkProductMutationRequestV1Schema,
  WorkProjectionDeltaRequestV1Schema,
  WorkProjectionDeltaV1Schema,
  WorkProjectionSchema,
  WorkProjectionSnapshotRequestV1Schema,
  WorkProjectionSnapshotV1Schema,
  WorkTopologyViewRequestV1Schema,
} from "../../contracts/index.ts";
import type { WorkRoute } from "./workApi.ts";

/**
 * The canonical Work routes this dashboard calls or documents.
 *
 * Each one names a core operation of the canonical `WorkOperation` descriptor
 * (`crates/tracedecay-api/src/work.rs`), which is what the daemon mounts and
 * what `src/dashboard/work_api.rs` publishes as the dashboard route document:
 * same operation id, same path, same request and response contract. They are
 * written out rather than derived because there is no generated route table on
 * the dashboard side, and a route invented here would be a request the daemon
 * has never mounted.
 *
 * Declared is not the same as called. Proposal decisions and direct execution
 * admission remain documented because they are mounted product operations;
 * the dashboard's current create and admission journeys instead use the
 * backend-owned prepare/mutate handoff so the browser never mints authority
 * identities, clocks, or revision pins.
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
 * The canonical structural execution-topology view. Unlike the attempt page,
 * this route publishes all four topology dimensions from the same verified
 * topology generation: placement lanes, branch policy, review policy, and
 * integration strategy. The topology lens must consume this projection rather
 * than reconstructing policy or placement groups from raw attempts.
 */
export const WORK_TOPOLOGY_ROUTE = {
  operation: "operation.work.topology",
  path: "/api/work/topology",
  request: WorkTopologyViewRequestV1Schema,
  response: ExecutionTopologyViewV1Schema,
} as const satisfies WorkRoute<unknown, unknown>;

/**
 * The bounded execution accounting read behind the Work, Observatory, and
 * Costs descriptor cells; this read describes retained observations under
 * the requested horizon.
 */
export const WORK_EXECUTION_TOPOLOGY_METRICS_ROUTE = {
  operation: "operation.work.topology_metrics",
  path: "/api/work/topology-metrics",
  request: ExecutionTopologyMetricsRequestV1Schema,
  response: ExecutionTopologyMetricsV1Schema,
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
 * A graph mutation is a two-step authority handoff. The prepare read mints the
 * exact command, including its selected scope, current graph authority, and
 * revision pins. The mutation route consumes that returned command unchanged.
 */
export const WORK_PREPARE_GRAPH_MUTATION_ROUTE = {
  operation: "operation.work.prepare_graph_mutation",
  path: "/api/work/prepare-graph-mutation",
  request: PrepareWorkProductMutationRequestV1Schema,
  response: WorkProductMutationRequestV1Schema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORK_MUTATE_GRAPH_ROUTE = {
  operation: "operation.work.mutate_graph",
  path: "/api/work/mutate-graph",
  request: WorkProductMutationRequestV1Schema,
  response: WorkProductMutationReceiptV1Schema,
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
  request: AdmitWorkExecutionRequestV1Schema,
  response: WorkProductMutationReceiptV1Schema,
} as const satisfies WorkRoute<unknown, unknown>;

export const WORK_ACCEPT_TASK_ROUTE = {
  operation: "operation.work.accept_task",
  path: "/api/work/accept-task",
  request: AcceptTaskCommandSchema,
  response: WorkProjectionSchema,
} as const satisfies WorkRoute<unknown, unknown>;
