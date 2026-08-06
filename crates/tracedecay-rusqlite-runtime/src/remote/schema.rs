use std::collections::BTreeSet;

use super::*;

const REQUIRED_REMOTE_TABLES: &[&str] = &[
    "remote_authorities",
    "remote_enrollment_grants",
    "remote_enrollments",
    "remote_spool_frames",
];

pub const REMOTE_NODE_LOCAL_SCHEMA: &str = "
CREATE TABLE remote_authorities (
    brain_id TEXT PRIMARY KEY,
    runtime_binding_json TEXT NOT NULL,
    authority_state_json TEXT NOT NULL,
    writer_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE remote_enrollment_grants (
    grant_id TEXT PRIMARY KEY,
    grant_json TEXT NOT NULL,
    admission_json TEXT NOT NULL,
    consumed_at INTEGER
) STRICT;

CREATE TABLE remote_enrollments (
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

CREATE TABLE remote_spool_frames (
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

";

/// Canonical repository-store fragment for replay identity and sequencing.
///
/// The registered final-schema authority consumes this exact fragment. It is
/// deliberately absent from [`REMOTE_NODE_LOCAL_SCHEMA`].
pub const REMOTE_OBSERVATION_EVENTS_SCHEMA: &str = "
CREATE TABLE remote_observation_events (
    event_id TEXT PRIMARY KEY,
    frame_digest TEXT NOT NULL,
    enrollment_id TEXT NOT NULL,
    enrollment_revision INTEGER NOT NULL CHECK (enrollment_revision > 0),
    node_id TEXT NOT NULL,
    policy_revision INTEGER NOT NULL CHECK (policy_revision > 0),
    capture_sequence INTEGER NOT NULL CHECK (capture_sequence > 0),
    previous_event_id TEXT REFERENCES remote_observation_events(event_id),
    observation_id TEXT NOT NULL UNIQUE REFERENCES observations(observation_id),
    writer_fence_json TEXT NOT NULL CHECK (json_valid(writer_fence_json)),
    captured_at INTEGER NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    command_digest TEXT NOT NULL,
    UNIQUE (enrollment_id, node_id, capture_sequence)
) STRICT;
";

pub fn validate_remote_schema(handle: &ExactSqlHandle) -> Result<(), RemoteSqliteStorageErrorV1> {
    let tables = remote_tables(handle)?;
    if tables.is_empty() {
        return Err(RemoteSqliteStorageErrorV1::MigrationRequired);
    }
    let required = REQUIRED_REMOTE_TABLES
        .iter()
        .copied()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if tables != required {
        return Err(RemoteSqliteStorageErrorV1::ResetRequired);
    }
    for sql in [
        "SELECT brain_id, runtime_binding_json, authority_state_json, writer_json, updated_at
         FROM remote_authorities LIMIT 0",
        "SELECT grant_id, grant_json, admission_json, consumed_at
         FROM remote_enrollment_grants LIMIT 0",
        "SELECT enrollment_id, brain_id, node_id, revision, credential_fingerprint,
                enrollment_json, commit_receipt_json
         FROM remote_enrollments LIMIT 0",
        "SELECT event_id, enrollment_id, sequence, previous_event_id, frame_digest,
                key_revision, nonce, ciphertext, state, last_attempt, attempt_started_at,
                receipt_json, finding, captured_at
         FROM remote_spool_frames LIMIT 0",
    ] {
        handle
            .query(
                ExactSqlStatement::new(sql.to_owned(), Vec::new())?,
                READ_WAIT,
            )
            .map_err(|_| RemoteSqliteStorageErrorV1::ResetRequired)?;
    }
    Ok(())
}

fn remote_tables(handle: &ExactSqlHandle) -> Result<BTreeSet<String>, RemoteSqliteStorageErrorV1> {
    let names = REQUIRED_REMOTE_TABLES
        .iter()
        .copied()
        .chain([
            "remote_observation_events",
            "remote_schema_versions_v1",
            "remote_observations_v1",
            "remote_authorities_v1",
            "remote_enrollment_grants_v1",
            "remote_enrollments_v1",
            "remote_spool_frames_v1",
            "remote_recovery_journal",
            "remote_recovery_journal_v1",
        ])
        .collect::<Vec<_>>();
    let placeholders = (1..=names.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = handle.query(
        ExactSqlStatement::new(
            format!(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name IN ({placeholders})"
            ),
            names.into_iter().map(text).collect(),
        )?,
        READ_WAIT,
    )?;
    rows.rows
        .iter()
        .map(|row| {
            row_text(row, 0)
                .map(str::to_owned)
                .map_err(|_| RemoteSqliteStorageErrorV1::Corruption)
        })
        .collect()
}
