//! The Work execution-topology read surface.
//!
//! Plan 11's topology lens decodes an application-owned
//! `ExecutionTopologyViewV1` with four independently decoded dimensions.
//! Every dimension here is read off data this build actually holds: the
//! placement lanes come from the durable placement relation joined to the
//! attempt page, and the branch/review/integration dimensions carry the
//! resolved work topology policy the run environment is pinned to. Nothing
//! is synthesized — a scope with no Work is the explicit `Absent` state, and
//! the view is always bound to the verified topology generation the attempt
//! page was read under.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::{
    BranchTopologyPolicyV1, CrossMergePolicyV1, ProtectedRefRuleV1, ReviewTopologyPolicyV1,
    TopologyGatePolicyV1, WorkTopologyPolicyV1, WorktreePlacementModeV1,
};
use tracedecay_domain::{RunId, TaskId, WorkAuthority};

use crate::work::WorkStoragePort;
use crate::work_attempt::{
    WorkAttemptListCoverageV1, WorkAttemptListCursorV1, WorkAttemptListRequestV1,
    WorkAttemptListV1, WorkAttemptService, WorkAttemptStoragePort, WorkAttemptTopologyBindingV1,
    WorkAttemptTopologyStateV1,
};
use crate::work_placement::{
    WorkPlacementReadingV1, WorkPlacementService, WorkPlacementStatusRequestV1,
    WorkPlacementStoragePort,
};
use crate::work_read::WorkProjectionReadPort;
use crate::{ApplicationProblem, RequestContext, SafeDiagnostic};

/// One page-bounded topology view read. The cursor vocabulary is the attempt
/// list's: a cursor minted under a superseded topology generation is a typed
/// staleness refusal, never a silently different page.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkTopologyViewRequestV1")]
pub struct WorkTopologyViewRequestV1 {
    pub page_size: u32,
    #[serde(default)]
    pub cursor: Option<WorkAttemptListCursorV1>,
}

/// One execution-placement lane: a distinct `(task, run)` pair from the
/// attempt page joined to its durable placement reading. Placement absence is
/// a state on the lane, not a dropped lane.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkTopologyPlacementLaneV1")]
pub struct WorkTopologyPlacementLaneV1 {
    pub task_id: TaskId,
    pub run_id: RunId,
    /// Attempts this lane carried within the requested page.
    pub attempt_count: u32,
    pub placement: WorkPlacementReadingV1,
}

/// The execution-placement dimension: the policy's placement mode plus one
/// lane per distinct `(task, run)` pair in page order.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkTopologyExecutionPlacementV1")]
pub struct WorkTopologyExecutionPlacementV1 {
    pub mode: WorktreePlacementModeV1,
    pub lanes: Vec<WorkTopologyPlacementLaneV1>,
}

/// The integration-strategy dimension, read verbatim from the resolved work
/// topology policy the runs in scope are admitted against.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[schemars(title = "WorkTopologyIntegrationStrategyV1")]
pub struct WorkTopologyIntegrationStrategyV1 {
    pub cross_merge: CrossMergePolicyV1,
    pub gates: TopologyGatePolicyV1,
    pub protected_refs: Vec<ProtectedRefRuleV1>,
}

/// The application-owned execution-topology view. Absence of any Work in
/// scope is a typed state, distinct from an authorized-but-empty page.
// A wire contract type; boxing the `View` dimensions would ripple through
// its construction and match sites for a response payload, not a hot
// allocation path (daemon_contract precedent).
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(title = "ExecutionTopologyViewV1")]
pub enum ExecutionTopologyViewV1 {
    /// No Work exists in this authority scope, so there is no topology to
    /// draw. Concealed scopes are refused before any read.
    Absent,
    /// The four dimensions, pinned to one verified topology generation.
    View {
        topology: WorkAttemptTopologyBindingV1,
        coverage: WorkAttemptListCoverageV1,
        execution_placement: WorkTopologyExecutionPlacementV1,
        branch_topology: BranchTopologyPolicyV1,
        review_topology: ReviewTopologyPolicyV1,
        integration_strategy: WorkTopologyIntegrationStrategyV1,
    },
}

/// Reads one execution-topology view: the attempt page (with the attempt
/// list's own bounds, cursor, and staleness contract), one placement reading
/// per distinct `(task, run)` lane, and the policy-carried dimensions.
pub fn execution_topology_view<S, P, W, PS>(
    attempts: &WorkAttemptService<S, P, W>,
    placements: &WorkPlacementService<PS>,
    policy: &WorkTopologyPolicyV1,
    context: &RequestContext,
    request: &WorkTopologyViewRequestV1,
    topology: impl FnOnce(&WorkAuthority) -> Result<WorkAttemptTopologyStateV1, ApplicationProblem>,
) -> Result<ExecutionTopologyViewV1, ApplicationProblem>
where
    S: WorkAttemptStoragePort,
    P: WorkProjectionReadPort,
    W: WorkStoragePort,
    PS: WorkPlacementStoragePort,
{
    if policy.validate().is_err() {
        // The registered policy is validated at project-open resolution, so
        // an invalid policy here is a broken runtime invariant, not caller
        // error.
        return Err(ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-topology.policy-invalid".to_owned(),
            message: "The resolved work topology policy is invalid.".to_owned(),
        }));
    }
    let list = attempts.list(
        context,
        &WorkAttemptListRequestV1 {
            page_size: request.page_size,
            cursor: request.cursor.clone(),
        },
        topology,
    )?;
    let WorkAttemptListV1::Listed {
        topology,
        attempts: page,
        coverage,
    } = list
    else {
        return Ok(ExecutionTopologyViewV1::Absent);
    };
    let mut lanes: Vec<WorkTopologyPlacementLaneV1> = Vec::new();
    for attempt in &page {
        let identity = attempt.identity();
        if let Some(lane) = lanes
            .iter_mut()
            .find(|lane| &lane.task_id == identity.task_id() && &lane.run_id == identity.run_id())
        {
            lane.attempt_count = lane.attempt_count.saturating_add(1);
            continue;
        }
        let placement = placements.status(
            context,
            &WorkPlacementStatusRequestV1 {
                task_id: identity.task_id().clone(),
                run_id: identity.run_id().clone(),
            },
        )?;
        lanes.push(WorkTopologyPlacementLaneV1 {
            task_id: identity.task_id().clone(),
            run_id: identity.run_id().clone(),
            attempt_count: 1,
            placement,
        });
    }
    Ok(ExecutionTopologyViewV1::View {
        topology,
        coverage,
        execution_placement: WorkTopologyExecutionPlacementV1 {
            mode: policy.placement.clone(),
            lanes,
        },
        branch_topology: policy.branch_topology.clone(),
        review_topology: policy.review_topology.clone(),
        integration_strategy: WorkTopologyIntegrationStrategyV1 {
            cross_merge: policy.cross_merge.clone(),
            gates: policy.gates.clone(),
            protected_refs: policy.protected_refs.clone(),
        },
    })
}
