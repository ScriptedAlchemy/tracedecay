//! Atomic, capability-rooted single-component writer with a recoverable
//! journal, component backup/restore, and the no-follow filesystem primitives.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_json_bytes;
use tracedecay_host_integration::host_bundle_recovery_required;
use tracedecay_host_integration::host_bundle_storage_failure;

use super::control::{
    HOST_BUNDLE_CONTROL_DIR, HOST_BUNDLE_JOURNAL_FILE, HOST_BUNDLE_LOCK_FILE,
    HOST_BUNDLE_QUARANTINE_DIR, HOST_COMPONENT_SET_JOURNAL_FILE, HOST_COMPONENT_SET_STAGE_DIR,
    MAX_CONTROL_FILE_BYTES, backup_name, component_set_journal_file, component_set_receipt_file,
    host_bundle_backup_receipt_file, host_bundle_restore_receipt_file, host_bundle_snapshot_name,
    is_safe_component, latest_host_component_set_receipt_at, receipt_file, validate_backup_receipt,
    validate_component_set_journal, validate_component_set_receipt, validate_journal,
    validate_receipt, validate_restore_receipt,
};
use super::model::{HostBundleExecutionRequestV1, HostBundleLifecycleStorageV1};
use super::planner::{
    HostArtifactActionV1, HostBundleLifecycleRequestV1, ObservedArtifactKindV1,
    ObservedHostArtifactV1, plan_verified_complete_lifecycle_mutation,
    validate_artifact_contents_for_operation,
};
use super::{
    HOST_BUNDLE_RECEIPT_SCHEMA_VERSION, HostBundleArtifactContentV1, HostBundleBackupArtifactV1,
    HostBundleBackupReceiptV1, HostBundleComponentV1, HostBundleError, HostBundleInstallReceiptV1,
    HostBundleJournalEntryV1, HostBundleJournalStateV1, HostBundleJournalV1,
    HostBundleLifecycleOpV1, HostBundleManifestV1, HostBundleReceiptArtifactV1,
    HostBundleRestoreReceiptV1, HostBundleRollbackBoundaryV1, HostBundleVerificationAdapterV1,
    HostComponentSetJournalV1, HostComponentSetReceiptV1, HostKindV1, MAX_ARTIFACT_CONTENT_BYTES,
    stock_host_kinds, validate_relative_install_path,
};

static HOST_BUNDLE_TEMP_NONCE: AtomicU64 = AtomicU64::new(1);

/// Atomic, capability-rooted host-bundle writer. Every descendant directory
/// is opened without following symlinks; files are staged, fsynced, renamed,
/// and followed by a directory sync before receipt publication.
pub struct HostBundleWriterV1 {
    pub(super) root_path: PathBuf,
    pub(super) lifecycle_root_path: PathBuf,
    root: Dir,
    control: Dir,
    _writer_lock: fs::File,
}

impl HostBundleWriterV1 {
    pub fn open(root_path: impl Into<PathBuf>) -> Result<Self, HostBundleError> {
        let root_path = root_path.into();
        Self::open_with_lifecycle_root(root_path.clone(), root_path)
    }

    pub fn open_with_lifecycle_root(
        root_path: impl Into<PathBuf>,
        lifecycle_root_path: impl Into<PathBuf>,
    ) -> Result<Self, HostBundleError> {
        let root_path = root_path.into();
        let lifecycle_root_path = lifecycle_root_path.into();
        ensure_bundle_root(&root_path)?;
        ensure_bundle_root(&lifecycle_root_path)?;
        let root = Dir::open_ambient_dir(&root_path, ambient_authority())
            .map_err(|_| HostBundleError::UnsafeInstallPath)?;
        let lifecycle_root = Dir::open_ambient_dir(&lifecycle_root_path, ambient_authority())
            .map_err(|_| HostBundleError::UnsafeInstallPath)?;
        let control = open_or_create_nofollow_dir(&lifecycle_root, HOST_BUNDLE_CONTROL_DIR)?;
        let writer_lock = open_writer_lock(&control)?;
        let mut writer = Self {
            root_path,
            lifecycle_root_path,
            root,
            control,
            _writer_lock: writer_lock,
        };
        writer.recover_interrupted_operation()?;
        Ok(writer)
    }

    /// Recover by rolling an incomplete transaction back from its immutable
    /// backups. A receipt matching the journal operation is a durable commit
    /// marker and is never rolled back after a crash between receipt/journal
    /// cleanup.
    pub fn recover_interrupted_operation(&mut self) -> Result<(), HostBundleError> {
        let Some(journal) = self.load_journal()? else {
            return Ok(());
        };
        validate_journal(&journal)?;
        if let Some(receipt) =
            self.load_receipt(journal.host, journal.component)?
                .filter(|receipt| {
                    receipt.operation_id == journal.operation_id
                        && receipt.operation == journal.operation
                        && receipt.manifest_digest == journal.manifest_digest
                })
        {
            self.remove_control_file(HOST_BUNDLE_JOURNAL_FILE)?;
            if receipt.rollback_boundary == HostBundleRollbackBoundaryV1::Passed {
                self.cleanup_unreferenced_backup_dir(journal.operation_id)?;
            }
            return Ok(());
        }

        let backup_dir = self.open_existing_backup_dir(journal.operation_id)?;
        for entry in journal.entries.iter().rev() {
            let (parent, name) = self.open_parent_nofollow(Path::new(&entry.relative_path))?;
            if let Some(backup_name) = &entry.backup_name {
                let backup_exists = match &backup_dir {
                    Some(backups) => regular_file_exists(backups, backup_name)?,
                    None => false,
                };
                if !entry.backup_created {
                    if !backup_exists {
                        continue;
                    }
                    if regular_file_exists(&parent, &name)? {
                        return Err(host_bundle_recovery_required!());
                    }
                }
                let backups = backup_dir
                    .as_ref()
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
                } else if regular_file_exists(&parent, &name)? {
                    return Err(host_bundle_recovery_required!());
                }
                backups
                    .rename(backup_name, &parent, &name)
                    .map_err(|_| host_bundle_storage_failure!())?;
                sync_cap_dir(backups)?;
                sync_cap_dir(&parent)?;
            } else if entry.wrote_new {
                remove_if_digest_matches(
                    &parent,
                    &name,
                    entry
                        .installed_digest
                        .ok_or(HostBundleError::ReceiptCorrupted)?,
                )?;
                sync_cap_dir(&parent)?;
            }
        }
        drop(backup_dir);
        match journal.previous_receipt {
            Some(receipt) => self.write_receipt(&receipt)?,
            None => self.remove_receipt(journal.host, journal.component)?,
        }
        self.remove_control_file(HOST_BUNDLE_JOURNAL_FILE)?;
        self.cleanup_unreferenced_backup_dir(journal.operation_id)
    }

    /// Verify first-party catalog identity, validate artifact bytes, plan ownership-aware
    /// mutations, then execute them atomically with a recoverable journal.
    #[hotpath::measure(label = "hosts.agent.host_bundle.execute")]
    pub fn execute(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
        verifier: &impl HostBundleVerificationAdapterV1,
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError> {
        if request.operation_id == [0; 16] {
            return Err(HostBundleError::InvalidManifest);
        }
        verifier.verify_manifest(manifest)?;
        let content_by_path = validate_artifact_contents(manifest, request, contents)?;
        // Scoped to this manifest's own host: another host's pending
        // component-set journal governs a disjoint artifact subtree.
        if self
            .load_component_set_journal_for(manifest.host)?
            .is_some()
        {
            return Err(host_bundle_recovery_required!());
        }
        self.recover_interrupted_operation()?;
        let previous_receipt = self.load_receipt(manifest.host, manifest.component)?;
        let manifest_digest = manifest.canonical_digest()?;
        if let Some(receipt) = previous_receipt.as_ref()
            && receipt.operation_id == request.operation_id
        {
            return if receipt.operation == request.lifecycle.operation
                && receipt.manifest_digest == manifest_digest
            {
                Ok(receipt.clone())
            } else {
                Err(HostBundleError::ReceiptCorrupted)
            };
        }
        let owned_receipt = previous_receipt
            .as_ref()
            .filter(|receipt| receipt.operation != HostBundleLifecycleOpV1::Uninstall);
        let manifest_observed = if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall
        {
            Vec::new()
        } else {
            self.observe_artifacts(manifest, owned_receipt)?
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
                        || !manifest
                            .artifacts
                            .iter()
                            .any(|artifact| artifact.relative_path == owned.relative_path)
                })
                .map(|owned| self.observe_owned_artifact(owned))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let plan = plan_verified_complete_lifecycle_mutation(
            manifest,
            &request.lifecycle,
            &manifest_observed,
            owned_receipt,
            &orphan_observed,
            verifier,
        )?;
        let mut journal = HostBundleJournalV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: request.operation_id,
            host: manifest.host,
            component: manifest.component,
            operation: request.lifecycle.operation,
            manifest_digest,
            state: HostBundleJournalStateV1::Prepared,
            previous_receipt: previous_receipt.clone(),
            entries: plan
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
                    installed_digest: manifest
                        .artifacts
                        .iter()
                        .find(|artifact| artifact.relative_path == mutation.relative_path)
                        .map(|artifact| artifact.artifact_digest)
                        .filter(|_| {
                            !matches!(mutation.action, HostArtifactActionV1::BackupThenRemove)
                        }),
                })
                .collect(),
        };
        self.write_journal(&journal)?;
        let backup_dir = self.open_or_create_backup_dir(request.operation_id)?;

        for (index, mutation) in plan.mutations.iter().enumerate() {
            let (parent, name) = self.open_parent_nofollow(Path::new(&mutation.relative_path))?;
            match mutation.action {
                HostArtifactActionV1::Noop => {}
                HostArtifactActionV1::WriteNew => {
                    journal.entries[index].wrote_new = true;
                    self.write_journal(&journal)?;
                    atomic_write_nofollow(
                        &parent,
                        &name,
                        content_by_path
                            .get(&mutation.relative_path)
                            .ok_or(HostBundleError::ArtifactContentMismatch)?,
                        false,
                    )?;
                }
                HostArtifactActionV1::BackupThenReplace => {
                    let backup_name = journal.entries[index]
                        .backup_name
                        .as_deref()
                        .ok_or(HostBundleError::ReceiptCorrupted)?;
                    move_regular_to_backup(&parent, &name, &backup_dir, backup_name)?;
                    journal.entries[index].backup_created = true;
                    self.write_journal(&journal)?;
                    journal.entries[index].wrote_new = true;
                    self.write_journal(&journal)?;
                    atomic_write_nofollow(
                        &parent,
                        &name,
                        content_by_path
                            .get(&mutation.relative_path)
                            .ok_or(HostBundleError::ArtifactContentMismatch)?,
                        false,
                    )?;
                }
                HostArtifactActionV1::BackupThenRemove => {
                    let backup_name = journal.entries[index]
                        .backup_name
                        .as_deref()
                        .ok_or(HostBundleError::ReceiptCorrupted)?;
                    move_regular_to_backup(&parent, &name, &backup_dir, backup_name)?;
                    journal.entries[index].backup_created = true;
                    self.write_journal(&journal)?;
                }
            }
        }
        drop(backup_dir);

        let receipt = HostBundleInstallReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: request.operation_id,
            host: manifest.host,
            component: manifest.component,
            operation: request.lifecycle.operation,
            manifest_digest,
            artifacts: if request.lifecycle.operation == HostBundleLifecycleOpV1::Uninstall {
                Vec::new()
            } else {
                manifest
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
            rollback_history: previous_receipt
                .as_ref()
                .map(|receipt| receipt.rollback_history.clone())
                .unwrap_or_default(),
        };
        self.write_receipt(&receipt)?;
        journal.state = HostBundleJournalStateV1::Committed;
        self.write_journal(&journal)?;
        self.remove_control_file(HOST_BUNDLE_JOURNAL_FILE)?;
        if receipt.rollback_boundary == HostBundleRollbackBoundaryV1::Passed {
            self.cleanup_unreferenced_backup_dir(request.operation_id)?;
        }
        Ok(receipt)
    }

    fn observe_artifacts(
        &self,
        manifest: &HostBundleManifestV1,
        receipt: Option<&HostBundleInstallReceiptV1>,
    ) -> Result<Vec<ObservedHostArtifactV1>, HostBundleError> {
        let mut observed = Vec::with_capacity(manifest.artifacts.len());
        for artifact in &manifest.artifacts {
            let (parent, name) = self.open_parent_nofollow(Path::new(&artifact.relative_path))?;
            let receipt_artifact = receipt.and_then(|receipt| {
                (receipt.host == manifest.host && receipt.component == manifest.component)
                    .then_some(receipt)
                    .and_then(|receipt| {
                        receipt
                            .artifacts
                            .iter()
                            .find(|record| record.relative_path == artifact.relative_path)
                    })
            });
            let (kind, digest) = match read_regular_nofollow(&parent, &name)? {
                None => (ObservedArtifactKindV1::Missing, None),
                Some(bytes) => {
                    let digest: [u8; 32] = Sha256::digest(&bytes).into();
                    (ObservedArtifactKindV1::RegularFile, Some(digest))
                }
            };
            observed.push(ObservedHostArtifactV1 {
                relative_path: artifact.relative_path.clone(),
                kind,
                artifact_digest: digest,
                ownership_marker: receipt_artifact.map(|record| record.ownership_marker.clone()),
                owned_artifact_digest: receipt_artifact.map(|record| record.artifact_digest),
                cataloged_ownership_marker: Some(artifact.ownership_marker.clone()),
            });
        }
        Ok(observed)
    }

    fn observe_owned_artifact(
        &self,
        owned: &HostBundleReceiptArtifactV1,
    ) -> Result<ObservedHostArtifactV1, HostBundleError> {
        let (parent, name) = self.open_parent_nofollow(Path::new(&owned.relative_path))?;
        let (kind, artifact_digest) = match read_regular_nofollow(&parent, &name)? {
            Some(bytes) => (
                ObservedArtifactKindV1::RegularFile,
                Some(Sha256::digest(&bytes).into()),
            ),
            None => (ObservedArtifactKindV1::Missing, None),
        };
        Ok(ObservedHostArtifactV1 {
            relative_path: owned.relative_path.clone(),
            kind,
            artifact_digest,
            ownership_marker: Some(owned.ownership_marker.clone()),
            owned_artifact_digest: Some(owned.artifact_digest),
            cataloged_ownership_marker: None,
        })
    }

    pub(super) fn open_parent_nofollow(
        &self,
        relative: &Path,
    ) -> Result<(Dir, String), HostBundleError> {
        validate_relative_install_path(relative)?;
        let mut parent = self
            .root
            .open_dir_nofollow(".")
            .map_err(|_| HostBundleError::UnsafeInstallPath)?;
        let components = relative.components().collect::<Vec<_>>();
        let Some(Component::Normal(last)) = components.last() else {
            return Err(HostBundleError::UnsafeInstallPath);
        };
        for component in &components[..components.len().saturating_sub(1)] {
            let Component::Normal(component) = component else {
                return Err(HostBundleError::UnsafeInstallPath);
            };
            let name = component
                .to_str()
                .ok_or(HostBundleError::UnsafeInstallPath)?;
            parent = open_or_create_nofollow_dir(&parent, name)?;
        }
        Ok((
            parent,
            last.to_str()
                .ok_or(HostBundleError::UnsafeInstallPath)?
                .to_owned(),
        ))
    }

    pub(super) fn open_or_create_backup_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Dir, HostBundleError> {
        let backups = open_or_create_nofollow_dir(&self.control, "backups")?;
        open_or_create_nofollow_dir(&backups, &hex::encode(operation_id))
    }

    pub(super) fn open_or_create_component_set_stage_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Dir, HostBundleError> {
        let stages = open_or_create_nofollow_dir(&self.control, HOST_COMPONENT_SET_STAGE_DIR)?;
        open_or_create_nofollow_dir(&stages, &hex::encode(operation_id))
    }

    pub(super) fn open_existing_backup_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<Dir>, HostBundleError> {
        let backups = match self.control.open_dir_nofollow("backups") {
            Ok(backups) => backups,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(HostBundleError::UnsafeInstallPath),
        };
        match backups.open_dir_nofollow(hex::encode(operation_id)) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(HostBundleError::UnsafeInstallPath),
        }
    }

    pub(super) fn load_receipt(
        &self,
        host: HostKindV1,
        component: HostBundleComponentV1,
    ) -> Result<Option<HostBundleInstallReceiptV1>, HostBundleError> {
        let receipt = read_control_json(&self.control, &receipt_file(host, component))?;
        let receipt = receipt
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted)
            })
            .transpose()?;
        if let Some(receipt) = &receipt {
            validate_receipt(receipt)?;
            if receipt.host != host || receipt.component != component {
                return Err(HostBundleError::ReceiptCorrupted);
            }
        }
        Ok(receipt)
    }

    #[hotpath::measure(label = "hosts.agent.host_bundle.receipt_persist")]
    pub(super) fn write_receipt(
        &self,
        receipt: &HostBundleInstallReceiptV1,
    ) -> Result<(), HostBundleError> {
        validate_receipt(receipt)?;
        let bytes = serde_json::to_vec(receipt).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(
            &self.control,
            &receipt_file(receipt.host, receipt.component),
            &bytes,
            true,
        )
    }

    pub(super) fn remove_receipt(
        &self,
        host: HostKindV1,
        component: HostBundleComponentV1,
    ) -> Result<(), HostBundleError> {
        self.remove_control_file(&receipt_file(host, component))
    }

    pub(super) fn load_component_set_receipt(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<HostComponentSetReceiptV1>, HostBundleError> {
        let receipt = read_control_json(&self.control, &component_set_receipt_file(operation_id))?
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted)
            })
            .transpose()?;
        if let Some(receipt) = &receipt {
            validate_component_set_receipt(receipt)?;
        }
        Ok(receipt)
    }

    #[hotpath::measure(label = "hosts.agent.host_bundle.component_set_receipt_persist")]
    pub(super) fn write_component_set_receipt(
        &self,
        receipt: &HostComponentSetReceiptV1,
    ) -> Result<(), HostBundleError> {
        validate_component_set_receipt(receipt)?;
        let bytes = serde_json::to_vec(receipt).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(
            &self.control,
            &component_set_receipt_file(receipt.operation_id),
            &bytes,
            false,
        )
    }

    pub(super) fn remove_component_set_receipt(
        &self,
        operation_id: [u8; 16],
    ) -> Result<(), HostBundleError> {
        self.remove_control_file(&component_set_receipt_file(operation_id))
    }

    pub(super) fn load_journal(&self) -> Result<Option<HostBundleJournalV1>, HostBundleError> {
        read_control_json(&self.control, HOST_BUNDLE_JOURNAL_FILE)?
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted)
            })
            .transpose()
    }

    fn read_component_set_journal_file(
        &self,
        file_name: &str,
    ) -> Result<Option<HostComponentSetJournalV1>, HostBundleError> {
        read_control_json(&self.control, file_name)?
            .map(|bytes| {
                serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted)
            })
            .transpose()
    }

    /// Load the pending component-set journal for one host.
    ///
    /// Journals are host-scoped so an interrupted transaction for host X never
    /// blocks an unrelated host Y. Journals written by an older binary live
    /// under the shared legacy name; they carry their own `host` field, so they
    /// are readable here and are attributed to exactly one host.
    pub(super) fn load_component_set_journal_for(
        &self,
        host: HostKindV1,
    ) -> Result<Option<HostComponentSetJournalV1>, HostBundleError> {
        if let Some(journal) =
            self.read_component_set_journal_file(&component_set_journal_file(host))?
        {
            return Ok(Some(journal));
        }
        Ok(self
            .read_component_set_journal_file(HOST_COMPONENT_SET_JOURNAL_FILE)?
            .filter(|journal| journal.host == host))
    }

    /// Load any pending component-set journal, host-scoped or legacy. Used by
    /// the host-blind recovery entry point, which must still be able to find a
    /// single outstanding transaction.
    pub(super) fn load_component_set_journal(
        &self,
    ) -> Result<Option<HostComponentSetJournalV1>, HostBundleError> {
        for host in stock_host_kinds() {
            if let Some(journal) =
                self.read_component_set_journal_file(&component_set_journal_file(host))?
            {
                return Ok(Some(journal));
            }
        }
        self.read_component_set_journal_file(HOST_COMPONENT_SET_JOURNAL_FILE)
    }

    /// Every host with a pending component-set journal. The recovery verb
    /// reports these; `--agent` narrows the set.
    pub fn pending_component_set_journal_hosts(&self) -> Result<Vec<HostKindV1>, HostBundleError> {
        let mut hosts = Vec::new();
        for host in stock_host_kinds() {
            if self.load_component_set_journal_for(host)?.is_some() {
                hosts.push(host);
            }
        }
        Ok(hosts)
    }

    pub fn pending_component_set_journal_operation(
        &self,
        host: HostKindV1,
    ) -> Result<Option<HostBundleLifecycleOpV1>, HostBundleError> {
        let Some(journal) = self.load_component_set_journal_for(host)? else {
            return Ok(None);
        };
        validate_component_set_journal(&journal)?;
        Ok(Some(journal.operation))
    }

    #[hotpath::measure(label = "hosts.agent.host_bundle.journal_persist")]
    fn write_journal(&self, journal: &HostBundleJournalV1) -> Result<(), HostBundleError> {
        validate_journal(journal)?;
        let bytes = serde_json::to_vec(journal).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(&self.control, HOST_BUNDLE_JOURNAL_FILE, &bytes, true)
    }

    #[hotpath::measure(label = "hosts.agent.host_bundle.component_set_journal_persist")]
    pub(super) fn write_component_set_journal(
        &self,
        journal: &HostComponentSetJournalV1,
    ) -> Result<(), HostBundleError> {
        validate_component_set_journal(journal)?;
        let bytes = serde_json::to_vec(journal).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(
            &self.control,
            &component_set_journal_file(journal.host),
            &bytes,
            true,
        )?;
        // A journal written by an older binary lives under the shared legacy
        // name. Once its host-scoped successor is durable, retire it so the
        // legacy file can never shadow or double-recover this transaction.
        if self
            .read_component_set_journal_file(HOST_COMPONENT_SET_JOURNAL_FILE)?
            .is_some_and(|legacy| legacy.host == journal.host)
        {
            self.remove_control_file(HOST_COMPONENT_SET_JOURNAL_FILE)?;
        }
        Ok(())
    }

    fn remove_control_file(&self, name: &str) -> Result<(), HostBundleError> {
        remove_regular_if_exists(&self.control, name)?;
        sync_cap_dir(&self.control)
    }

    /// Last-resort operator escape when convergent recovery still cannot
    /// resolve a host's component-set journal (genuinely foreign bytes at a
    /// path the transaction created, for example).
    ///
    /// The journal is *moved* into a quarantine directory rather than deleted:
    /// the transaction's immutable backups stay on disk beside it, so the
    /// pre-transaction bytes remain recoverable by hand and nothing about the
    /// failure is destroyed. Only the authority file that blocks further
    /// mutation of this host is set aside. This replaces the previous recovery
    /// path, which was hand-deleting the journal.
    ///
    /// Returns the quarantined path, or `None` when no journal was pending.
    pub fn quarantine_component_set_journal(
        &mut self,
        host: HostKindV1,
        now_unix: u64,
    ) -> Result<Option<PathBuf>, HostBundleError> {
        let mut moved = None;
        for file in [
            component_set_journal_file(host),
            HOST_COMPONENT_SET_JOURNAL_FILE.to_string(),
        ] {
            // The legacy shared file belongs to whichever host wrote it; never
            // quarantine another host's journal from under it.
            if file == HOST_COMPONENT_SET_JOURNAL_FILE
                && self
                    .read_component_set_journal_file(&file)?
                    .is_none_or(|journal| journal.host != host)
            {
                continue;
            }
            if !regular_file_exists(&self.control, &file)? {
                continue;
            }
            let quarantine =
                open_or_create_nofollow_dir(&self.control, HOST_BUNDLE_QUARANTINE_DIR)?;
            let target = format!("{now_unix}.{file}");
            if !is_safe_component(&target) {
                return Err(HostBundleError::UnsafeInstallPath);
            }
            self.control
                .rename(&file, &quarantine, &target)
                .map_err(|_| host_bundle_storage_failure!())?;
            sync_cap_dir(&quarantine)?;
            sync_cap_dir(&self.control)?;
            moved = Some(
                self.lifecycle_root_path
                    .join(HOST_BUNDLE_CONTROL_DIR)
                    .join(HOST_BUNDLE_QUARANTINE_DIR)
                    .join(target),
            );
        }
        Ok(moved)
    }

    pub(super) fn remove_component_set_journal(
        &self,
        host: HostKindV1,
    ) -> Result<(), HostBundleError> {
        self.remove_control_file(&component_set_journal_file(host))?;
        if self
            .read_component_set_journal_file(HOST_COMPONENT_SET_JOURNAL_FILE)?
            .is_some_and(|legacy| legacy.host == host)
        {
            self.remove_control_file(HOST_COMPONENT_SET_JOURNAL_FILE)?;
        }
        Ok(())
    }

    /// Retires an operation's rollback backups once no receipt still names it.
    ///
    /// Every caller must drop its `Dir` capability on the operation's backup
    /// directory first: `cap_std` opens directories without `FILE_SHARE_DELETE`,
    /// so on Windows a live handle makes the removal below fail with a sharing
    /// violation and turns a completed transaction into a storage failure.
    pub(super) fn cleanup_unreferenced_backup_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<(), HostBundleError> {
        let control_path = self.lifecycle_root_path.join(HOST_BUNDLE_CONTROL_DIR);
        let mut referenced = false;
        for entry in fs::read_dir(&control_path).map_err(|_| host_bundle_storage_failure!())? {
            let Ok(entry) = entry else {
                return Ok(());
            };
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with("receipt.") || !name.ends_with(".v1.json") {
                continue;
            }
            let Ok(bytes) = fs::read(entry.path()) else {
                return Ok(());
            };
            let Ok(receipt) = serde_json::from_slice::<HostBundleInstallReceiptV1>(&bytes) else {
                return Ok(());
            };
            if validate_receipt(&receipt).is_err() {
                return Ok(());
            }
            referenced |= receipt.rollback_history.contains(&operation_id);
        }
        if referenced {
            return Ok(());
        }
        let backup_path = control_path.join("backups").join(hex::encode(operation_id));
        match fs::symlink_metadata(&backup_path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(&backup_path).map_err(|_| host_bundle_storage_failure!())?;
            }
            Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(host_bundle_storage_failure!()),
        }
        if let Some(backups) = backup_path.parent() {
            let _ = fs::remove_dir(backups);
        }
        Ok(())
    }

    pub(super) fn cleanup_component_set_boundary(
        &self,
        operation_id: [u8; 16],
    ) -> Result<(), HostBundleError> {
        self.cleanup_unreferenced_backup_dir(operation_id)?;
        self.remove_component_set_stage_dir(operation_id)
    }

    fn remove_component_set_stage_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<(), HostBundleError> {
        let stage_path = self
            .lifecycle_root_path
            .join(HOST_BUNDLE_CONTROL_DIR)
            .join(HOST_COMPONENT_SET_STAGE_DIR)
            .join(hex::encode(operation_id));
        match fs::symlink_metadata(&stage_path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(&stage_path).map_err(|_| host_bundle_storage_failure!())?;
            }
            Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(host_bundle_storage_failure!()),
        }
        if let Some(stages) = stage_path.parent() {
            let _ = fs::remove_dir(stages);
        }
        Ok(())
    }

    /// Snapshot one installed component without mutating host state. Replaying
    /// the same operation id returns the existing receipt after revalidation.
    /// A missing, edited, or foreign artifact fails before receipt publication.
    pub fn backup_component<V: HostBundleVerificationAdapterV1>(
        &self,
        manifest: &HostBundleManifestV1,
        operation_id: [u8; 16],
        explicit_confirmation: bool,
        verifier: &V,
    ) -> Result<HostBundleBackupReceiptV1, HostBundleError> {
        if operation_id == [0; 16] {
            return Err(HostBundleError::InvalidManifest);
        }
        if !explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        manifest.validate_structure()?;
        verifier.verify_manifest(manifest)?;
        if let Some(receipt) = self.load_backup_receipt(operation_id)? {
            validate_backup_receipt(&receipt)?;
            self.read_backup_contents(&receipt)?;
            return (receipt.manifest == *manifest)
                .then_some(receipt)
                .ok_or(HostBundleError::ReceiptCorrupted);
        }

        let source_receipt = self
            .load_receipt(manifest.host, manifest.component)?
            .filter(|receipt| receipt.operation != HostBundleLifecycleOpV1::Uninstall)
            .ok_or(HostBundleError::InvalidObservedState)?;
        if source_receipt.manifest_digest != manifest.canonical_digest()?
            || source_receipt.artifacts.len() != manifest.artifacts.len()
        {
            return Err(HostBundleError::InvalidObservedState);
        }
        let source_receipt_digest: [u8; 32] = Sha256::digest(
            canonical_json_bytes(&source_receipt)
                .map_err(|_| HostBundleError::CanonicalizationFailed)?,
        )
        .into();
        let snapshot_dir = self.open_or_create_snapshot_dir(operation_id)?;
        let mut artifacts = Vec::with_capacity(source_receipt.artifacts.len());
        for (index, owned) in source_receipt.artifacts.iter().enumerate() {
            let expected = manifest
                .artifacts
                .iter()
                .find(|artifact| artifact.relative_path == owned.relative_path)
                .filter(|artifact| {
                    artifact.artifact_digest == owned.artifact_digest
                        && artifact.ownership_marker == owned.ownership_marker
                })
                .ok_or(HostBundleError::InvalidObservedState)?;
            let (parent, name) = self.open_parent_nofollow(Path::new(&expected.relative_path))?;
            let bytes = read_regular_nofollow(&parent, &name)?
                .ok_or(HostBundleError::InvalidObservedState)?;
            if <[u8; 32]>::from(Sha256::digest(&bytes)) != expected.artifact_digest {
                return Err(HostBundleError::OwnershipConflict(format!(
                    "{}: deployed bytes no longer match the receipt-owned content (marker {:?})",
                    expected.relative_path, expected.ownership_marker
                )));
            }
            let snapshot_name = host_bundle_snapshot_name(index, &expected.relative_path);
            match read_regular_nofollow(&snapshot_dir, &snapshot_name)? {
                Some(existing) if existing == bytes => {}
                Some(_) => return Err(HostBundleError::ReceiptCorrupted),
                None => atomic_write_nofollow(&snapshot_dir, &snapshot_name, &bytes, false)?,
            }
            artifacts.push(HostBundleBackupArtifactV1 {
                relative_path: expected.relative_path.clone(),
                artifact_digest: expected.artifact_digest,
                ownership_marker: expected.ownership_marker.clone(),
                snapshot_name,
            });
        }
        sync_cap_dir(&snapshot_dir)?;
        let receipt = HostBundleBackupReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id,
            host: manifest.host,
            component: manifest.component,
            manifest: manifest.clone(),
            source_receipt_digest,
            artifacts,
        };
        self.write_backup_receipt(&receipt)?;
        Ok(receipt)
    }

    /// Restore a named component backup through the ordinary Repair
    /// transaction. Any failure rolls the host files back to their pre-restore
    /// bytes; replaying `operation_id` returns the durable terminal receipt.
    pub fn restore_component_backup<V: HostBundleVerificationAdapterV1>(
        &mut self,
        backup_operation_id: [u8; 16],
        operation_id: [u8; 16],
        explicit_confirmation: bool,
        verifier: &V,
    ) -> Result<HostBundleRestoreReceiptV1, HostBundleError> {
        if backup_operation_id == [0; 16] || operation_id == [0; 16] {
            return Err(HostBundleError::InvalidManifest);
        }
        if !explicit_confirmation {
            return Err(HostBundleError::ConfirmationRequired);
        }
        if let Some(receipt) = self.load_restore_receipt(operation_id)? {
            validate_restore_receipt(&receipt)?;
            return (receipt.backup_operation_id == backup_operation_id)
                .then_some(receipt)
                .ok_or(HostBundleError::ReceiptCorrupted);
        }
        let backup = self
            .load_backup_receipt(backup_operation_id)?
            .ok_or(HostBundleError::InvalidObservedState)?;
        validate_backup_receipt(&backup)?;
        verifier.verify_manifest(&backup.manifest)?;
        let contents = self.read_backup_contents(&backup)?;
        let request = HostBundleExecutionRequestV1 {
            lifecycle: HostBundleLifecycleRequestV1 {
                operation: HostBundleLifecycleOpV1::Repair,
                expected_host: backup.host,
                expected_component: backup.component,
                explicit_confirmation: true,
                hermes_profile_bindings: u8::from(backup.host == HostKindV1::Hermes),
                // The operator explicitly confirmed restoring this exact
                // named backup, which is adoption authority over the backup's
                // recorded deploy paths whatever bytes sit there now.
                adopt_receiptless: true,
            },
            operation_id,
        };
        let restored_receipt = self.execute(&backup.manifest, &request, &contents, verifier)?;
        let receipt = HostBundleRestoreReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id,
            backup_operation_id,
            restored_receipt,
        };
        self.write_restore_receipt(&receipt)?;
        Ok(receipt)
    }

    pub fn publish_feedback_component_set_receipt(
        &self,
        manifest: &HostBundleManifestV1,
        component_receipt: &HostBundleInstallReceiptV1,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        if manifest.host != component_receipt.host
            || manifest.component != component_receipt.component
            || manifest.canonical_digest()? != component_receipt.manifest_digest
        {
            return Err(HostBundleError::WrongTarget);
        }
        let previous = latest_host_component_set_receipt_at(
            &self.lifecycle_root_path,
            component_receipt.host,
        )?;
        let mut component_manifests = previous
            .as_ref()
            .map(|receipt| receipt.component_manifests.clone())
            .unwrap_or_default();
        component_manifests.retain(|previous| previous.component != manifest.component);
        component_manifests.push(manifest.clone());
        component_manifests.sort_by_key(|manifest| manifest.component);
        let mut component_receipts = previous
            .map(|receipt| receipt.component_receipts)
            .unwrap_or_default();
        component_receipts.retain(|previous| previous.component != component_receipt.component);
        component_receipts.push(component_receipt.clone());
        component_receipts.sort_by_key(|receipt| receipt.component);
        let receipt = HostComponentSetReceiptV1 {
            schema_version: HOST_BUNDLE_RECEIPT_SCHEMA_VERSION,
            operation_id: component_receipt.operation_id,
            host: component_receipt.host,
            operation: component_receipt.operation,
            component_manifests,
            component_receipts,
            confirmed_plan_digest: None,
            base_registration_revision: None,
            current_registration_revision: None,
            artifact_state_revision: None,
        };
        self.write_component_set_receipt(&receipt)?;
        Ok(receipt)
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn lifecycle_root_path(&self) -> &Path {
        &self.lifecycle_root_path
    }

    fn open_or_create_snapshot_dir(&self, operation_id: [u8; 16]) -> Result<Dir, HostBundleError> {
        let snapshots = open_or_create_nofollow_dir(&self.control, "snapshots")?;
        open_or_create_nofollow_dir(&snapshots, &hex::encode(operation_id))
    }

    fn open_existing_snapshot_dir(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<Dir>, HostBundleError> {
        let snapshots = match self.control.open_dir_nofollow("snapshots") {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(HostBundleError::UnsafeInstallPath),
        };
        match snapshots.open_dir_nofollow(hex::encode(operation_id)) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(HostBundleError::UnsafeInstallPath),
        }
    }

    fn read_backup_contents(
        &self,
        receipt: &HostBundleBackupReceiptV1,
    ) -> Result<Vec<HostBundleArtifactContentV1>, HostBundleError> {
        validate_backup_receipt(receipt)?;
        let snapshot_dir = self
            .open_existing_snapshot_dir(receipt.operation_id)?
            .ok_or(HostBundleError::ReceiptCorrupted)?;
        receipt
            .artifacts
            .iter()
            .map(|artifact| {
                let bytes = read_regular_nofollow(&snapshot_dir, &artifact.snapshot_name)?
                    .ok_or(HostBundleError::ReceiptCorrupted)?;
                if <[u8; 32]>::from(Sha256::digest(&bytes)) != artifact.artifact_digest {
                    return Err(HostBundleError::ReceiptCorrupted);
                }
                Ok(HostBundleArtifactContentV1 {
                    relative_path: artifact.relative_path.clone(),
                    bytes,
                })
            })
            .collect()
    }

    fn load_backup_receipt(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<HostBundleBackupReceiptV1>, HostBundleError> {
        read_control_json(
            &self.control,
            &host_bundle_backup_receipt_file(operation_id),
        )?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted))
        .transpose()
    }

    fn write_backup_receipt(
        &self,
        receipt: &HostBundleBackupReceiptV1,
    ) -> Result<(), HostBundleError> {
        validate_backup_receipt(receipt)?;
        let bytes = serde_json::to_vec(receipt).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(
            &self.control,
            &host_bundle_backup_receipt_file(receipt.operation_id),
            &bytes,
            false,
        )
    }

    fn load_restore_receipt(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<HostBundleRestoreReceiptV1>, HostBundleError> {
        read_control_json(
            &self.control,
            &host_bundle_restore_receipt_file(operation_id),
        )?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| HostBundleError::ReceiptCorrupted))
        .transpose()
    }

    fn write_restore_receipt(
        &self,
        receipt: &HostBundleRestoreReceiptV1,
    ) -> Result<(), HostBundleError> {
        validate_restore_receipt(receipt)?;
        let bytes = serde_json::to_vec(receipt).map_err(|_| HostBundleError::ReceiptCorrupted)?;
        atomic_write_nofollow(
            &self.control,
            &host_bundle_restore_receipt_file(receipt.operation_id),
            &bytes,
            false,
        )
    }
}

impl HostBundleLifecycleStorageV1 for HostBundleWriterV1 {
    fn recover_lifecycle(&mut self) -> Result<(), HostBundleError> {
        self.recover_interrupted_operation()
    }

    fn execute_lifecycle<V: HostBundleVerificationAdapterV1>(
        &mut self,
        manifest: &HostBundleManifestV1,
        request: &HostBundleExecutionRequestV1,
        contents: &[HostBundleArtifactContentV1],
        verifier: &V,
    ) -> Result<HostBundleInstallReceiptV1, HostBundleError> {
        self.execute(manifest, request, contents, verifier)
    }
}

fn validate_artifact_contents(
    manifest: &HostBundleManifestV1,
    request: &HostBundleExecutionRequestV1,
    contents: &[HostBundleArtifactContentV1],
) -> Result<BTreeMap<String, Vec<u8>>, HostBundleError> {
    validate_artifact_contents_for_operation(manifest, request.lifecycle.operation, contents)
}

fn ensure_bundle_root(root: &Path) -> Result<(), HostBundleError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(HostBundleError::UnsafeInstallPath);
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(host_bundle_storage_failure!()),
    }
    fs::create_dir_all(root).map_err(|_| host_bundle_storage_failure!())?;
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        _ => Err(HostBundleError::UnsafeInstallPath),
    }
}

fn open_or_create_nofollow_dir(parent: &Dir, name: &str) -> Result<Dir, HostBundleError> {
    if !is_safe_component(name) {
        return Err(HostBundleError::UnsafeInstallPath);
    }
    match parent.open_dir_nofollow(name) {
        Ok(directory) => Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            parent
                .create_dir(name)
                .map_err(|_| host_bundle_storage_failure!())?;
            parent
                .open_dir_nofollow(name)
                .map_err(|_| HostBundleError::UnsafeInstallPath)
        }
        Err(_) => Err(HostBundleError::UnsafeInstallPath),
    }
}

fn open_writer_lock(control: &Dir) -> Result<fs::File, HostBundleError> {
    let mut options = CapOpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    let file = control
        .open_with(HOST_BUNDLE_LOCK_FILE, &options)
        .map_err(|_| HostBundleError::UnsafeInstallPath)?
        .into_std();
    file.try_lock_exclusive()
        .map_err(|_| host_bundle_recovery_required!())?;
    Ok(file)
}

pub(super) fn read_regular_nofollow(
    parent: &Dir,
    name: &str,
) -> Result<Option<Vec<u8>>, HostBundleError> {
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if metadata.len() > MAX_ARTIFACT_CONTENT_BYTES as u64 {
                return Err(HostBundleError::ArtifactContentMismatch);
            }
        }
        Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(host_bundle_storage_failure!()),
    }
    let mut options = CapOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = parent
        .open_with(name, &options)
        .map_err(|_| HostBundleError::UnsafeInstallPath)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_ARTIFACT_CONTENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| host_bundle_storage_failure!())?;
    if bytes.len() > MAX_ARTIFACT_CONTENT_BYTES {
        return Err(HostBundleError::ArtifactContentMismatch);
    }
    Ok(Some(bytes))
}

pub(super) fn regular_file_exists(parent: &Dir, name: &str) -> Result<bool, HostBundleError> {
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(HostBundleError::UnsafeInstallPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(host_bundle_storage_failure!()),
    }
}

fn remove_regular_if_exists(parent: &Dir, name: &str) -> Result<(), HostBundleError> {
    if regular_file_exists(parent, name)? {
        parent
            .remove_file(name)
            .map_err(|_| host_bundle_storage_failure!())?;
    }
    Ok(())
}

pub(super) fn remove_if_digest_matches(
    parent: &Dir,
    name: &str,
    expected_digest: [u8; 32],
) -> Result<(), HostBundleError> {
    let Some(bytes) = read_regular_nofollow(parent, name)? else {
        return Ok(());
    };
    let actual: [u8; 32] = Sha256::digest(&bytes).into();
    if actual != expected_digest {
        return Err(host_bundle_recovery_required!());
    }
    parent
        .remove_file(name)
        .map_err(|_| host_bundle_storage_failure!())
}

pub(super) fn move_regular_to_backup(
    parent: &Dir,
    name: &str,
    backup_dir: &Dir,
    backup_name: &str,
) -> Result<(), HostBundleError> {
    if !regular_file_exists(parent, name)? || !is_safe_component(backup_name) {
        return Err(HostBundleError::UnsafeInstallPath);
    }
    if regular_file_exists(backup_dir, backup_name)? {
        return Err(host_bundle_recovery_required!());
    }
    parent
        .rename(name, backup_dir, backup_name)
        .map_err(|_| host_bundle_storage_failure!())?;
    sync_cap_dir(parent)?;
    sync_cap_dir(backup_dir)
}

pub(super) fn atomic_write_nofollow(
    parent: &Dir,
    name: &str,
    bytes: &[u8],
    replace_existing: bool,
) -> Result<(), HostBundleError> {
    if !is_safe_component(name) || bytes.len() > MAX_ARTIFACT_CONTENT_BYTES {
        return Err(HostBundleError::ArtifactContentMismatch);
    }
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() && replace_existing => {}
        Ok(metadata) if metadata.file_type().is_file() => {
            return Err(HostBundleError::OwnershipConflict(format!(
                "{name}: a file appeared at this deploy path after planning"
            )));
        }
        Ok(_) => return Err(HostBundleError::UnsafeInstallPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(host_bundle_storage_failure!()),
    }
    for _ in 0..32 {
        let temporary = format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            HOST_BUNDLE_TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
        );
        let mut options = CapOpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        let mut file = match parent.open_with(&temporary, &options) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(host_bundle_storage_failure!()),
        };
        let result = (|| {
            file.write_all(bytes)
                .map_err(|_| host_bundle_storage_failure!())?;
            file.sync_all()
                .map_err(|_| host_bundle_storage_failure!())?;
            drop(file);
            // A rename changes the final directory entry rather than following
            // a final symlink; the preflight and capability parent prevent
            // traversal through any descendant component.
            if replace_existing {
                parent
                    .rename(&temporary, parent, name)
                    .map_err(|_| host_bundle_storage_failure!())?;
            } else {
                parent
                    .hard_link(&temporary, parent, name)
                    .map_err(|error| {
                        if error.kind() == io::ErrorKind::AlreadyExists {
                            HostBundleError::OwnershipConflict(format!(
                                "{name}: a file appeared at this deploy path mid-write"
                            ))
                        } else {
                            host_bundle_storage_failure!()
                        }
                    })?;
                parent
                    .remove_file(&temporary)
                    .map_err(|_| host_bundle_storage_failure!())?;
            }
            sync_cap_dir(parent)
        })();
        if result.is_err() {
            let _ = parent.remove_file(&temporary);
        }
        return result;
    }
    Err(host_bundle_storage_failure!())
}

/// Flushes the deploy parent so a preceding rename, hard link, or unlink is
/// durable.
///
/// Delegates to the shared capability-directory primitive rather than issuing
/// the fsync here: Windows has no directory flush, and `FlushFileBuffers` on a
/// directory handle fails with `ERROR_ACCESS_DENIED`, which turned every
/// atomic host-bundle publication into a storage failure.
pub(super) fn sync_cap_dir(dir: &Dir) -> Result<(), HostBundleError> {
    tracedecay_private_fs::capability_dir::sync_directory(dir)
        .map_err(|_| host_bundle_storage_failure!())
}

fn read_control_json(parent: &Dir, name: &str) -> Result<Option<Vec<u8>>, HostBundleError> {
    let Some(bytes) = read_regular_nofollow(parent, name)? else {
        return Ok(None);
    };
    if bytes.is_empty() || bytes.len() > MAX_CONTROL_FILE_BYTES {
        return Err(HostBundleError::ReceiptCorrupted);
    }
    Ok(Some(bytes))
}
