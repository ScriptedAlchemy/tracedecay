//! Integrity-checked, all-or-nothing store-family backup and restore.
//!
//! Physical discovery, SQLite access, and publication remain behind closed
//! ports. In particular, paths are never used as store identity.

mod canonical;
mod driver;
mod filesystem;
mod model;
mod orchestrator;
pub use driver::{
    OnlineBackupError, OnlineBackupSource, RestorePublicationAuthority, SqliteOnlineBackupDriver,
    verify_sqlite_snapshot,
};
pub use filesystem::{
    BackupFilesystemError, BackupRoot, FilesystemBackupStore, PublishedLocatorError,
    PublishedRestore, PublishedStoreLocators,
};
mod ports;
mod sqlite;
mod validation;

pub use model::{
    ArtifactIdentity, ArtifactManifest, BACKUP_FORMAT_VERSION, BackupManifest, BackupSetId,
    DeletionState, FrozenFamilySnapshot, ManifestError, PayloadId, PrivacyClass, RestoreTarget,
    SchemaVersion, Sha256Digest, SnapshotArtifact, StagingId, StoredBackupManifest,
};
pub use orchestrator::{BackupRestoreError, BackupRestoreOrchestrator};
pub use ports::{BackupDriver, BackupFilesystem, Cancellation};
pub use sqlite::{
    MAX_PAGES_PER_STEP, MAX_STEP_PAUSE, SqliteBackupConfigurationError, SqliteBackupError,
    SqliteBackupFilesystem, SqliteBackupOptions, SqliteBackupProgress, backup_sqlite,
};
#[cfg(test)]
mod tests;
