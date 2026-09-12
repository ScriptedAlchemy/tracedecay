//! Composition of an injected verifier and lifecycle storage, plus the
//! feedback-path rollback switch built on that composition.

use serde::{Deserialize, Serialize};

use super::control::validate_receipt;
use super::model::{
    CompetingHostExtensionClaimV1, HostBundleExecutionRequestV1, HostBundleLifecyclePreviewV1,
    HostBundleLifecycleStorageV1, HostBundleRollbackSeamV1,
};
use super::planner::{
    HostArtifactActionV1, ObservedHostArtifactV1, plan_verified_complete_lifecycle_mutation,
    validate_competing_extension_claims,
};
use super::{
    HostBundleArtifactContentV1, HostBundleError, HostBundleInstallReceiptV1,
    HostBundleLifecycleOpV1, HostBundleManifestV1, HostBundleRollbackBoundaryV1,
    HostBundleVerificationAdapterV1, HostKindV1,
};

/// Production-composition seam for independently injected cryptographic and
/// filesystem authorities. It verifies before it asks storage to recover or
/// mutate, so an incompatible catalog entry cannot trigger filesystem access.
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
        let backup_relative_paths = plan
            .mutations
            .iter()
            .filter(|mutation| {
                matches!(
                    mutation.action,
                    HostArtifactActionV1::BackupThenReplace
                        | HostArtifactActionV1::BackupThenRemove
                )
            })
            .map(|mutation| mutation.relative_path.clone())
            .collect();
        Ok(HostBundleLifecyclePreviewV1 {
            confirmation_required: !request.lifecycle.explicit_confirmation
                || !competing_extension_claims.is_empty(),
            competing_extension_claims: competing_extension_claims.to_vec(),
            rollback: HostBundleRollbackSeamV1 {
                operation_id: request.operation_id,
                host: manifest.host,
                component: manifest.component,
                backup_relative_paths,
                interrupted_recovery_required: plan.rollback_required,
            },
            plan,
        })
    }

    pub fn recover(&mut self) -> Result<(), HostBundleError> {
        self.storage.recover_lifecycle()
    }

    pub fn execute(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError> {
        self.verifier.verify_manifest(manifest)?;
        self.storage.recover_lifecycle()?;
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

/// Durable evidence that one host's feedback path moved between two verified
/// embedded first-party core bundle versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackPathRollbackReceiptV1 {
    pub host: HostKindV1,
    pub previous_manifest_digest: [u8; 32],
    pub applied_manifest_digest: [u8; 32],
    pub apply_receipt: HostBundleInstallReceiptV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackPathRestoreReceiptV1 {
    pub switch_operation_id: [u8; 16],
    pub restore_receipt: HostBundleInstallReceiptV1,
}

/// Concrete feedback rollback switch backed by the same digest-verified,
/// receipt-based atomic host-bundle lifecycle as
/// install/update/repair/uninstall. It owns no host-local scorer, scheduler,
/// store, or feedback business logic.
pub struct FeedbackPathRollbackSwitchV1<V, S> {
    lifecycle: HostBundleLifecycleRuntimeV1<V, S>,
}

impl<V, S> FeedbackPathRollbackSwitchV1<V, S> {
    pub fn new(lifecycle: HostBundleLifecycleRuntimeV1<V, S>) -> Self {
        Self { lifecycle }
    }

    pub fn into_lifecycle(self) -> HostBundleLifecycleRuntimeV1<V, S> {
        self.lifecycle
    }
}

impl<V, S> FeedbackPathRollbackSwitchV1<V, S>
where
    V: HostBundleVerificationAdapterV1,
    S: HostBundleLifecycleStorageV1,
{
    #[allow(clippy::too_many_arguments)]
    pub fn feedback_rollback_switch_dry_run(
        &self,
        previous_manifest: &HostBundleManifestV1,
        target_manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        manifest_observed: &[ObservedHostArtifactV1],
        owned_receipt: Option<&HostBundleInstallReceiptV1>,
        orphan_observed: &[ObservedHostArtifactV1],
        competing_extension_claims: &[CompetingHostExtensionClaimV1],
    ) -> Result<HostBundleLifecyclePreviewV1, HostBundleError> {
        validate_feedback_switch_manifests(previous_manifest, target_manifest)?;
        self.lifecycle.verifier.verify_manifest(previous_manifest)?;
        self.lifecycle.dry_run(
            target_manifest,
            request,
            manifest_observed,
            owned_receipt,
            orphan_observed,
            competing_extension_claims,
        )
    }

    pub fn feedback_rollback_switch_apply(
        &mut self,
        previous_manifest: &HostBundleManifestV1,
        target_manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        target_contents: &[HostBundleArtifactContentV1],
        competing_extension_claims: &[CompetingHostExtensionClaimV1],
    ) -> Result<FeedbackPathRollbackReceiptV1, HostBundleError> {
        validate_feedback_switch_manifests(previous_manifest, target_manifest)?;
        self.lifecycle.verifier.verify_manifest(previous_manifest)?;
        if !request.lifecycle.explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        let previous_manifest_digest = previous_manifest.canonical_digest()?;
        let applied_manifest_digest = target_manifest.canonical_digest()?;
        let apply_receipt = self.lifecycle.execute_confirmed(
            target_manifest,
            request,
            target_contents,
            competing_extension_claims,
        )?;
        Ok(FeedbackPathRollbackReceiptV1 {
            host: target_manifest.host,
            previous_manifest_digest,
            applied_manifest_digest,
            apply_receipt,
        })
    }

    pub fn feedback_rollback_switch_restore(
        &mut self,
        switch_receipt: &FeedbackPathRollbackReceiptV1,
        previous_manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        previous_contents: &[HostBundleArtifactContentV1],
        competing_extension_claims: &[CompetingHostExtensionClaimV1],
    ) -> Result<FeedbackPathRestoreReceiptV1, HostBundleError> {
        if !request.lifecycle.explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        validate_receipt(&switch_receipt.apply_receipt)?;
        if switch_receipt.apply_receipt.rollback_boundary != HostBundleRollbackBoundaryV1::Passed {
            return Err(HostBundleError::ReceiptCorrupted);
        }
        if previous_manifest.host != switch_receipt.host
            || previous_manifest.canonical_digest()? != switch_receipt.previous_manifest_digest
            || request.lifecycle.operation != HostBundleLifecycleOpV1::Repair
            || request.lifecycle.expected_host != switch_receipt.host
            || request.lifecycle.expected_component != previous_manifest.component
            || switch_receipt.apply_receipt.host != switch_receipt.host
            || switch_receipt.apply_receipt.component != previous_manifest.component
            || switch_receipt.apply_receipt.manifest_digest
                != switch_receipt.applied_manifest_digest
        {
            return Err(HostBundleError::WrongTarget);
        }
        let restore_receipt = self.lifecycle.execute_confirmed(
            previous_manifest,
            request,
            previous_contents,
            competing_extension_claims,
        )?;
        Ok(FeedbackPathRestoreReceiptV1 {
            switch_operation_id: switch_receipt.apply_receipt.operation_id,
            restore_receipt,
        })
    }
}

fn validate_feedback_switch_manifests(
    previous_manifest: &HostBundleManifestV1,
    target_manifest: &HostBundleManifestV1,
) -> Result<(), HostBundleError> {
    if previous_manifest.host != target_manifest.host
        || previous_manifest.component != target_manifest.component
        || previous_manifest.canonical_digest()? == target_manifest.canonical_digest()?
    {
        return Err(HostBundleError::WrongTarget);
    }
    Ok(())
}
