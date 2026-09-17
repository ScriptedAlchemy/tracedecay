//! Scope-bound Work authority and proposal-routing contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{ConfigurationRevisionId, TaskId, WorkAuthority};
use tracedecay_policy::work_loop::{
    WorkBudgetEnvelopeV1, WorkContentLocationLimitV1, WorkPriorOutcomeV1, WorkRouteCandidateV1,
    WorkRouteOverrideV1,
};

use crate::{ApplicationProblem, LegalAction, RequestContext, RetryDirective, SafeDiagnostic};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkRoutingSnapshotErrorV1 {
    #[error("proposal routing is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("proposal routing is unavailable")]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewProposalDispositionV1 {
    Rejected,
    Superseded,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRoutingSnapshotV1 {
    #[serde(default)]
    pub configuration_revision: Option<ConfigurationRevisionId>,
    #[serde(default)]
    pub eligible_routes: Vec<WorkRouteCandidateV1>,
    #[serde(default)]
    pub budget: Option<WorkBudgetEnvelopeV1>,
    #[serde(default)]
    pub content_location: Option<WorkContentLocationLimitV1>,
    #[serde(default)]
    pub prior_outcomes: Vec<WorkPriorOutcomeV1>,
    #[serde(default)]
    pub human_override: Option<WorkRouteOverrideV1>,
}

impl WorkRoutingSnapshotV1 {
    pub(crate) fn canonicalize(mut self) -> Self {
        self.eligible_routes
            .sort_by(|left, right| left.route_id.cmp(&right.route_id));
        self.eligible_routes
            .dedup_by(|left, right| left.route_id == right.route_id);
        self.prior_outcomes.sort_by(|left, right| {
            (left.route_id.as_str(), left.observed_at)
                .cmp(&(right.route_id.as_str(), right.observed_at))
        });
        self
    }
}

pub trait WorkRoutingSnapshotPortV1: Send + Sync {
    fn routing_snapshot(
        &self,
        context: &RequestContext,
        task_id: &TaskId,
    ) -> Result<WorkRoutingSnapshotV1, WorkRoutingSnapshotErrorV1>;
}

pub(crate) fn work_authority(
    context: &RequestContext,
) -> Result<WorkAuthority, ApplicationProblem> {
    WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .map_err(|_| ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "application.work.invalid-history".to_owned(),
            message: "The Work command or stored history is invalid.".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    })
}
