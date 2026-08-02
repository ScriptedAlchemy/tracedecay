use super::*;

const REMOTE_SCHEMA_VERSION_V1: i64 = 1;

pub const REMOTE_SCHEMA_V1: &str = "
CREATE TABLE remote_schema_versions_v1 (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    version INTEGER NOT NULL CHECK (version > 0)
) STRICT;
INSERT INTO remote_schema_versions_v1 (singleton, version) VALUES (1, 1);

CREATE TABLE remote_authorities_v1 (
    brain_id TEXT PRIMARY KEY,
    runtime_binding_json TEXT NOT NULL,
    authority_state_json TEXT NOT NULL,
    writer_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE remote_enrollment_grants_v1 (
    grant_id TEXT PRIMARY KEY,
    grant_json TEXT NOT NULL,
    admission_json TEXT NOT NULL,
    consumed_at INTEGER
) STRICT;

CREATE TABLE remote_enrollments_v1 (
    enrollment_id TEXT PRIMARY KEY,
    brain_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    credential_fingerprint TEXT NOT NULL,
    enrollment_json TEXT NOT NULL,
    commit_receipt_json TEXT NOT NULL,
    UNIQUE (credential_fingerprint),
    UNIQUE (brain_id, node_id, revision)
) STRICT;

CREATE TABLE remote_spool_frames_v1 (
    event_id TEXT PRIMARY KEY,
    enrollment_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    previous_event_id TEXT,
    frame_digest TEXT NOT NULL,
    key_revision INTEGER NOT NULL CHECK (key_revision > 0),
    nonce BLOB NOT NULL CHECK (length(nonce) = 12),
    ciphertext BLOB NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN (
            'pending', 'admitted', 'duplicate', 'acknowledged',
            'rejected', 'quarantined', 'garbage_collection_eligible'
        )
    ),
    last_attempt INTEGER NOT NULL DEFAULT 0 CHECK (last_attempt >= 0),
    attempt_started_at INTEGER,
    receipt_json TEXT,
    finding TEXT,
    captured_at INTEGER NOT NULL,
    UNIQUE (enrollment_id, sequence)
) STRICT;

CREATE TABLE remote_observations_v1 (
    observation_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    observation_json TEXT NOT NULL,
    runtime_binding_json TEXT NOT NULL,
    writer_fence_json TEXT NOT NULL,
    replay_receipt_json TEXT NOT NULL,
    committed_at INTEGER NOT NULL
) STRICT;

CREATE TABLE remote_recovery_journal_v1 (
    operation_id TEXT PRIMARY KEY,
    operation_kind TEXT NOT NULL CHECK (operation_kind IN ('backup', 'restore', 'failover')),
    state TEXT NOT NULL,
    request_json TEXT NOT NULL,
    receipt_json TEXT,
    updated_at INTEGER NOT NULL
) STRICT;
";

/// Installs V1 only from an explicit migration path holding write authority.
pub fn install_remote_schema_v1(
    handle: &MigrationSqlHandle,
) -> Result<(), RemoteSqliteStorageErrorV1> {
    if remote_schema_version(handle)?.is_some() {
        return validate_remote_schema(handle);
    }
    let transaction = handle.begin_schema_migration_immediate()?;
    transaction.execute_schema_batch_step(REMOTE_SCHEMA_V1.to_owned())?;
    transaction.commit()?;
    validate_remote_schema(handle)
}

pub fn validate_remote_schema(
    handle: &MigrationSqlHandle,
) -> Result<(), RemoteSqliteStorageErrorV1> {
    match remote_schema_version(handle)? {
        None => Err(RemoteSqliteStorageErrorV1::MigrationRequired),
        Some(REMOTE_SCHEMA_VERSION_V1) => Ok(()),
        Some(actual) => Err(RemoteSqliteStorageErrorV1::UnsupportedSchema { actual }),
    }
}

fn remote_schema_version(
    handle: &MigrationSqlHandle,
) -> Result<Option<i64>, RemoteSqliteStorageErrorV1> {
    let tables = query(
        handle,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        vec![text("remote_schema_versions_v1")],
    )?;
    if tables.rows.is_empty() {
        return Ok(None);
    }
    let versions = query(
        handle,
        "SELECT version FROM remote_schema_versions_v1 WHERE singleton = 1",
        Vec::new(),
    )?;
    match versions.rows.as_slice() {
        [] => Ok(None),
        [row] => match row.values.as_slice() {
            [MigrationSqlValue::Integer(version)] => Ok(Some(*version)),
            _ => Err(RemoteSqliteStorageErrorV1::Corruption),
        },
        _ => Err(RemoteSqliteStorageErrorV1::Corruption),
    }
}
