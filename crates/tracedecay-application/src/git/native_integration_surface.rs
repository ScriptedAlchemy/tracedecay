//! Public native-integration surface: `stack_snapshot`,
//! `preflight_native_integration`, `approve_native_integration`,
//! `apply_native_integration`, `native_integration_status`, and
//! `cancel_native_integration`.
//!
//! Plan 36 slice 1 extends "the shipped application and CLI/MCP surfaces with
//! `stack_snapshot` and `preflight_native_integration`", slice 3 adds
//! `apply_native_integration`, `native_integration_status`, and
//! `cancel_native_integration`, `approve_native_integration` is the
//! owner-decided (2026-08-07) sixth operation that issues the one-use
//! apply approval, and slice 4 requires the whole journey to be
//! exposed consistently through CLI and MCP over one application result. That
//! is a different family from the Plan 08 Git *index-transaction* bindings,
//! which stay limited to `git_preview`/`git_apply`; this module never exposes
//! `stage_hunks`, `unstage_hunks`, or `commit_index`.
//!
//! Requests carry exact typed identity only. Filesystem paths, free-form
//! object IDs, Git arguments, commit messages, remotes, branch display names,
//! and provider topology are unrepresentable here. Results are bounded
//! projections: identity, digests, disposition, and audit metadata, never
//! patch or source bodies.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ActorId, CapabilityId as DomainCapabilityId, ManifestDigest, MechanicalIntegrationModeV1,
    NativeIntegrationApprovalId, NativeIntegrationApprovalV1, NativeIntegrationPhaseV1,
    NativeIntegrationPreviewDispositionV1, NativeIntegrationPreviewId, NativeIntegrationPreviewV1,
    NativeIntegrationReceiptV1, NativeIntegrationSelectionV1, NativeIntegrationTerminalOutcomeV1,
    NativeIntegrationTransactionId, NativeIntegrationTransactionStatusV1, ProjectId, RefId,
    RepositoryId, UtcMicros, WorktreeInventoryEpoch,
};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, CodecBindingKey, ContributionId,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    ExecutableSchemaAuthority, IdempotencyContract, InverseContract, InverseUnavailableReason,
    LifecycleClass, OperationId, PrivacyClass, ProfileId, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RouteExposureV1, RoutingContractV1, SchemaId,
    SchemaRef, ScopeDimension, ScopeRequirement, ServiceId, StreamingContract, TerminalState,
    TerminalStateContract, UseCaseId,
};

use crate::CancellationSignal;
use crate::current_bindings;
use crate::error::ApplicationContractError;
use crate::git::native_integration::{
    NativeIntegrationCancelDispositionV1, NativeIntegrationPortError,
    NativeIntegrationPreflightOutcomeV1, NativeIntegrationStackResolutionOutcomeV1,
    NativeIntegrationStackResolutionPort, NativeIntegrationStackResolutionRequestV1,
};
use crate::git::worktree::{
    NativeWorktreeSurfaceResultV1, WorktreeCleanupConfirmRequestV1,
    WorktreeCleanupInspectRequestV1, WorktreeCleanupReconcileRequestV1, WorktreeCleanupRemovalV1,
    WorktreeCleanupRemoveRequestV1, WorktreeInventoryRequestV1,
};
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;
mod stack_snapshot;

pub use stack_snapshot::NativeIntegrationStackSnapshotSurfaceRequest;

/// Canonical wire operation names for the native-integration journey.
pub const NATIVE_INTEGRATION_STACK_SNAPSHOT_OPERATION: &str = "stack_snapshot";
pub const NATIVE_INTEGRATION_PREFLIGHT_OPERATION: &str = "preflight_native_integration";
pub const NATIVE_INTEGRATION_APPROVE_OPERATION: &str = "approve_native_integration";
pub const NATIVE_INTEGRATION_APPLY_OPERATION: &str = "apply_native_integration";
pub const NATIVE_INTEGRATION_STATUS_OPERATION: &str = "native_integration_status";
pub const NATIVE_INTEGRATION_CANCEL_OPERATION: &str = "cancel_native_integration";
pub use crate::git::worktree::{
    NATIVE_INTEGRATION_WORKTREE_CONFIRM_OPERATION, NATIVE_INTEGRATION_WORKTREE_INSPECT_OPERATION,
    NATIVE_INTEGRATION_WORKTREE_INVENTORY_OPERATION,
    NATIVE_INTEGRATION_WORKTREE_RECONCILE_OPERATION, NATIVE_INTEGRATION_WORKTREE_REMOVE_OPERATION,
};

// ---------------------------------------------------------------------------
// stack_snapshot
// ---------------------------------------------------------------------------

/// Application service for `stack_snapshot`.
///
/// It reauthorizes and freezes the visible node/edge set and inventory epoch
/// before preflight, and never discovers roots, edges, or topology itself: the
/// injected Plan 16 authority answers, and a hidden or denied node reveals no
/// identity, count, or topology through this result.
pub struct NativeIntegrationStackSnapshotService<P> {
    port: P,
}

impl<P: NativeIntegrationStackResolutionPort> NativeIntegrationStackSnapshotService<P> {
    pub const fn new(port: P) -> Self {
        Self { port }
    }

    pub fn snapshot(
        &self,
        request: NativeIntegrationStackResolutionRequestV1,
        cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationStackResolutionOutcomeV1, super::NativeIntegrationContractError>
    {
        request.validate()?;
        let outcome = self.port.resolve(&request, cancellation)?;
        if let NativeIntegrationStackResolutionOutcomeV1::Complete(selection) = &outcome {
            selection
                .validate()
                .map_err(ApplicationContractError::from)?;
        }
        Ok(outcome)
    }
}

// ---------------------------------------------------------------------------
// Remaining surface requests
// ---------------------------------------------------------------------------

/// Exact semantic evidence revisions joined to native conflict evidence.
///
/// Mirrors [`super::NativeIntegrationEvidenceRevisionsV1`] on the wire; the
/// application type stays the single validation authority.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationEvidenceRevisionsWireV1 {
    pub graph_revision_digest: ManifestDigest,
    pub test_revision_digest: ManifestDigest,
    pub schema_revision_digest: ManifestDigest,
    pub migration_revision_digest: ManifestDigest,
}

impl From<NativeIntegrationEvidenceRevisionsWireV1>
    for super::NativeIntegrationEvidenceRevisionsV1
{
    fn from(value: NativeIntegrationEvidenceRevisionsWireV1) -> Self {
        Self {
            graph_revision_digest: value.graph_revision_digest,
            test_revision_digest: value.test_revision_digest,
            schema_revision_digest: value.schema_revision_digest,
            migration_revision_digest: value.migration_revision_digest,
        }
    }
}

/// Read-only preflight over one frozen snapshot identity.
///
/// `preferred_mode` selects only one of the three fixed mechanical encodings.
/// It cannot change topology, commit order, or the commit set.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationPreflightSurfaceRequest {
    pub snapshot: NativeIntegrationStackSnapshotSurfaceRequest,
    pub evidence: NativeIntegrationEvidenceRevisionsWireV1,
    #[serde(default)]
    pub preferred_mode: Option<MechanicalIntegrationModeV1>,
}

/// Exact approval-issuance request (the owner-decided sixth operation).
///
/// The caller names one unexpired preview by exact identity *and* digest;
/// approving an identity without its content digest is unrepresentable. The
/// daemon mints the one-use approval bound to the requesting principal, the
/// apply capability, the current grant lineage, and a bounded expiry — none
/// of which the caller can choose.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationApproveSurfaceRequest {
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
}

/// Exact one-use apply request.
///
/// Apply accepts only an unexpired preview identity/digest plus a one-use
/// content-bound approval. Arbitrary Git arguments, caller-supplied paths,
/// SHAs, patches, commit lists, messages, environment, or config are
/// unrepresentable.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationApplySurfaceRequest {
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
    pub approval_id: NativeIntegrationApprovalId,
    pub approval_digest: ManifestDigest,
    pub transaction_id: NativeIntegrationTransactionId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationStatusSurfaceRequest {
    pub transaction_id: NativeIntegrationTransactionId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationCancelSurfaceRequest {
    pub transaction_id: NativeIntegrationTransactionId,
}

// ---------------------------------------------------------------------------
// Bounded surface result
// ---------------------------------------------------------------------------

/// Why a native-integration operation produced no advancing state.
///
/// Every variant is read-only and truthful. None of them authorizes apply, and
/// a denied or absent target is reported indistinguishably from an unavailable
/// authority so no identity, path, count, or topology leaks.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationSurfaceUnavailableV1 {
    /// No native-integration runtime authority is mounted for this daemon.
    AuthorityUnmounted,
    Partial,
    Stale,
    Denied,
    ResetRequired,
    DurabilityUncertain,
    Cancelled,
    ApprovalConflict,
    TransactionConflict,
    RecoveryRequired,
    NeedsInspection,
    UnknownTransaction,
}

impl From<&NativeIntegrationPortError> for NativeIntegrationSurfaceUnavailableV1 {
    fn from(value: &NativeIntegrationPortError) -> Self {
        match value {
            NativeIntegrationPortError::Unavailable | NativeIntegrationPortError::Native(_) => {
                Self::AuthorityUnmounted
            }
            NativeIntegrationPortError::Stale => Self::Stale,
            NativeIntegrationPortError::Denied => Self::Denied,
            NativeIntegrationPortError::ApprovalConflict => Self::ApprovalConflict,
            NativeIntegrationPortError::TransactionConflict => Self::TransactionConflict,
            NativeIntegrationPortError::Cancelled => Self::Cancelled,
            NativeIntegrationPortError::RecoveryRequired => Self::RecoveryRequired,
            NativeIntegrationPortError::NeedsInspection => Self::NeedsInspection,
            NativeIntegrationPortError::ResetRequired => Self::ResetRequired,
            NativeIntegrationPortError::DurabilityUncertain => Self::DurabilityUncertain,
        }
    }
}

/// Bounded projection of one frozen selection.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationSnapshotProjectionV1 {
    pub selection_digest: ManifestDigest,
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub source_ref: RefId,
    pub destination_ref: RefId,
    pub inventory_epoch: WorktreeInventoryEpoch,
    pub frozen_at: UtcMicros,
}

impl NativeIntegrationSnapshotProjectionV1 {
    /// Project one frozen selection. Every field is read from the selection
    /// itself, so a caller cannot restate an epoch or capture time the
    /// authority did not freeze.
    pub fn project(
        selection: &NativeIntegrationSelectionV1,
    ) -> Result<Self, ApplicationContractError> {
        let (inventory_epoch, frozen_at) = match selection {
            NativeIntegrationSelectionV1::DeclaredStackEdge(edge) => {
                (edge.revision.inventory_epoch, edge.captured_at)
            }
            NativeIntegrationSelectionV1::IndependentBranch(branch) => {
                (branch.inventory_epoch, branch.captured_at)
            }
        };
        Ok(Self {
            selection_digest: selection.digest().clone(),
            project_id: selection.project_id()?.clone(),
            repository_id: selection.repository_id()?.clone(),
            source_ref: selection.source_ref()?.clone(),
            destination_ref: selection.destination_ref()?.clone(),
            inventory_epoch,
            frozen_at,
        })
    }
}

/// Bounded projection of one immutable preview.
///
/// Candidate trees, conflict bodies, and ordered commit objects stay behind the
/// preview digest: the surface reports identity, classification, and expiry so
/// a caller can approve exactly this preview and nothing else.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationPreviewProjectionV1 {
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
    pub selection: NativeIntegrationSnapshotProjectionV1,
    pub disposition: NativeIntegrationPreviewDispositionV1,
    pub ordered_commit_count: u32,
    pub created_at: UtcMicros,
    pub expires_at: UtcMicros,
}

impl NativeIntegrationPreviewProjectionV1 {
    pub fn project(preview: &NativeIntegrationPreviewV1) -> Result<Self, ApplicationContractError> {
        Ok(Self {
            preview_id: preview.preview_id.clone(),
            preview_digest: preview.preview_digest.clone(),
            selection: NativeIntegrationSnapshotProjectionV1::project(&preview.selection)?,
            disposition: preview.disposition.clone(),
            ordered_commit_count: u32::try_from(preview.ordered_commits.len()).map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "native integration ordered commit count",
                }
            })?,
            created_at: preview.created_at,
            expires_at: preview.expires_at,
        })
    }
}

/// Bounded projection of one issued one-use approval.
///
/// The approval digest is the caller's proof-of-issuance handle for apply;
/// the preview binding and expiry are audit metadata. No preview body,
/// candidate tree, or commit content crosses this boundary.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationApprovalProjectionV1 {
    pub approval_id: NativeIntegrationApprovalId,
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
    pub principal: ActorId,
    pub capability: DomainCapabilityId,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub approval_digest: ManifestDigest,
}

impl NativeIntegrationApprovalProjectionV1 {
    pub fn project(approval: &NativeIntegrationApprovalV1) -> Self {
        Self {
            approval_id: approval.approval_id.clone(),
            preview_id: approval.preview_id.clone(),
            preview_digest: approval.preview_digest.clone(),
            principal: approval.principal.clone(),
            capability: approval.capability.clone(),
            issued_at: approval.issued_at,
            expires_at: approval.expires_at,
            approval_digest: approval.approval_digest.clone(),
        }
    }
}

/// Bounded projection of one durable transaction status.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationStatusProjectionV1 {
    pub transaction_id: NativeIntegrationTransactionId,
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
    pub repository_id: RepositoryId,
    pub destination_ref: RefId,
    pub phase: NativeIntegrationPhaseV1,
    pub phase_revision: u64,
    pub cancellation_requested: bool,
    pub terminal_outcome: Option<NativeIntegrationTerminalOutcomeV1>,
    pub updated_at: UtcMicros,
}

impl From<&NativeIntegrationTransactionStatusV1> for NativeIntegrationStatusProjectionV1 {
    fn from(status: &NativeIntegrationTransactionStatusV1) -> Self {
        Self {
            transaction_id: status.transaction_id.clone(),
            preview_id: status.preview_id.clone(),
            preview_digest: status.preview_digest.clone(),
            repository_id: status.repository_id.clone(),
            destination_ref: status.destination_ref.clone(),
            phase: status.phase,
            phase_revision: status.phase_revision,
            cancellation_requested: status.cancellation_requested,
            terminal_outcome: status.terminal_outcome,
            updated_at: status.updated_at,
        }
    }
}

/// Bounded projection of one durable terminal receipt.
///
/// The receipt digest and final ref/tree identity are audit metadata; no patch,
/// worktree body, or source content crosses this boundary.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationReceiptProjectionV1 {
    pub status: NativeIntegrationStatusProjectionV1,
    pub terminal_outcome: NativeIntegrationTerminalOutcomeV1,
    pub final_ref_tip: String,
    pub final_tree: String,
    pub completed_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

impl NativeIntegrationReceiptProjectionV1 {
    pub fn project(receipt: &NativeIntegrationReceiptV1) -> Result<Self, ApplicationContractError> {
        let terminal_outcome =
            receipt
                .status
                .terminal_outcome
                .ok_or(ApplicationContractError::Inconsistent {
                    field: "native integration receipt terminal outcome",
                })?;
        Ok(Self {
            status: NativeIntegrationStatusProjectionV1::from(&receipt.status),
            terminal_outcome,
            final_ref_tip: receipt.final_ref_tip.as_str().to_owned(),
            final_tree: receipt.final_tree.as_str().to_owned(),
            completed_at: receipt.completed_at,
            receipt_digest: receipt.receipt_digest.clone(),
        })
    }
}

/// Cancellation disposition. After the native commit point the committed
/// receipt is returned instead of a cancellation claim.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationCancellationProjectionV1 {
    CancellationRequested,
    AlreadyTerminal(NativeIntegrationTerminalOutcomeV1),
    CommitPointPassed,
}

/// One typed result for every native-integration surface operation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum NativeIntegrationSurfaceResultV1 {
    StackSnapshot(NativeIntegrationSnapshotProjectionV1),
    Preview(NativeIntegrationPreviewProjectionV1),
    Approval(NativeIntegrationApprovalProjectionV1),
    Receipt(NativeIntegrationReceiptProjectionV1),
    Status(NativeIntegrationStatusProjectionV1),
    Cancellation(NativeIntegrationCancellationProjectionV1),
    Worktree(NativeWorktreeSurfaceResultV1),
    Unavailable {
        reason: NativeIntegrationSurfaceUnavailableV1,
    },
}

impl NativeIntegrationSurfaceResultV1 {
    pub const fn unavailable(reason: NativeIntegrationSurfaceUnavailableV1) -> Self {
        Self::Unavailable { reason }
    }

    /// Whether this result advanced or proved durable state. Every other
    /// result is read-only evidence and never authorizes apply.
    pub const fn is_advancing(&self) -> bool {
        matches!(
            self,
            Self::Receipt(_)
                | Self::Worktree(NativeWorktreeSurfaceResultV1::Removal(
                    WorktreeCleanupRemovalV1::Removed { .. }
                ))
        )
    }

    pub fn from_stack_resolution(
        outcome: &NativeIntegrationStackResolutionOutcomeV1,
    ) -> Result<Self, ApplicationContractError> {
        Ok(match outcome {
            NativeIntegrationStackResolutionOutcomeV1::Complete(selection) => {
                Self::StackSnapshot(NativeIntegrationSnapshotProjectionV1::project(selection)?)
            }
            NativeIntegrationStackResolutionOutcomeV1::Partial => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::Partial)
            }
            NativeIntegrationStackResolutionOutcomeV1::Stale => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::Stale)
            }
            NativeIntegrationStackResolutionOutcomeV1::Denied => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::Denied)
            }
            NativeIntegrationStackResolutionOutcomeV1::Unavailable => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::AuthorityUnmounted)
            }
            NativeIntegrationStackResolutionOutcomeV1::ResetRequired => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::ResetRequired)
            }
            NativeIntegrationStackResolutionOutcomeV1::DurabilityUncertain => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::DurabilityUncertain)
            }
        })
    }

    pub fn from_preflight(
        outcome: &NativeIntegrationPreflightOutcomeV1,
    ) -> Result<Self, ApplicationContractError> {
        Ok(match outcome {
            NativeIntegrationPreflightOutcomeV1::Preview(preview) => {
                Self::Preview(NativeIntegrationPreviewProjectionV1::project(preview)?)
            }
            NativeIntegrationPreflightOutcomeV1::Partial => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::Partial)
            }
            NativeIntegrationPreflightOutcomeV1::Stale => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::Stale)
            }
            NativeIntegrationPreflightOutcomeV1::Denied => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::Denied)
            }
            NativeIntegrationPreflightOutcomeV1::Unavailable => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::AuthorityUnmounted)
            }
            NativeIntegrationPreflightOutcomeV1::ResetRequired => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::ResetRequired)
            }
            NativeIntegrationPreflightOutcomeV1::DurabilityUncertain => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::DurabilityUncertain)
            }
            NativeIntegrationPreflightOutcomeV1::Cancelled => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::Cancelled)
            }
        })
    }

    pub fn from_cancel(disposition: NativeIntegrationCancelDispositionV1) -> Self {
        match disposition {
            NativeIntegrationCancelDispositionV1::CancellationRequested => {
                Self::Cancellation(NativeIntegrationCancellationProjectionV1::CancellationRequested)
            }
            NativeIntegrationCancelDispositionV1::AlreadyTerminal(outcome) => Self::Cancellation(
                NativeIntegrationCancellationProjectionV1::AlreadyTerminal(outcome),
            ),
            NativeIntegrationCancelDispositionV1::CommitPointPassed => {
                Self::Cancellation(NativeIntegrationCancellationProjectionV1::CommitPointPassed)
            }
            NativeIntegrationCancelDispositionV1::UnknownTransaction => {
                Self::unavailable(NativeIntegrationSurfaceUnavailableV1::UnknownTransaction)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Catalog contribution
// ---------------------------------------------------------------------------

struct NativeIntegrationSurfaceSpec {
    operation: &'static str,
    capability: &'static str,
    use_case: &'static str,
    request_schema: &'static str,
    result_schema: &'static str,
    effect: EffectClass,
    summary: &'static str,
    description: &'static str,
    example: &'static str,
    surfaces: &'static [BindingSurface],
}

/// Native-integration mutations are exposed through CLI and MCP only. HTTP is
/// deliberately excluded for the same reason `git_preview`/`git_apply` are:
/// apply is an authoritative native mutation and there is no transport
/// fallback path.
const NATIVE_INTEGRATION_SURFACES: [BindingSurface; 2] = [BindingSurface::Cli, BindingSurface::Mcp];
/// The read-only status projection additionally serves the dashboard consumer
/// over the same application result. No mutating operation gains a dashboard
/// binding: the dashboard can observe a transaction but never advance one.
const NATIVE_INTEGRATION_STATUS_SURFACES: [BindingSurface; 3] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Dashboard,
];
const NATIVE_WORKTREE_SURFACES: [BindingSurface; 3] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
];

const NATIVE_INTEGRATION_SPECS: [NativeIntegrationSurfaceSpec; 11] = [
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_STACK_SNAPSHOT_OPERATION,
        capability: "capability.application.native-integration.stack-snapshot",
        use_case: "use-case.application.native-integration.stack-snapshot",
        request_schema: "schema.application.native-integration.stack-snapshot.request",
        result_schema: "schema.application.native-integration.stack-snapshot.result",
        effect: EffectClass::Read,
        summary: "Freeze one authorized branch-stack selection",
        description: "Reauthorize and freeze the visible node/edge set, repository tips, and \
                      inventory epoch into the immutable snapshot identity preflight consumes.",
        example: "Freeze this authorized branch-stack edge for preflight",
        surfaces: &NATIVE_INTEGRATION_SURFACES,
    },
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_PREFLIGHT_OPERATION,
        capability: "capability.application.native-integration.preflight",
        use_case: "use-case.application.native-integration.preflight",
        request_schema: "schema.application.native-integration.preflight.request",
        result_schema: "schema.application.native-integration.preflight.result",
        effect: EffectClass::Preview,
        summary: "Preflight one frozen native integration",
        description: "Compute one immutable preview in a private daemon-owned environment \
                      without touching real refs, indexes, or worktrees.",
        example: "Preflight the frozen branch-stack edge",
        surfaces: &NATIVE_INTEGRATION_SURFACES,
    },
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_APPROVE_OPERATION,
        capability: "capability.application.native-integration.approve",
        use_case: "use-case.application.native-integration.approve",
        request_schema: "schema.application.native-integration.approve.request",
        result_schema: "schema.application.native-integration.approve.result",
        effect: EffectClass::Administrative,
        summary: "Issue a one-use approval for one exact preview",
        description: "Mint and durably record one one-use content-bound approval naming the \
                      requesting principal, the apply capability, and the exact preview digest. \
                      Approving an identity without its content digest is unrepresentable.",
        example: "Approve this native-integration preview for apply",
        surfaces: &NATIVE_INTEGRATION_SURFACES,
    },
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_APPLY_OPERATION,
        capability: "capability.application.native-integration.apply",
        use_case: "use-case.application.native-integration.apply",
        request_schema: "schema.application.native-integration.apply.request",
        result_schema: "schema.application.native-integration.apply.result",
        effect: EffectClass::Administrative,
        summary: "Apply one approved native-integration preview",
        description: "Apply exactly one unexpired preview under a one-use content-bound \
                      approval through the daemon transaction, returning one terminal receipt.",
        example: "Apply the approved native-integration preview",
        surfaces: &NATIVE_INTEGRATION_SURFACES,
    },
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_STATUS_OPERATION,
        capability: "capability.application.native-integration.status",
        use_case: "use-case.application.native-integration.status",
        request_schema: "schema.application.native-integration.status.request",
        result_schema: "schema.application.native-integration.status.result",
        effect: EffectClass::Read,
        summary: "Read one native-integration transaction status",
        description: "Read the durable phase, cancellation request, and terminal outcome of \
                      one native-integration transaction.",
        example: "Show the status of this native-integration transaction",
        surfaces: &NATIVE_INTEGRATION_STATUS_SURFACES,
    },
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_CANCEL_OPERATION,
        capability: "capability.application.native-integration.cancel",
        use_case: "use-case.application.native-integration.cancel",
        request_schema: "schema.application.native-integration.cancel.request",
        result_schema: "schema.application.native-integration.cancel.result",
        effect: EffectClass::Administrative,
        summary: "Request native-integration cancellation",
        description: "Request cancellation of one native-integration transaction. After the \
                      native commit point the committed receipt is returned instead.",
        example: "Cancel this native-integration transaction",
        surfaces: &NATIVE_INTEGRATION_SURFACES,
    },
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_WORKTREE_INVENTORY_OPERATION,
        capability: "capability.application.native-integration.worktree-inventory",
        use_case: "use-case.application.native-integration.worktree-inventory",
        request_schema: "schema.application.native-integration.worktree-inventory.request",
        result_schema: "schema.application.native-integration.worktree-inventory.result",
        effect: EffectClass::Read,
        summary: "Inventory explicitly authorized native worktrees",
        description: "Read only the native worktree administration records covered by one persisted scope-set revision and digest.",
        example: "Inventory the explicitly authorized repository worktrees",
        surfaces: &NATIVE_WORKTREE_SURFACES,
    },
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_WORKTREE_INSPECT_OPERATION,
        capability: "capability.application.native-integration.worktree-cleanup-inspect",
        use_case: "use-case.application.native-integration.worktree-cleanup-inspect",
        request_schema: "schema.application.native-integration.worktree-cleanup-inspect.request",
        result_schema: "schema.application.native-integration.worktree-cleanup-inspect.result",
        effect: EffectClass::Read,
        summary: "Freshly inspect one linked worktree for cleanup",
        description: "Re-read exact native worktree state and emit a digest-bound cleanup inspection without mutating Git.",
        example: "Inspect this exact linked worktree before cleanup",
        surfaces: &NATIVE_WORKTREE_SURFACES,
    },
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_WORKTREE_CONFIRM_OPERATION,
        capability: "capability.application.native-integration.worktree-cleanup-confirm",
        use_case: "use-case.application.native-integration.worktree-cleanup-confirm",
        request_schema: "schema.application.native-integration.worktree-cleanup-confirm.request",
        result_schema: "schema.application.native-integration.worktree-cleanup-confirm.result",
        effect: EffectClass::Preview,
        summary: "Confirm one exact safe worktree inspection",
        description: "Revalidate the inspection digest and mint a confirmation proof only when clean, unlocked, unheld, and non-unique linked-worktree evidence still holds.",
        example: "Confirm this inspected worktree for removal",
        surfaces: &NATIVE_WORKTREE_SURFACES,
    },
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_WORKTREE_REMOVE_OPERATION,
        capability: "capability.application.native-integration.worktree-cleanup-remove",
        use_case: "use-case.application.native-integration.worktree-cleanup-remove",
        request_schema: "schema.application.native-integration.worktree-cleanup-remove.request",
        result_schema: "schema.application.native-integration.worktree-cleanup-remove.result",
        effect: EffectClass::Administrative,
        summary: "Remove one separately confirmed linked worktree",
        description: "Remove only the exact clean, unlocked, unheld, non-unique linked worktree registration and root; branches are never deleted.",
        example: "Remove the confirmed linked worktree",
        surfaces: &NATIVE_WORKTREE_SURFACES,
    },
    NativeIntegrationSurfaceSpec {
        operation: NATIVE_INTEGRATION_WORKTREE_RECONCILE_OPERATION,
        capability: "capability.application.native-integration.worktree-cleanup-reconcile",
        use_case: "use-case.application.native-integration.worktree-cleanup-reconcile",
        request_schema: "schema.application.native-integration.worktree-cleanup-reconcile.request",
        result_schema: "schema.application.native-integration.worktree-cleanup-reconcile.result",
        effect: EffectClass::Read,
        summary: "Reconcile one worktree cleanup outcome",
        description: "Re-read exact native administration state after removal or restart and distinguish removed, still-present, stale, and uncertain outcomes.",
        example: "Reconcile the confirmed worktree removal",
        surfaces: &NATIVE_WORKTREE_SURFACES,
    },
];

/// Catalog contribution for the public native-integration journey.
pub fn native_integration_surface_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let mut capabilities = Vec::with_capacity(NATIVE_INTEGRATION_SPECS.len());
    let mut bindings =
        Vec::with_capacity(NATIVE_INTEGRATION_SPECS.len() * NATIVE_INTEGRATION_SURFACES.len());

    for spec in &NATIVE_INTEGRATION_SPECS {
        let capability_id = CapabilityId::new(spec.capability)?;
        let (spec_bindings, binding_ids) = current_bindings(
            &capability_id,
            spec.operation,
            spec.surfaces.iter().copied(),
        )?;
        bindings.extend(spec_bindings);
        capabilities.push(capability(spec, capability_id, binding_ids)?);
    }

    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new(
            "contribution.application.native-integration-surface",
        )?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?;
    let schemas = native_integration_executable_schemas(&contribution)?;
    Ok(contribution.with_executable_schemas(schemas)?)
}

/// Daemon-owned public HTTP bindings for native worktree administration.
///
/// Native integration stack mutation remains CLI/MCP-only. Only worktree
/// operations whose canonical surface includes HTTP are projected here, so
/// the API router and official SDKs consume the same catalog authority.
pub fn native_worktree_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, ApplicationContractError> {
    let contribution = native_integration_surface_catalog_contribution()?;
    let service_id = ServiceId::new("service.application.native-integration")?;
    let mut bindings = Vec::new();

    for spec in NATIVE_INTEGRATION_SPECS
        .iter()
        .filter(|spec| spec.surfaces.contains(&BindingSurface::Http))
    {
        let capability_id = CapabilityId::new(spec.capability)?;
        let manifest = contribution
            .capabilities()
            .iter()
            .find(|manifest| manifest.capability_id() == &capability_id)
            .ok_or(ApplicationContractError::Inconsistent {
                field: "native worktree executable capability",
            })?;
        let executable_schema = contribution.executable_schema(&capability_id).ok_or(
            ApplicationContractError::Inconsistent {
                field: "native worktree executable schema",
            },
        )?;
        let http_binding = contribution
            .bindings()
            .iter()
            .find(|binding| {
                binding.capability_id() == &capability_id
                    && binding.surface() == BindingSurface::Http
            })
            .ok_or(ApplicationContractError::Inconsistent {
                field: "native worktree HTTP binding",
            })?;
        bindings.push(ExecutableBindingAvailabilityV1::available(
            ExecutableBindingV1::daemon_owned(
                manifest,
                OperationId::new(format!("operation.application.{}", spec.operation))?,
                service_id.clone(),
                executable_schema.request_schema().clone(),
                executable_schema.result_schema().clone(),
                CodecBindingKey::new(format!(
                    "codec.application.native-integration.{}.json.v1",
                    spec.operation
                ))?,
                RouteExposureV1::Public {
                    binding_id: http_binding.binding_id().clone(),
                    route_path: format!("/application/native-integration/{}", spec.operation),
                },
            )?,
        ));
    }

    Ok(ExecutableBindingRegistryV1::new(bindings)?)
}

/// Resolve one native-integration wire operation to its canonical application
/// operation. Callers use this to bind the exact capability and use case an
/// authorization grant must name; there is no generic forwarding path.
pub fn native_integration_surface_operation(
    name: &str,
) -> Result<Option<ApplicationOperation>, ApplicationContractError> {
    NATIVE_INTEGRATION_SPECS
        .iter()
        .find(|spec| spec.operation == name)
        .map(|spec| {
            let result_schema = schema(spec.result_schema)?;
            Ok(ApplicationOperation::new(
                CapabilityId::new(spec.capability)?,
                UseCaseId::new(spec.use_case)?,
                ResultContractRef::from_schema(&result_schema),
                true,
            ))
        })
        .transpose()
}

pub fn native_integration_surface_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    NATIVE_INTEGRATION_SPECS
        .iter()
        .map(handler_descriptor)
        .collect()
}

fn native_integration_executable_schemas(
    contribution: &CatalogContributionV1,
) -> Result<Vec<ExecutableSchemaAuthority>, ApplicationContractError> {
    let mut schemas = Vec::with_capacity(NATIVE_INTEGRATION_SPECS.len());
    macro_rules! add {
        ($operation:expr, $request:ty) => {
            schemas.push(executable_schema::<
                $request,
                NativeIntegrationSurfaceResultV1,
            >(
                contribution,
                $operation,
                concat!("tracedecay_application::git::", stringify!($request)),
                "tracedecay_application::git::NativeIntegrationSurfaceResultV1",
            )?)
        };
    }
    add!(
        NATIVE_INTEGRATION_STACK_SNAPSHOT_OPERATION,
        NativeIntegrationStackSnapshotSurfaceRequest
    );
    add!(
        NATIVE_INTEGRATION_PREFLIGHT_OPERATION,
        NativeIntegrationPreflightSurfaceRequest
    );
    add!(
        NATIVE_INTEGRATION_APPROVE_OPERATION,
        NativeIntegrationApproveSurfaceRequest
    );
    add!(
        NATIVE_INTEGRATION_APPLY_OPERATION,
        NativeIntegrationApplySurfaceRequest
    );
    add!(
        NATIVE_INTEGRATION_STATUS_OPERATION,
        NativeIntegrationStatusSurfaceRequest
    );
    add!(
        NATIVE_INTEGRATION_CANCEL_OPERATION,
        NativeIntegrationCancelSurfaceRequest
    );
    add!(
        NATIVE_INTEGRATION_WORKTREE_INVENTORY_OPERATION,
        WorktreeInventoryRequestV1
    );
    add!(
        NATIVE_INTEGRATION_WORKTREE_INSPECT_OPERATION,
        WorktreeCleanupInspectRequestV1
    );
    add!(
        NATIVE_INTEGRATION_WORKTREE_CONFIRM_OPERATION,
        WorktreeCleanupConfirmRequestV1
    );
    add!(
        NATIVE_INTEGRATION_WORKTREE_REMOVE_OPERATION,
        WorktreeCleanupRemoveRequestV1
    );
    add!(
        NATIVE_INTEGRATION_WORKTREE_RECONCILE_OPERATION,
        WorktreeCleanupReconcileRequestV1
    );
    Ok(schemas)
}

fn executable_schema<Request, Response>(
    contribution: &CatalogContributionV1,
    operation: &str,
    request_rust_type_path: &'static str,
    result_rust_type_path: &'static str,
) -> Result<ExecutableSchemaAuthority, ApplicationContractError>
where
    Request: JsonSchema,
    Response: JsonSchema,
{
    let spec = spec_for(operation)?;
    let capability_id = CapabilityId::new(spec.capability)?;
    let manifest = contribution
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == &capability_id)
        .ok_or(ApplicationContractError::Inconsistent {
            field: "native integration schema capability",
        })?;
    Ok(ExecutableSchemaAuthority::for_types_at_paths::<
        Request,
        Response,
    >(
        manifest, request_rust_type_path, result_rust_type_path
    )?)
}

fn spec_for(
    operation: &str,
) -> Result<&'static NativeIntegrationSurfaceSpec, ApplicationContractError> {
    NATIVE_INTEGRATION_SPECS
        .iter()
        .find(|spec| spec.operation == operation)
        .ok_or(ApplicationContractError::Inconsistent {
            field: "native integration schema operation",
        })
}

fn capability(
    spec: &NativeIntegrationSurfaceSpec,
    capability_id: CapabilityId,
    binding_ids: Vec<BindingId>,
) -> Result<CapabilityManifestV1, ApplicationContractError> {
    let is_effect = spec.effect.is_effect();
    Ok(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id,
        use_case_id: UseCaseId::new(spec.use_case)?,
        routing: RoutingContractV1::new(
            1,
            spec.summary,
            spec.description,
            vec![spec.example.to_owned()],
        )?,
        request_schema: schema(spec.request_schema)?,
        result_schema: schema(spec.result_schema)?,
        effect: spec.effect,
        scope: ScopeRequirement::new(vec![
            ScopeDimension::Project,
            ScopeDimension::Repository,
            ScopeDimension::Worktree,
        ])?,
        // Stack resolution, preflight, and apply stay separate capabilities:
        // preflight permission never implies apply.
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Resumable,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(cancellation_points(spec.effect))?,
        deadline: DeadlineContract::new(30_000, deadline_behavior(spec.effect))?,
        pagination: None,
        idempotency: if is_effect {
            IdempotencyContract::Required
        } else {
            IdempotencyContract::NotRequired
        },
        // Rebase, revert, force-push, and history rewriting are impossible
        // through this surface, so no shipped inverse exists.
        inverse: if is_effect {
            InverseContract::Unavailable {
                reason: InverseUnavailableReason::NoShippedInverse,
            }
        } else {
            InverseContract::NotApplicable
        },
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::Configuration,
            RevalidationPoint::ExpectedState,
        ])?,
        reconciliation: if is_effect {
            ReconciliationContract::Required
        } else {
            ReconciliationContract::NotRequired
        },
        receipt: if is_effect {
            ReceiptContract::DurableEffect
        } else {
            ReceiptContract::Operation
        },
        terminal_states: TerminalStateContract::new(terminal_states(spec.effect))?,
        availability: AvailabilityContract::Available,
        binding_ids,
        profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
        required_features: Vec::new(),
    })?)
}

fn cancellation_points(effect: EffectClass) -> Vec<CancellationPoint> {
    if effect.is_effect() {
        vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeEffect,
            CancellationPoint::EffectInFlight,
            CancellationPoint::AfterCommit,
        ]
    } else {
        vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeRead,
            CancellationPoint::DuringRead,
        ]
    }
}

fn deadline_behavior(effect: EffectClass) -> DeadlineBehavior {
    if effect.is_effect() {
        DeadlineBehavior::ReturnEffectReceipt
    } else {
        DeadlineBehavior::ReturnOperationReceipt
    }
}

fn terminal_states(effect: EffectClass) -> Vec<TerminalState> {
    if effect.is_effect() {
        vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::EffectUnknown,
            TerminalState::Partial,
        ]
    } else {
        vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::Partial,
        ]
    }
}

fn handler_descriptor(
    spec: &NativeIntegrationSurfaceSpec,
) -> Result<ApplicationHandlerDescriptor, ApplicationContractError> {
    let result_schema = schema(spec.result_schema)?;
    ApplicationHandlerDescriptor::new(
        ApplicationOperation::new(
            CapabilityId::new(spec.capability)?,
            UseCaseId::new(spec.use_case)?,
            ResultContractRef::from_schema(&result_schema),
            true,
        ),
        schema(spec.request_schema)?,
        result_schema,
    )
}

fn schema(id: &str) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(SchemaId::new(id)?, 1)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_snapshot_schema_requires_exact_registered_scope_set_identity() {
        let schema = serde_json::to_value(schemars::schema_for!(
            NativeIntegrationStackSnapshotSurfaceRequest
        ))
        .expect("stack-snapshot schema");
        let properties = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("stack-snapshot schema properties");

        assert!(properties.contains_key("authorized_scope_set_id"));
        assert!(properties.contains_key("authorized_scope_set_revision"));
        assert!(properties.contains_key("authorized_scope_set_digest"));
    }

    #[test]
    fn every_native_integration_journey_operation_is_bound_to_cli_and_mcp() {
        let contribution = native_integration_surface_catalog_contribution().expect("contribution");
        for operation in [
            NATIVE_INTEGRATION_STACK_SNAPSHOT_OPERATION,
            NATIVE_INTEGRATION_PREFLIGHT_OPERATION,
            NATIVE_INTEGRATION_APPROVE_OPERATION,
            NATIVE_INTEGRATION_APPLY_OPERATION,
            NATIVE_INTEGRATION_STATUS_OPERATION,
            NATIVE_INTEGRATION_CANCEL_OPERATION,
        ] {
            for surface in [BindingSurface::Cli, BindingSurface::Mcp] {
                assert!(
                    contribution.bindings().iter().any(|binding| {
                        binding.operation().as_str() == operation && binding.surface() == surface
                    }),
                    "{operation} is not bound to {surface:?}"
                );
            }
        }
    }

    #[test]
    fn only_the_transaction_journey_is_withheld_from_http_and_no_index_step_is_added() {
        let contribution = native_integration_surface_catalog_contribution().expect("contribution");
        for operation in [
            NATIVE_INTEGRATION_STACK_SNAPSHOT_OPERATION,
            NATIVE_INTEGRATION_PREFLIGHT_OPERATION,
            NATIVE_INTEGRATION_APPROVE_OPERATION,
            NATIVE_INTEGRATION_APPLY_OPERATION,
            NATIVE_INTEGRATION_STATUS_OPERATION,
            NATIVE_INTEGRATION_CANCEL_OPERATION,
        ] {
            assert!(contribution.bindings().iter().all(|binding| {
                binding.operation().as_str() != operation
                    || binding.surface() != BindingSurface::Http
            }));
        }
        for operation in [
            NATIVE_INTEGRATION_WORKTREE_INVENTORY_OPERATION,
            NATIVE_INTEGRATION_WORKTREE_INSPECT_OPERATION,
            NATIVE_INTEGRATION_WORKTREE_CONFIRM_OPERATION,
            NATIVE_INTEGRATION_WORKTREE_REMOVE_OPERATION,
            NATIVE_INTEGRATION_WORKTREE_RECONCILE_OPERATION,
        ] {
            assert!(contribution.bindings().iter().any(|binding| {
                binding.operation().as_str() == operation
                    && binding.surface() == BindingSurface::Http
            }));
        }
        assert!(contribution.bindings().iter().all(|binding| {
            let operation = binding.operation().as_str();
            !operation.contains("stage_hunks")
                && !operation.contains("unstage_hunks")
                && !operation.contains("commit_index")
        }));
    }

    #[test]
    fn every_capability_is_schema_backed_and_separately_authorized() {
        let contribution = native_integration_surface_catalog_contribution().expect("contribution");
        assert_eq!(contribution.capabilities().len(), 11);
        for manifest in contribution.capabilities() {
            assert!(
                contribution
                    .executable_schema(manifest.capability_id())
                    .is_some(),
                "{:?} has no executable schema",
                manifest.capability_id()
            );
        }
        let ids: Vec<_> = contribution
            .capabilities()
            .iter()
            .map(|manifest| manifest.capability_id().as_str().to_owned())
            .collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "capabilities must be distinct");
    }

    #[test]
    fn handler_descriptors_cover_every_declared_capability() {
        let contribution = native_integration_surface_catalog_contribution().expect("contribution");
        let descriptors = native_integration_surface_handler_descriptors().expect("descriptors");
        assert_eq!(descriptors.len(), contribution.capabilities().len());
    }

    #[test]
    fn only_a_receipt_advances_durable_state() {
        for result in [
            NativeIntegrationSurfaceResultV1::unavailable(
                NativeIntegrationSurfaceUnavailableV1::AuthorityUnmounted,
            ),
            NativeIntegrationSurfaceResultV1::unavailable(
                NativeIntegrationSurfaceUnavailableV1::Denied,
            ),
            NativeIntegrationSurfaceResultV1::from_cancel(
                NativeIntegrationCancelDispositionV1::CancellationRequested,
            ),
        ] {
            assert!(!result.is_advancing(), "{result:?}");
        }
    }

    #[test]
    fn every_port_failure_maps_to_a_truthful_unavailable_reason() {
        use NativeIntegrationSurfaceUnavailableV1 as Reason;
        for (error, expected) in [
            (
                NativeIntegrationPortError::Unavailable,
                Reason::AuthorityUnmounted,
            ),
            (
                NativeIntegrationPortError::Native("boom".to_owned()),
                Reason::AuthorityUnmounted,
            ),
            (NativeIntegrationPortError::Stale, Reason::Stale),
            (NativeIntegrationPortError::Denied, Reason::Denied),
            (
                NativeIntegrationPortError::ApprovalConflict,
                Reason::ApprovalConflict,
            ),
            (
                NativeIntegrationPortError::TransactionConflict,
                Reason::TransactionConflict,
            ),
            (NativeIntegrationPortError::Cancelled, Reason::Cancelled),
            (
                NativeIntegrationPortError::RecoveryRequired,
                Reason::RecoveryRequired,
            ),
            (
                NativeIntegrationPortError::NeedsInspection,
                Reason::NeedsInspection,
            ),
            (
                NativeIntegrationPortError::ResetRequired,
                Reason::ResetRequired,
            ),
            (
                NativeIntegrationPortError::DurabilityUncertain,
                Reason::DurabilityUncertain,
            ),
        ] {
            assert_eq!(
                NativeIntegrationSurfaceUnavailableV1::from(&error),
                expected
            );
        }
    }

    #[test]
    fn unavailable_results_round_trip_over_the_wire() {
        let result = NativeIntegrationSurfaceResultV1::unavailable(
            NativeIntegrationSurfaceUnavailableV1::AuthorityUnmounted,
        );
        let encoded = serde_json::to_value(&result).expect("encode");
        assert_eq!(encoded["outcome"], "unavailable");
        assert_eq!(encoded["reason"], "authority_unmounted");
        let decoded: NativeIntegrationSurfaceResultV1 =
            serde_json::from_value(encoded).expect("decode");
        assert_eq!(decoded, result);
    }
}
