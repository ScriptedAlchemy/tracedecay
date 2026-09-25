//! Atomic, capability-rooted single-component writer with in-memory rollback,
//! and the no-follow filesystem primitives.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use sha2::{Digest, Sha256};
use tracedecay_host_integration::host_bundle_storage_failure;

use super::control::{
    HOST_BUNDLE_CONTROL_DIR, HOST_BUNDLE_LOCK_FILE, MAX_CONTROL_FILE_BYTES,
    component_set_receipt_file, is_safe_component, latest_host_component_set_receipt_at,
    parse_receipt, receipt_file, receipt_identity_from_file_name, receipt_schema_probe,
    validate_component_set_receipt, validate_receipt, writer_lock_file,
};
use super::model::{HostBundleExecutionRequestV1, HostBundleLifecycleStorageV1};
use super::planner::{
    HostArtifactActionV1, HostArtifactMutationV1, ObservedArtifactKindV1, ObservedHostArtifactV1,
    plan_verified_complete_lifecycle_mutation, validate_artifact_contents_for_operation,
};
use super::{
    HOST_BUNDLE_RECEIPT_SCHEMA_VERSION, HostBundleArtifactContentV1, HostBundleError,
    HostBundleInstallReceiptV1, HostBundleLifecycleOpV1, HostBundleManifestV1,
    HostBundleReceiptArtifactV1, HostBundleVerificationAdapterV1, HostComponentSetReceiptV1,
    HostComponentV1, HostKindV1, MAX_ARTIFACT_CONTENT_BYTES, validate_relative_install_path,
};

static HOST_BUNDLE_TEMP_NONCE: AtomicU64 = AtomicU64::new(1);

/// Exclusive owner of one host's mutable bundle state.
///
/// The lock is released when the writer switches hosts or is dropped. A
/// second host does not share this file: their artifact trees and receipts are
/// already disjoint.
struct HostWriterLock {
    host: HostKindV1,
    file: fs::File,
}

impl Drop for HostWriterLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            tracing::warn!(
                error = %error,
                host = ?self.host,
                "host bundle writer lock could not be released"
            );
        }
    }
}

/// Pre-mutation state of one artifact path, held only for the running
/// operation so a failure can put the path back.
pub(super) struct ArtifactUndo {
    relative_path: String,
    previous: Option<Vec<u8>>,
    written_digest: Option<[u8; 32]>,
}

/// Atomic, capability-rooted host-bundle writer. Every descendant directory
/// is opened without following symlinks; files are staged, fsynced, renamed,
/// and followed by a directory sync before receipt publication.
///
/// Rollback bytes live only in memory for the running operation. A process
/// killed mid-operation leaves whatever it wrote; the next install or repair
/// converges from the observed state. Mutation acquires
/// `writer.{slug}.v1.lock` for the host being written and holds it until the
/// writer switches hosts or drops.
pub struct HostBundleWriterV1 {
    pub(super) root_path: PathBuf,
    pub(super) lifecycle_root_path: PathBuf,
    root: Dir,
    control: Dir,
    host_lock: Option<HostWriterLock>,
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
        Ok(Self {
            root_path,
            lifecycle_root_path,
            root,
            control,
            host_lock: None,
        })
    }

    /// Acquire this host's writer lock, releasing any other host's lock first.
    ///
    /// Same-host re-entry on this writer is a no-op. A different process that
    /// already holds the host lock is refused; that is the one shared writer
    /// this host actually needs. Independent hosts are not refused.
    pub(super) fn ensure_host_lock(&mut self, host: HostKindV1) -> Result<(), HostBundleError> {
        if self
            .host_lock
            .as_ref()
            .is_some_and(|lock| lock.host == host)
        {
            return Ok(());
        }
        self.host_lock = None;
        if writer_lock_file(host) == HOST_BUNDLE_LOCK_FILE {
            return Err(HostBundleError::UnsafeInstallPath);
        }
        self.host_lock = Some(open_host_writer_lock(&self.control, host)?);
        Ok(())
    }

    /// Verify first-party catalog identity, validate artifact bytes, plan ownership-aware
    /// mutations, then execute them, putting every touched path back on failure.
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
        self.ensure_host_lock(manifest.host)?;
        verifier.verify_manifest(manifest)?;
        let content_by_path = validate_artifact_contents(manifest, request, contents)?;
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
        let mut undo = Vec::new();
        let applied = plan.mutations.iter().try_for_each(|mutation| {
            if let Some(record) = self.apply_artifact_mutation(mutation, &content_by_path)? {
                undo.push(record);
            }
            Ok(())
        });
        if let Err(error) = applied {
            return Err(self.undo_after_failure(error, &undo));
        }

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
        };
        if let Err(error) = self.write_receipt(&receipt) {
            return Err(self.undo_after_failure(error, &undo));
        }
        Ok(receipt)
    }

    /// Apply one planned mutation, returning what the path held before when
    /// the mutation changed it.
    pub(super) fn apply_artifact_mutation(
        &self,
        mutation: &HostArtifactMutationV1,
        content_by_path: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Option<ArtifactUndo>, HostBundleError> {
        let (parent, name) = self.open_parent_nofollow(Path::new(&mutation.relative_path))?;
        let content = || {
            content_by_path
                .get(&mutation.relative_path)
                .ok_or(HostBundleError::ArtifactContentMismatch)
        };
        let (previous, written_digest) = match mutation.action {
            HostArtifactActionV1::Noop => return Ok(None),
            HostArtifactActionV1::WriteNew => {
                let bytes = content()?;
                atomic_write_nofollow(&parent, &name, bytes, false)?;
                (None, Some(Sha256::digest(bytes).into()))
            }
            HostArtifactActionV1::Replace => {
                let previous = read_regular_nofollow(&parent, &name)?
                    .ok_or(HostBundleError::InvalidObservedState)?;
                let bytes = content()?;
                atomic_write_nofollow(&parent, &name, bytes, true)?;
                (Some(previous), Some(Sha256::digest(bytes).into()))
            }
            HostArtifactActionV1::Remove => {
                let previous = read_regular_nofollow(&parent, &name)?
                    .ok_or(HostBundleError::InvalidObservedState)?;
                parent
                    .remove_file(&name)
                    .map_err(|_| host_bundle_storage_failure!())?;
                sync_cap_dir(&parent)?;
                (Some(previous), None)
            }
        };
        Ok(Some(ArtifactUndo {
            relative_path: mutation.relative_path.clone(),
            previous,
            written_digest,
        }))
    }

    /// Put every recorded path back in reverse order. A path that holds
    /// neither its prior bytes nor this operation's output was changed by
    /// another writer: it is left alone and reported once every other path has
    /// been put back.
    pub(super) fn undo_artifact_mutations(
        &self,
        undo: &[ArtifactUndo],
    ) -> Result<(), HostBundleError> {
        let mut conflict = None;
        for record in undo.iter().rev() {
            let (parent, name) = self.open_parent_nofollow(Path::new(&record.relative_path))?;
            let live = read_regular_nofollow(&parent, &name)?;
            if live == record.previous {
                continue;
            }
            let live_digest = live
                .as_ref()
                .map(|bytes| <[u8; 32]>::from(Sha256::digest(bytes)));
            if live.is_some() && live_digest != record.written_digest {
                conflict.get_or_insert_with(|| {
                    HostBundleError::OwnershipConflict(format!(
                        "{}: changed by another writer while this operation rolled back",
                        record.relative_path
                    ))
                });
                continue;
            }
            match &record.previous {
                Some(bytes) => atomic_write_nofollow(&parent, &name, bytes, live.is_some())?,
                None => {
                    parent
                        .remove_file(&name)
                        .map_err(|_| host_bundle_storage_failure!())?;
                    sync_cap_dir(&parent)?;
                }
            }
        }
        conflict.map_or(Ok(()), Err)
    }

    fn undo_after_failure(&self, error: HostBundleError, undo: &[ArtifactUndo]) -> HostBundleError {
        match self.undo_artifact_mutations(undo) {
            Ok(()) => error,
            Err(undo_error) => undo_error,
        }
    }

    /// Delete this host's receipts written under an older receipt schema.
    /// Only an explicitly adopting lifecycle calls this; every other lifecycle
    /// reports [`HostBundleError::ReinstallRequired`] for them.
    pub(super) fn discard_stale_receipts(
        &mut self,
        host: HostKindV1,
    ) -> Result<(), HostBundleError> {
        self.ensure_host_lock(host)?;
        let control_path = self.lifecycle_root_path.join(HOST_BUNDLE_CONTROL_DIR);
        let entries = match fs::read_dir(&control_path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(host_bundle_storage_failure!()),
        };
        for entry in entries {
            let entry = entry.map_err(|_| host_bundle_storage_failure!())?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let owned_by_host = if name.starts_with("component-set-receipt.") {
                None
            } else if let Some((receipt_host, _)) = receipt_identity_from_file_name(name) {
                Some(receipt_host == host)
            } else {
                continue;
            };
            let Some(bytes) = read_control_json(&self.control, name)? else {
                continue;
            };
            let Some(probe) = receipt_schema_probe(&bytes) else {
                continue;
            };
            if probe.is_current() || !owned_by_host.unwrap_or(probe.host == Some(host)) {
                continue;
            }
            self.remove_control_file(name)?;
        }
        Ok(())
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

    pub(super) fn load_receipt(
        &self,
        host: HostKindV1,
        component: HostComponentV1,
    ) -> Result<Option<HostBundleInstallReceiptV1>, HostBundleError> {
        let receipt = read_control_json(&self.control, &receipt_file(host, component))?
            .map(|bytes| parse_receipt::<HostBundleInstallReceiptV1>(&bytes))
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
        component: HostComponentV1,
    ) -> Result<(), HostBundleError> {
        self.remove_control_file(&receipt_file(host, component))
    }

    pub(super) fn load_component_set_receipt(
        &self,
        operation_id: [u8; 16],
    ) -> Result<Option<HostComponentSetReceiptV1>, HostBundleError> {
        let receipt = read_control_json(&self.control, &component_set_receipt_file(operation_id))?
            .map(|bytes| parse_receipt::<HostComponentSetReceiptV1>(&bytes))
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

    fn remove_control_file(&self, name: &str) -> Result<(), HostBundleError> {
        remove_regular_if_exists(&self.control, name)?;
        sync_cap_dir(&self.control)
    }

    pub fn publish_feedback_component_set_receipt(
        &mut self,
        manifest: &HostBundleManifestV1,
        component_receipt: &HostBundleInstallReceiptV1,
    ) -> Result<HostComponentSetReceiptV1, HostBundleError> {
        if manifest.host != component_receipt.host
            || manifest.component != component_receipt.component
            || manifest.canonical_digest()? != component_receipt.manifest_digest
        {
            return Err(HostBundleError::WrongTarget);
        }
        self.ensure_host_lock(component_receipt.host)?;
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
}

impl HostBundleLifecycleStorageV1 for HostBundleWriterV1 {
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
            match parent.create_dir(name) {
                Ok(()) => {}
                // Two hosts may create a shared parent (`.config/`)
                // at once. The directory is a namespace, not a shared state
                // object; the loser retries the open instead of failing.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(host_bundle_storage_failure!()),
            }
            parent
                .open_dir_nofollow(name)
                .map_err(|_| HostBundleError::UnsafeInstallPath)
        }
        Err(_) => Err(HostBundleError::UnsafeInstallPath),
    }
}

fn open_host_writer_lock(
    control: &Dir,
    host: HostKindV1,
) -> Result<HostWriterLock, HostBundleError> {
    let name = writer_lock_file(host);
    if !is_safe_component(&name) {
        return Err(HostBundleError::UnsafeInstallPath);
    }
    let mut options = CapOpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    let file = control
        .open_with(&name, &options)
        .map_err(|_| HostBundleError::UnsafeInstallPath)?
        .into_std();
    file.try_lock()
        .map_err(|_| HostBundleError::HostWriterBusy)?;
    Ok(HostWriterLock { host, file })
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
