//! Request contracts retained while the legacy Work authority is unmounted.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ManifestDigest, ProposalId, RuntimeEvidenceRef, TaskId, UtcMicros, WorkAuthority,
    WorkCommandId, WorkVersion,
};

use crate::{ApplicationProblem, LegalAction, RequestContext, RetryDirective, SafeDiagnostic};

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkCommand {
    pub task_id: TaskId,
    pub title: String,
    #[serde(default)]
    pub dependencies: BTreeSet<TaskId>,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplanDependenciesCommand {
    pub task_id: TaskId,
    #[serde(default)]
    pub dependencies: BTreeSet<TaskId>,
    pub expected_version: WorkVersion,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewProposalCommand {
    pub task_id: TaskId,
    pub proposal_id: ProposalId,
    pub proposal_digest: ManifestDigest,
    pub expected_version: WorkVersion,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

/// A proposal review records a non-accepting disposition.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewProposalDispositionV1 {
    Rejected,
    Superseded,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewProposalRequestV1 {
    pub review: ReviewProposalCommand,
    pub disposition: ReviewProposalDispositionV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AcceptProposalCommand {
    pub review: ReviewProposalCommand,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdmitExecutionCommand {
    pub task_id: TaskId,
    pub expected_version: WorkVersion,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttachRuntimeEvidenceCommand {
    pub task_id: TaskId,
    pub evidence: RuntimeEvidenceRef,
    pub expected_version: WorkVersion,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AcceptTaskCommand {
    pub task_id: TaskId,
    pub expected_version: WorkVersion,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

/// Read-only proposal generation is pinned to the current Work version.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerateProposalRequest {
    pub task_id: TaskId,
    pub proposal_id: ProposalId,
    pub proposal_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GeneratedWorkProposal {
    pub task_id: TaskId,
    pub proposal_id: ProposalId,
    pub proposal_digest: ManifestDigest,
    pub based_on_version: WorkVersion,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WorkReadiness {
    Ready,
    Blocked {
        active_dependencies: BTreeSet<TaskId>,
    },
    Accepted,
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
            code: "application.work.invalid-authority".to_owned(),
            message: "The Work authority is invalid.".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    })
}
