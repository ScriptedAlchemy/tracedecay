use super::model::*;
use crate::maintenance::ExclusiveMaintenancePermit;
use tracedecay_store::{FrozenWatermarkVectorV1, StoreRuntimeBindingV1};

pub trait Cancellation {
    fn is_cancelled(&self) -> bool;
}

/// Backup-set storage. Implementations map typed identities to private paths;
/// callers cannot supply or discover raw filesystem paths.
pub trait BackupFilesystem {
    type Error: std::error::Error + Send + Sync + 'static;

    fn begin_backup(&mut self, backup: &BackupSetId) -> Result<StagingId, Self::Error>;
    fn write_staged(
        &mut self,
        staging: &StagingId,
        artifact: &ArtifactIdentity,
        bytes: &[u8],
    ) -> Result<(), Self::Error>;
    fn read_staged(
        &self,
        staging: &StagingId,
        artifact: &ArtifactIdentity,
    ) -> Result<Vec<u8>, Self::Error>;
    fn write_manifest(
        &mut self,
        staging: &StagingId,
        manifest: &StoredBackupManifest,
    ) -> Result<(), Self::Error>;
    fn read_staged_manifest(
        &self,
        staging: &StagingId,
    ) -> Result<StoredBackupManifest, Self::Error>;
    /// Durably and atomically makes the isolated staging tree visible as the
    /// backup set. It must not expose a partial tree on error.
    fn commit_backup(
        &mut self,
        staging: StagingId,
        backup: &BackupSetId,
    ) -> Result<(), Self::Error>;
    fn abort_staging(&mut self, staging: StagingId);
    fn load_manifest(&self, backup: &BackupSetId) -> Result<StoredBackupManifest, Self::Error>;
    fn read_backup(
        &self,
        backup: &BackupSetId,
        artifact: &ArtifactIdentity,
    ) -> Result<Vec<u8>, Self::Error>;
}

/// Closed database authority. It exposes no SQL and performs no discovery.
pub trait BackupDriver {
    type Error: std::error::Error + Send + Sync + 'static;

    fn freeze_families(
        &mut self,
        required: &FrozenWatermarkVectorV1,
        cancellation: &dyn Cancellation,
    ) -> Result<FrozenFamilySnapshot, Self::Error>;
    /// Allocates only private staging resources. Every replacement binding is
    /// supplied by the canonical registry authority outside this driver.
    fn allocate_restore_target(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        replacement_bindings: Vec<StoreRuntimeBindingV1>,
    ) -> Result<RestoreTarget, Self::Error>;
    fn verify_restore(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        target: &RestoreTarget,
        manifest: &BackupManifest,
    ) -> Result<FrozenFamilySnapshot, Self::Error>;
    /// Atomically installs `target` using its externally issued higher fences.
    /// This consumes the sole maintenance permit at the publication boundary.
    /// The former store is preserved recovery input only and must never be
    /// reopened as a writable fallback, including when acknowledgement is
    /// uncertain.
    fn publish_restore(
        &mut self,
        permit: ExclusiveMaintenancePermit,
        recovery_source: &FrozenWatermarkVectorV1,
        target: RestoreTarget,
    ) -> Result<(), Self::Error>;
    fn abandon_restore(&mut self, permit: &ExclusiveMaintenancePermit, target: RestoreTarget);
}
