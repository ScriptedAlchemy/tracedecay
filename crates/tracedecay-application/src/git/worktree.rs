//! Explicit-root native worktree inventory and cleanup contracts.
//!
//! This module deliberately keeps paths and Git command lines out of the
//! application boundary. A request carries the persisted scope-set identity
//! and one explicit project/repository(/worktree) target. The daemon resolves
//! that identity through its registered scope-set store and supplies the
//! native Git authority; callers cannot submit an `AuthorizedScopeSet`, use a
//! CWD, or widen a target by naming a path.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::git::{GitOidV1, GitOperationStateV1};
use tracedecay_domain::{
    ManifestDigest, ProjectId, RefId, RepositoryId, ScopeSetId, ScopeSetRevision, UtcMicros,
    WorktreeId, WorktreeInventoryEpoch, WorktreeInventorySnapshotId, canonical_sha256,
};

use crate::{AuthorizedScopeSet, CancellationSignal};

/// Canonical operation names for the explicit-root worktree journey.
pub const NATIVE_INTEGRATION_WORKTREE_INVENTORY_OPERATION: &str = "worktree_inventory";
pub const NATIVE_INTEGRATION_WORKTREE_INSPECT_OPERATION: &str = "worktree_cleanup_inspect";
pub const NATIVE_INTEGRATION_WORKTREE_CONFIRM_OPERATION: &str = "worktree_cleanup_confirm";
pub const NATIVE_INTEGRATION_WORKTREE_REMOVE_OPERATION: &str = "worktree_cleanup_remove";
pub const NATIVE_INTEGRATION_WORKTREE_RECONCILE_OPERATION: &str = "worktree_cleanup_reconcile";

/// A target selected by exact registered identity. Repository inventory names
/// one repository; cleanup names one worktree within that repository.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeWorktreeTargetV1 {
    Repository {
        project_id: ProjectId,
        repository_id: RepositoryId,
    },
    Worktree {
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
    },
}

impl NativeWorktreeTargetV1 {
    pub fn validate(&self) -> Result<(), WorktreeContractError> {
        match self {
            Self::Repository {
                project_id,
                repository_id,
            } => {
                project_id.validate()?;
                repository_id.validate()?;
            }
            Self::Worktree {
                project_id,
                repository_id,
                worktree_id,
            } => {
                project_id.validate()?;
                repository_id.validate()?;
                worktree_id.validate()?;
            }
        }
        Ok(())
    }

    pub fn project_id(&self) -> &ProjectId {
        match self {
            Self::Repository { project_id, .. } | Self::Worktree { project_id, .. } => project_id,
        }
    }

    pub fn repository_id(&self) -> &RepositoryId {
        match self {
            Self::Repository { repository_id, .. } | Self::Worktree { repository_id, .. } => {
                repository_id
            }
        }
    }

    pub fn worktree_id(&self) -> Option<&WorktreeId> {
        match self {
            Self::Repository { .. } => None,
            Self::Worktree { worktree_id, .. } => Some(worktree_id),
        }
    }
}

/// Shared persisted authorization binding present on every operation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeWorktreeScopeBindingV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub target: NativeWorktreeTargetV1,
}

impl NativeWorktreeScopeBindingV1 {
    pub fn validate(&self) -> Result<(), WorktreeContractError> {
        self.scope_set_id.validate()?;
        self.scope_set_revision.validate()?;
        self.scope_set_digest.validate()?;
        self.target.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeInventoryRequestV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub target: NativeWorktreeTargetV1,
}

impl WorktreeInventoryRequestV1 {
    pub fn binding(&self) -> NativeWorktreeScopeBindingV1 {
        NativeWorktreeScopeBindingV1 {
            scope_set_id: self.scope_set_id.clone(),
            scope_set_revision: self.scope_set_revision,
            scope_set_digest: self.scope_set_digest.clone(),
            target: self.target.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), WorktreeContractError> {
        self.binding().validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeCleanupInspectRequestV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub target: NativeWorktreeTargetV1,
}

impl WorktreeCleanupInspectRequestV1 {
    pub fn binding(&self) -> NativeWorktreeScopeBindingV1 {
        NativeWorktreeScopeBindingV1 {
            scope_set_id: self.scope_set_id.clone(),
            scope_set_revision: self.scope_set_revision,
            scope_set_digest: self.scope_set_digest.clone(),
            target: self.target.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), WorktreeContractError> {
        self.binding().validate()?;
        if self.target.worktree_id().is_none() {
            return Err(WorktreeContractError::Inconsistent {
                field: "cleanup target worktree",
            });
        }
        Ok(())
    }
}

/// Confirmation names the exact inspection digest, not just a worktree id.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeCleanupConfirmRequestV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub target: NativeWorktreeTargetV1,
    pub inspection_digest: ManifestDigest,
}

impl WorktreeCleanupConfirmRequestV1 {
    pub fn binding(&self) -> NativeWorktreeScopeBindingV1 {
        NativeWorktreeScopeBindingV1 {
            scope_set_id: self.scope_set_id.clone(),
            scope_set_revision: self.scope_set_revision,
            scope_set_digest: self.scope_set_digest.clone(),
            target: self.target.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), WorktreeContractError> {
        self.binding().validate()?;
        self.inspection_digest.validate()?;
        if self.target.worktree_id().is_none() {
            return Err(WorktreeContractError::Inconsistent {
                field: "cleanup target worktree",
            });
        }
        Ok(())
    }
}

/// Removal carries both proofs so a stale confirmation cannot be replayed
/// against another inspection of the same opaque worktree identity.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeCleanupRemoveRequestV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub target: NativeWorktreeTargetV1,
    pub inspection_digest: ManifestDigest,
    pub confirmation_digest: ManifestDigest,
}

impl WorktreeCleanupRemoveRequestV1 {
    pub fn binding(&self) -> NativeWorktreeScopeBindingV1 {
        NativeWorktreeScopeBindingV1 {
            scope_set_id: self.scope_set_id.clone(),
            scope_set_revision: self.scope_set_revision,
            scope_set_digest: self.scope_set_digest.clone(),
            target: self.target.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), WorktreeContractError> {
        self.binding().validate()?;
        self.inspection_digest.validate()?;
        self.confirmation_digest.validate()?;
        if self.target.worktree_id().is_none() {
            return Err(WorktreeContractError::Inconsistent {
                field: "cleanup target worktree",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeCleanupReconcileRequestV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub target: NativeWorktreeTargetV1,
    pub confirmation_digest: ManifestDigest,
}

/// Transport envelope used by CLI, MCP, HTTP, and the daemon invocation
/// contract. Each operation keeps its own request type and therefore cannot
/// accidentally accept a cleanup proof on inventory or vice versa.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub enum NativeWorktreeSurfaceRequest {
    Inventory(WorktreeInventoryRequestV1),
    Inspect(WorktreeCleanupInspectRequestV1),
    Confirm(WorktreeCleanupConfirmRequestV1),
    Remove(WorktreeCleanupRemoveRequestV1),
    Reconcile(WorktreeCleanupReconcileRequestV1),
}

impl NativeWorktreeSurfaceRequest {
    pub const fn operation(&self) -> &'static str {
        match self {
            Self::Inventory(_) => NATIVE_INTEGRATION_WORKTREE_INVENTORY_OPERATION,
            Self::Inspect(_) => NATIVE_INTEGRATION_WORKTREE_INSPECT_OPERATION,
            Self::Confirm(_) => NATIVE_INTEGRATION_WORKTREE_CONFIRM_OPERATION,
            Self::Remove(_) => NATIVE_INTEGRATION_WORKTREE_REMOVE_OPERATION,
            Self::Reconcile(_) => NATIVE_INTEGRATION_WORKTREE_RECONCILE_OPERATION,
        }
    }
}

impl WorktreeCleanupReconcileRequestV1 {
    pub fn binding(&self) -> NativeWorktreeScopeBindingV1 {
        NativeWorktreeScopeBindingV1 {
            scope_set_id: self.scope_set_id.clone(),
            scope_set_revision: self.scope_set_revision,
            scope_set_digest: self.scope_set_digest.clone(),
            target: self.target.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), WorktreeContractError> {
        self.binding().validate()?;
        self.confirmation_digest.validate()?;
        if self.target.worktree_id().is_none() {
            return Err(WorktreeContractError::Inconsistent {
                field: "cleanup target worktree",
            });
        }
        Ok(())
    }
}

/// Native Git worktree kind visible to an authorized caller.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeKindV1 {
    Main,
    Linked,
    Bare,
}

/// Presence is intentionally not a bool: stale, unavailable and foreign are
/// policy-safe states with different reconciliation consequences.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreePresenceV1 {
    Present,
    Stale,
    Unavailable,
    Foreign,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeObservationV1 {
    Yes,
    No,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCoverageV1 {
    Complete,
    Partial,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeInventoryEntryV1 {
    pub target: NativeWorktreeTargetV1,
    pub presence: WorktreePresenceV1,
    pub kind: Option<WorktreeKindV1>,
    pub worktree_id: Option<WorktreeId>,
    pub reference: Option<RefId>,
    pub head: Option<GitOidV1>,
    pub clean: WorktreeObservationV1,
    pub locked: WorktreeObservationV1,
    pub holder: WorktreeObservationV1,
    pub unique_data: WorktreeObservationV1,
    pub operation: Option<GitOperationStateV1>,
    pub observed_at: UtcMicros,
    pub evidence_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeInventorySnapshotV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub snapshot_id: WorktreeInventorySnapshotId,
    pub epoch: WorktreeInventoryEpoch,
    pub entries: Vec<WorktreeInventoryEntryV1>,
    pub coverage: WorktreeCoverageV1,
    pub observed_at: UtcMicros,
    pub digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeInspectionV1 {
    pub target: NativeWorktreeTargetV1,
    pub presence: WorktreePresenceV1,
    pub kind: Option<WorktreeKindV1>,
    pub worktree_id: WorktreeId,
    pub reference: Option<RefId>,
    pub head: Option<GitOidV1>,
    pub clean: WorktreeObservationV1,
    pub locked: WorktreeObservationV1,
    pub holder: WorktreeObservationV1,
    pub unique_data: WorktreeObservationV1,
    pub operation: Option<GitOperationStateV1>,
    pub observed_at: UtcMicros,
    pub inspection_digest: ManifestDigest,
}

impl WorktreeInspectionV1 {
    pub fn removal_eligible(&self) -> bool {
        self.presence == WorktreePresenceV1::Present
            && self.kind == Some(WorktreeKindV1::Linked)
            && self.clean == WorktreeObservationV1::No
            && self.locked == WorktreeObservationV1::No
            && self.holder == WorktreeObservationV1::No
            && self.unique_data == WorktreeObservationV1::No
            && self.operation.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeCleanupConfirmationV1 {
    pub target: NativeWorktreeTargetV1,
    pub inspection_digest: ManifestDigest,
    pub confirmation_digest: ManifestDigest,
    pub confirmed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WorktreeCleanupRemovalV1 {
    Removed {
        confirmation_digest: ManifestDigest,
        observed_at: UtcMicros,
    },
    AlreadyRemoved {
        confirmation_digest: ManifestDigest,
        observed_at: UtcMicros,
    },
    Denied,
    Stale,
    DurabilityUncertain,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WorktreeCleanupReconciliationV1 {
    Removed {
        confirmation_digest: ManifestDigest,
        observed_at: UtcMicros,
    },
    StillPresent,
    DurabilityUncertain,
    Stale,
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WorktreeInventoryOutcomeV1 {
    Snapshot(Box<WorktreeInventorySnapshotV1>),
    Stale,
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WorktreeInspectionOutcomeV1 {
    Inspection(Box<WorktreeInspectionV1>),
    Stale,
    Foreign,
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum WorktreeConfirmationOutcomeV1 {
    Confirmed(Box<WorktreeCleanupConfirmationV1>),
    Stale,
    Denied,
    NeedsInspection,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum NativeWorktreeSurfaceResultV1 {
    Inventory(WorktreeInventoryOutcomeV1),
    Inspection(WorktreeInspectionOutcomeV1),
    Confirmation(WorktreeConfirmationOutcomeV1),
    Removal(WorktreeCleanupRemovalV1),
    Reconciliation(WorktreeCleanupReconciliationV1),
}

#[derive(Debug, Error)]
pub enum WorktreeContractError {
    #[error("worktree contract identity is invalid: {0}")]
    Domain(#[from] tracedecay_domain::DomainError),
    #[error("worktree contract is inconsistent: {field}")]
    Inconsistent { field: &'static str },
    #[error("authorized scope-set authority is unavailable")]
    ScopeSetUnavailable,
    #[error("authorized scope-set was not found or is not authorized")]
    ScopeSetDenied,
    #[error("native worktree authority is unavailable")]
    AuthorityUnavailable,
    #[error("native worktree authority denied the target")]
    Denied,
    #[error("native worktree evidence is stale")]
    Stale,
    #[error("native worktree operation is uncertain and requires reconciliation")]
    DurabilityUncertain,
    #[error("native worktree operation failed: {0}")]
    Native(String),
}

pub trait AuthorizedScopeSetPort: Send + Sync {
    fn read(
        &self,
        scope_set_id: &ScopeSetId,
    ) -> Result<Option<AuthorizedScopeSet>, WorktreeContractError>;
}

pub trait NativeWorktreePort: Send + Sync {
    fn inventory(
        &self,
        request: &WorktreeInventoryRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeInventoryOutcomeV1, WorktreeContractError>;

    fn inspect(
        &self,
        request: &WorktreeCleanupInspectRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeInspectionOutcomeV1, WorktreeContractError>;

    fn confirm(
        &self,
        request: &WorktreeCleanupConfirmRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeConfirmationOutcomeV1, WorktreeContractError>;

    fn remove(
        &self,
        request: &WorktreeCleanupRemoveRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeCleanupRemovalV1, WorktreeContractError>;

    fn reconcile(
        &self,
        request: &WorktreeCleanupReconcileRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeCleanupReconciliationV1, WorktreeContractError>;
}

/// Application service that makes persisted scope-set identity the sole
/// authorization input before any native operation is called.
pub struct NativeWorktreeService<S, P> {
    scope_sets: S,
    port: P,
}

impl<S, P> NativeWorktreeService<S, P>
where
    S: AuthorizedScopeSetPort,
    P: NativeWorktreePort,
{
    pub const fn new(scope_sets: S, port: P) -> Self {
        Self { scope_sets, port }
    }

    fn authorize(
        &self,
        binding: &NativeWorktreeScopeBindingV1,
    ) -> Result<AuthorizedScopeSet, WorktreeContractError> {
        binding.validate()?;
        let scope_set = self
            .scope_sets
            .read(&binding.scope_set_id)?
            .ok_or(WorktreeContractError::ScopeSetDenied)?;
        scope_set
            .validate()
            .map_err(|_| WorktreeContractError::ScopeSetDenied)?;
        if scope_set.revision() != binding.scope_set_revision
            || scope_set.digest() != &binding.scope_set_digest
        {
            return Err(WorktreeContractError::Stale);
        }
        let authorized = scope_set.roots().iter().any(|root| {
            root.scope().project_id == *binding.target.project_id()
                && root.scope().repository_id == *binding.target.repository_id()
                && binding
                    .target
                    .worktree_id()
                    .is_none_or(|worktree| root.scope().worktree_id == *worktree)
        });
        if !authorized {
            return Err(WorktreeContractError::Denied);
        }
        Ok(scope_set)
    }

    pub fn inventory(
        &self,
        request: &WorktreeInventoryRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeInventoryOutcomeV1, WorktreeContractError> {
        request.validate()?;
        let scope_set = self.authorize(&request.binding())?;
        self.port.inventory(request, &scope_set, cancellation)
    }

    pub fn inspect(
        &self,
        request: &WorktreeCleanupInspectRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeInspectionOutcomeV1, WorktreeContractError> {
        request.validate()?;
        let scope_set = self.authorize(&request.binding())?;
        self.port.inspect(request, &scope_set, cancellation)
    }

    pub fn confirm(
        &self,
        request: &WorktreeCleanupConfirmRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeConfirmationOutcomeV1, WorktreeContractError> {
        request.validate()?;
        let scope_set = self.authorize(&request.binding())?;
        self.port.confirm(request, &scope_set, cancellation)
    }

    pub fn remove(
        &self,
        request: &WorktreeCleanupRemoveRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeCleanupRemovalV1, WorktreeContractError> {
        request.validate()?;
        let scope_set = self.authorize(&request.binding())?;
        self.port.remove(request, &scope_set, cancellation)
    }

    pub fn reconcile(
        &self,
        request: &WorktreeCleanupReconcileRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeCleanupReconciliationV1, WorktreeContractError> {
        request.validate()?;
        let scope_set = self.authorize(&request.binding())?;
        self.port.reconcile(request, &scope_set, cancellation)
    }
}

/// Seal one inspection digest after all fields have been observed. Ports use
/// this helper when issuing a confirmation; callers only ever receive the
/// resulting digest.
pub fn worktree_inspection_digest(
    inspection: &WorktreeInspectionV1,
) -> Result<ManifestDigest, WorktreeContractError> {
    canonical_sha256(inspection).map_err(|_| WorktreeContractError::Inconsistent {
        field: "worktree inspection digest",
    })
}

pub fn worktree_confirmation_digest(
    target: &NativeWorktreeTargetV1,
    inspection_digest: &ManifestDigest,
    _confirmed_at: UtcMicros,
) -> Result<ManifestDigest, WorktreeContractError> {
    // The confirmation is replayable after a daemon restart. The observed
    // inspection, not the server timestamp, is the proof identity; the
    // timestamp remains audit metadata on the confirmation projection.
    canonical_sha256(&(
        "tracedecay.native-worktree-confirmation.v1",
        target,
        inspection_digest,
    ))
    .map_err(|_| WorktreeContractError::Inconsistent {
        field: "worktree confirmation digest",
    })
}
