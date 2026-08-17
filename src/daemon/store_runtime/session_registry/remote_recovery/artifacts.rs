use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tracedecay_application::remote::recovery::RecoveryAuthorityExpectationV1;
use tracedecay_domain::{ManifestDigest, ProjectId, UtcMicros};
use tracedecay_runtime_core::storage::PrivateStoreIo;
use tracedecay_rusqlite_runtime::remote::RemoteRecoveryPhysicalEffectErrorV1;
use tracedecay_store::{ShardWatermarkV1, StoreShardIdV1};

use super::DatabaseAuthority;

const MAX_BACKUP_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoteBackupManifestV1 {
    pub(super) version: String,
    pub(super) backup_id: String,
    pub(super) expected: RecoveryAuthorityExpectationV1,
    pub(super) policy_digest: ManifestDigest,
    pub(super) project_id: ProjectId,
    pub(super) source_shard: StoreShardIdV1,
    pub(super) destination_bytes: u64,
    pub(super) destination_sha256: [u8; 32],
    pub(super) source_watermark: ShardWatermarkV1,
    pub(super) committed_at: UtcMicros,
}

pub(super) struct BackupSnapshotV1 {
    pub(super) source_watermark: ShardWatermarkV1,
    pub(super) destination_bytes: u64,
    pub(super) destination_sha256: [u8; 32],
}

pub(super) fn read_json_manifest<T: DeserializeOwned>(
    path: &Path,
) -> Result<T, RemoteRecoveryPhysicalEffectErrorV1> {
    let file =
        std::fs::File::open(path).map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
    if file
        .metadata()
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?
        .len()
        > MAX_BACKUP_MANIFEST_BYTES
    {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    let mut bytes = Vec::new();
    file.take(MAX_BACKUP_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
    if bytes.len() as u64 > MAX_BACKUP_MANIFEST_BYTES {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    serde_json::from_slice(&bytes).map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)
}

pub(super) fn converge_interrupted_restore(
    destination: &Path,
    staging: &Path,
    rollback: &Path,
    expected_destination_sha256: [u8; 32],
) -> Result<bool, RemoteRecoveryPhysicalEffectErrorV1> {
    if !sha256_file(destination).is_ok_and(|digest| digest == expected_destination_sha256) {
        return Ok(false);
    }
    if rollback.exists() && !staging.exists() {
        validate_isolated_restore(destination)?;
        validate_isolated_restore(rollback)?;
        return Ok(true);
    }
    if rollback.exists() {
        validate_isolated_restore(rollback)?;
    } else if staging.exists() {
        validate_isolated_restore(staging)?;
        DatabaseAuthority::replace_file_atomically(
            staging,
            rollback,
            "interrupted remote restore rollback",
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired)?;
        PrivateStoreIo::sync_sqlite_family(rollback)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::ForwardRecoveryRequired)?;
    }
    Ok(true)
}

pub(super) fn validate_isolated_restore(
    path: &Path,
) -> Result<(), RemoteRecoveryPhysicalEffectErrorV1> {
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    let integrity_check: String = connection
        .query_row("PRAGMA integrity_check", (), |row| row.get(0))
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    if integrity_check != "ok" {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    let foreign_key_violation: i64 = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check LIMIT 1)",
            (),
            |row| row.get(0),
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    for required in [
        "observations",
        "remote_observation_events",
        "remote_writer_fences",
    ] {
        let present: i64 = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
                 )",
                [required],
                |row| row.get(0),
            )
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        if present != 1 {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
        }
    }
    if foreign_key_violation != 0 {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    Ok(())
}

pub(super) fn sqlite_identity(path: &Path) -> Result<u64, RemoteRecoveryPhysicalEffectErrorV1> {
    tracedecay_runtime_core::db::sqlite_generation_identity(path)
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)
}

pub(super) fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(super) fn digest_bytes(
    digest: &ManifestDigest,
) -> Result<[u8; 32], RemoteRecoveryPhysicalEffectErrorV1> {
    let suffix = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    let decoded =
        hex::decode(suffix).map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    decoded
        .try_into()
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)
}

pub(super) fn digest_from_bytes(
    digest: [u8; 32],
) -> Result<ManifestDigest, RemoteRecoveryPhysicalEffectErrorV1> {
    ManifestDigest::new(format!("sha256:{}", hex::encode(digest)))
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)
}

pub(super) fn classify_runtime_error(error: String) -> RemoteRecoveryPhysicalEffectErrorV1 {
    if error.contains("cancel") {
        RemoteRecoveryPhysicalEffectErrorV1::Cancelled
    } else if error.contains("timed out") || error.contains("deadline") {
        RemoteRecoveryPhysicalEffectErrorV1::TimedOut
    } else {
        RemoteRecoveryPhysicalEffectErrorV1::Unavailable
    }
}

pub(super) fn safe_digest_suffix(
    digest: &ManifestDigest,
) -> Result<&str, RemoteRecoveryPhysicalEffectErrorV1> {
    digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(RemoteRecoveryPhysicalEffectErrorV1::Corruption)
}

pub(super) fn sha256_file(path: &Path) -> Result<[u8; 32], RemoteRecoveryPhysicalEffectErrorV1> {
    let mut file =
        std::fs::File::open(path).map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_database(path: &Path, marker: &str) {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=DELETE;
                 CREATE TABLE observations (marker TEXT NOT NULL);
                 CREATE TABLE remote_observation_events (event_id TEXT PRIMARY KEY);
                 CREATE TABLE remote_writer_fences (authority_key TEXT PRIMARY KEY);
                 PRAGMA foreign_keys=ON;",
            )
            .unwrap();
        connection
            .execute("INSERT INTO observations VALUES (?1)", [marker])
            .unwrap();
    }

    #[test]
    fn interrupted_restore_retains_exchanged_original_on_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("sessions.db");
        let staging = temporary.path().join("sessions.restore.staging");
        let rollback = temporary.path().join("sessions.restore.rollback");
        project_database(&destination, "restored");
        project_database(&staging, "original");
        let restored_digest = sha256_file(&destination).unwrap();

        assert!(
            converge_interrupted_restore(&destination, &staging, &rollback, restored_digest)
                .unwrap()
        );
        assert!(!staging.exists());
        assert!(rollback.exists());
        assert_eq!(sha256_file(&destination).unwrap(), restored_digest);
        validate_isolated_restore(&rollback).unwrap();
    }

    #[test]
    fn restore_restart_does_not_converge_an_unpublished_destination() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("sessions.db");
        let staging = temporary.path().join("sessions.restore.staging");
        let rollback = temporary.path().join("sessions.restore.rollback");
        project_database(&destination, "original");
        project_database(&staging, "restored");
        let restored_digest = sha256_file(&staging).unwrap();

        assert!(
            !converge_interrupted_restore(&destination, &staging, &rollback, restored_digest)
                .unwrap()
        );
        assert!(staging.exists());
        assert!(!rollback.exists());
    }

    #[test]
    fn restore_restart_rejects_wrong_destination_even_with_a_rollback_receipt() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("sessions.db");
        let staging = temporary.path().join("sessions.restore.staging");
        let rollback = temporary.path().join("sessions.restore.rollback");
        project_database(&destination, "foreign-valid-destination");
        project_database(&rollback, "original");
        let requested = temporary.path().join("requested.db");
        project_database(&requested, "requested-restored-authority");
        let requested_digest = sha256_file(&requested).unwrap();

        assert!(
            !converge_interrupted_restore(&destination, &staging, &rollback, requested_digest)
                .unwrap()
        );
        assert!(rollback.exists());
        assert!(!staging.exists());
    }
}
