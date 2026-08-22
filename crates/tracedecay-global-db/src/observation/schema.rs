use std::collections::BTreeSet;

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::super::global_db_operation_error;

/// Typed reset authority for the observation store. No observation shape has
/// ever shipped in a published release (`observations` is absent from both the
/// v0.0.66 package and `origin/master`), so any schema drift here is a
/// branch-local development artifact and refuses admission with
/// [`ResetRequired`](tracedecay_runtime_core::errors::TraceDecayError::ResetRequired)
/// instead of migrating.
pub const OBSERVATION_AUTHORITY: &str = "observations";

/// Marker proving `observations` was created with the canonical AUTOINCREMENT
/// DDL below; the authority schema contract's AUTOINCREMENT invariant consumes
/// it. It is recorded at creation, never by rewriting an existing table.
pub(super) const OBSERVATION_SCHEMA_MIGRATION: &str = "observations-v2-canonical-autoincrement";

/// Canonical `observations` column set. Shared by the admission refusal below
/// and the scoped operator reset in [`super::reset`] so the two can never
/// disagree about what counts as a refused shape.
pub(super) const OBSERVATION_CANONICAL_COLUMNS: &[&str] = &[
    "sequence",
    "observation_id",
    "payload_digest",
    "receipt_id",
    "observation_json",
    "committed_cursor_json",
];

/// Canonical provider-neutral `source_cursor_advances` column set, shared with
/// [`super::reset`] like [`OBSERVATION_CANONICAL_COLUMNS`].
pub(super) const SOURCE_CURSOR_ADVANCES_CANONICAL_COLUMNS: &[&str] = &[
    "source_json",
    "scope_json",
    "coverage_json",
    "reason",
    "receipt_id",
];

/// Canonical `observation_admission_refusals` column set: the immutable
/// refusal signature plus the production admission-work telemetry counters
/// every admission pass accumulates onto its marker row.
const ADMISSION_REFUSALS_CANONICAL_COLUMNS: &[&str] = &[
    "observation_id",
    "refused_payload_digest",
    "retained_payload_digest",
    "refused_at",
    "stored_rows_decoded",
    "identity_derivations",
    "payload_digests",
    "runtime_commands",
];

pub(super) const OBSERVATION_SCHEMA_OPERATION: &str = "ensure observation authority schema";

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

async fn table_columns(
    conn: &impl QueryExecutor,
    table: &str,
) -> tracedecay_runtime_core::errors::Result<BTreeSet<String>> {
    let mut rows = conn
        .query("SELECT name FROM pragma_table_xinfo(?1)", params![table])
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

fn canonical_column_set(columns: &[&str]) -> BTreeSet<String> {
    columns.iter().map(|column| (*column).to_string()).collect()
}

/// Refuses a store whose `observations` or `source_cursor_advances` table
/// carries anything but the canonical shape (plus, for `observations`, its
/// creation marker). The alternative shapes — the `idempotency_key` column
/// era, unmarked non-AUTOINCREMENT tables, and the byte-offset
/// `source_cursor_advances` predecessor — were branch-local and never shipped
/// in a published release, so there is no sanctioned migration: the store
/// surfaces a typed `ResetRequired` naming this authority instead of
/// rewriting data in place. Runs at schema installation for fresh stores and
/// at the attach boundary for existing ones.
async fn require_admitted_observation_shape(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    if observation_table_exists(conn).await? {
        let columns = table_columns(conn, "observations").await?;
        let recorded = migration_recorded(conn, OBSERVATION_SCHEMA_MIGRATION).await?;
        if columns != canonical_column_set(OBSERVATION_CANONICAL_COLUMNS) || !recorded {
            return Err(
                tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                    OBSERVATION_AUTHORITY,
                    "observations carries a pre-release branch-local shape that no \
                     published binary ever wrote; there is no sanctioned migration, \
                     reset the observation authority to recreate it at the canonical \
                     schema",
                ),
            );
        }
    }
    let advances = table_columns(conn, "source_cursor_advances").await?;
    if !advances.is_empty()
        && advances != canonical_column_set(SOURCE_CURSOR_ADVANCES_CANONICAL_COLUMNS)
    {
        return Err(
            tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                OBSERVATION_AUTHORITY,
                "source_cursor_advances carries a pre-release branch-local shape \
                 that no published binary ever wrote; there is no sanctioned \
                 migration, reset the observation authority to recreate it at the \
                 canonical schema",
            ),
        );
    }
    let refusals = table_columns(conn, "observation_admission_refusals").await?;
    if !refusals.is_empty()
        && refusals != canonical_column_set(ADMISSION_REFUSALS_CANONICAL_COLUMNS)
    {
        return Err(
            tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                OBSERVATION_AUTHORITY,
                "observation_admission_refusals carries a pre-release branch-local \
                 shape that no published binary ever wrote; there is no sanctioned \
                 migration, reset the observation authority to recreate it at the \
                 canonical schema",
            ),
        );
    }
    Ok(())
}

/// Canonical observation-authority DDL. Shared with the scoped operator reset
/// in [`super::reset`], which recreates these tables after dropping a refused
/// authority, so the installer and the reset can never produce different
/// shapes.
pub(super) const OBSERVATION_AUTHORITY_SCHEMA_SQL: &str =
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
        CREATE TABLE IF NOT EXISTS remote_writer_fences (
            authority_key TEXT PRIMARY KEY,
            writer_fence_json TEXT NOT NULL CHECK(json_valid(writer_fence_json)),
            frontier_sequence INTEGER NOT NULL CHECK(frontier_sequence >= 0),
            updated_at INTEGER NOT NULL
        ) STRICT;
        CREATE TABLE IF NOT EXISTS remote_observation_events (
            event_id TEXT PRIMARY KEY,
            frame_digest TEXT NOT NULL,
            enrollment_id TEXT NOT NULL,
            enrollment_revision INTEGER NOT NULL CHECK(enrollment_revision > 0),
            node_id TEXT NOT NULL,
            policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
            capture_sequence INTEGER NOT NULL CHECK(capture_sequence > 0),
            previous_event_id TEXT REFERENCES remote_observation_events(event_id),
            observation_id TEXT NOT NULL UNIQUE REFERENCES observations(observation_id),
            writer_fence_json TEXT NOT NULL CHECK(json_valid(writer_fence_json)),
            captured_at INTEGER NOT NULL,
            idempotency_key TEXT NOT NULL UNIQUE,
            command_digest TEXT NOT NULL,
            UNIQUE(enrollment_id, node_id, capture_sequence)
        ) STRICT;
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
        CREATE TABLE IF NOT EXISTS observation_admission_refusals (
            observation_id TEXT NOT NULL,
            refused_payload_digest TEXT NOT NULL,
            retained_payload_digest TEXT NOT NULL,
            refused_at INTEGER NOT NULL,
            stored_rows_decoded INTEGER NOT NULL DEFAULT 0 CHECK(stored_rows_decoded >= 0),
            identity_derivations INTEGER NOT NULL DEFAULT 0 CHECK(identity_derivations >= 0),
            payload_digests INTEGER NOT NULL DEFAULT 0 CHECK(payload_digests >= 0),
            runtime_commands INTEGER NOT NULL DEFAULT 0 CHECK(runtime_commands >= 0),
            PRIMARY KEY(observation_id, refused_payload_digest),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );
        CREATE TRIGGER IF NOT EXISTS observation_admission_refusals_immutable_update
        BEFORE UPDATE ON observation_admission_refusals
        WHEN NEW.observation_id IS NOT OLD.observation_id
          OR NEW.refused_payload_digest IS NOT OLD.refused_payload_digest
          OR NEW.retained_payload_digest IS NOT OLD.retained_payload_digest
          OR NEW.refused_at IS NOT OLD.refused_at
        BEGIN
            SELECT RAISE(ABORT, 'observation admission refusals are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS observation_admission_refusals_immutable_delete
        BEFORE DELETE ON observation_admission_refusals BEGIN
            SELECT RAISE(ABORT, 'observation admission refusals are immutable');
        END;
        CREATE TABLE IF NOT EXISTS projection_queue (
            observation_id TEXT PRIMARY KEY,
            observation_sequence INTEGER NOT NULL UNIQUE,
            attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
            next_retry_at_micros INTEGER NOT NULL DEFAULT 0 CHECK(next_retry_at_micros >= 0),
            last_error TEXT,
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );";

pub async fn ensure_observation_schema(
    conn: &(impl Executor + Sync),
) -> tracedecay_runtime_core::errors::Result<()> {
    let table_preexisted = observation_table_exists(conn).await?;
    tracedecay_runtime_core::db::retrieval_anchor_schema::install_retrieval_anchor_schema(
        conn,
        OBSERVATION_SCHEMA_OPERATION,
    )
    .await?;
    conn.execute_batch(OBSERVATION_AUTHORITY_SCHEMA_SQL)
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    if !table_preexisted {
        conn.execute(
            "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
            params![OBSERVATION_SCHEMA_MIGRATION],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    }
    require_admitted_observation_shape(conn).await?;
    Ok(())
}
