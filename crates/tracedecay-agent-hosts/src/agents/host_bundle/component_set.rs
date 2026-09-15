//! Aggregate component-set transaction: one journal, one registration
//! adapter, and one rollback boundary spanning every component of a host.

use std::collections::BTreeMap;
use std::path::Path;

use cap_std::fs::Dir;
use sha2::{Digest, Sha256};
use tracedecay_host_integration::host_bundle_recovery_required;
use tracedecay_host_integration::host_bundle_stale_preview;
use tracedecay_host_integration::host_bundle_storage_failure;

use super::control::{
    backup_name, component_set_from_journal, component_set_receipt_matches,
    component_set_receipt_matches_preview, component_set_stage_name,
    validate_component_set_journal, validate_component_set_receipt, validate_component_set_request,
};
use super::model::{
    HostComponentSetExecutionRequestV1, HostComponentSetLifecyclePreviewV1,
    HostComponentSetLifecycleRequestV1, HostComponentSetRegistrationV1, HostComponentSetV1,
};
use super::planner::{
    HostArtifactActionV1, HostBundleLifecycleRequestV1, HostBundleMutationPlanV1,
    component_receiptless_adoption, dry_run_host_component_set_lifecycle_with_lifecycle_root_at,
    observe_artifact_at, plan_verified_complete_lifecycle_mutation,
    validate_artifact_contents_for_operation,
};
use super::writer::{
    HostBundleWriterV1, atomic_write_nofollow, move_regular_to_backup, read_regular_nofollow,
    regular_file_exists, remove_if_digest_matches, sync_cap_dir,
};
use super::{
    HOST_BUNDLE_RECEIPT_SCHEMA_VERSION, HostBundleComponentV1, HostBundleError,
    HostBundleInstallReceiptV1, HostBundleJournalEntryV1, HostBundleLifecycleOpV1,
    HostBundleManifestV1, HostBundleReceiptArtifactV1, HostBundleRollbackBoundaryV1,
    HostBundleVerificationAdapterV1, HostComponentSetJournalComponentV1,
    HostComponentSetJournalStateV1, HostComponentSetJournalV1, HostComponentSetReceiptV1,
    HostKindV1,
};

/// Public component-set lifecycle façade over the capability-rooted writer.
/// It keeps the existing per-component receipt API intact while ensuring the
/// default host lifecycle has one aggregate recovery boundary.
pub struct HostComponentSetTransactionV1<'a> {
    writer: &'a mut HostBundleWriterV1,
}

impl<'a> HostComponentSetTransactionV1<'a> {
    pub fn new(writer: &'a mut HostBundleWriterV1) -> Self {
        Self { writer }
    }

    /// Recover whichever single component-set journal is outstanding. Callers
    /// that know the host should prefer [`Self::recover_host`], which never
    /// hands another host's journal to this registration authority.
    pub fn recover<R: HostComponentSetRegistrationV1>(
        &mut self,
        registration: &mut R,
    ) -> Result<(), HostBundleError> {
        self.writer
            .recover_component_set_operation(None, registration)?;
        self.writer.recover_interrupted_operation()
    }

    /// Recover only `host`'s pending component-set journal. Other hosts'
    /// journals are left untouched: their artifact path spaces are disjoint,
    /// and their registration state belongs to a different adapter.
    pub fn recover_host<R: HostComponentSetRegistrationV1>(
        &mut self,
        host: HostKindV1,
        registration: &mut R,
    ) -> Result<(), HostBundleError> {
        self.writer
            .recover_component_set_operation(Some(host), registration)?;
        self.writer.recover_interrupted_operation()
    }

    pub fn preview<V: HostBundleVerificationAdapterV1, R: HostComponentSetRegistrationV1>(
        &mut self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        verifier: &V,
        registration: &mut R,
    ) -> Result<HostComponentSetLifecyclePreviewV1, HostBundleError> {
        // Only this host's own pending journal blocks the preview. A wedged
        // transaction for an unrelated host mutates a disjoint path space and
        // is not a reason to refuse work here.
        if self.writer.load_journal()?.is_some()
            || self
                .writer
                .load_component_set_journal_for(component_set.host)?
                .is_some()
        {
            return Err(host_bundle_recovery_required!());
        }
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
        // Host-scoped: a pending journal for an unrelated host governs a
        // disjoint artifact subtree and belongs to a different registration
        // adapter, so it must neither be recovered here nor block this work.
        self.recover_host(component_set.host, registration)?;
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
    /// Execute a complete canonical host component set under one aggregate
    /// journal. Every component is preflighted and staged before any owned
    /// file is moved; receipts are published only after all artifacts and the
    /// host registration authority verify successfully.
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
        if self.load_journal()?.is_some() {
            return Err(host_bundle_recovery_required!());
        }
        // Never clobber this host's own outstanding journal: it is the only
        // durable record of how to roll the earlier transaction back.
        if self
            .load_component_set_journal_for(component_set.host)?
            .is_some()
        {
            return Err(host_bundle_recovery_required!());
        }
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

        // Resolve receiptless-adoption authority through the same adapter the
        // preview used, so the replanned mutations match the confirmed plan.
        let adoption_by_component: BTreeMap<HostBundleComponentV1, bool> = component_set
            .components
            .iter()
            .map(|component| {
                (
                    component.manifest.component,
                    component_receiptless_adoption(
                        request,
                        registration,
                        component.manifest.component,
                    ),
                )
            })
            .collect();
        let prepared =
            self.preflight_component_set(component_set, request, verifier, &adoption_by_component)?;
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

        let mut journal = HostComponentSetJournalV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: request.operation_id,
            host: component_set.host,
            operation: request.lifecycle.operation,
            explicit_confirmation: request.lifecycle.explicit_confirmation,
            hermes_profile_bindings: request.lifecycle.hermes_profile_bindings,
            confirmed_plan_digest: confirmed_preview.map(|preview| preview.plan_digest),
            base_registration_revision: confirmed_preview
                .map(|preview| preview.base_registration_revision),
            current_registration_revision: confirmed_preview
                .map(|preview| preview.current_registration_revision),
            artifact_state_revision: confirmed_preview
                .map(|preview| preview.artifact_state_revision),
            state: HostComponentSetJournalStateV1::Prepared,
            registration_staged: false,
            registration_applied: false,
            components: prepared
                .iter()
                .map(|component| HostComponentSetJournalComponentV1 {
                    manifest: component.manifest.clone(),
                    previous_receipt: component.previous_receipt.clone(),
                    entries: component
                        .plan
                        .mutations
                        .iter()
                        .map(|mutation| HostBundleJournalEntryV1 {
                            relative_path: mutation.relative_path.clone(),
                            backup_name: matches!(
                                mutation.action,
                                HostArtifactActionV1::BackupThenReplace
                                    | HostArtifactActionV1::BackupThenRemove
                            )
                            .then(|| backup_name(request.operation_id, &mutation.relative_path)),
                            backup_created: false,
                            wrote_new: false,
                            installed_digest: component
                                .manifest
                                .artifacts
                                .iter()
                                .find(|artifact| artifact.relative_path == mutation.relative_path)
                                .map(|artifact| artifact.artifact_digest)
                                .filter(|_| {
                                    !matches!(
                                        mutation.action,
                                        HostArtifactActionV1::BackupThenRemove
                                    )
                                }),
                        })
                        .collect(),
                })
                .collect(),
        };
        self.write_component_set_journal(&journal)?;

        let result = (|| {
            self.stage_component_set_assets(&prepared, request.operation_id)?;
            journal.registration_staged = true;
            self.write_component_set_journal(&journal)?;
            registration.stage(component_set, request)?;
            journal.state = HostComponentSetJournalStateV1::Staged;
            self.write_component_set_journal(&journal)?;

            let backup_dir = self.open_or_create_backup_dir(request.operation_id)?;
            self.backup_component_set_entries(&prepared, &mut journal, &backup_dir)?;
            drop(backup_dir);
            self.write_component_set_entries(&prepared, &mut journal)?;

            // Mark this before calling into host registration: a failing
            // adapter can still have made a partial native mutation.
            journal.registration_applied = true;
            self.write_component_set_journal(&journal)?;
            registration.apply(component_set, request)?;
            journal.state = HostComponentSetJournalStateV1::Applied;
            self.write_component_set_journal(&journal)?;

            self.verify_component_set_artifacts(&journal)?;
            registration.verify(component_set, request)?;
            journal.state = HostComponentSetJournalStateV1::Verified;
            self.write_component_set_journal(&journal)?;

            let receipt =
                component_set_receipt_from_prepared(&prepared, request, confirmed_preview)?;
            for component_receipt in &receipt.component_receipts {
                self.write_receipt(component_receipt)?;
            }
            self.write_component_set_receipt(&receipt)?;
            journal.state = HostComponentSetJournalStateV1::Committed;
            self.write_component_set_journal(&journal)?;

            // Registration cleanup and backup retirement happen only after the
            // aggregate and every component receipt have crossed commit.
            registration.commit(component_set, request)?;
            self.cleanup_component_set_boundary(request.operation_id)?;
            self.remove_component_set_journal(component_set.host)?;
            Ok(receipt)
        })();

        match result {
            Ok(receipt) => Ok(receipt),
            Err(error) if journal.state == HostComponentSetJournalStateV1::Committed => {
                // The durable receipts prove commit. Keep the journal for a
                // restarted transaction to finish registration/backup cleanup.
                Err(error)
            }
            Err(error) => {
                if self
                    .rollback_component_set(component_set, request, registration, &mut journal)
                    .is_err()
                {
                    Err(host_bundle_recovery_required!())
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Resume a component-set operation left by a failed apply or a process
    /// interruption. A fully published aggregate receipt wins; any other
    /// state is rolled back in reverse component and artifact order.
    fn recover_component_set_operation<R: HostComponentSetRegistrationV1>(
        &mut self,
        host: Option<HostKindV1>,
        registration: &mut R,
    ) -> Result<(), HostBundleError> {
        let loaded = match host {
            Some(host) => self.load_component_set_journal_for(host)?,
            None => self.load_component_set_journal()?,
        };
        let Some(mut journal) = loaded else {
            return Ok(());
        };
        validate_component_set_journal(&journal)?;
        let component_set = component_set_from_journal(&journal);
        let request = HostComponentSetExecutionRequestV1 {
            lifecycle: HostComponentSetLifecycleRequestV1 {
                operation: journal.operation,
                expected_host: journal.host,
                expected_components: journal
                    .components
                    .iter()
                    .map(|component| component.manifest.component)
                    .collect(),
                explicit_confirmation: journal.explicit_confirmation,
                hermes_profile_bindings: journal.hermes_profile_bindings,
                // Recovery replays or rolls back the journaled mutations; it
                // never re-plans, so it can never adopt anything new.
                explicit_adoption: false,
            },
            operation_id: journal.operation_id,
        };

        if journal.state == HostComponentSetJournalStateV1::Committed
            || self.component_set_commit_is_complete(&journal)?
        {
            registration.commit(&component_set, &request)?;
            self.cleanup_component_set_boundary(journal.operation_id)?;
            self.remove_component_set_journal(journal.host)?;
            return Ok(());
        }

        if journal.state == HostComponentSetJournalStateV1::RolledBack {
            // A rolled-back journal keeps whichever flags the failed attempt
            // had reached, so they describe the interrupted work rather than
            // the compensation still owed. Re-attempt it unconditionally: the
            // adapter contract is idempotent and no-ops when it finds no staged
            // registration backup, while skipping it would strand a mutated
            // native host configuration with nothing left to compensate it.
            registration.rollback(&component_set, &request)?;
            self.cleanup_component_set_boundary(journal.operation_id)?;
            self.remove_component_set_journal(journal.host)?;
            return Ok(());
        }

        if journal.registration_compensation_required() {
            registration.rollback(&component_set, &request)?;
        }
        self.restore_component_set_artifacts(&journal)?;
        journal.state = HostComponentSetJournalStateV1::RolledBack;
        self.write_component_set_journal(&journal)?;
        self.cleanup_component_set_boundary(journal.operation_id)?;
        self.remove_component_set_journal(journal.host)
    }

    fn preflight_component_set<V: HostBundleVerificationAdapterV1>(
        &self,
        component_set: &HostComponentSetV1,
        request: &HostComponentSetExecutionRequestV1,
        verifier: &V,
        adoption_by_component: &BTreeMap<HostBundleComponentV1, bool>,
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
                adopt_receiptless: adoption_by_component
                    .get(&component.manifest.component)
                    .copied()
                    .unwrap_or(false),
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

    fn stage_component_set_assets(
        &self,
        prepared: &[PreparedHostComponentSetComponentV1],
        operation_id: [u8; 16],
    ) -> Result<(), HostBundleError> {
        let stage = self.open_or_create_component_set_stage_dir(operation_id)?;
        for component in prepared {
            for (relative_path, bytes) in &component.content_by_path {
                let stage_name =
                    component_set_stage_name(component.manifest.component, relative_path);
                atomic_write_nofollow(&stage, &stage_name, bytes, false)?;
            }
        }
        sync_cap_dir(&stage)
    }

    fn backup_component_set_entries(
        &self,
        prepared: &[PreparedHostComponentSetComponentV1],
        journal: &mut HostComponentSetJournalV1,
        backup_dir: &Dir,
    ) -> Result<(), HostBundleError> {
        for (component_index, prepared_component) in prepared.iter().enumerate() {
            for (entry_index, mutation) in prepared_component.plan.mutations.iter().enumerate() {
                if !matches!(
                    mutation.action,
                    HostArtifactActionV1::BackupThenReplace
                        | HostArtifactActionV1::BackupThenRemove
                ) {
                    continue;
                }
                let backup_name = journal.components[component_index].entries[entry_index]
                    .backup_name
                    .clone()
                    .ok_or(HostBundleError::ReceiptCorrupted)?;
                let (parent, name) =
                    self.open_parent_nofollow(Path::new(&mutation.relative_path))?;
                move_regular_to_backup(&parent, &name, backup_dir, &backup_name)?;
                journal.components[component_index].entries[entry_index].backup_created = true;
                self.write_component_set_journal(journal)?;
            }
        }
        Ok(())
    }

    fn write_component_set_entries(
        &self,
        prepared: &[PreparedHostComponentSetComponentV1],
        journal: &mut HostComponentSetJournalV1,
    ) -> Result<(), HostBundleError> {
        for (component_index, prepared_component) in prepared.iter().enumerate() {
            for (entry_index, mutation) in prepared_component.plan.mutations.iter().enumerate() {
                let (parent, name) =
                    self.open_parent_nofollow(Path::new(&mutation.relative_path))?;
                match mutation.action {
                    HostArtifactActionV1::Noop | HostArtifactActionV1::BackupThenRemove => {}
                    HostArtifactActionV1::WriteNew | HostArtifactActionV1::BackupThenReplace => {
                        journal.components[component_index].entries[entry_index].wrote_new = true;
                        self.write_component_set_journal(journal)?;
                        atomic_write_nofollow(
                            &parent,
                            &name,
                            prepared_component
                                .content_by_path
                                .get(&mutation.relative_path)
                                .ok_or(HostBundleError::ArtifactContentMismatch)?,
                            false,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn verify_component_set_artifacts(
        &self,
        journal: &HostComponentSetJournalV1,
    ) -> Result<(), HostBundleError> {
        for component in &journal.components {
            for entry in &component.entries {
                let (parent, name) = self.open_parent_nofollow(Path::new(&entry.relative_path))?;
                let observed = read_regular_nofollow(&parent, &name)?;
                match (entry.installed_digest, observed) {
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
                            entry.relative_path
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
        journal: &mut HostComponentSetJournalV1,
    ) -> Result<(), HostBundleError> {
        if journal.registration_compensation_required() {
            registration.rollback(component_set, request)?;
        }
        self.restore_component_set_artifacts(journal)?;
        self.remove_component_set_receipt(journal.operation_id)?;
        journal.state = HostComponentSetJournalStateV1::RolledBack;
        // Leave the completed rollback journal and its backups for an explicit
        // restart reconciliation boundary; a new transaction invokes recover.
        self.write_component_set_journal(journal)
    }

    fn restore_component_set_artifacts(
        &self,
        journal: &HostComponentSetJournalV1,
    ) -> Result<(), HostBundleError> {
        let backup_dir = self.open_existing_backup_dir(journal.operation_id)?;
        for component in journal.components.iter().rev() {
            for entry in component.entries.iter().rev() {
                self.restore_component_set_entry(entry, backup_dir.as_ref())?;
            }
        }
        for component in journal.components.iter().rev() {
            match &component.previous_receipt {
                Some(receipt) => self.write_receipt(receipt)?,
                None => self.remove_receipt(journal.host, component.manifest.component)?,
            }
        }
        Ok(())
    }

    /// Restore one journal entry to its pre-transaction state.
    ///
    /// Rollback must be able to CONVERGE when a second writer touched a
    /// deployed path after this transaction wrote it. A post-apply fault can
    /// leave live bytes that are neither the backup nor this transaction's
    /// cataloged output. Before the convergence rules below, that state was
    /// unrecoverable: rollback
    /// returned `RecoveryRequired` forever, the journal stayed behind, and
    /// every later host transaction failed up front.
    ///
    /// Two content equalities are provably safe to converge on, because in both
    /// cases the operator-visible end state is byte-identical to a successful
    /// restore:
    ///
    /// 1. **Live bytes equal the pre-transaction backup.** The end state
    ///    rollback wants is already true; renaming the backup over it would
    ///    produce the same bytes. Treat the path as restored.
    /// 2. **Live bytes equal this entry's cataloged install target
    ///    (`installed_digest`).** Those bytes are provably this transaction's
    ///    own output, so removing them is a restore and not third-party data
    ///    loss. This also closes the crash window between the artifact write
    ///    and the `wrote_new` journal update.
    ///
    /// Anything else — foreign bytes that match neither — stays fail-closed
    /// with `RecoveryRequired`, and the operator resolves it explicitly with
    /// `tracedecay host-bundle recover`.
    fn restore_component_set_entry(
        &self,
        entry: &HostBundleJournalEntryV1,
        backup_dir: Option<&Dir>,
    ) -> Result<(), HostBundleError> {
        let (parent, name) = self.open_parent_nofollow(Path::new(&entry.relative_path))?;
        if let Some(backup_name) = &entry.backup_name {
            let backup_bytes = match backup_dir {
                Some(backups) => read_regular_nofollow(backups, backup_name)?,
                None => None,
            };
            let backup_exists = backup_bytes.is_some();
            // Convergence rule 1: the live file already holds the exact
            // pre-transaction bytes, so this path needs no mutation at all.
            // The backup stays until the boundary cleanup retires the whole
            // operation directory, which keeps a repeated restore idempotent.
            if let (Some(backup), Some(live)) = (
                backup_bytes.as_ref(),
                read_regular_nofollow(&parent, &name)?,
            ) && live == *backup
            {
                return Ok(());
            }
            if !entry.backup_created {
                if !backup_exists {
                    return Ok(());
                }
                if regular_file_exists(&parent, &name)? {
                    return Err(host_bundle_recovery_required!());
                }
            }
            let backups = backup_dir
                .filter(|_| backup_exists)
                .ok_or(host_bundle_recovery_required!())?;
            if entry.wrote_new {
                remove_if_digest_matches(
                    &parent,
                    &name,
                    entry
                        .installed_digest
                        .ok_or(HostBundleError::ReceiptCorrupted)?,
                )?;
            } else if let Some(live) = read_regular_nofollow(&parent, &name)? {
                // Convergence rule 2. `installed_digest` is `None` for a
                // BackupThenRemove entry, which has no cataloged target and
                // therefore stays fail-closed.
                let installed = entry
                    .installed_digest
                    .ok_or(host_bundle_recovery_required!())?;
                if <[u8; 32]>::from(Sha256::digest(&live)) != installed {
                    return Err(host_bundle_recovery_required!());
                }
                parent
                    .remove_file(&name)
                    .map_err(|_| host_bundle_storage_failure!())?;
            }
            backups
                .rename(backup_name, &parent, &name)
                .map_err(|_| host_bundle_storage_failure!())?;
            sync_cap_dir(backups)?;
            sync_cap_dir(&parent)
        } else if entry.wrote_new {
            // No backup: the path did not exist before the transaction, so
            // rollback wants it gone. `remove_if_digest_matches` already
            // converges on the two safe outcomes (already absent, or holding
            // this transaction's cataloged bytes). Foreign bytes at a path this
            // transaction created are genuinely ambiguous — removing them could
            // destroy another writer's file — so that case stays fail-closed.
            remove_if_digest_matches(
                &parent,
                &name,
                entry
                    .installed_digest
                    .ok_or(HostBundleError::ReceiptCorrupted)?,
            )?;
            sync_cap_dir(&parent)
        } else {
            Ok(())
        }
    }

    fn component_set_commit_is_complete(
        &self,
        journal: &HostComponentSetJournalV1,
    ) -> Result<bool, HostBundleError> {
        let Some(receipt) = self.load_component_set_receipt(journal.operation_id)? else {
            return Ok(false);
        };
        let component_set = component_set_from_journal(journal);
        let request = HostComponentSetExecutionRequestV1 {
            lifecycle: HostComponentSetLifecycleRequestV1 {
                operation: journal.operation,
                expected_host: journal.host,
                expected_components: component_set
                    .components
                    .iter()
                    .map(|component| component.manifest.component)
                    .collect(),
                explicit_confirmation: true,
                hermes_profile_bindings: u8::from(journal.host == HostKindV1::Hermes),
                // Receipt matching compares durable identity; adoption
                // authority is a planning input and plays no part here.
                explicit_adoption: false,
            },
            operation_id: journal.operation_id,
        };
        component_set_receipt_matches(&receipt, &component_set, &request)
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
    // * The operation is an Update — the only incremental one. Install
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
            // An unchanged companion — one whose plan writes nothing and whose
            // manifest is byte-identical to its durable receipt — keeps its
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
            let mut rollback_history = component
                .previous_receipt
                .as_ref()
                .map(|receipt| receipt.rollback_history.clone())
                .unwrap_or_default();
            // A Repair that overwrites a receipt-owned path whose bytes drifted
            // from the catalog backs up genuinely foreign content — a user edit,
            // never tracedecay's own prior output, because Repair replaces a
            // path only when its observed digest differs from the cataloged one,
            // which for an unchanged Repair manifest is also the previously
            // owned digest. Referencing this operation from the receipt keeps the
            // commit boundary from retiring that backup, so an operator can still
            // recover the overwritten bytes. Ordinary Update backups hold
            // tracedecay's own output and stay retired on commit.
            if request.lifecycle.operation == HostBundleLifecycleOpV1::Repair
                && component
                    .plan
                    .mutations
                    .iter()
                    .any(|mutation| mutation.action == HostArtifactActionV1::BackupThenReplace)
                && !rollback_history.contains(&request.operation_id)
            {
                rollback_history.push(request.operation_id);
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
                rollback_boundary: HostBundleRollbackBoundaryV1::Passed,
                rollback_history,
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
