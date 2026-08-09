//! One-transaction admission of a Work-product attempt.

use thiserror::Error;
use tracedecay_domain::{
    WorkAttemptV1, WorkAuthority, WorkGraphChangeV1, WorkProductAuthorizedRelationScopeV1,
    WorkProductEventPayloadV1, configuration::TopologyConcurrencyPolicyV1,
};

use crate::{
    WorkRetryAttemptOutcomeV1, WorkRetryWriteV1, WorkSynthesisAdmissionRecordV1,
    WorkSynthesisInsertOutcome,
};

use super::{WorkProductEventCommitV1, WorkProductEventDraftV1, WorkProductPortContextV1};

/// Everything the durable authority needs to admit one ordinary attempt.
///
/// `authority` is retained explicitly because the product graph is
/// profile-owned while attempt rows are scoped by the exact registered
/// project/repository/worktree/actor/policy authority. The adapter must write
/// both records in one transaction; callers cannot reconstruct this binding
/// later from the profile selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkProductAttemptAdmissionV1 {
    pub product_context: WorkProductPortContextV1,
    pub product_draft: WorkProductEventDraftV1,
    pub authority: WorkAuthority,
    pub attempt: WorkAttemptV1,
    pub concurrency: TopologyConcurrencyPolicyV1,
}

impl WorkProductAttemptAdmissionV1 {
    pub fn validate(&self) -> Result<(), WorkProductAttemptAdmissionErrorV1> {
        if self.product_context.actor() != self.authority.actor_id()
            || &self.product_draft.actor_id != self.authority.actor_id()
            || self.product_draft.policy_revision_id.as_str()
                != self.authority.policy_digest().as_str()
            || !selection_covers_authority(
                self.product_context.authorized_scope().selection(),
                &self.authority,
            )
            || self.attempt.identity().task_id() != self.product_task_id()
            || self.product_attempt() != Some(self.attempt.identity())
            || self.product_draft.expected_graph_version
                != Some(self.attempt.projection_binding().graph_version())
        {
            return Err(WorkProductAttemptAdmissionErrorV1::InvalidAdmission);
        }
        Ok(())
    }

    fn product_task_id(&self) -> &tracedecay_domain::TaskId {
        match &self.product_draft.payload {
            WorkProductEventPayloadV1::Changed { change } => match change.as_ref() {
                WorkGraphChangeV1::AcceptedAttemptLinked { task_id, .. } => task_id,
                _ => self.attempt.identity().task_id(),
            },
            WorkProductEventPayloadV1::Created { .. } => self.attempt.identity().task_id(),
        }
    }

    fn product_attempt(&self) -> Option<&tracedecay_domain::WorkAttemptIdentityV1> {
        match &self.product_draft.payload {
            WorkProductEventPayloadV1::Changed { change } => match change.as_ref() {
                WorkGraphChangeV1::AcceptedAttemptLinked { identity, .. } => Some(identity),
                _ => None,
            },
            WorkProductEventPayloadV1::Created { .. } => None,
        }
    }
}

/// Atomic result for an ordinary attempt. Product replay and attempt replay
/// are one outcome; a half-replay is a conflict, never a successful repair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkProductAttemptAdmissionOutcomeV1 {
    Inserted {
        product: WorkProductEventCommitV1,
        attempt: WorkAttemptV1,
    },
    Replayed {
        product: WorkProductEventCommitV1,
        attempt: WorkAttemptV1,
    },
}

/// Everything the durable authority needs to admit one retry and retain its
/// adjudication receipt in the same transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkProductRetryAdmissionV1 {
    pub admission: WorkProductAttemptAdmissionV1,
    pub retry: WorkRetryWriteV1,
}

impl WorkProductRetryAdmissionV1 {
    pub fn validate(&self) -> Result<(), WorkProductAttemptAdmissionErrorV1> {
        self.admission.validate()?;
        if self.retry.attempt != self.admission.attempt
            || self.admission.product_draft.command_id != self.retry.receipt.command.command_id
        {
            return Err(WorkProductAttemptAdmissionErrorV1::InvalidAdmission);
        }
        Ok(())
    }
}

/// One product-linked synthesis admission. The synthesis record and the
/// attempt it contains share the product admission's exact identity and are
/// committed with the graph event or not at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkProductSynthesisAdmissionV1 {
    pub admission: WorkProductAttemptAdmissionV1,
    pub synthesis: WorkSynthesisAdmissionRecordV1,
}

impl WorkProductSynthesisAdmissionV1 {
    pub fn validate(&self) -> Result<(), WorkProductAttemptAdmissionErrorV1> {
        self.admission.validate()?;
        if self.synthesis.result.attempt != self.admission.attempt {
            return Err(WorkProductAttemptAdmissionErrorV1::InvalidAdmission);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProductAttemptAdmissionErrorV1 {
    #[error("Work product attempt admission is invalid")]
    InvalidAdmission,
    #[error("Work product attempt was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work product graph version changed")]
    VersionConflict,
    #[error("Work product attempt identity or command conflicts")]
    IdentityConflict,
    #[error("Work product attempt command idempotency key conflicts")]
    IdempotencyConflict,
    #[error("Work product attempt capacity is exhausted")]
    CapacityExceeded,
    #[error("Work product attempt authority is unavailable")]
    Unavailable,
    #[error("Work product attempt admission was cancelled")]
    Cancelled,
    #[error("Work product attempt admission timed out")]
    TimedOut,
    #[error("Work product attempt commit durability is uncertain")]
    DurabilityUncertain,
}

fn selection_covers_authority(
    selection: &tracedecay_domain::WorkProductSelectionScopeV1,
    authority: &WorkAuthority,
) -> bool {
    selection.relation_scopes().is_some_and(|relations| {
        relations.iter().any(|relation| match relation {
            WorkProductAuthorizedRelationScopeV1::Project { project_id } => {
                project_id == authority.project_id()
            }
            WorkProductAuthorizedRelationScopeV1::Repository {
                project_id,
                repository_id,
            } => project_id == authority.project_id() && repository_id == authority.repository_id(),
        })
    })
}

/// Canonical one-transaction authority for product linkage and runtime rows.
pub trait WorkProductAttemptAdmissionPortV1: Send + Sync {
    fn admit_attempt(
        &self,
        admission: &WorkProductAttemptAdmissionV1,
    ) -> Result<WorkProductAttemptAdmissionOutcomeV1, WorkProductAttemptAdmissionErrorV1>;

    fn admit_retry(
        &self,
        admission: &WorkProductRetryAdmissionV1,
    ) -> Result<
        (WorkProductEventCommitV1, WorkRetryAttemptOutcomeV1),
        WorkProductAttemptAdmissionErrorV1,
    >;

    fn admit_synthesis(
        &self,
        admission: &WorkProductSynthesisAdmissionV1,
    ) -> Result<
        (WorkProductEventCommitV1, WorkSynthesisInsertOutcome),
        WorkProductAttemptAdmissionErrorV1,
    >;
}
