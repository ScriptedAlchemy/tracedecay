use std::{error::Error, fmt};

use super::canonical::{manifest_digest, sha256};
use super::validation::*;
use super::{model::*, ports::*};
use crate::maintenance::ExclusiveMaintenancePermit;
use tracedecay_store::{FrozenWatermarkVectorV1, StoreRuntimeBindingV1};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackupRestoreError {
    Cancelled,
    Manifest(ManifestError),
    ManifestDigestMismatch,
    ArtifactDigestMismatch(Box<ArtifactIdentity>),
    Driver(String),
    Filesystem(String),
    RestoreTargetNotNewer,
    RestoreTargetMismatch,
    RestoreVerificationMismatch,
}

impl fmt::Display for BackupRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "backup/restore failure: {self:?}")
    }
}

impl Error for BackupRestoreError {}

pub struct BackupRestoreOrchestrator<'a, D, F> {
    driver: &'a mut D,
    filesystem: &'a mut F,
}

impl<'a, D, F> BackupRestoreOrchestrator<'a, D, F>
where
    D: BackupDriver,
    F: BackupFilesystem,
{
    pub fn new(driver: &'a mut D, filesystem: &'a mut F) -> Self {
        Self { driver, filesystem }
    }

    pub fn backup(
        &mut self,
        required: &FrozenWatermarkVectorV1,
        backup_set: BackupSetId,
        cancellation: &dyn Cancellation,
    ) -> Result<StoredBackupManifest, BackupRestoreError> {
        check_cancelled(cancellation)?;
        let snapshot = self
            .driver
            .freeze_families(required, cancellation)
            .map_err(driver_error)?;
        if &snapshot.frozen_watermarks != required {
            return Err(BackupRestoreError::Manifest(ManifestError::InvalidIdentity));
        }
        validate_snapshot(&snapshot)?;
        let staging = self
            .filesystem
            .begin_backup(&backup_set)
            .map_err(fs_error)?;
        let outcome =
            self.stage_and_verify_backup(&snapshot, backup_set.clone(), &staging, cancellation);
        match outcome {
            Ok(manifest) => {
                if let Err(error) = self.filesystem.commit_backup(staging.clone(), &backup_set) {
                    self.filesystem.abort_staging(staging);
                    return Err(fs_error(error));
                }
                Ok(manifest)
            }
            Err(error) => {
                self.filesystem.abort_staging(staging);
                Err(error)
            }
        }
    }

    fn stage_and_verify_backup(
        &mut self,
        snapshot: &FrozenFamilySnapshot,
        backup_set: BackupSetId,
        staging: &StagingId,
        cancellation: &dyn Cancellation,
    ) -> Result<StoredBackupManifest, BackupRestoreError> {
        let mut artifacts = Vec::with_capacity(snapshot.artifacts.len());
        for artifact in &snapshot.artifacts {
            check_cancelled(cancellation)?;
            self.filesystem
                .write_staged(staging, &artifact.identity, &artifact.bytes)
                .map_err(fs_error)?;
            let persisted = self
                .filesystem
                .read_staged(staging, &artifact.identity)
                .map_err(fs_error)?;
            let expected = sha256(&artifact.bytes);
            if sha256(&persisted) != expected {
                return Err(BackupRestoreError::ArtifactDigestMismatch(Box::new(
                    artifact.identity.clone(),
                )));
            }
            artifacts.push(ArtifactManifest {
                identity: artifact.identity.clone(),
                byte_length: artifact
                    .bytes
                    .len()
                    .try_into()
                    .map_err(|_| BackupRestoreError::Manifest(ManifestError::LengthOverflow))?,
                sha256: expected,
            });
        }
        let manifest = BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            backup_set,
            frozen_watermarks: snapshot.frozen_watermarks.clone(),
            schema_version: snapshot.schema_version,
            privacy: snapshot.privacy,
            deletion: snapshot.deletion,
            payload_closure: snapshot.payload_closure.clone(),
            artifacts,
        };
        validate_manifest(&manifest)?;
        let stored = StoredBackupManifest {
            manifest_sha256: manifest_digest(&manifest),
            manifest,
        };
        self.filesystem
            .write_manifest(staging, &stored)
            .map_err(fs_error)?;
        let persisted = self
            .filesystem
            .read_staged_manifest(staging)
            .map_err(fs_error)?;
        if persisted != stored || manifest_digest(&persisted.manifest) != persisted.manifest_sha256
        {
            return Err(BackupRestoreError::ManifestDigestMismatch);
        }
        check_cancelled(cancellation)?;
        Ok(stored)
    }

    pub fn restore(
        &mut self,
        backup_set: &BackupSetId,
        permit: ExclusiveMaintenancePermit,
        mut replacement_bindings: Vec<StoreRuntimeBindingV1>,
        cancellation: &dyn Cancellation,
    ) -> Result<Vec<StoreRuntimeBindingV1>, BackupRestoreError> {
        check_cancelled(cancellation)?;
        let stored = self
            .filesystem
            .load_manifest(backup_set)
            .map_err(fs_error)?;
        if manifest_digest(&stored.manifest) != stored.manifest_sha256 {
            return Err(BackupRestoreError::ManifestDigestMismatch);
        }
        validate_manifest(&stored.manifest)?;
        if stored.manifest.backup_set != *backup_set {
            return Err(BackupRestoreError::Manifest(ManifestError::InvalidIdentity));
        }
        replacement_bindings.sort_by(|left, right| left.shard_id.cmp(&right.shard_id));
        let requested_replacements = replacement_bindings.clone();
        let target = self
            .driver
            .allocate_restore_target(&permit, replacement_bindings)
            .map_err(driver_error)?;
        if target.replacement_bindings != requested_replacements {
            self.driver.abandon_restore(&permit, target);
            return Err(BackupRestoreError::RestoreTargetMismatch);
        }
        let preparation = self.stage_and_verify_restore(
            &permit,
            backup_set,
            &stored.manifest,
            &target,
            cancellation,
        );
        if let Err(error) = preparation {
            self.driver.abandon_restore(&permit, target);
            return Err(error);
        }
        let expected = target.replacement_bindings.clone();
        // Publication consumes both linear capabilities. If installation
        // returns an error, its recovery evidence is deliberately not treated
        // as disposable: the commit may have crossed an I/O acknowledgement
        // boundary.
        self.driver
            .publish_restore(permit, &stored.manifest.frozen_watermarks, target)
            .map_err(driver_error)?;
        Ok(expected)
    }

    fn stage_and_verify_restore(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        backup_set: &BackupSetId,
        manifest: &BackupManifest,
        target: &RestoreTarget,
        cancellation: &dyn Cancellation,
    ) -> Result<(), BackupRestoreError> {
        validate_replacement_bindings(&manifest.frozen_watermarks, target)?;
        for artifact in &manifest.artifacts {
            check_cancelled(cancellation)?;
            let bytes = self
                .filesystem
                .read_backup(backup_set, &artifact.identity)
                .map_err(fs_error)?;
            if bytes.len() as u64 != artifact.byte_length || sha256(&bytes) != artifact.sha256 {
                return Err(BackupRestoreError::ArtifactDigestMismatch(Box::new(
                    artifact.identity.clone(),
                )));
            }
            self.filesystem
                .write_staged(&target.staging, &artifact.identity, &bytes)
                .map_err(fs_error)?;
        }
        let evidence = self
            .driver
            .verify_restore(permit, target, manifest)
            .map_err(driver_error)?;
        verify_restore_evidence(&evidence, manifest, target)?;
        check_cancelled(cancellation)?;
        Ok(())
    }
}

fn check_cancelled(cancellation: &dyn Cancellation) -> Result<(), BackupRestoreError> {
    if cancellation.is_cancelled() {
        Err(BackupRestoreError::Cancelled)
    } else {
        Ok(())
    }
}

fn driver_error(error: impl Error) -> BackupRestoreError {
    BackupRestoreError::Driver(error.to_string())
}
fn fs_error(error: impl Error) -> BackupRestoreError {
    BackupRestoreError::Filesystem(error.to_string())
}
