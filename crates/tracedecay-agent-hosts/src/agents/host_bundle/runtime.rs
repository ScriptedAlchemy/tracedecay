//! Composition of host-bundle verification and lifecycle storage.

use super::model::{
    CompetingHostExtensionClaimV1, HostBundleExecutionRequestV1, HostBundleLifecyclePreviewV1,
    HostBundleLifecycleStorageV1,
};
use super::planner::{
    ObservedHostArtifactV1, plan_verified_complete_lifecycle_mutation,
    validate_competing_extension_claims,
};
use super::{
    HostBundleArtifactContentV1, HostBundleError, HostBundleInstallReceiptV1, HostBundleManifestV1,
    HostBundleVerificationAdapterV1,
};

/// Production-composition seam for independently injected cryptographic and
/// filesystem authorities. It verifies before it asks storage to mutate, so an
/// incompatible catalog entry cannot trigger filesystem access.
pub struct HostBundleLifecycleRuntimeV1<V, S> {
    verifier: V,
    storage: S,
}

impl<V, S> HostBundleLifecycleRuntimeV1<V, S> {
    pub fn new(verifier: V, storage: S) -> Self {
        Self { verifier, storage }
    }

    pub fn into_storage(self) -> S {
        self.storage
    }
}

impl<V, S> HostBundleLifecycleRuntimeV1<V, S>
where
    V: HostBundleVerificationAdapterV1,
    S: HostBundleLifecycleStorageV1,
{
    #[allow(clippy::too_many_arguments)]
    pub fn dry_run(
        &self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        manifest_observed: &[ObservedHostArtifactV1],
        owned_receipt: Option<&HostBundleInstallReceiptV1>,
        orphan_observed: &[ObservedHostArtifactV1],
        competing_extension_claims: &[CompetingHostExtensionClaimV1],
    ) -> Result<HostBundleLifecyclePreviewV1, HostBundleError> {
        if request.operation_id == [0; 16] {
            return Err(HostBundleError::InvalidManifest);
        }
        validate_competing_extension_claims(competing_extension_claims)?;
        self.verifier.verify_manifest(manifest)?;
        let mut planning_request = request.lifecycle.clone();
        planning_request.explicit_confirmation = true;
        let plan = plan_verified_complete_lifecycle_mutation(
            manifest,
            &planning_request,
            manifest_observed,
            owned_receipt,
            orphan_observed,
            &self.verifier,
        )?;
        Ok(HostBundleLifecyclePreviewV1 {
            confirmation_required: !request.lifecycle.explicit_confirmation
                || !competing_extension_claims.is_empty(),
            competing_extension_claims: competing_extension_claims.to_vec(),
            plan,
        })
    }

    pub fn execute(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError> {
        self.verifier.verify_manifest(manifest)?;
        self.storage
            .execute_lifecycle(manifest, request, contents, &self.verifier)
    }

    pub fn execute_confirmed(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
        competing_extension_claims: &[CompetingHostExtensionClaimV1],
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError> {
        validate_competing_extension_claims(competing_extension_claims)?;
        if !competing_extension_claims.is_empty() && !request.lifecycle.explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        self.execute(manifest, request, contents)
    }
}
