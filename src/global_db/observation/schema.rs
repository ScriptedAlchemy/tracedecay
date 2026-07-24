use std::collections::BTreeSet;

use tracedecay_domain::{ProjectionGenerationId, SanitizationReceiptV1, UtcMicros};
use tracedecay_store::{
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use crate::db::engine::{Executor, QueryExecutor, params};

use super::super::{global_db_operation_error, global_db_operation_message};
use super::persist::persist_observation_retrieval_anchor;
use super::provenance_backfill::backfill_observation_repository_provenance;

const OBSERVATION_SCHEMA_MIGRATION: &str = "observations-v2-canonical-autoincrement";

const OBSERVATION_ANCHOR_SCHEMA_MIGRATION: &str = "observation-retrieval-anchors-v2";

const LEGACY_OBSERVATION_PROJECTION_GENERATION: &str = "projection.legacy-observation-import.v1";

pub(super) const OBSERVATION_SCHEMA_OPERATION: &str = "migrate observation authority schema";

async fn observation_table_exists(conn: &impl QueryExecutor) -> crate::errors::Result<bool> {
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

async fn observation_columns(conn: &impl QueryExecutor) -> crate::errors::Result<BTreeSet<String>> {
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

async fn migration_recorded(
    conn: &impl QueryExecutor,
    migration: &str,
) -> crate::errors::Result<bool> {
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
    conn: &(impl Executor + QueryExecutor),
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
    // `crate::migrate::durability` model existed, failed the whole strict
    // post-update because of it (see `crate::doctor::heal`'s module doc).
    // `observations` must stay classified `Recoverable` -- re-derivable by
    // re-running sanitization/projection over recoverable transcript
    // sources -- for that failure to stay non-blocking; assert it here so a
    // future reclassification cannot silently drift from the code it
    // documents.
    debug_assert!(
        matches!(
            crate::migrate::durability::session_authority_table_class("observations"),
            crate::migrate::durability::StoreDurabilityClass::Recoverable
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
    conn: &(impl Executor + QueryExecutor),
) -> crate::errors::Result<()> {
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

pub(in crate::global_db) async fn backfill_observation_retrieval_anchors(
    conn: &(impl Executor + QueryExecutor),
) -> crate::errors::Result<()> {
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
        let (_, _, alias_collisions) =
            persist_observation_retrieval_anchor(conn, observation.observation_id(), &anchor)
                .await
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        for collision in alias_collisions {
            tracing::warn!(
                existing_anchor_id = collision.existing_anchor_id.as_str(),
                candidate_anchor_id = collision.candidate_anchor_id.as_str(),
                "anchor backfill preserved alias binding; candidate stays reachable by id only"
            );
        }
    }
    conn.execute(
        "INSERT OR REPLACE INTO global_schema_migrations(migration) VALUES (?1)",
        params![OBSERVATION_ANCHOR_SCHEMA_MIGRATION],
    )
    .await
    .map(|_| ())
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

pub(in crate::global_db) async fn ensure_observation_schema(
    conn: &(impl Executor + QueryExecutor + Sync),
) -> crate::errors::Result<()> {
    let table_preexisted = observation_table_exists(conn).await?;
    crate::db::retrieval_anchor_schema::install_retrieval_anchor_schema(
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
    backfill_observation_retrieval_anchors(conn).await?;
    backfill_observation_repository_provenance(conn).await
}
