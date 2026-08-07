//! Transport-neutral native integration preview/apply/status/cancel boundary.
//!
//! Requests bind exact project/repository/worktree/ref/commit/tree evidence.
//! Filesystem paths, free-form object IDs, Git arguments, commit messages,
//! remotes, and provider mutations are intentionally unrepresentable.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    BranchStackId, BranchStackRevisionId, ManifestDigest, NativeIntegrationApprovalV1,
    NativeIntegrationDirectionV1, NativeIntegrationPreviewId, NativeIntegrationPreviewV1,
    NativeIntegrationReceiptV1, NativeIntegrationSelectionV1, NativeIntegrationTerminalOutcomeV1,
    NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1, StackNodeId, UtcMicros,
    WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
};

use crate::{
    ApplicationContractError, CancellationSignal, RequestAdmission, RequestContext, ResolvedScope,
};

/// Caller-visible selection proof. The topology authority resolves it into an
/// immutable domain selection and never discovers roots or edges.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "binding", rename_all = "snake_case")]
pub enum NativeIntegrationSelectionBindingV1 {
    DeclaredStackEdge {
        stack_id: BranchStackId,
        revision_id: BranchStackRevisionId,
        revision_digest: ManifestDigest,
        source_node_id: StackNodeId,
        destination_node_id: StackNodeId,
        direction: NativeIntegrationDirectionV1,
    },
    IndependentBranch {
        proposal_digest: ManifestDigest,
    },
}

impl NativeIntegrationSelectionBindingV1 {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        match self {
            Self::DeclaredStackEdge {
                stack_id,
                revision_id,
                revision_digest,
                source_node_id,
                destination_node_id,
                direction,
            } => {
                stack_id.validate()?;
                revision_id.validate()?;
                revision_digest.validate()?;
                source_node_id.validate()?;
                destination_node_id.validate()?;
                if source_node_id == destination_node_id
                    || *direction == NativeIntegrationDirectionV1::IntegrateIndependentBranch
                {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "native integration stack edge",
                    });
                }
            }
            Self::IndependentBranch { proposal_digest } => proposal_digest.validate()?,
        }
        Ok(())
    }
}

/// Exact Plan 16 identity passed to the injected topology authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationStackResolutionRequestV1 {
    pub source: ResolvedScope,
    pub destination: ResolvedScope,
    pub authorized_scope_set_digest: ManifestDigest,
    pub inventory_snapshot_id: WorktreeInventorySnapshotId,
    pub inventory_epoch: WorktreeInventoryEpoch,
    pub selection: NativeIntegrationSelectionBindingV1,
    pub grant_digest: ManifestDigest,
    pub policy_digest: ManifestDigest,
    pub observed_at: UtcMicros,
}

impl NativeIntegrationStackResolutionRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.source.validate()?;
        self.destination.validate()?;
        self.authorized_scope_set_digest.validate()?;
        self.inventory_snapshot_id.validate()?;
        self.inventory_epoch.validate()?;
        self.selection.validate()?;
        self.grant_digest.validate()?;
        self.policy_digest.validate()?;
        if self.source.project_id != self.destination.project_id
            || self.source.repository_id != self.destination.repository_id
            || self.source.worktree_id == self.destination.worktree_id
            || self.source.reference.is_none()
            || self.destination.reference.is_none()
            || self.source.reference == self.destination.reference
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "native integration exact root pair",
            });
        }
        Ok(())
    }
}

/// Typed graph/topology resolution. Hidden or denied roots reveal no topology.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeIntegrationStackResolutionOutcomeV1 {
    Complete(Box<NativeIntegrationSelectionV1>),
    Partial,
    Stale,
    Denied,
    Unavailable,
    ResetRequired,
    DurabilityUncertain,
}

/// Injected canonical project-graph/topology query. Implementations bind this
/// request to the daemon's one project/profile graph registry; they never open
/// a graph store from this application layer.
pub trait NativeIntegrationStackResolutionPort: Send + Sync {
    fn resolve(
        &self,
        request: &NativeIntegrationStackResolutionRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationPortError>;
}

/// Exact semantic evidence revisions joined to native conflict evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationEvidenceRevisionsV1 {
    pub graph_revision_digest: ManifestDigest,
    pub test_revision_digest: ManifestDigest,
    pub schema_revision_digest: ManifestDigest,
    pub migration_revision_digest: ManifestDigest,
}

impl NativeIntegrationEvidenceRevisionsV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.graph_revision_digest.validate()?;
        self.test_revision_digest.validate()?;
        self.schema_revision_digest.validate()?;
        self.migration_revision_digest.validate()?;
        Ok(())
    }
}

/// Read-only preflight request. `preferred_mode` can only select one of the
/// three fixed mechanical encodings; it cannot change topology or commits.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationPreflightRequestV1 {
    pub context: RequestContext,
    pub topology: NativeIntegrationStackResolutionRequestV1,
    pub evidence: NativeIntegrationEvidenceRevisionsV1,
    pub preview_id: NativeIntegrationPreviewId,
    pub preferred_mode: Option<tracedecay_domain::MechanicalIntegrationModeV1>,
    pub preview_expires_at: UtcMicros,
    pub observed_at: UtcMicros,
}

/// Truthful read-only outcome when exact topology or native evidence is not
/// available. These states never mint an applicable preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeIntegrationPreflightOutcomeV1 {
    Preview(Box<NativeIntegrationPreviewV1>),
    Partial,
    Stale,
    Denied,
    Unavailable,
    ResetRequired,
    DurabilityUncertain,
    Cancelled,
}

impl NativeIntegrationPreflightRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.context.admission_at(self.observed_at) != RequestAdmission::Admitted {
            return Err(ApplicationContractError::Inconsistent {
                field: "native integration preflight admission",
            });
        }
        self.topology.validate()?;
        self.evidence.validate()?;
        self.preview_id.validate()?;
        if self.context.scope().project_id != self.topology.destination.project_id
            || self.context.scope().repository_id != self.topology.destination.repository_id
            || self.context.scope().worktree_id != self.topology.destination.worktree_id
            || self.observed_at.0 >= self.preview_expires_at.0
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "native integration preflight binding",
            });
        }
        Ok(())
    }
}

/// Exact one-use apply request.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationApplyRequestV1 {
    pub context: RequestContext,
    pub transaction_id: NativeIntegrationTransactionId,
    pub preview: NativeIntegrationPreviewV1,
    pub approval: NativeIntegrationApprovalV1,
    pub observed_at: UtcMicros,
}

impl NativeIntegrationApplyRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.context.admission_at(self.observed_at) != RequestAdmission::Admitted {
            return Err(ApplicationContractError::Inconsistent {
                field: "native integration apply admission",
            });
        }
        self.transaction_id.validate()?;
        self.preview.validate()?;
        self.approval.validate()?;
        if self.approval.preview_id != self.preview.preview_id
            || self.approval.preview_digest != self.preview.preview_digest
            || self.approval.grant_digest != self.preview.grant_digest
            || self.context.actor() != &self.approval.principal
            || self.context.scope().project_id != self.preview.repository_snapshot.project_id
            || self.context.scope().repository_id != self.preview.repository_snapshot.repository_id
            || self.context.scope().reference.as_ref()
                != Some(&self.preview.repository_snapshot.destination_ref)
            || self.preview.expires_at.0 <= self.observed_at.0
            || self.approval.expires_at.0 <= self.observed_at.0
            || !matches!(
                self.preview.disposition,
                tracedecay_domain::NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_)
            )
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "native integration apply preview approval",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationStatusRequestV1 {
    pub transaction_id: NativeIntegrationTransactionId,
}

impl NativeIntegrationStatusRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.transaction_id.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationCancelRequestV1 {
    pub transaction_id: NativeIntegrationTransactionId,
    pub requested_at: UtcMicros,
}

impl NativeIntegrationCancelRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.transaction_id.validate()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationCancelDispositionV1 {
    CancellationRequested,
    AlreadyTerminal(NativeIntegrationTerminalOutcomeV1),
    CommitPointPassed,
    UnknownTransaction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeIntegrationRecoveryRequestV1 {
    pub transaction_id: NativeIntegrationTransactionId,
    pub observed_at: UtcMicros,
}

/// Closed runtime boundary. Mutation, journal fsync, repository queues,
/// one-use approval CAS, native object/ref writes, rollback, and restart
/// recovery remain behind this port.
pub trait NativeIntegrationPort {
    fn preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, NativeIntegrationPortError>;

    fn apply(
        &self,
        request: &NativeIntegrationApplyRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationPortError>;

    fn status(
        &self,
        request: &NativeIntegrationStatusRequestV1,
    ) -> Result<Option<NativeIntegrationTransactionStatusV1>, NativeIntegrationPortError>;

    fn cancel(
        &self,
        request: &NativeIntegrationCancelRequestV1,
    ) -> Result<NativeIntegrationCancelDispositionV1, NativeIntegrationPortError>;

    fn recover(
        &self,
        request: &NativeIntegrationRecoveryRequestV1,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationPortError>;
}

/// Stable failure taxonomy. Read-only partial/unavailable is represented by a
/// preview disposition; these failures mean the operation itself could not
/// produce a trustworthy result.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum NativeIntegrationPortError {
    #[error("native integration authority is unavailable")]
    Unavailable,
    #[error("native integration selection or preview is stale")]
    Stale,
    #[error("native integration authorization was denied")]
    Denied,
    #[error("native integration approval was already consumed or conflicts")]
    ApprovalConflict,
    #[error("native integration transaction compare-and-set failed")]
    TransactionConflict,
    #[error("native integration was cancelled before the commit point")]
    Cancelled,
    #[error("native integration requires recovery")]
    RecoveryRequired,
    #[error("native integration requires inspection")]
    NeedsInspection,
    #[error("native integration durable state requires reset")]
    ResetRequired,
    #[error("native integration durable outcome is uncertain")]
    DurabilityUncertain,
    #[error("native integration failed: {0}")]
    Native(String),
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum NativeIntegrationContractError {
    #[error(transparent)]
    Contract(#[from] ApplicationContractError),
    #[error(transparent)]
    Port(#[from] NativeIntegrationPortError),
}

/// Thin application validator over the one runtime kernel.
pub struct NativeIntegrationService<P> {
    port: P,
}

impl<P: NativeIntegrationPort> NativeIntegrationService<P> {
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    pub fn preflight(
        &self,
        request: NativeIntegrationPreflightRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, NativeIntegrationContractError> {
        request.validate()?;
        let outcome = self.port.preflight(&request, cancellation)?;
        if let NativeIntegrationPreflightOutcomeV1::Preview(preview) = &outcome {
            preview.validate().map_err(ApplicationContractError::from)?;
        }
        Ok(outcome)
    }

    pub fn apply(
        &self,
        request: NativeIntegrationApplyRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationContractError> {
        request.validate()?;
        let receipt = self.port.apply(&request, cancellation)?;
        receipt.validate().map_err(ApplicationContractError::from)?;
        Ok(receipt)
    }

    pub fn status(
        &self,
        request: NativeIntegrationStatusRequestV1,
    ) -> Result<Option<NativeIntegrationTransactionStatusV1>, NativeIntegrationContractError> {
        request.validate()?;
        let status = self.port.status(&request)?;
        if let Some(status) = &status {
            status.validate().map_err(ApplicationContractError::from)?;
        }
        Ok(status)
    }

    pub fn cancel(
        &self,
        request: NativeIntegrationCancelRequestV1,
    ) -> Result<NativeIntegrationCancelDispositionV1, NativeIntegrationContractError> {
        request.validate()?;
        Ok(self.port.cancel(&request)?)
    }
}
