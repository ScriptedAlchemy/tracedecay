//! Verified SQLite backup adaptation and isolated staged restore.
//!
//! Staging paths remain private runtime implementation details. A staged
//! database cannot be opened for serving until destination bytes, immutable
//! SQLite integrity, reference closure, and current policy replay all succeed.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use sha2::{Digest, Sha256};
use tracedecay_store::{
    BackupArtifactKindV1, BackupArtifactV1, BackupManifestV1, CurrentPolicyReplayV1,
    ShardWatermarkV1, StoreRuntimeBindingV1,
};

use crate::{
    OnlineBackupReceipt, PersistentWriter, RuntimeWriteAuthority, WriterActorError,
    backup::verify_sqlite_snapshot,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBackupArtifactReceiptV1 {
    pub artifact: BackupArtifactV1,
    pub source_frontier: ShardWatermarkV1,
}

pub async fn create_verified_sqlite_backup_artifact(
    writer: &PersistentWriter,
    destination: PathBuf,
    authority: Arc<dyn RuntimeWriteAuthority>,
    artifact_id: String,
    family: String,
) -> Result<VerifiedBackupArtifactReceiptV1, RemoteRuntimeRecoveryErrorV1> {
    let receipt = writer
        .snapshot_to(destination, authority)
        .await
        .map_err(RemoteRuntimeRecoveryErrorV1::Writer)?;
    artifact_from_online_receipt(artifact_id, family, receipt)
}

fn artifact_from_online_receipt(
    artifact_id: String,
    family: String,
    receipt: OnlineBackupReceipt,
) -> Result<VerifiedBackupArtifactReceiptV1, RemoteRuntimeRecoveryErrorV1> {
    validate_identifier(&artifact_id)?;
    validate_identifier(&family)?;
    if receipt.destination_bytes == 0 || receipt.destination_sha256.0 == [0; 32] {
        return Err(RemoteRuntimeRecoveryErrorV1::BackupReceiptInvalid);
    }
    Ok(VerifiedBackupArtifactReceiptV1 {
        artifact: BackupArtifactV1 {
            artifact_id,
            family,
            kind: BackupArtifactKindV1::SqliteDatabase,
            bytes: receipt.destination_bytes,
            sha256: receipt.destination_sha256.0,
            references: Vec::new(),
        },
        source_frontier: receipt.source_watermark,
    })
}

pub trait CurrentRestorePolicyAuthorityV1 {
    fn replay_current_policy(
        &self,
        staged_database: &Path,
        expected: &CurrentPolicyReplayV1,
    ) -> Result<CurrentPolicyReplayV1, String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeStagedRestoreStateV1 {
    Isolated,
    BytesVerified,
    ClosureVerified,
    PolicyReplayed,
    ReadyForPublication,
    Published,
    RolledBack,
    ForwardRecoveryRequired,
}

pub struct IsolatedStagedRestoreV1 {
    path: PathBuf,
    expected_binding: StoreRuntimeBindingV1,
    manifest_digest: [u8; 32],
    state: RuntimeStagedRestoreStateV1,
}

impl IsolatedStagedRestoreV1 {
    pub fn state(&self) -> RuntimeStagedRestoreStateV1 {
        self.state
    }

    pub fn expected_binding(&self) -> &StoreRuntimeBindingV1 {
        &self.expected_binding
    }

    pub fn replay_current_policy(
        &mut self,
        authority: &dyn CurrentRestorePolicyAuthorityV1,
        expected: &CurrentPolicyReplayV1,
    ) -> Result<(), RemoteRuntimeRecoveryErrorV1> {
        if self.state != RuntimeStagedRestoreStateV1::ClosureVerified {
            return Err(RemoteRuntimeRecoveryErrorV1::InvalidTransition);
        }
        expected
            .validate()
            .map_err(|_| RemoteRuntimeRecoveryErrorV1::PolicyReplayRejected)?;
        let applied = authority
            .replay_current_policy(&self.path, expected)
            .map_err(RemoteRuntimeRecoveryErrorV1::PolicyReplayFailed)?;
        if &applied != expected {
            return Err(RemoteRuntimeRecoveryErrorV1::PolicyReplayRejected);
        }
        self.state = RuntimeStagedRestoreStateV1::PolicyReplayed;
        verify_sqlite_snapshot(&self.path)
            .map_err(|error| RemoteRuntimeRecoveryErrorV1::Sqlite(error.to_string()))?;
        self.state = RuntimeStagedRestoreStateV1::ReadyForPublication;
        Ok(())
    }

    pub fn publish(
        mut self,
        destination: &Path,
        expected_manifest_digest: [u8; 32],
        installed_binding: &StoreRuntimeBindingV1,
    ) -> Result<PublishedRestoreReceiptV1, RemoteRuntimeRecoveryErrorV1> {
        if self.state != RuntimeStagedRestoreStateV1::ReadyForPublication {
            return Err(RemoteRuntimeRecoveryErrorV1::InvalidTransition);
        }
        if self.manifest_digest != expected_manifest_digest {
            return Err(RemoteRuntimeRecoveryErrorV1::ManifestChanged);
        }
        if installed_binding != &self.expected_binding {
            return Err(RemoteRuntimeRecoveryErrorV1::BindingMismatch);
        }
        if destination.exists() {
            return Err(RemoteRuntimeRecoveryErrorV1::DestinationExists);
        }
        fs::hard_link(&self.path, destination)
            .map_err(|error| RemoteRuntimeRecoveryErrorV1::Io(error.to_string()))?;
        if let Err(error) = sync_parent(destination) {
            let rollback = fs::remove_file(destination).and_then(|_| sync_parent_io(destination));
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback) => Err(RemoteRuntimeRecoveryErrorV1::ForwardRecovery(format!(
                    "{error}; publication rollback failed: {rollback}"
                ))),
            };
        }
        self.state = RuntimeStagedRestoreStateV1::Published;
        let bytes = fs::metadata(destination)
            .map_err(|error| RemoteRuntimeRecoveryErrorV1::Io(error.to_string()))?
            .len();
        let digest = sha256_file(destination)?;
        let _ = fs::remove_file(&self.path);
        Ok(PublishedRestoreReceiptV1 {
            binding: installed_binding.clone(),
            destination_bytes: bytes,
            destination_sha256: digest,
            manifest_digest: self.manifest_digest,
        })
    }

    pub fn rollback(mut self) -> Result<(), RemoteRuntimeRecoveryErrorV1> {
        if self.state == RuntimeStagedRestoreStateV1::Published {
            return Err(RemoteRuntimeRecoveryErrorV1::InvalidTransition);
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {
                self.state = RuntimeStagedRestoreStateV1::RolledBack;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.state = RuntimeStagedRestoreStateV1::RolledBack;
                Ok(())
            }
            Err(error) => {
                self.state = RuntimeStagedRestoreStateV1::ForwardRecoveryRequired;
                Err(RemoteRuntimeRecoveryErrorV1::ForwardRecovery(
                    error.to_string(),
                ))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedRestoreReceiptV1 {
    pub binding: StoreRuntimeBindingV1,
    pub destination_bytes: u64,
    pub destination_sha256: [u8; 32],
    pub manifest_digest: [u8; 32],
}

pub fn stage_sqlite_restore(
    source: &Path,
    staging_destination: PathBuf,
    manifest: &BackupManifestV1,
    manifest_digest: [u8; 32],
    artifact_id: &str,
    now_micros: i64,
) -> Result<IsolatedStagedRestoreV1, RemoteRuntimeRecoveryErrorV1> {
    manifest
        .validate(now_micros)
        .map_err(|error| RemoteRuntimeRecoveryErrorV1::ManifestInvalid(error.to_string()))?;
    if manifest_digest == [0; 32] {
        return Err(RemoteRuntimeRecoveryErrorV1::ManifestChanged);
    }
    let artifact = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == artifact_id)
        .ok_or(RemoteRuntimeRecoveryErrorV1::ArtifactUnavailable)?;
    if artifact.kind != BackupArtifactKindV1::SqliteDatabase {
        return Err(RemoteRuntimeRecoveryErrorV1::ArtifactKindMismatch);
    }
    if staging_destination.exists() {
        return Err(RemoteRuntimeRecoveryErrorV1::DestinationExists);
    }
    copy_private(source, &staging_destination)?;
    let staged = (|| {
        let bytes = fs::metadata(&staging_destination)
            .map_err(|error| RemoteRuntimeRecoveryErrorV1::Io(error.to_string()))?
            .len();
        let digest = sha256_file(&staging_destination)?;
        if bytes != artifact.bytes || digest != artifact.sha256 {
            return Err(RemoteRuntimeRecoveryErrorV1::DestinationBytesMismatch);
        }
        verify_sqlite_snapshot(&staging_destination)
            .map_err(|error| RemoteRuntimeRecoveryErrorV1::Sqlite(error.to_string()))?;
        let inventory = manifest
            .artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if manifest.artifacts.iter().any(|artifact| {
            artifact
                .references
                .iter()
                .any(|reference| !inventory.contains(reference.as_str()))
        }) {
            return Err(RemoteRuntimeRecoveryErrorV1::ReferenceClosure);
        }
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = fs::remove_file(&staging_destination);
        return Err(error);
    }
    Ok(IsolatedStagedRestoreV1 {
        path: staging_destination,
        expected_binding: manifest.runtime.clone(),
        manifest_digest,
        state: RuntimeStagedRestoreStateV1::ClosureVerified,
    })
}

fn copy_private(source: &Path, destination: &Path) -> Result<(), RemoteRuntimeRecoveryErrorV1> {
    let mut input =
        File::open(source).map_err(|error| RemoteRuntimeRecoveryErrorV1::Io(error.to_string()))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options
        .open(destination)
        .map_err(|error| RemoteRuntimeRecoveryErrorV1::Io(error.to_string()))?;
    if let Err(error) = io::copy(&mut input, &mut output)
        .and_then(|_| output.flush())
        .and_then(|_| output.sync_all())
    {
        drop(output);
        let _ = fs::remove_file(destination);
        return Err(RemoteRuntimeRecoveryErrorV1::Io(error.to_string()));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<[u8; 32], RemoteRuntimeRecoveryErrorV1> {
    let mut file =
        File::open(path).map_err(|error| RemoteRuntimeRecoveryErrorV1::Io(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| RemoteRuntimeRecoveryErrorV1::Io(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn validate_identifier(value: &str) -> Result<(), RemoteRuntimeRecoveryErrorV1> {
    if value.is_empty()
        || value.len() > 512
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(RemoteRuntimeRecoveryErrorV1::InvalidIdentifier)
    } else {
        Ok(())
    }
}

fn sync_parent(destination: &Path) -> Result<(), RemoteRuntimeRecoveryErrorV1> {
    sync_parent_io(destination).map_err(|error| RemoteRuntimeRecoveryErrorV1::Io(error.to_string()))
}

#[cfg(unix)]
fn sync_parent_io(destination: &Path) -> io::Result<()> {
    File::open(
        destination.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent")
        })?,
    )?
    .sync_all()
}

#[cfg(not(unix))]
fn sync_parent_io(_destination: &Path) -> io::Result<()> {
    Ok(())
}

#[derive(Debug)]
pub enum RemoteRuntimeRecoveryErrorV1 {
    Writer(WriterActorError),
    BackupReceiptInvalid,
    ManifestInvalid(String),
    ManifestChanged,
    ArtifactUnavailable,
    ArtifactKindMismatch,
    DestinationExists,
    DestinationBytesMismatch,
    ReferenceClosure,
    PolicyReplayRejected,
    PolicyReplayFailed(String),
    BindingMismatch,
    InvalidTransition,
    InvalidIdentifier,
    Sqlite(String),
    Io(String),
    ForwardRecovery(String),
}

impl std::fmt::Display for RemoteRuntimeRecoveryErrorV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Writer(error) => write!(formatter, "writer backup failed: {error}"),
            Self::BackupReceiptInvalid => formatter.write_str("online backup receipt is invalid"),
            Self::ManifestInvalid(error) => {
                write!(formatter, "backup manifest is invalid: {error}")
            }
            Self::ManifestChanged => formatter.write_str("backup manifest changed"),
            Self::ArtifactUnavailable => formatter.write_str("backup artifact is unavailable"),
            Self::ArtifactKindMismatch => {
                formatter.write_str("backup artifact kind does not match")
            }
            Self::DestinationExists => formatter.write_str("restore destination already exists"),
            Self::DestinationBytesMismatch => {
                formatter.write_str("restore destination bytes do not match the manifest")
            }
            Self::ReferenceClosure => {
                formatter.write_str("restore reference closure is incomplete")
            }
            Self::PolicyReplayRejected => formatter.write_str("current policy replay was rejected"),
            Self::PolicyReplayFailed(error) => {
                write!(formatter, "current policy replay failed: {error}")
            }
            Self::BindingMismatch => {
                formatter.write_str("installed authority binding does not match")
            }
            Self::InvalidTransition => formatter.write_str("invalid staged restore transition"),
            Self::InvalidIdentifier => formatter.write_str("identifier is not canonical"),
            Self::Sqlite(error) => write!(formatter, "SQLite verification failed: {error}"),
            Self::Io(error) => write!(formatter, "restore I/O failed: {error}"),
            Self::ForwardRecovery(error) => write!(formatter, "forward recovery required: {error}"),
        }
    }
}

impl std::error::Error for RemoteRuntimeRecoveryErrorV1 {}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn corrupt_destination_bytes_are_removed_before_serving() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.sqlite3");
        let connection = Connection::open(&source).unwrap();
        connection
            .execute_batch("CREATE TABLE t(v INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        drop(connection);
        let staged = root.path().join("staged.sqlite3");
        let digest = sha256_file(&source).unwrap();
        let bytes = fs::metadata(&source).unwrap().len();
        let binding =
            crate::test_support::binding(&crate::test_support::metadata("restore", "restore", 'r'));
        let writer: tracedecay_domain::RemoteWriterFenceV1 =
            serde_json::from_value(serde_json::json!({
                "brain_id": "brain.runtime",
                "shard_id": "shard.remote",
                "generation_id": "generation.remote",
                "placement_revision": 1,
                "authority_epoch": binding.authority_epoch.get(),
                "authority_node_id": "node.authority"
            }))
            .unwrap();
        let authentication: tracedecay_store::AuthenticatedManifestContextV1 =
            serde_json::from_value(serde_json::json!({
                "enrollment": {
                    "enrollment_id": "enrollment.remote",
                    "brain_id": "brain.runtime",
                    "node_id": "node.authority",
                    "fingerprint": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "revision": 1,
                    "issued_at": 1,
                    "expires_at": 10,
                    "revoked_at": null,
                    "capabilities": ["read_backup"],
                    "scope": {
                        "project_id": "project.runtime",
                        "repository_id": "repository.runtime",
                        "worktree_id": "worktree.runtime",
                        "reference": "refs/heads/main",
                        "snapshot_id": "snapshot.runtime"
                    }
                },
                "authorization_revision": 1,
                "authentication_receipt_id": "authentication.remote",
                "authenticated_at": 1
            }))
            .unwrap();
        let manifest = BackupManifestV1 {
            backup_id: "backup.1".into(),
            authentication,
            writer,
            runtime: binding.clone(),
            schema_digest: [1; 32],
            source_frontier: ShardWatermarkV1 {
                shard_id: binding.shard_id,
                incarnation: binding.incarnation,
                authority_epoch: binding.authority_epoch,
                commit_sequence: tracedecay_store::CommitSequenceV1(1),
            },
            parent_backup_id: None,
            lineage_digest: [2; 32],
            created_at_micros: 1,
            expires_at_micros: 10,
            coverage: tracedecay_store::BackupCoverageV1::Complete,
            artifacts: vec![BackupArtifactV1 {
                artifact_id: "db".into(),
                family: "profile".into(),
                kind: BackupArtifactKindV1::SqliteDatabase,
                bytes: bytes + 1,
                sha256: digest,
                references: vec![],
            }],
            total_bytes: bytes + 1,
            artifact_count: 1,
        };
        assert!(matches!(
            stage_sqlite_restore(&source, staged.clone(), &manifest, [9; 32], "db", 2),
            Err(RemoteRuntimeRecoveryErrorV1::DestinationBytesMismatch)
        ));
        assert!(!staged.exists());
    }
}
