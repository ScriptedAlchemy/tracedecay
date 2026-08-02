use std::collections::BTreeSet;

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::super::{global_db_operation_error, global_db_operation_message};

const OBSERVATION_SCHEMA_MIGRATION: &str = "observations-v2-canonical-autoincrement";

pub const OBSERVATION_ANCHOR_SCHEMA_MIGRATION: &str = "observation-retrieval-anchors-v2";

pub(super) const OBSERVATION_SCHEMA_OPERATION: &str = "migrate observation authority schema";

async fn observation_table_exists(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<bool> {
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

async fn observation_columns(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<BTreeSet<String>> {
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

pub(super) async fn migration_recorded(
    conn: &impl QueryExecutor,
    migration: &str,
) -> tracedecay_runtime_core::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM global_schema_migrations WHERE migration = ?1",
            params![migration],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

async fn migrate_observation_schema(
    conn: &impl Executor,
    table_preexisted: bool,
) -> tracedecay_runtime_core::errors::Result<()> {
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
    super::super::schema_contract::validate_observation_migration_source(
        conn,
        columns.contains("idempotency_key"),
    )
    .await?;
    let recorded = migration_recorded(conn, OBSERVATION_SCHEMA_MIGRATION).await?;
    if !table_preexisted || (recorded && columns == required) {
        conn.execute(
            "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
            params![OBSERVATION_SCHEMA_MIGRATION],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        return Ok(());
    }

    // This full-table rewrite is exactly the operation that interrupted a
    // real dogfood upgrade on a 15GB `sessions.db` and, before the
    // `tracedecay_runtime_core::durability` model existed, failed the whole strict
    // post-update because of it (see `crate::doctor::heal`'s module doc).
    // `observations` must stay classified `Recoverable` -- re-derivable by
    // re-running sanitization/projection over recoverable transcript
    // sources -- for that failure to stay non-blocking; assert it here so a
    // future reclassification cannot silently drift from the code it
    // documents.
    debug_assert!(
        matches!(
            tracedecay_runtime_core::durability::session_authority_table_class("observations"),
            tracedecay_runtime_core::durability::StoreDurabilityClass::Recoverable
        ),
        "the observations full-table rewrite must only ever run against a table \
         proven Recoverable by the upgrade durability model"
    );
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

async fn migrate_source_cursor_advances_schema(
    conn: &impl Executor,
) -> tracedecay_runtime_core::errors::Result<()> {
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
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

pub async fn ensure_observation_schema(
    conn: &(impl Executor + Sync),
) -> tracedecay_runtime_core::errors::Result<()> {
    let table_preexisted = observation_table_exists(conn).await?;
    tracedecay_runtime_core::db::retrieval_anchor_schema::install_retrieval_anchor_schema(
        conn,
        OBSERVATION_SCHEMA_OPERATION,
    )
    .await?;
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
        CREATE TABLE IF NOT EXISTS observation_retrieval_anchors (
            observation_id TEXT PRIMARY KEY,
            anchor_id TEXT NOT NULL UNIQUE,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(anchor_id) REFERENCES retrieval_anchors(anchor_id)
        );
        CREATE TABLE IF NOT EXISTS observation_repository_provenance (
            observation_id TEXT PRIMARY KEY,
            availability_json TEXT NOT NULL CHECK(json_valid(availability_json)),
            capture_json TEXT CHECK(capture_json IS NULL OR json_valid(capture_json)),
            retrieval_anchor_id TEXT UNIQUE,
            owner_json TEXT CHECK(owner_json IS NULL OR json_valid(owner_json)),
            CHECK((capture_json IS NULL) = (retrieval_anchor_id IS NULL)),
            CHECK((owner_json IS NULL) = (retrieval_anchor_id IS NULL)),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id),
            FOREIGN KEY(retrieval_anchor_id, owner_json)
                REFERENCES retrieval_anchors(anchor_id, owner_json)
        );
        CREATE TRIGGER IF NOT EXISTS observation_retrieval_anchors_immutable_update
        BEFORE UPDATE ON observation_retrieval_anchors BEGIN
            SELECT RAISE(ABORT, 'observation retrieval anchor bindings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS observation_retrieval_anchors_immutable_delete
        BEFORE DELETE ON observation_retrieval_anchors BEGIN
            SELECT RAISE(ABORT, 'observation retrieval anchor bindings are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS observation_repository_provenance_immutable_update
        BEFORE UPDATE ON observation_repository_provenance BEGIN
            SELECT RAISE(ABORT, 'observation repository provenance is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS observation_repository_provenance_immutable_delete
        BEFORE DELETE ON observation_repository_provenance BEGIN
            SELECT RAISE(ABORT, 'observation repository provenance is immutable');
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
        CREATE TABLE IF NOT EXISTS observation_backfill_watermarks (
            migration TEXT NOT NULL PRIMARY KEY,
            backfilled_through INTEGER NOT NULL CHECK(backfilled_through >= 0)
        );
        CREATE TABLE IF NOT EXISTS projection_queue (
            observation_id TEXT PRIMARY KEY,
            observation_sequence INTEGER NOT NULL UNIQUE,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );",
    )
    .await
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    super::super::ensure_table_columns(
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
    // The retrieval-anchor and repository-provenance backfills deliberately do
    // NOT run here: this function executes inside the schema-upgrade
    // mega-transaction, where a cancelled open (the warmup deadline interrupts
    // in-flight statements) would roll every row of a large-store backfill back
    // and re-arm the same full scan on the next open. `super::backfill` runs
    // both after that transaction commits and pages their progress durably.
    Ok(())
}
