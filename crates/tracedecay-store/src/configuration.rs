//! Persistence contracts for the revisioned configuration control plane.
//!
//! Concrete SQLite mechanics live in the root adapter. This crate keeps
//! storage ports typed, append-only, and free of transport/daemon concerns.

use std::future::Future;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::configuration::{
    ConfigurationAuditEvent, ConfigurationIdempotencyKey, ConfigurationReceiptId,
    ConfigurationRevisionId, ConfigurationSnapshotV1, ProtectedChange, ProtectedChangePlan,
    RollbackModeV1,
};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, DomainError, ManifestDigest, UtcMicros, canonical_sha256,
};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConfigurationStoreError {
    #[error("configuration store conflict")]
    RevisionConflict,
    #[error("configuration change plan expired")]
    PlanExpired,
    #[error("configuration change plan is stale")]
    PlanStale,
    #[error("configuration idempotency key conflicts with prior input")]
    IdempotencyConflict,
    #[error("configuration store data is invalid: {0}")]
    InvalidData(String),
    #[error("configuration store unavailable")]
    Unavailable,
}

impl From<DomainError> for ConfigurationStoreError {
    fn from(error: DomainError) -> Self {
        Self::InvalidData(error.to_string())
    }
}

pub type ConfigurationStoreResult<T> = Result<T, ConfigurationStoreError>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigurationRevisionRecordV1 {
    pub revision_id: ConfigurationRevisionId,
    pub parent_revision_id: Option<ConfigurationRevisionId>,
    pub snapshot: ConfigurationSnapshotV1,
    pub actor_id: ActorId,
    pub operation_kind: String,
    pub created_at: UtcMicros,
}

impl ConfigurationRevisionRecordV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.revision_id.validate()?;
        self.parent_revision_id
            .as_ref()
            .map_or(Ok(()), ConfigurationRevisionId::validate)?;
        self.snapshot.validate()?;
        self.actor_id.validate()?;
        if !tracedecay_domain::canonical_text::is_canonical_text(&self.operation_kind) {
            return Err(DomainError::NonCanonical {
                field: "configuration operation kind",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationMutationReceiptV1 {
    pub receipt_id: ConfigurationReceiptId,
    pub actor_id: ActorId,
    pub idempotency_key: ConfigurationIdempotencyKey,
    pub base_revision_id: ConfigurationRevisionId,
    pub result_revision_id: ConfigurationRevisionId,
    pub operation_digest: ManifestDigest,
    pub authorization_policy_epoch: u64,
    pub authorization_policy_digest: AccessPolicyDigest,
    pub authority_revalidated_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
    pub created_at: UtcMicros,
    pub effective_deadline_at: UtcMicros,
}

impl ConfigurationMutationReceiptV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.receipt_id.validate()?;
        self.actor_id.validate()?;
        self.idempotency_key.validate()?;
        self.base_revision_id.validate()?;
        self.result_revision_id.validate()?;
        self.operation_digest.validate()?;
        self.authorization_policy_digest.validate()?;
        self.receipt_digest.validate()?;
        if self.authorization_policy_epoch == 0
            || self.authority_revalidated_at > self.created_at
            || self.effective_deadline_at <= self.created_at
        {
            return Err(DomainError::InvalidRange {
                field: "configuration mutation receipt deadline",
            });
        }
        Ok(())
    }
}

/// Atomic write requested by the application layer. A concrete store must
/// append the revision, receipt, plan terminal event (when applicable), and
/// audit event in one transaction or commit none of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationCommitV1 {
    pub expected_base_revision_id: ConfigurationRevisionId,
    pub next_revision: ConfigurationRevisionRecordV1,
    pub receipt: ConfigurationMutationReceiptV1,
    pub change_plan: Option<ProtectedChangePlan>,
    pub audit_event: ConfigurationAuditEvent,
}

impl ConfigurationCommitV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.expected_base_revision_id.validate()?;
        self.next_revision.validate()?;
        self.receipt.validate()?;
        self.change_plan
            .as_ref()
            .map_or(Ok(()), ProtectedChangePlan::validate)?;
        self.audit_event.validate()?;
        if self.expected_base_revision_id != self.receipt.base_revision_id
            || self.next_revision.revision_id != self.receipt.result_revision_id
        {
            return Err(DomainError::SnapshotMismatch {
                field: "configuration commit revision binding",
            });
        }
        Ok(())
    }
}

/// Exact protected operation retained by the durable control-plane store.
///
/// `ProtectedChangePlan` intentionally contains only a redacted diff for
/// callers.  The typed operation is a separate store-contract value so an
/// apply after restart can reconstruct the exact approved mutation without
/// treating the redacted summary as executable authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigurationProtectedOperationV1 {
    Change(Box<ProtectedChange>),
    Rollback {
        target_revision_id: ConfigurationRevisionId,
        mode: RollbackModeV1,
    },
}

impl ConfigurationProtectedOperationV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Change(change) => change.validate(),
            Self::Rollback {
                target_revision_id, ..
            } => target_revision_id.validate(),
        }
    }

    pub fn operation_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate()?;
        match self {
            Self::Change(change) => change.compute_digest(),
            Self::Rollback {
                target_revision_id,
                mode,
            } => canonical_sha256(&(
                "tracedecay.configuration.rollback.v1",
                target_revision_id,
                mode,
            )),
        }
    }
}

/// One redacted plan paired with its exact, sealed-store-only operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationProtectedPlanRecordV1 {
    pub plan: ProtectedChangePlan,
    pub operation: ConfigurationProtectedOperationV1,
}

impl ConfigurationProtectedPlanRecordV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.plan.validate()?;
        self.operation.validate()?;
        if self.plan.operation_digest != self.operation.operation_digest()? {
            return Err(DomainError::SnapshotMismatch {
                field: "configuration protected plan operation binding",
            });
        }
        Ok(())
    }
}

/// Append-only configuration persistence contract.
pub trait ConfigurationRevisionStore {
    fn current_revision(
        &self,
    ) -> impl Future<Output = ConfigurationStoreResult<ConfigurationRevisionRecordV1>> + Send;

    fn read_revision(
        &self,
        revision_id: &ConfigurationRevisionId,
    ) -> impl Future<Output = ConfigurationStoreResult<Option<ConfigurationRevisionRecordV1>>> + Send;

    fn save_change_plan(
        &self,
        plan: &ConfigurationProtectedPlanRecordV1,
    ) -> impl Future<Output = ConfigurationStoreResult<()>> + Send;

    fn read_change_plan(
        &self,
        plan_id: &tracedecay_domain::configuration::ChangePlanId,
    ) -> impl Future<Output = ConfigurationStoreResult<Option<ConfigurationProtectedPlanRecordV1>>> + Send;

    fn commit(
        &self,
        commit: ConfigurationCommitV1,
    ) -> impl Future<Output = ConfigurationStoreResult<ConfigurationMutationReceiptV1>> + Send;

    fn audit(
        &self,
        after: Option<&tracedecay_domain::configuration::ConfigurationAuditEventId>,
        limit: usize,
    ) -> impl Future<Output = ConfigurationStoreResult<Vec<ConfigurationAuditEvent>>> + Send;
}
