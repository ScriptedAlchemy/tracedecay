use std::collections::BTreeSet;

use libsql::{Connection, params};
use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationCollisionOutcomeV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceIdentityV1, ProjectionGenerationId,
    RetrievalAnchorRecordV2, RetrievalAnchorTargetV2, SanitizationReceiptV1, UtcMicros,
    classify_observation_collision,
};
use tracedecay_store::observation::{
    CursorAdvanceOutcome, ObservationCoverageReason, ObservationCursorAdvance,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationCommitReceipt, ObservationPersistOutcome,
    ObservationProjectionStatus, ObservationReplayRequest, ObservationStoreError,
    ObservationStoreResult, StoredObservation, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

use super::{GlobalDb, global_db_operation_error, global_db_operation_message};

const OBSERVATION_SCHEMA_MIGRATION: &str = "observations-v2-canonical-autoincrement";
const OBSERVATION_ANCHOR_SCHEMA_MIGRATION: &str = "observation-retrieval-anchors-v2";
const LEGACY_OBSERVATION_PROJECTION_GENERATION: &str = "projection.legacy-observation-import.v1";
const OBSERVATION_SCHEMA_OPERATION: &str = "migrate observation authority schema";

async fn observation_table_exists(conn: &Connection) -> crate::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'observations'",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

async fn observation_columns(conn: &Connection) -> crate::errors::Result<BTreeSet<String>> {
    let mut rows = conn
        .query("SELECT name FROM pragma_table_xinfo('observations')", ())
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    let mut columns = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
    {
        columns.insert(
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?,
        );
    }
    Ok(columns)
}

async fn migration_recorded(conn: &Connection) -> crate::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM global_schema_migrations WHERE migration = ?1",
            params![OBSERVATION_SCHEMA_MIGRATION],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

async fn migrate_observation_schema(
    conn: &Connection,
    table_preexisted: bool,
) -> crate::errors::Result<()> {
    let columns = observation_columns(conn).await?;
    let required = [
        "sequence",
        "observation_id",
        "payload_digest",
        "receipt_id",
        "observation_json",
        "committed_cursor_json",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    let mut allowed = required.clone();
    allowed.insert("idempotency_key".to_string());
    if !required.is_subset(&columns) || !columns.is_subset(&allowed) {
        return Err(global_db_operation_message(
            OBSERVATION_SCHEMA_OPERATION,
            "observations has unsupported columns for canonical migration",
        ));
    }
    super::schema_contract::validate_observation_migration_source(
        conn,
        columns.contains("idempotency_key"),
    )
    .await?;
    let recorded = migration_recorded(conn).await?;
    if !table_preexisted || (recorded && columns == required) {
        conn.execute(
            "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
            params![OBSERVATION_SCHEMA_MIGRATION],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        return Ok(());
    }

    conn.execute_batch(
        "PRAGMA defer_foreign_keys = ON;
             DROP TRIGGER IF EXISTS observations_immutable_update;
             DROP TRIGGER IF EXISTS observations_immutable_delete;
             DROP TABLE IF EXISTS observations_canonical_v2;
             CREATE TABLE observations_canonical_v2 (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                observation_id TEXT NOT NULL UNIQUE,
                payload_digest TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                observation_json TEXT NOT NULL,
                committed_cursor_json TEXT NOT NULL,
                FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
             );
             INSERT INTO observations_canonical_v2
                (sequence, observation_id, payload_digest, receipt_id,
                 observation_json, committed_cursor_json)
             SELECT sequence, observation_id, payload_digest, receipt_id,
                    observation_json, committed_cursor_json
             FROM observations;
             DROP TABLE observations;
             ALTER TABLE observations_canonical_v2 RENAME TO observations;",
    )
    .await
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    conn.execute(
        "INSERT OR REPLACE INTO global_schema_migrations(migration) VALUES (?1)",
        params![OBSERVATION_SCHEMA_MIGRATION],
    )
    .await
    .map(|_| ())
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

async fn migrate_source_cursor_advances_schema(conn: &Connection) -> crate::errors::Result<()> {
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_table_xinfo('source_cursor_advances')",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    let mut columns = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
    {
        columns.insert(
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?,
        );
    }
    let provider_neutral = [
        "source_json",
        "scope_json",
        "coverage_json",
        "reason",
        "receipt_id",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    if columns == provider_neutral {
        return Ok(());
    }
    let legacy = [
        "source_json",
        "scope_json",
        "file_generation",
        "start_offset",
        "end_offset",
        "reason",
        "receipt_id",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    if columns != legacy {
        return Err(global_db_operation_message(
            OBSERVATION_SCHEMA_OPERATION,
            "source_cursor_advances has unsupported columns",
        ));
    }
    conn.execute_batch(
        "CREATE TABLE source_cursor_advances_v2 (
            source_json TEXT NOT NULL,
            scope_json TEXT NOT NULL,
            coverage_json TEXT NOT NULL,
            reason TEXT NOT NULL,
            receipt_id TEXT,
            PRIMARY KEY(source_json, scope_json, coverage_json),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
         );
         INSERT INTO source_cursor_advances_v2
            (source_json, scope_json, coverage_json, reason, receipt_id)
         SELECT source_json, scope_json,
                json_object(
                    'generation', CAST(file_generation AS INTEGER),
                    'ordering_domain', 'file_bytes',
                    'range', json_object(
                        'start', CAST(start_offset AS INTEGER),
                        'end', CAST(end_offset AS INTEGER)
                    )
                ),
                reason, receipt_id
         FROM source_cursor_advances;
         DROP TABLE source_cursor_advances;
         ALTER TABLE source_cursor_advances_v2 RENAME TO source_cursor_advances;",
    )
    .await
    .map(|_| ())
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

async fn backfill_observation_retrieval_anchors(conn: &Connection) -> crate::errors::Result<()> {
    let mut rows = conn
        .query(
            "SELECT observation.observation_json, observation.receipt_id,
                    receipt.receipt_json
             FROM observations AS observation
             LEFT JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = observation.receipt_id
             LEFT JOIN observation_retrieval_anchors AS anchor
               ON anchor.observation_id = observation.observation_id
             WHERE anchor.observation_id IS NULL
             ORDER BY observation.sequence",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    let mut legacy_rows = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
    {
        legacy_rows.push((
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?,
            row.get::<String>(1)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?,
            row.get::<Option<String>>(2)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?,
        ));
    }
    drop(rows);

    for (observation_json, receipt_id, receipt_json) in legacy_rows {
        let receipt_json = receipt_json.ok_or_else(|| {
            global_db_operation_message(
                OBSERVATION_SCHEMA_OPERATION,
                "legacy observation receipt is unavailable for anchor backfill",
            )
        })?;
        let observation: tracedecay_domain::DurableObservationV1 =
            serde_json::from_str(&observation_json)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        let receipt: SanitizationReceiptV1 = serde_json::from_str(&receipt_json)
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        if observation.receipt() != &receipt
            || observation.receipt().receipt().receipt_id().as_str() != receipt_id
        {
            return Err(global_db_operation_message(
                OBSERVATION_SCHEMA_OPERATION,
                "legacy observation receipt does not validate for anchor backfill",
            ));
        }
        let projection_generation =
            ProjectionGenerationId::new(LEGACY_OBSERVATION_PROJECTION_GENERATION)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        let authorization = build_observation_resolution_authorization_v1(
            &observation,
            "legacy-observation-import.v1",
        )
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        let anchor = build_observation_retrieval_anchor_v2(
            &observation,
            projection_generation,
            UtcMicros(0),
            authorization,
        )
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        persist_observation_retrieval_anchor(conn, observation.observation_id(), &anchor)
            .await
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO global_schema_migrations(migration) VALUES (?1)",
        params![OBSERVATION_ANCHOR_SCHEMA_MIGRATION],
    )
    .await
    .map(|_| ())
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

pub(super) async fn ensure_observation_schema(conn: &Connection) -> crate::errors::Result<()> {
    let table_preexisted = observation_table_exists(conn).await?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS global_schema_migrations (
            migration TEXT PRIMARY KEY
        );
        CREATE TABLE IF NOT EXISTS sanitization_receipts (
            receipt_id TEXT PRIMARY KEY,
            sanitizer_version TEXT NOT NULL,
            payload_digest TEXT NOT NULL,
            receipt_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS observations (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            observation_id TEXT NOT NULL UNIQUE,
            payload_digest TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            observation_json TEXT NOT NULL,
            committed_cursor_json TEXT NOT NULL,
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS retrieval_anchors (
            anchor_id TEXT PRIMARY KEY,
            anchor_json TEXT NOT NULL CHECK(json_valid(anchor_json)),
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            projection_generation TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS observation_retrieval_anchors (
            observation_id TEXT PRIMARY KEY,
            anchor_id TEXT NOT NULL UNIQUE,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(anchor_id) REFERENCES retrieval_anchors(anchor_id)
        );
        CREATE TABLE IF NOT EXISTS retrieval_anchor_aliases (
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            alias_kind TEXT NOT NULL,
            locator_digest TEXT NOT NULL,
            anchor_id TEXT NOT NULL,
            PRIMARY KEY(owner_json, alias_kind, locator_digest),
            UNIQUE(anchor_id, alias_kind, locator_digest),
            FOREIGN KEY(anchor_id) REFERENCES retrieval_anchors(anchor_id)
        );
        CREATE TRIGGER IF NOT EXISTS retrieval_anchors_immutable_update
        BEFORE UPDATE ON retrieval_anchors BEGIN
            SELECT RAISE(ABORT, 'retrieval anchors are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS retrieval_anchors_immutable_delete
        BEFORE DELETE ON retrieval_anchors BEGIN
            SELECT RAISE(ABORT, 'retrieval anchors are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS observation_retrieval_anchors_immutable_update
        BEFORE UPDATE ON observation_retrieval_anchors BEGIN
            SELECT RAISE(ABORT, 'observation retrieval anchor bindings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS observation_retrieval_anchors_immutable_delete
        BEFORE DELETE ON observation_retrieval_anchors BEGIN
            SELECT RAISE(ABORT, 'observation retrieval anchor bindings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS retrieval_anchor_aliases_immutable_update
        BEFORE UPDATE ON retrieval_anchor_aliases BEGIN
            SELECT RAISE(ABORT, 'retrieval anchor aliases are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS retrieval_anchor_aliases_immutable_delete
        BEFORE DELETE ON retrieval_anchor_aliases BEGIN
            SELECT RAISE(ABORT, 'retrieval anchor aliases are immutable');
        END;
        CREATE TABLE IF NOT EXISTS source_cursors (
            source_json TEXT NOT NULL,
            scope_json TEXT NOT NULL,
            cursor_json TEXT NOT NULL,
            PRIMARY KEY(source_json, scope_json)
        );
        CREATE TABLE IF NOT EXISTS source_cursor_advances (
            source_json TEXT NOT NULL,
            scope_json TEXT NOT NULL,
            coverage_json TEXT NOT NULL,
            reason TEXT NOT NULL,
            receipt_id TEXT,
            PRIMARY KEY(source_json, scope_json, coverage_json),
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
        );
        CREATE TABLE IF NOT EXISTS projection_queue (
            observation_id TEXT PRIMARY KEY,
            observation_sequence INTEGER NOT NULL UNIQUE,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );",
    )
    .await
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    super::ensure_table_columns(
        conn,
        "source_cursor_advances",
        &[(
            "receipt_id",
            "ALTER TABLE source_cursor_advances
             ADD COLUMN receipt_id TEXT REFERENCES sanitization_receipts(receipt_id)",
        )],
    )
    .await
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    migrate_source_cursor_advances_schema(conn).await?;
    migrate_observation_schema(conn, table_preexisted).await?;
    backfill_observation_retrieval_anchors(conn).await
}

fn storage(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> ObservationStoreError {
    ObservationStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

fn storage_message(operation: &'static str, message: impl Into<String>) -> ObservationStoreError {
    storage(operation, std::io::Error::other(message.into()))
}

#[cfg(tracedecay_observation_fault_harness)]
const TEST_OBSERVATION_PERSIST_BARRIER_DIR_ENV: &str =
    "TRACEDECAY_TEST_OBSERVATION_PERSIST_BARRIER_DIR";

#[cfg(tracedecay_observation_fault_harness)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ObservationPersistTestBarrierStage {
    PostWritePreCommit,
    PostCommitPreAck,
}

#[cfg(tracedecay_observation_fault_harness)]
impl ObservationPersistTestBarrierStage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PostWritePreCommit => "post-write-pre-commit",
            Self::PostCommitPreAck => "post-commit-pre-ack",
        }
    }
}

/// One-shot, cross-process test barrier at a selected authoritative boundary.
///
/// The daemon atomically claims an `armed` file, publishes `arrived`, and waits for `release`.
/// The wait is bounded so a failed test cannot leave a live daemon blocked indefinitely.
#[cfg(tracedecay_observation_fault_harness)]
async fn wait_at_observation_persist_test_barrier(
    stage: ObservationPersistTestBarrierStage,
    session_id: &str,
) -> ObservationStoreResult<()> {
    let Some(root) = std::env::var_os(TEST_OBSERVATION_PERSIST_BARRIER_DIR_ENV) else {
        return Ok(());
    };
    let root = std::path::PathBuf::from(root);
    let armed = root.join("armed");
    let expected = match std::fs::read_to_string(&armed) {
        Ok(expected) => expected,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage("read observation test barrier", error)),
    };
    let Some((expected_stage, expected_session)) = expected.split_once('\n') else {
        return Err(storage_message(
            "read observation test barrier",
            "armed barrier must contain a stage and session identifier",
        ));
    };
    if expected_stage.trim() != stage.as_str() || expected_session.trim() != session_id {
        return Ok(());
    }
    let claimed = root.join("claimed");
    match std::fs::rename(&armed, &claimed) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage("claim observation test barrier", error)),
    }
    std::fs::write(root.join("arrived"), b"arrived\n")
        .map_err(|error| storage("publish observation test barrier arrival", error))?;

    let release = root.join("release");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match release.try_exists() {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return Err(storage("read observation test barrier release", error)),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(storage_message(
                "wait at observation test barrier",
                "timed out waiting for release",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn encode<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> ObservationStoreResult<String> {
    serde_json::to_string(value).map_err(|error| storage(operation, error))
}

fn decode<T: serde::de::DeserializeOwned>(
    value: &str,
    operation: &'static str,
) -> ObservationStoreResult<T> {
    serde_json::from_str(value).map_err(|error| storage(operation, error))
}

fn encode_json_string<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> ObservationStoreResult<String> {
    match serde_json::to_value(value).map_err(|error| storage(operation, error))? {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(storage_message(operation, "encoded value is not a string")),
    }
}

async fn persist_observation_retrieval_anchor(
    conn: &Connection,
    observation_id: &CanonicalObservationIdV1,
    candidate: &RetrievalAnchorRecordV2,
) -> ObservationStoreResult<(RetrievalAnchorRecordV2, ProjectionGenerationId)> {
    let anchor_json = encode(candidate, "encode retrieval anchor")?;
    let owner_json = encode(candidate.owner(), "encode retrieval anchor owner")?;
    conn.execute(
        "INSERT OR IGNORE INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            candidate.anchor_id().as_str(),
            anchor_json.as_str(),
            owner_json.as_str(),
            candidate.projection_generation().as_str(),
        ],
    )
    .await
    .map_err(|error| storage("insert retrieval anchor", error))?;
    let mut rows = conn
        .query(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            params![candidate.anchor_id().as_str()],
        )
        .await
        .map_err(|error| storage("read retrieval anchor", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("read retrieval anchor", error))?
        .ok_or_else(|| storage_message("read retrieval anchor", "anchor insert disappeared"))?;
    let stored_json = row
        .get::<String>(0)
        .map_err(|error| storage("read retrieval anchor", error))?;
    let stored_owner_json = row
        .get::<String>(1)
        .map_err(|error| storage("read retrieval anchor", error))?;
    let stored_projection_generation = row
        .get::<String>(2)
        .map_err(|error| storage("read retrieval anchor", error))?;
    drop(rows);
    let stored: RetrievalAnchorRecordV2 = decode(&stored_json, "decode retrieval anchor")?;
    let projection_generation = ProjectionGenerationId::new(stored_projection_generation)
        .map_err(ObservationStoreError::RetrievalAnchorContract)?;
    if stored.anchor_id() != candidate.anchor_id()
        || stored.owner() != candidate.owner()
        || stored_owner_json != encode(stored.owner(), "verify retrieval anchor owner")?
        || stored.projection_generation() != &projection_generation
        || !matches!(
            stored.target(),
            RetrievalAnchorTargetV2::ExactObservation(target) if target == observation_id
        )
    {
        return Err(ObservationStoreError::RetrievalAnchorObservationMismatch);
    }

    conn.execute(
        "INSERT OR IGNORE INTO observation_retrieval_anchors (observation_id, anchor_id)
         VALUES (?1, ?2)",
        params![observation_id.as_str(), stored.anchor_id().as_str()],
    )
    .await
    .map_err(|error| storage("bind observation retrieval anchor", error))?;
    let mut rows = conn
        .query(
            "SELECT anchor_id FROM observation_retrieval_anchors WHERE observation_id = ?1",
            params![observation_id.as_str()],
        )
        .await
        .map_err(|error| storage("verify observation retrieval anchor", error))?;
    let bound_anchor_id = rows
        .next()
        .await
        .map_err(|error| storage("verify observation retrieval anchor", error))?
        .ok_or_else(|| {
            storage_message(
                "verify observation retrieval anchor",
                "observation anchor binding disappeared",
            )
        })?
        .get::<String>(0)
        .map_err(|error| storage("verify observation retrieval anchor", error))?;
    drop(rows);
    if bound_anchor_id != stored.anchor_id().as_str() {
        return Err(ObservationStoreError::RetrievalAnchorObservationMismatch);
    }

    for alias in stored.aliases() {
        let alias_kind = encode_json_string(&alias.kind(), "encode retrieval anchor alias kind")?;
        let locator_digest = encode_json_string(
            alias.locator_digest(),
            "encode retrieval anchor alias digest",
        )?;
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO retrieval_anchor_aliases (
                    owner_json, alias_kind, locator_digest, anchor_id
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    stored_owner_json.as_str(),
                    alias_kind.as_str(),
                    locator_digest.as_str(),
                    stored.anchor_id().as_str(),
                ],
            )
            .await
            .map_err(|error| storage("insert retrieval anchor alias", error))?;
        if inserted == 0 {
            let mut rows = conn
                .query(
                    "SELECT anchor_id FROM retrieval_anchor_aliases
                     WHERE owner_json = ?1 AND alias_kind = ?2 AND locator_digest = ?3",
                    params![
                        stored_owner_json.as_str(),
                        alias_kind.as_str(),
                        locator_digest.as_str(),
                    ],
                )
                .await
                .map_err(|error| storage("read retrieval anchor alias", error))?;
            let existing_anchor_id = rows
                .next()
                .await
                .map_err(|error| storage("read retrieval anchor alias", error))?
                .ok_or_else(|| {
                    storage_message(
                        "read retrieval anchor alias",
                        "alias conflict row disappeared",
                    )
                })?
                .get::<String>(0)
                .map_err(|error| storage("read retrieval anchor alias", error))?;
            if existing_anchor_id != stored.anchor_id().as_str() {
                return Err(ObservationStoreError::RetrievalAnchorAliasCollision {
                    alias: Box::new(alias.clone()),
                    existing_anchor_id: Box::new(
                        tracedecay_domain::RetrievalAnchorId::new(existing_anchor_id)
                            .map_err(ObservationStoreError::RetrievalAnchorContract)?,
                    ),
                    candidate_anchor_id: Box::new(stored.anchor_id().clone()),
                });
            }
        }
    }
    Ok((stored, projection_generation))
}

fn decode_sequence(value: i64, operation: &'static str) -> ObservationStoreResult<u64> {
    u64::try_from(value).map_err(|_| storage_message(operation, "negative observation sequence"))
}

async fn read_observation_row(
    conn: &Connection,
    sql: &'static str,
    value: &str,
    operation: &'static str,
) -> ObservationStoreResult<Option<ObservationCommitReceipt>> {
    let mut rows = conn
        .query(sql, params![value])
        .await
        .map_err(|error| storage(operation, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(operation, error))?
    else {
        return Ok(None);
    };
    let sequence = decode_sequence(
        row.get::<i64>(0)
            .map_err(|error| storage(operation, error))?,
        operation,
    )?;
    let observation_json = row
        .get::<String>(1)
        .map_err(|error| storage(operation, error))?;
    let cursor_json = row
        .get::<String>(2)
        .map_err(|error| storage(operation, error))?;
    let anchor_json = row
        .get::<String>(3)
        .map_err(|error| storage(operation, error))?;
    let projection_generation = row
        .get::<String>(4)
        .map_err(|error| storage(operation, error))?;
    Ok(Some(ObservationCommitReceipt::new(
        sequence,
        decode(&observation_json, operation)?,
        decode(&cursor_json, operation)?,
        decode(&anchor_json, operation)?,
        ProjectionGenerationId::new(projection_generation)
            .map_err(ObservationStoreError::RetrievalAnchorContract)?,
    )?))
}

async fn read_by_observation_id(
    conn: &Connection,
    observation_id: &CanonicalObservationIdV1,
) -> ObservationStoreResult<Option<ObservationCommitReceipt>> {
    read_observation_row(
        conn,
        "SELECT observation.sequence, observation.observation_json,
                observation.committed_cursor_json, anchor.anchor_json,
                anchor.projection_generation
         FROM observations AS observation
         JOIN observation_retrieval_anchors AS binding
           ON binding.observation_id = observation.observation_id
         JOIN retrieval_anchors AS anchor ON anchor.anchor_id = binding.anchor_id
         WHERE observation.observation_id = ?1",
        observation_id.as_str(),
        "read observation",
    )
    .await
}

async fn read_cursor(
    conn: &Connection,
    source_json: &str,
    scope_json: &str,
) -> ObservationStoreResult<Option<ObservationSourceCursorV1>> {
    let mut rows = conn
        .query(
            "SELECT cursor_json FROM source_cursors
             WHERE source_json = ?1 AND scope_json = ?2",
            params![source_json, scope_json],
        )
        .await
        .map_err(|error| storage("read observation source cursor", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read observation source cursor", error))?
    else {
        return Ok(None);
    };
    let cursor_json = row
        .get::<String>(0)
        .map_err(|error| storage("read observation source cursor", error))?;
    decode(&cursor_json, "decode observation source cursor").map(Some)
}

async fn cursor_advance_receipt_matches(
    conn: &Connection,
    source_json: &str,
    scope_json: &str,
    advance: &ObservationCursorAdvance,
) -> ObservationStoreResult<bool> {
    let coverage_json = encode(&advance.coverage(), "encode observation coverage")?;
    let mut rows = conn
        .query(
            "SELECT reason, receipt_id FROM source_cursor_advances
             WHERE source_json = ?1 AND scope_json = ?2 AND coverage_json = ?3",
            params![source_json, scope_json, coverage_json],
        )
        .await
        .map_err(|error| storage("read source cursor advance receipt", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read source cursor advance receipt", error))?
    else {
        return Ok(false);
    };
    let reason = row
        .get::<String>(0)
        .map_err(|error| storage("read source cursor advance receipt", error))?;
    let receipt_id = row
        .get::<Option<String>>(1)
        .map_err(|error| storage("read source cursor advance receipt", error))?;
    Ok(reason == advance.reason().as_str()
        && receipt_id.as_deref()
            == advance
                .sanitization_receipt()
                .map(|receipt| receipt.receipt().receipt_id().as_str()))
}

async fn persist_sanitization_receipt(
    conn: &Connection,
    receipt: &SanitizationReceiptV1,
) -> ObservationStoreResult<()> {
    let receipt_json = encode(receipt, "encode sanitization receipt")?;
    let receipt_id = receipt.receipt().receipt_id().as_str();
    let sanitizer_version = receipt.receipt().sanitizer_version().as_str();
    let payload_digest = receipt
        .payload()
        .map_or("", |payload| payload.digest().as_str());
    conn.execute(
        "INSERT INTO sanitization_receipts
            (receipt_id, sanitizer_version, payload_digest, receipt_json)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(receipt_id) DO NOTHING",
        params![
            receipt_id,
            sanitizer_version,
            payload_digest,
            receipt_json.as_str()
        ],
    )
    .await
    .map_err(|error| storage("insert sanitization receipt", error))?;
    let mut rows = conn
        .query(
            "SELECT receipt_json FROM sanitization_receipts WHERE receipt_id = ?1",
            params![receipt_id],
        )
        .await
        .map_err(|error| storage("verify sanitization receipt", error))?;
    let stored = rows
        .next()
        .await
        .map_err(|error| storage("verify sanitization receipt", error))?
        .ok_or_else(|| {
            storage_message("verify sanitization receipt", "receipt insert disappeared")
        })?
        .get::<String>(0)
        .map_err(|error| storage("verify sanitization receipt", error))?;
    if stored != receipt_json {
        return Err(ObservationStoreError::SanitizationReceiptCollision);
    }
    Ok(())
}

async fn write_cursor(
    conn: &Connection,
    source_json: &str,
    scope_json: &str,
    cursor_json: &str,
) -> ObservationStoreResult<()> {
    conn.execute(
        "INSERT INTO source_cursors (source_json, scope_json, cursor_json)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(source_json, scope_json) DO UPDATE SET
            cursor_json = excluded.cursor_json",
        params![source_json, scope_json, cursor_json],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("advance observation source cursor", error))
}

async fn apply_cursor_advance(
    conn: &Connection,
    advance: &ObservationCursorAdvance,
) -> ObservationStoreResult<CursorAdvanceOutcome> {
    let source_json = encode(advance.next_cursor().source(), "encode observation source")?;
    let scope_json = encode(advance.next_cursor().scope(), "encode observation scope")?;
    let actual_cursor = read_cursor(conn, &source_json, &scope_json).await?;
    if actual_cursor.as_ref() == Some(advance.next_cursor()) {
        return if cursor_advance_receipt_matches(conn, &source_json, &scope_json, advance).await? {
            Ok(CursorAdvanceOutcome::ExactDuplicate)
        } else {
            Err(ObservationStoreError::CursorAdvanceCollision)
        };
    }
    if actual_cursor.as_ref() != advance.expected_cursor() {
        return Err(ObservationStoreError::CursorConflict {
            expected: Box::new(advance.expected_cursor().cloned()),
            actual: Box::new(actual_cursor),
        });
    }
    if let Some(receipt) = advance.sanitization_receipt() {
        persist_sanitization_receipt(conn, receipt).await?;
    }
    let receipt_id = advance
        .sanitization_receipt()
        .map(|receipt| receipt.receipt().receipt_id().as_str());
    let coverage_json = encode(&advance.coverage(), "encode observation coverage")?;
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO source_cursor_advances(
                source_json, scope_json, coverage_json, reason, receipt_id
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                source_json.as_str(),
                scope_json.as_str(),
                coverage_json.as_str(),
                advance.reason().as_str(),
                receipt_id,
            ],
        )
        .await
        .map_err(|error| storage("persist cursor coverage receipt", error))?;
    if inserted == 0
        && !cursor_advance_receipt_matches(conn, &source_json, &scope_json, advance).await?
    {
        return Err(ObservationStoreError::CursorAdvanceCollision);
    }
    let cursor_json = encode(advance.next_cursor(), "encode committed observation cursor")?;
    write_cursor(conn, &source_json, &scope_json, &cursor_json).await?;
    Ok(CursorAdvanceOutcome::Committed)
}

async fn read_projection_status(
    conn: &Connection,
    observation_id: &CanonicalObservationIdV1,
) -> ObservationStoreResult<ObservationProjectionStatus> {
    let mut rows = conn
        .query(
            "SELECT EXISTS(
                SELECT 1 FROM projection_queue WHERE observation_id = ?1
             )",
            params![observation_id.as_str()],
        )
        .await
        .map_err(|error| storage("read observation projection status", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("read observation projection status", error))?
        .ok_or_else(|| {
            storage_message(
                "read observation projection status",
                "projection status query returned no row",
            )
        })?;
    match row
        .get::<i64>(0)
        .map_err(|error| storage("read observation projection status", error))?
    {
        0 => Ok(ObservationProjectionStatus::NotQueued),
        _ => Ok(ObservationProjectionStatus::Queued),
    }
}

impl GlobalDb {
    pub(crate) async fn persist_observation_result(
        &self,
        write: AnchoredObservationWrite,
    ) -> ObservationStoreResult<ObservationPersistOutcome> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage("begin observation transaction", error))?;
        let candidate = write.observation();
        if let Some(existing) =
            read_by_observation_id(&transaction, candidate.observation_id()).await?
        {
            let existing_observation = existing.observation();
            let outcome = classify_observation_collision(existing_observation, candidate);
            return match outcome {
                ObservationCollisionOutcomeV1::ExactDuplicate
                    if existing_observation.identity() == candidate.identity()
                        && existing_observation.receipt() == candidate.receipt() =>
                {
                    Ok(ObservationPersistOutcome::ExactDuplicate(existing))
                }
                ObservationCollisionOutcomeV1::ExactDuplicate
                    if existing_observation.identity() == candidate.identity() =>
                {
                    Err(ObservationStoreError::SanitizationReceiptCollision)
                }
                ObservationCollisionOutcomeV1::ExactDuplicate => {
                    let identity = candidate.identity();
                    let mut advance =
                        ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
                            identity.source().clone(),
                            identity.scope().clone(),
                            identity.generation(),
                            identity.ordering_domain(),
                            write.expected_cursor().cloned(),
                            identity.position(),
                            ObservationCoverageReason::DuplicateObservation,
                            candidate.receipt().clone(),
                        )?;
                    match (
                        write.next_cursor().file_identity(),
                        write.next_cursor().resume_fingerprint(),
                    ) {
                        (Some(file_identity), Some(resume_fingerprint)) => {
                            advance =
                                advance.with_resume_checkpoint(file_identity, resume_fingerprint);
                        }
                        (None, None) => {}
                        _ => {
                            return Err(storage_message(
                                "cover duplicate observation",
                                "cursor resume checkpoint is incomplete",
                            ));
                        }
                    }
                    let advance_outcome = apply_cursor_advance(&transaction, &advance).await?;
                    if advance_outcome == CursorAdvanceOutcome::Committed {
                        transaction.commit().await.map_err(|error| {
                            storage("commit duplicate observation coverage", error)
                        })?;
                    }
                    Ok(ObservationPersistOutcome::CoveredDuplicate(
                        ObservationCommitReceipt::new(
                            existing.sequence(),
                            candidate.clone(),
                            write.next_cursor().clone(),
                            existing.retrieval_anchor().clone(),
                            existing.projection_generation().clone(),
                        )?,
                    ))
                }
                ObservationCollisionOutcomeV1::IdentityCollision => {
                    Err(ObservationStoreError::ObservationCollision {
                        observation_id: Box::new(candidate.observation_id().clone()),
                        existing_digest: Box::new(
                            existing_observation.payload_reference().digest().clone(),
                        ),
                        candidate_digest: Box::new(candidate.payload_reference().digest().clone()),
                        outcome,
                    })
                }
                ObservationCollisionOutcomeV1::Distinct => Err(storage_message(
                    "classify observation collision",
                    "matching observation identifier classified as distinct",
                )),
            };
        }
        let source_json = encode(candidate.source(), "encode observation source")?;
        let scope_json = encode(candidate.scope(), "encode observation scope")?;
        let actual_cursor = read_cursor(&transaction, &source_json, &scope_json).await?;
        if actual_cursor.as_ref() != write.expected_cursor() {
            return Err(ObservationStoreError::CursorConflict {
                expected: Box::new(write.expected_cursor().cloned()),
                actual: Box::new(actual_cursor),
            });
        }

        let observation_json = encode(candidate, "encode observation")?;
        let cursor_json = encode(write.next_cursor(), "encode committed observation cursor")?;
        let receipt = candidate.receipt();
        let receipt_id = receipt.receipt().receipt_id().as_str();
        let payload_digest = candidate.payload_reference().digest().as_str();
        persist_sanitization_receipt(&transaction, receipt).await?;

        transaction
            .execute(
                "INSERT INTO observations
                        (observation_id, payload_digest, receipt_id,
                         observation_json, committed_cursor_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    candidate.observation_id().as_str(),
                    payload_digest,
                    receipt_id,
                    observation_json.as_str(),
                    cursor_json.as_str()
                ],
            )
            .await
            .map_err(|error| storage("insert immutable observation", error))?;
        let sequence = decode_sequence(
            transaction.last_insert_rowid(),
            "insert immutable observation",
        )?;
        let (retrieval_anchor, projection_generation) = persist_observation_retrieval_anchor(
            &transaction,
            candidate.observation_id(),
            write.retrieval_anchor(),
        )
        .await?;
        let committed = ObservationCommitReceipt::new(
            sequence,
            candidate.clone(),
            write.next_cursor().clone(),
            retrieval_anchor,
            projection_generation,
        )?;

        write_cursor(&transaction, &source_json, &scope_json, &cursor_json).await?;
        transaction
            .execute(
                "INSERT INTO projection_queue (observation_id, observation_sequence)
                 VALUES (?1, ?2)",
                params![
                    candidate.observation_id().as_str(),
                    i64::try_from(committed.sequence()).map_err(|_| storage_message(
                        "enqueue observation projection",
                        "observation sequence exceeds SQLite integer range"
                    ))?
                ],
            )
            .await
            .map_err(|error| storage("enqueue observation projection", error))?;

        #[cfg(tracedecay_observation_fault_harness)]
        wait_at_observation_persist_test_barrier(
            ObservationPersistTestBarrierStage::PostWritePreCommit,
            candidate.source().session_id().as_str(),
        )
        .await?;

        transaction
            .commit()
            .await
            .map_err(|error| storage("commit observation transaction", error))?;
        #[cfg(tracedecay_observation_fault_harness)]
        wait_at_observation_persist_test_barrier(
            ObservationPersistTestBarrierStage::PostCommitPreAck,
            candidate.source().session_id().as_str(),
        )
        .await?;
        Ok(ObservationPersistOutcome::Committed(committed))
    }

    pub(crate) async fn get_observation_source_cursor_result(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> ObservationStoreResult<Option<ObservationSourceCursorV1>> {
        let source_json = encode(source, "encode observation source")?;
        let scope_json = encode(scope, "encode observation scope")?;
        read_cursor(&self.conn, &source_json, &scope_json).await
    }

    pub(crate) async fn advance_observation_source_cursor_result(
        &self,
        advance: ObservationCursorAdvance,
    ) -> ObservationStoreResult<CursorAdvanceOutcome> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage("begin observation cursor transaction", error))?;
        let outcome = apply_cursor_advance(&transaction, &advance).await?;
        if outcome == CursorAdvanceOutcome::Committed {
            transaction
                .commit()
                .await
                .map_err(|error| storage("commit observation cursor transaction", error))?;
        }
        Ok(outcome)
    }

    pub(crate) async fn get_observation_result(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ObservationStoreResult<Option<StoredObservation>> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage("begin observation read snapshot", error))?;
        let Some(receipt) = read_by_observation_id(&snapshot, observation_id).await? else {
            return Ok(None);
        };
        let projection_status = read_projection_status(&snapshot, observation_id).await?;
        Ok(Some(StoredObservation::from_commit_receipt(
            receipt,
            projection_status,
        )))
    }

    pub(crate) async fn replay_observations_result(
        &self,
        request: ObservationReplayRequest,
    ) -> ObservationStoreResult<Vec<StoredObservation>> {
        let after_sequence = i64::try_from(request.after_sequence()).map_err(|_| {
            storage_message(
                "replay observations",
                "observation replay sequence exceeds SQLite integer range",
            )
        })?;
        let limit = i64::try_from(request.limit()).map_err(|_| {
            storage_message(
                "replay observations",
                "observation replay limit exceeds SQLite integer range",
            )
        })?;
        let mut rows = self
            .conn
            .query(
                "SELECT observations.sequence, observations.observation_json,
                        observations.committed_cursor_json, anchor.anchor_json,
                        anchor.projection_generation,
                        EXISTS(
                            SELECT 1 FROM projection_queue
                            WHERE projection_queue.observation_id = observations.observation_id
                        )
                 FROM observations
                 JOIN observation_retrieval_anchors AS binding
                   ON binding.observation_id = observations.observation_id
                 JOIN retrieval_anchors AS anchor ON anchor.anchor_id = binding.anchor_id
                 WHERE sequence > ?1 ORDER BY sequence ASC LIMIT ?2",
                params![after_sequence, limit],
            )
            .await
            .map_err(|error| storage("replay observations", error))?;
        let mut observations = Vec::with_capacity(request.limit());
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("replay observations", error))?
        {
            let sequence = decode_sequence(
                row.get::<i64>(0)
                    .map_err(|error| storage("replay observations", error))?,
                "replay observations",
            )?;
            let observation_json = row
                .get::<String>(1)
                .map_err(|error| storage("replay observations", error))?;
            let committed_cursor_json = row
                .get::<String>(2)
                .map_err(|error| storage("replay observations", error))?;
            let anchor_json = row
                .get::<String>(3)
                .map_err(|error| storage("replay observations", error))?;
            let projection_generation = row
                .get::<String>(4)
                .map_err(|error| storage("replay observations", error))?;
            let projection_status = match row
                .get::<i64>(5)
                .map_err(|error| storage("replay observations", error))?
            {
                0 => ObservationProjectionStatus::NotQueued,
                _ => ObservationProjectionStatus::Queued,
            };
            observations.push(StoredObservation::from_commit_receipt(
                ObservationCommitReceipt::new(
                    sequence,
                    decode(&observation_json, "decode replayed observation")?,
                    decode(&committed_cursor_json, "decode replayed observation cursor")?,
                    decode(&anchor_json, "decode replayed observation anchor")?,
                    ProjectionGenerationId::new(projection_generation)
                        .map_err(ObservationStoreError::RetrievalAnchorContract)?,
                )?,
                projection_status,
            ));
        }
        Ok(observations)
    }
}
