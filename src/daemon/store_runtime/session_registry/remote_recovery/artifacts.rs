use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tracedecay_application::remote::recovery::RecoveryAuthorityExpectationV1;
use tracedecay_domain::{ManifestDigest, ProjectId, RemoteWriterFenceV1, UtcMicros};
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
    if rollback.exists() && !staging.exists() {
        validate_isolated_restore(destination)?;
        validate_isolated_restore(rollback)?;
        return Ok(true);
    }
    if !sha256_file(destination).is_ok_and(|digest| digest == expected_destination_sha256) {
        return Ok(false);
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

/// Reapplies state whose current value is authoritative over an older backup.
///
/// Recovery may restore historical payload, but it must never roll back a
/// writer fence, deletion disposition, quarantine, retention tombstone, or
/// current configuration decision. The destination is quiesced by the caller,
/// so the attached read is an exact final snapshot.
pub(super) fn replay_current_authority_state(
    current: &Path,
    staging: &Path,
) -> Result<(), RemoteRecoveryPhysicalEffectErrorV1> {
    let mut connection = rusqlite::Connection::open_with_flags(
        staging,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    let current = current
        .to_str()
        .ok_or(RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    connection
        .execute("ATTACH DATABASE ?1 AS current_authority", [current])
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    validate_exact_final_schema(&connection)?;
    validate_authority_reference_closure(&connection)?;
    let transaction = connection
        .transaction()
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    transaction
        .execute_batch("PRAGMA defer_foreign_keys=ON")
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    let authority_triggers = suspend_table_triggers(&transaction, AUTHORITY_STATE_TABLES)?;
    for table in AUTHORITY_STATE_TABLES {
        copy_current_rows(&transaction, table)?;
    }
    restore_table_triggers(&transaction, authority_triggers)?;
    tracedecay_global_db::observation::retention::replay_current_release_state_for_restore(
        &transaction,
    )
    .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    let violation: i64 = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check LIMIT 1)",
            (),
            |row| row.get(0),
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    if violation != 0 {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    transaction
        .commit()
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    connection
        .execute_batch("DETACH DATABASE current_authority")
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    PrivateStoreIo::sync_sqlite_family(staging)
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Unavailable)?;
    validate_isolated_restore(staging)
}

const AUTHORITY_STATE_TABLES: &[&str] = &[
    "configuration_format",
    "configuration_revisions",
    "configuration_entries",
    "configuration_topology_policies",
    "configuration_topology_roots",
    "configuration_topology_protected_refs",
    "configuration_source_bindings",
    "configuration_access_rules",
    "configuration_change_plans",
    "configuration_change_plan_operations",
    "configuration_change_plan_events",
    "configuration_mutation_receipts",
    "configuration_audit_events",
    "configuration_audit_redaction_keys",
    "configuration_credential_references",
    "configuration_component_activation_events",
    "remote_writer_fences",
    "source_cursors",
    "observation_backfill_watermarks",
    "retrieval_anchor_dispositions",
    "retrieval_anchor_derivative_tombstones",
    "git_index_repository_quarantines",
    "observation_projection_dispositions",
    "observation_projection_rebuild_dispositions",
    "session_temporal_migration_dispositions",
    "session_query_cursor_keys",
];

fn validate_exact_final_schema(
    connection: &rusqlite::Connection,
) -> Result<(), RemoteRecoveryPhysicalEffectErrorV1> {
    let mismatch: i64 = connection
        .query_row(
            "SELECT EXISTS(
                SELECT type, name, tbl_name, COALESCE(sql, '')
                FROM current_authority.sqlite_master
                WHERE name NOT LIKE 'sqlite_%'
                EXCEPT
                SELECT type, name, tbl_name, COALESCE(sql, '')
                FROM main.sqlite_master
                WHERE name NOT LIKE 'sqlite_%'
             ) OR EXISTS(
                SELECT type, name, tbl_name, COALESCE(sql, '')
                FROM main.sqlite_master
                WHERE name NOT LIKE 'sqlite_%'
                EXCEPT
                SELECT type, name, tbl_name, COALESCE(sql, '')
                FROM current_authority.sqlite_master
                WHERE name NOT LIKE 'sqlite_%'
             )",
            (),
            |row| row.get(0),
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    if mismatch != 0 {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    let current_version: i64 = connection
        .query_row("PRAGMA current_authority.user_version", (), |row| {
            row.get(0)
        })
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    let staging_version: i64 = connection
        .query_row("PRAGMA main.user_version", (), |row| row.get(0))
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    if current_version != staging_version {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    for table in AUTHORITY_STATE_TABLES {
        let present: i64 = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM main.sqlite_master
                    WHERE type = 'table' AND name = ?1
                 )",
                [table],
                |row| row.get(0),
            )
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        if present != 1 {
            return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
        }
    }
    Ok(())
}

fn validate_authority_reference_closure(
    connection: &rusqlite::Connection,
) -> Result<(), RemoteRecoveryPhysicalEffectErrorV1> {
    let missing: i64 = connection
        .query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM current_authority.retrieval_anchor_dispositions AS disposition
                LEFT JOIN main.retrieval_anchors AS anchor
                  ON anchor.anchor_id = disposition.anchor_id
                 AND anchor.owner_json = disposition.owner_json
                LEFT JOIN main.retrieval_anchors AS replacement
                  ON replacement.anchor_id = disposition.superseded_by
                 AND replacement.owner_json = disposition.owner_json
                WHERE anchor.anchor_id IS NULL
                   OR (
                       disposition.superseded_by IS NOT NULL
                       AND replacement.anchor_id IS NULL
                   )
             ) OR EXISTS(
                SELECT 1
                FROM current_authority.retrieval_anchor_derivative_tombstones AS tombstone
                LEFT JOIN main.retrieval_anchor_reverse_lineage AS lineage
                  ON lineage.source_anchor_id = tombstone.source_anchor_id
                 AND lineage.owner_json = tombstone.owner_json
                 AND lineage.derivative_kind = tombstone.derivative_kind
                 AND lineage.derivative_id = tombstone.derivative_id
                WHERE lineage.source_anchor_id IS NULL
             ) OR EXISTS(
                SELECT 1
                FROM current_authority.git_index_repository_quarantines AS quarantine
                LEFT JOIN main.git_index_transaction_journals AS journal
                  ON journal.transaction_id = quarantine.transaction_id
                WHERE journal.transaction_id IS NULL
             ) OR EXISTS(
                SELECT 1
                FROM current_authority.observation_projection_dispositions AS disposition
                LEFT JOIN main.observations AS observation
                  ON observation.observation_id = disposition.observation_id
                LEFT JOIN main.sanitization_receipts AS receipt
                  ON receipt.receipt_id = disposition.receipt_id
                WHERE observation.observation_id IS NULL OR receipt.receipt_id IS NULL
             ) OR EXISTS(
                SELECT 1
                FROM current_authority.observation_projection_rebuild_dispositions
                    AS disposition
                LEFT JOIN main.observation_projection_rebuilds AS rebuild
                  ON rebuild.projector_version = disposition.projector_version
                 AND rebuild.generation = disposition.generation
                LEFT JOIN main.observations AS observation
                  ON observation.observation_id = disposition.observation_id
                LEFT JOIN main.sanitization_receipts AS receipt
                  ON receipt.receipt_id = disposition.receipt_id
                WHERE rebuild.projector_version IS NULL
                   OR observation.observation_id IS NULL
                   OR receipt.receipt_id IS NULL
             ) OR EXISTS(
                SELECT 1
                FROM current_authority.session_temporal_migration_dispositions
                    AS disposition
                LEFT JOIN main.session_temporal_generations AS generation
                  ON generation.session_id = disposition.session_id
                 AND generation.generation = disposition.generation
                WHERE generation.session_id IS NULL
             )",
            (),
            |row| row.get(0),
        )
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    if missing != 0 {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    Ok(())
}

fn suspend_table_triggers(
    transaction: &rusqlite::Transaction<'_>,
    tables: &[&str],
) -> Result<Vec<(String, String)>, RemoteRecoveryPhysicalEffectErrorV1> {
    let table_names = tables
        .iter()
        .map(|table| format!("'{}'", table.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT name, sql FROM main.sqlite_master
         WHERE type = 'trigger' AND tbl_name IN ({table_names})
         ORDER BY name"
    );
    let triggers = {
        let mut statement = transaction
            .prepare(&sql)
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        let rows = statement
            .query_map((), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?
    };
    for (name, _) in &triggers {
        transaction
            .execute(&format!("DROP TRIGGER {}", quoted(name)), ())
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    }
    Ok(triggers)
}

fn restore_table_triggers(
    transaction: &rusqlite::Transaction<'_>,
    triggers: Vec<(String, String)>,
) -> Result<(), RemoteRecoveryPhysicalEffectErrorV1> {
    for (_, sql) in triggers {
        transaction
            .execute(&sql, ())
            .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    }
    Ok(())
}

fn copy_current_rows(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
) -> Result<(), RemoteRecoveryPhysicalEffectErrorV1> {
    let current = table_columns(transaction, "current_authority", table)?;
    let staging = table_columns(transaction, "main", table)?;
    if current != staging || current.is_empty() {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    let columns = current
        .iter()
        .map(|(column, _)| quoted(column))
        .collect::<Vec<_>>()
        .join(", ");
    let primary_key = table_primary_key(transaction, "current_authority", table)?;
    if primary_key.is_empty() || primary_key != table_primary_key(transaction, "main", table)? {
        return Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption);
    }
    let identity = primary_key
        .iter()
        .map(|column| {
            format!(
                "current_authority.{}.{} = main.{}.{}",
                quoted(table),
                quoted(column),
                quoted(table),
                quoted(column)
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    let delete = format!(
        "DELETE FROM main.{}
         WHERE NOT EXISTS (
            SELECT 1 FROM current_authority.{} WHERE {identity}
         )",
        quoted(table),
        quoted(table)
    );
    transaction
        .execute(&delete, ())
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    let sql = format!(
        "INSERT OR REPLACE INTO main.{} ({columns})
         SELECT {columns} FROM current_authority.{}",
        quoted(table),
        quoted(table)
    );
    transaction
        .execute(&sql, ())
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    Ok(())
}

fn table_primary_key(
    connection: &rusqlite::Connection,
    schema: &str,
    table: &str,
) -> Result<Vec<String>, RemoteRecoveryPhysicalEffectErrorV1> {
    let sql = format!("PRAGMA {schema}.table_info({})", quoted(table));
    let mut statement = connection
        .prepare(&sql)
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    let rows = statement
        .query_map((), |row| {
            Ok((row.get::<_, i64>(5)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    let mut columns = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    columns.retain(|(ordinal, _)| *ordinal > 0);
    columns.sort_by_key(|(ordinal, _)| *ordinal);
    Ok(columns.into_iter().map(|(_, column)| column).collect())
}

fn table_columns(
    connection: &rusqlite::Connection,
    schema: &str,
    table: &str,
) -> Result<Vec<(String, String)>, RemoteRecoveryPhysicalEffectErrorV1> {
    let sql = format!("PRAGMA {schema}.table_info({})", quoted(table));
    let mut statement = connection
        .prepare(&sql)
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    let rows = statement
        .query_map((), |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| RemoteRecoveryPhysicalEffectErrorV1::Corruption)
}

fn quoted(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
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

pub(super) fn require_current_writer_fence(
    current: Option<(RemoteWriterFenceV1, u64)>,
    expected: &RemoteWriterFenceV1,
) -> Result<u64, RemoteRecoveryPhysicalEffectErrorV1> {
    match current {
        Some((fence, frontier)) if fence == *expected => Ok(frontier),
        Some((fence, _)) if fence.fences(expected) => {
            Err(RemoteRecoveryPhysicalEffectErrorV1::StaleAuthority)
        }
        _ => Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer_fence(authority_epoch: u64, authority_node_id: &str) -> RemoteWriterFenceV1 {
        serde_json::from_value(serde_json::json!({
            "brain_id": "brain.remote",
            "shard_id": "shard.remote",
            "generation_id": "generation.remote",
            "placement_revision": authority_epoch,
            "authority_epoch": authority_epoch,
            "authority_node_id": authority_node_id,
        }))
        .unwrap()
    }

    #[test]
    fn newer_same_lineage_authority_is_stale_not_corrupt() {
        let expected = writer_fence(8, "node.old");
        let current = writer_fence(9, "node.promoted");

        assert_eq!(
            require_current_writer_fence(Some((expected.clone(), 41)), &expected),
            Ok(41)
        );
        assert_eq!(
            require_current_writer_fence(Some((current, 42)), &expected),
            Err(RemoteRecoveryPhysicalEffectErrorV1::StaleAuthority)
        );
        let unrelated: RemoteWriterFenceV1 = serde_json::from_value(serde_json::json!({
            "brain_id": "brain.other",
            "shard_id": "shard.remote",
            "generation_id": "generation.remote",
            "placement_revision": 9,
            "authority_epoch": 9,
            "authority_node_id": "node.other",
        }))
        .unwrap();
        assert_eq!(
            require_current_writer_fence(Some((unrelated, 43)), &expected),
            Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption)
        );
    }

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
    fn restore_rejects_a_noncanonical_schema_before_interpreting_state() {
        let temporary = tempfile::tempdir().unwrap();
        let current = temporary.path().join("sessions.current.db");
        let staging = temporary.path().join("sessions.restore.db");
        project_database(&current, "current");
        project_database(&staging, "staging");
        rusqlite::Connection::open(&staging)
            .unwrap()
            .execute(
                "CREATE TABLE branch_local_shape (id INTEGER PRIMARY KEY)",
                (),
            )
            .unwrap();

        assert_eq!(
            replay_current_authority_state(&current, &staging),
            Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption)
        );
    }

    #[test]
    fn authority_overlay_rejects_missing_typed_reference_closure() {
        let temporary = tempfile::tempdir().unwrap();
        let current = temporary.path().join("sessions.current.db");
        let staging = temporary.path().join("sessions.restore.db");
        rusqlite::Connection::open(&current)
            .unwrap()
            .execute_batch(
                "CREATE TABLE retrieval_anchor_dispositions (
                    anchor_id TEXT, owner_json TEXT, superseded_by TEXT
                 );
                 CREATE TABLE retrieval_anchor_derivative_tombstones (
                    source_anchor_id TEXT, owner_json TEXT,
                    derivative_kind TEXT, derivative_id TEXT
                 );
                 CREATE TABLE git_index_repository_quarantines (
                    transaction_id TEXT
                 );
                 CREATE TABLE observation_projection_dispositions (
                    observation_id TEXT, receipt_id TEXT
                 );
                 CREATE TABLE observation_projection_rebuild_dispositions (
                    projector_version TEXT, generation TEXT,
                    observation_id TEXT, receipt_id TEXT
                 );
                 CREATE TABLE session_temporal_migration_dispositions (
                    session_id TEXT, generation INTEGER
                 );
                 INSERT INTO retrieval_anchor_dispositions
                 VALUES ('anchor.missing', '{}', NULL);",
            )
            .unwrap();
        let connection = rusqlite::Connection::open(&staging).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE retrieval_anchors (
                    anchor_id TEXT, owner_json TEXT
                 );
                 CREATE TABLE retrieval_anchor_reverse_lineage (
                    source_anchor_id TEXT, owner_json TEXT,
                    derivative_kind TEXT, derivative_id TEXT
                 );
                 CREATE TABLE git_index_transaction_journals (
                    transaction_id TEXT
                 );
                 CREATE TABLE observations (observation_id TEXT);
                 CREATE TABLE sanitization_receipts (receipt_id TEXT);
                 CREATE TABLE observation_projection_rebuilds (
                    projector_version TEXT, generation TEXT
                 );
                 CREATE TABLE session_temporal_generations (
                    session_id TEXT, generation INTEGER
                 );",
            )
            .unwrap();
        connection
            .execute(
                "ATTACH DATABASE ?1 AS current_authority",
                [current.to_str().unwrap()],
            )
            .unwrap();

        assert_eq!(
            validate_authority_reference_closure(&connection),
            Err(RemoteRecoveryPhysicalEffectErrorV1::Corruption)
        );
    }

    #[test]
    fn authority_overlay_preserves_current_cursor_key_rotation_and_retirement() {
        let temporary = tempfile::tempdir().unwrap();
        let current = temporary.path().join("sessions.current.db");
        let staging = temporary.path().join("sessions.restore.db");
        let schema = "CREATE TABLE session_query_cursor_keys (
            key_id TEXT PRIMARY KEY,
            key_version INTEGER NOT NULL UNIQUE CHECK(key_version > 0),
            key_material BLOB NOT NULL,
            created_at INTEGER NOT NULL,
            retired_at INTEGER CHECK(retired_at IS NULL OR retired_at >= created_at)
        );";
        let current_connection = rusqlite::Connection::open(&current).unwrap();
        current_connection.execute_batch(schema).unwrap();
        current_connection
            .execute(
                "INSERT INTO session_query_cursor_keys
                 VALUES ('cursor.retired', 1, X'01', 10, 20)",
                (),
            )
            .unwrap();
        current_connection
            .execute(
                "INSERT INTO session_query_cursor_keys
                 VALUES ('cursor.active', 2, X'02', 30, NULL)",
                (),
            )
            .unwrap();
        drop(current_connection);
        let mut staging_connection = rusqlite::Connection::open(&staging).unwrap();
        staging_connection.execute_batch(schema).unwrap();
        staging_connection
            .execute(
                "INSERT INTO session_query_cursor_keys
                 VALUES ('cursor.stale', 9, X'09', 1, NULL)",
                (),
            )
            .unwrap();
        staging_connection
            .execute(
                "ATTACH DATABASE ?1 AS current_authority",
                [current.to_str().unwrap()],
            )
            .unwrap();

        let transaction = staging_connection.transaction().unwrap();
        copy_current_rows(&transaction, "session_query_cursor_keys").unwrap();
        transaction.commit().unwrap();

        let mut statement = staging_connection
            .prepare(
                "SELECT key_id, key_version, hex(key_material), created_at, retired_at
                 FROM session_query_cursor_keys ORDER BY key_version",
            )
            .unwrap();
        let rows = statement
            .query_map((), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "cursor.retired".to_owned(),
                    1,
                    "01".to_owned(),
                    10,
                    Some(20)
                ),
                ("cursor.active".to_owned(), 2, "02".to_owned(), 30, None),
            ]
        );
    }
}
