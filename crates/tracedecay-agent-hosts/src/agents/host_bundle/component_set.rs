//! Aggregate component-set transaction: one registration adapter and one
//! in-memory rollback boundary spanning every component of a host.

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};
use tracedecay_host_integration::host_bundle_stale_preview;

use super::control::{
    component_set_receipt_matches, component_set_receipt_matches_preview,
    validate_component_set_receipt, validate_component_set_request,
};
use super::model::{
    HostComponentSetExecutionRequestV1, HostComponentSetLifecyclePreviewV1,
    HostComponentSetRegistrationV1, HostComponentSetV1,
};
use super::planner::{
    HostArtifactActionV1, HostBundleLifecycleRequestV1, HostBundleMutationPlanV1,
    dry_run_host_component_set_lifecycle_with_lifecycle_root_at, observe_artifact_at,
    plan_verified_complete_lifecycle_mutation, validate_artifact_contents_for_operation,
};
use super::writer::{ArtifactUndo, HostBundleWriterV1, read_regular_nofollow};
use super::{
    HOST_BUNDLE_RECEIPT_SCHEMA_VERSION, HostBundleError, HostBundleInstallReceiptV1,
    HostBundleLifecycleOpV1, HostBundleManifestV1, HostBundleReceiptArtifactV1,
    HostBundleVerificationAdapterV1, HostComponentSetReceiptV1,
};

/// Public component-set lifecycle façade over the capability-rooted writer.
/// It keeps the existing per-component receipt API intact while ensuring the
/// default host lifecycle has one aggregate rollback boundary.
pub struct HostComponentSetTransactionV1<'a> {
    writer: &'a mut HostBundleWriterV1,
}

impl<'a> HostComponentSetTransactionV1<'a> {
    pub fn new(writer: &'a mut HostBundleWriterV1) -> Self {
        Self { writer }
    }

    pub fn preview<V: HostBundleVerificationAdapterV1, R: HostComponentSetRegistrationV1>(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetLifecyclePreviewV1, HostBundleError> {
        self.writer.discard_stale_receipts_for_reinstall(request)?;
        dry_run_host_component_set_lifecycle_with_lifecycle_root_at(
            &self.writer.root_path,
            &self.writer.lifecycle_root_path,
            component_set,
            request,
            verifier,
            registration,
        )
    }

    pub fn execute_confirmed<
        V: HostBundleVerificationAdapterV1,
        R: HostComponentSetRegistrationV1,
    >(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        preview: &HostComponentSetLifecyclePreviewV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        if !request.lifecycle.explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        validate_component_set_request(component_set, request)?;
        if preview.operation_id != request.operation_id {
            return Err(host_bundle_stale_preview!());
        }
        if let Some(receipt) = self
            .writer
            .load_component_set_receipt(request.operation_id)?
        {
            if !component_set_receipt_matches(&receipt, component_set, request)? {
                return Err(HostBundleError::ReceiptCorrupted);
            }
            return component_set_receipt_matches_preview(&receipt, preview)
                .then_some(receipt)
                .ok_or(host_bundle_stale_preview!());
        }
        // The re-preview reports why this plan can no longer be applied.
        // `StalePreview` is reserved for genuine drift between the confirmed
        // preview and what is observed now (checked below); a typed refusal
        // such as an ownership conflict or a catalog mismatch is a standing
        // condition that retrying cannot clear, and laundering it into
        // "stale, retry" hides the only diagnostic the operator has.
        let current = self.preview(component_set, request, verifier, registration)?;
        if current.operation_id != preview.operation_id
            || current.plan_digest != preview.plan_digest
            || current.base_registration_revision != preview.base_registration_revision
            || current.current_registration_revision != preview.current_registration_revision
            || current.artifact_state_revision != preview.artifact_state_revision
            || current.component_plans != preview.component_plans
            || current.competing_extension_claims != preview.competing_extension_claims
        {
            return Err(host_bundle_stale_preview!());
        }
        registration.confirm_preview(component_set, request, preview)?;
        self.writer.execute_confirmed_component_set(
            component_set,
            request,
            preview,
            verifier,
            registration,
        )
    }

    pub fn execute<V: HostBundleVerificationAdapterV1, R: HostComponentSetRegistrationV1>(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        self.writer
            .execute_component_set(component_set, request, verifier, registration)
    }
}

struct PreparedHostComponentSetComponentV1 {
    manifest: HostBundleManifestV1,
    content_by_path: BTreeMap<String, Vec<u8>>,
    plan: HostBundleMutationPlanV1,
    previous_receipt: Option<HostBundleInstallReceiptV1>,
}

impl HostBundleWriterV1 {
    /// An explicitly adopting install, reinstall, or update replaces receipts
    /// written under an older schema; every other request leaves them to fail
    /// as [`HostBundleError::ReinstallRequired`].
    fn discard_stale_receipts_for_reinstall(
        &mut self,
        request: &HostComponentSetExecutionRequestV1,
    ) -> Result<(), HostBundleError> {
        if request.lifecycle.explicit_adoption
            && request.lifecycle.operation != HostBundleLifecycleOpV1::Uninstall
        {
            self.discard_stale_receipts(request.lifecycle.expected_host)?;
        }
        Ok(())
    }

    /// Execute a complete canonical host component set as one operation.
    /// Every component is preflighted before any owned file changes; receipts
    /// are published only after all artifacts and the host registration
    /// authority verify successfully.
    pub fn execute_component_set<
        V: HostBundleVerificationAdapterV1,
        R: HostComponentSetRegistrationV1,
    >(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        self.execute_component_set_with_preview(
            component_set,
            request,
            None,
            verifier,
            registration,
        )
    }

    fn execute_confirmed_component_set<
        V: HostBundleVerificationAdapterV1,
        R: HostComponentSetRegistrationV1,
    >(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        preview: &HostComponentSetLifecyclePreviewV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        self.execute_component_set_with_preview(
            component_set,
            request,
            Some(preview),
            verifier,
            registration,
        )
    }

    #[hotpath::measure(label = "hosts.agent.host_bundle.component_set_execute")]
    fn execute_component_set_with_preview<
        V: HostBundleVerificationAdapterV1,
        R: HostComponentSetRegistrationV1,
    >(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        confirmed_preview: Option<&HostComponentSetLifecyclePreviewV1>,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        validate_component_set_request(component_set, request)?;
        self.ensure_host_lock(component_set.host)?;
        self.discard_stale_receipts_for_reinstall(request)?;
        if let Some(receipt) = self.load_component_set_receipt(request.operation_id)? {
            if !component_set_receipt_matches(&receipt, component_set, request)? {
                return Err(HostBundleError::ReceiptCorrupted);
            }
            if confirmed_preview
                .is_some_and(|preview| !component_set_receipt_matches_preview(&receipt, preview))
            {
                return Err(host_bundle_stale_preview!());
            }
            return Ok(receipt);
        }

        let prepared = self.preflight_component_set(component_set, request, verifier)?;
        // Declare the exact write set before any adapter observes state, so a
        // registration surface that is also one of these artifacts can tell
        // this transaction's own write apart from a foreign edit.
        let declared_writes = prepared
            .iter()
            .flat_map(|component| component.plan.mutations.iter())
            .map(|mutation| self.root_path.join(&mutation.relative_path))
            .collect::<Vec<_>>();
        registration.declare_artifact_writes(component_set, request, &declared_writes)?;
        registration.preflight(component_set, request)?;

        let mut undo = Vec::new();
        let result = (|| {
            registration.stage(component_set, request)?;
            for component in &prepared {
                for mutation in &component.plan.mutations {
                    if let Some(record) =
                        self.apply_artifact_mutation(mutation, &component.content_by_path)?
                    {
                        undo.push(record);
                    }
                }
            }
            registration.apply(component_set, request)?;
            self.verify_component_set_artifacts(&prepared)?;
            registration.verify(component_set, request)?;

            let receipt =
                component_set_receipt_from_prepared(&prepared, request, confirmed_preview)?;
            for component_receipt in &receipt.component_receipts {
                self.write_receipt(component_receipt)?;
            }
            self.write_component_set_receipt(&receipt)?;
            Ok(receipt)
        })();

        match result {
            Ok(receipt) => {
                registration.commit(component_set, request)?;
                Ok(receipt)
            }
            Err(error) => Err(
                match self.rollback_component_set(
                    component_set,
                    request,
                    registration,
                    &prepared,
                    &undo,
                ) {
                    Ok(()) => error,
                    Err(rollback_error) => rollback_error,
                },
            ),
        }
    }

    fn preflight_component_set<V: HostBundleVerificationAdapterV1>(
        &self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        verifier: &V,
    ) -> Result<Vec<PreparedHostComponentSetComponentV1>, HostBundleError> {
        let mut prepared = Vec::with_capacity(component_set.components.len());
        let mut claimed_paths = BTreeMap::new();

        for component in &component_set.components {
            component.manifest.validate_structure()?;
            verifier.verify_manifest(&component.manifest)?;
            let content_by_path = validate_artifact_contents_for_operation(
                &component.manifest,
                request.lifecycle.operation,
                &component.contents,
            )?;
            let previous_receipt =
                self.load_receipt(component.manifest.host, component.manifest.component)?;
            let owned_receipt = previous_receipt
                .as_ref()
                .filter(|receipt| receipt.operation != HostBundleLifecycleOpV1::Uninstall);
            let manifest_observed =
                if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall {
                    Vec::new()
                } else {
                    component
                        .manifest
                        .artifacts
                        .iter()
                        .map(|artifact| {
                            let owned = owned_receipt.and_then(|receipt| {
                                receipt
                                    .artifacts
                                    .iter()
                                    .find(|owned| owned.relative_path == artifact.relative_path)
                            });
                            observe_artifact_at(
                                &self.root_path,
                                &artifact.relative_path,
                                owned.map(|owned| owned.ownership_marker.clone()),
                                owned.map(|owned| owned.artifact_digest),
                                Some(artifact.ownership_marker.clone()),
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?
                };
            let orphan_observed = if matches!(
                request.lifecycle.operation,
                HostBundleLifecycleOpV1::Update
                    | HostBundleLifecycleOpV1::Repair
                    | HostBundleLifecycleOpV1::Uninstall
            ) {
                owned_receipt
                    .into_iter()
                    .flat_map(|receipt| &receipt.artifacts)
                    .filter(|owned| {
                        request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall
                            || !component
                                .manifest
                                .artifacts
                                .iter()
                                .any(|artifact| artifact.relative_path == owned.relative_path)
                    })
                    .map(|owned| {
                        observe_artifact_at(
                            &self.root_path,
                            &owned.relative_path,
                            Some(owned.ownership_marker.clone()),
                            Some(owned.artifact_digest),
                            None,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            let lifecycle = HostBundleLifecycleRequestV1 {
                operation: request.lifecycle.operation,
                expected_host: request.lifecycle.expected_host,
                expected_component: component.manifest.component,
                explicit_confirmation: request.lifecycle.explicit_confirmation,
                hermes_profile_bindings: request.lifecycle.hermes_profile_bindings,
                adopt_receiptless: request.lifecycle.explicit_adoption,
            };
            let plan = plan_verified_complete_lifecycle_mutation(
                &component.manifest,
                &lifecycle,
                &manifest_observed,
                owned_receipt,
                &orphan_observed,
                verifier,
            )?;
            for mutation in &plan.mutations {
                if claimed_paths
                    .insert(mutation.relative_path.clone(), component.manifest.component)
                    .is_some()
                {
                    return Err(HostBundleError::InvalidObservedState);
                }
            }
            prepared.push(PreparedHostComponentSetComponentV1 {
                manifest: component.manifest.clone(),
                content_by_path,
                plan,
                previous_receipt,
            });
        }
        Ok(prepared)
    }

    fn verify_component_set_artifacts(
        &self,
        prepared: &[PreparedHostComponentSetComponentV1],
    ) -> Result<(), HostBundleError> {
        for component in prepared {
            for mutation in &component.plan.mutations {
                let expected = (mutation.action != HostArtifactActionV1::Remove)
                    .then(|| {
                        component
                            .manifest
                            .artifacts
                            .iter()
                            .find(|artifact| artifact.relative_path == mutation.relative_path)
                            .map(|artifact| artifact.artifact_digest)
                    })
                    .flatten();
                let (parent, name) =
                    self.open_parent_nofollow(Path::new(&mutation.relative_path))?;
                match (expected, read_regular_nofollow(&parent, &name)?) {
                    (Some(expected), Some(bytes)) => {
                        let digest: [u8; 32] = Sha256::digest(&bytes).into();
                        if digest != expected {
                            return Err(HostBundleError::ArtifactContentMismatch);
                        }
                    }
                    (Some(_), None) => return Err(HostBundleError::ArtifactContentMismatch),
                    (None, None) => {}
                    (None, Some(_)) => {
                        return Err(HostBundleError::OwnershipConflict(format!(
                            "{}: a file appeared at a path this transaction removed",
                            mutation.relative_path
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn rollback_component_set<R: HostComponentSetRegistrationV1>(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        registration: &mut R,
        prepared: &[PreparedHostComponentSetComponentV1],
        undo: &[ArtifactUndo],
    ) -> Result<(), HostBundleError> {
        registration.rollback(component_set, request)?;
        self.undo_artifact_mutations(undo)?;
        for component in prepared.iter().rev() {
            match &component.previous_receipt {
                Some(receipt) => self.write_receipt(receipt)?,
                None => self.remove_receipt(component_set.host, component.manifest.component)?,
            }
        }
        self.remove_component_set_receipt(request.operation_id)
    }
}

fn component_set_receipt_from_prepared(
    prepared: &[PreparedHostComponentSetComponentV1],
    request: &HostComponentSetExecutionRequestV1,
    confirmed_preview: Option<&HostComponentSetLifecyclePreviewV1>,
) -> Result<HostComponentSetReceiptV1, HostBundleError> {
    // Provenance is preserved only for a *companion*: a component an incremental
    // Update left untouched while it did real work on a sibling. Two gates bound
    // this:
    //
    // * The operation is an Update, the only incremental one. Install
    //   first-deploys every component, Repair re-asserts ownership of the whole
    //   cataloged set, and Uninstall removes it, so each legitimately stamps its
    //   operation onto every receipt, changed or not.
    // * The set performed at least one effective artifact write. A transaction
    //   that writes nothing anywhere is a pure no-op re-run of the identical
    //   set, and still records its operation rather than reusing the prior one.
    let preserves_untouched_companions = request.lifecycle.operation
        == HostBundleLifecycleOpV1::Update
        && prepared.iter().any(|component| {
            component
                .plan
                .mutations
                .iter()
                .any(|mutation| mutation.action != HostArtifactActionV1::Noop)
        });
    let component_receipts = prepared
        .iter()
        .map(|component| {
            // An unchanged companion, one whose plan writes nothing and whose
            // manifest is byte-identical to its durable receipt, keeps its
            // original operation provenance. "Writes nothing" must be read from
            // each mutation's action, not from an empty mutation list: a
            // component with manifest artifacts always plans one Noop mutation
            // per artifact.
            if preserves_untouched_companions
                && component
                    .plan
                    .mutations
                    .iter()
                    .all(|mutation| mutation.action == HostArtifactActionV1::Noop)
                && let Some(previous_receipt) = &component.previous_receipt
                && previous_receipt.manifest_digest == component.manifest.canonical_digest()?
            {
                return Ok(previous_receipt.clone());
            }
            Ok(HostBundleInstallReceiptV1 {
                schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
                operation_id: request.operation_id,
                host: component.manifest.host,
                component: component.manifest.component,
                operation: request.lifecycle.operation,
                manifest_digest: component.manifest.canonical_digest()?,
                artifacts: if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall {
                    Vec::new()
                } else {
                    component
                        .manifest
                        .artifacts
                        .iter()
                        .map(|artifact| HostBundleReceiptArtifactV1 {
                            relative_path: artifact.relative_path.clone(),
                            artifact_digest: artifact.artifact_digest,
                            ownership_marker: artifact.ownership_marker.clone(),
                        })
                        .collect()
                },
            })
        })
        .collect::<Result<Vec<_>, HostBundleError>>()?;
    let receipt = HostComponentSetReceiptV1 {
        schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
        operation_id: request.operation_id,
        host: request.lifecycle.expected_host,
        operation: request.lifecycle.operation,
        component_manifests: prepared
            .iter()
            .map(|component| component.manifest.clone())
            .collect(),
        component_receipts,
        confirmed_plan_digest: confirmed_preview.map(|preview| preview.plan_digest),
        base_registration_revision: confirmed_preview
            .map(|preview| preview.base_registration_revision),
        current_registration_revision: confirmed_preview
            .map(|preview| preview.current_registration_revision),
        artifact_state_revision: confirmed_preview.map(|preview| preview.artifact_state_revision),
    };
    validate_component_set_receipt(&receipt)?;
    Ok(receipt)
}
