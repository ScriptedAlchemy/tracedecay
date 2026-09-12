//! Lifecycle request, component-set, and preview contracts shared by the
//! planner, the writer, and daemon composition.

use std::path::PathBuf;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_host_integration::host_bundle_stale_preview;

use super::planner::{HostBundleLifecycleRequestV1, HostBundleMutationPlanV1};
use super::{
    HostBundleArtifactContentV1, HostBundleComponentV1, HostBundleError,
    HostBundleInstallReceiptV1, HostBundleLifecycleOpV1, HostBundleManifestV1,
    HostBundleVerificationAdapterV1, HostCapabilityV1, HostKindV1,
};

/// Execution-specific input kept separate from the public lifecycle request
/// so existing plan consumers do not accidentally gain filesystem authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleExecutionRequestV1 {
    pub lifecycle: HostBundleLifecycleRequestV1,
    pub operation_id: [u8; 16],
}

/// One verified component in the canonical set for a host lifecycle operation.
/// The content remains outside receipts and journals; it is staged and checked
/// against the embedded manifest before any host path is changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostComponentSetEntryV1 {
    pub manifest: HostBundleManifestV1,
    pub contents: Vec<HostBundleArtifactContentV1>,
}

/// The complete, host-specific component set that must be committed or rolled
/// back as one lifecycle boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostComponentSetV1 {
    pub host: HostKindV1,
    pub components: Vec<HostComponentSetEntryV1>,
}

/// Set-level lifecycle authority. Component selection is explicit so a
/// default install and an explicit `--component` use the same transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostComponentSetLifecycleRequestV1 {
    pub operation: HostBundleLifecycleOpV1,
    pub expected_host: HostKindV1,
    pub expected_components: Vec<HostBundleComponentV1>,
    pub explicit_confirmation: bool,
    pub hermes_profile_bindings: u8,
    /// Operator-confirmed authority (`--yes --adopt`) to take ownership of
    /// receiptless files at cataloged deploy paths even when the host adapter
    /// recognizes no legacy provenance in them. Distinct from
    /// `explicit_confirmation`, which only confirms the previewed plan.
    pub explicit_adoption: bool,
}

/// One operation id spans every component, registration mutation, receipt,
/// backup, and recovery record in a component-set transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostComponentSetExecutionRequestV1 {
    pub lifecycle: HostComponentSetLifecycleRequestV1,
    pub operation_id: [u8; 16],
}

/// Immutable component-set dry run consumed by confirmed apply. The plan
/// digest binds the operation id, complete component inventory, artifact
/// actions, and exact registration revision observed before preview returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostComponentSetLifecyclePreviewV1 {
    pub operation_id: [u8; 16],
    pub plan_digest: [u8; 32],
    pub base_registration_revision: [u8; 32],
    pub current_registration_revision: [u8; 32],
    pub artifact_state_revision: [u8; 32],
    pub component_plans: Vec<HostBundleMutationPlanV1>,
    /// Third-party extensions the registration authority found already claiming
    /// a surface this component set would register, ordered by extension id.
    pub competing_extension_claims: Vec<CompetingHostExtensionClaimV1>,
    pub confirmation_required: bool,
}

/// A third-party host extension claiming a surface `TraceDecay` would register.
/// The digest points to bounded discovery evidence; raw host config is never
/// retained in lifecycle requests or receipts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CompetingHostExtensionClaimV1 {
    pub extension_id: String,
    pub capability: HostCapabilityV1,
    pub evidence_digest: [u8; 32],
}

/// Explicit host-level rollback handoff returned by a dry run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleRollbackSeamV1 {
    pub operation_id: [u8; 16],
    pub host: HostKindV1,
    pub component: HostBundleComponentV1,
    pub backup_relative_paths: Vec<String>,
    pub interrupted_recovery_required: bool,
}

/// Read-only lifecycle result. Producing this value verifies the embedded
/// first-party manifest and exact ownership observations but never opens a
/// writer, creates a control directory, writes a receipt, or recovers a journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostBundleLifecyclePreviewV1 {
    pub plan: HostBundleMutationPlanV1,
    pub confirmation_required: bool,
    pub competing_extension_claims: Vec<CompetingHostExtensionClaimV1>,
    pub rollback: HostBundleRollbackSeamV1,
}

/// Injected lifecycle storage boundary. The concrete no-follow writer below
/// implements this trait, while daemon wiring can provide its opened authority
/// without exposing a filesystem path or mutation capability to callers.
pub trait HostBundleLifecycleStorageV1 {
    fn recover_lifecycle(&mut self) -> Result<(), HostBundleError>;

    fn execute_lifecycle<V: HostBundleVerificationAdapterV1>(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
        verifier: &V,
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError>;
}

/// Host-native registration boundary coordinated with an artifact component
/// set. Implementations persist their own bounded registration backups during
/// `stage`; the aggregate writer records the state transition in its recovery
/// journal and invokes these hooks in reverse on failure or restart.
pub trait HostComponentSetRegistrationV1 {
    /// Exact revision of the host registration state that this adapter may
    /// mutate. Concrete host adapters hash their bounded native config;
    /// artifact-only adapters use this stable no-registration revision.
    fn current_revision(
        &self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<[u8; 32], HostBundleError> {
        Ok(Sha256::digest(b"tracedecay.host-registration.none.v1").into())
    }

    /// Recognize this host component's receiptless deployment as a prior
    /// first-party install ("legacy provenance"). Pre-receipt installers
    /// wrote cataloged deploy paths without v2 receipts, so receipt evidence
    /// alone cannot tell their files from a user's; a cataloged path alone
    /// must never be treated as ownership. Implementations inspect durable
    /// host state (for example a bundle's own manifest naming tracedecay)
    /// and fail closed: the default recognizes nothing, so adoption then
    /// requires the operator's explicit `--yes --adopt`.
    fn receiptless_component_provenance(&self, _component: HostBundleComponentV1) -> bool {
        false
    }

    /// Bounded read-only discovery of third-party extensions that already
    /// claim a surface this component set would register. Discovery reports;
    /// it never grants authority to disable, replace, or adopt the competing
    /// extension. Adapters that cannot observe a host's registration surface
    /// must refuse rather than report an empty slice.
    fn discover_competing_extension_claims(
        &self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<Vec<CompetingHostExtensionClaimV1>, HostBundleError> {
        Ok(Vec::new())
    }

    /// Bind the adapter to the confirmed preview immediately before staging.
    /// Implementations may retain the revision and recheck it while capturing
    /// their rollback backup.
    fn confirm_preview(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        preview: &HostComponentSetLifecyclePreviewV1,
    ) -> Result<(), HostBundleError> {
        if preview.operation_id != request.operation_id
            || preview.current_registration_revision != preview.base_registration_revision
            || self.current_revision(component_set, request)? != preview.base_registration_revision
        {
            return Err(host_bundle_stale_preview!());
        }
        Ok(())
    }

    /// Absolute host paths this transaction will write itself, declared before
    /// any mutation runs.
    ///
    /// A host may register itself through a file that is also one of this
    /// component set's managed artifacts, in which case the adapter's own
    /// registration revision changes as a direct consequence of the
    /// transaction's declared write. Adapters use this set to tell their own
    /// writes apart from a foreign edit; every path outside it stays under
    /// full drift protection.
    fn declare_artifact_writes(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
        _paths: &[PathBuf],
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn preflight(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn stage(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn apply(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn verify(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn commit(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }

    fn rollback(
        &mut self,
        _component_set: &HostComponentSetV1,
        _request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        Ok(())
    }
}
