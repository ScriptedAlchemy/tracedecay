use std::collections::BTreeSet;

use tracedecay_domain::integration::NativeHostIdentityV1;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::super::global_db_operation_error;

/// Typed reset authority for the observation store. No observation shape has
/// ever shipped in a published release (`observations` is absent from both the
/// v0.0.66 package and `origin/master`), so any schema drift here is a
/// branch-local development artifact and refuses admission with
/// [`ResetRequired`](tracedecay_domain::errors::TraceDecayError::ResetRequired)
/// instead of migrating.
pub const OBSERVATION_AUTHORITY: &str = "observations";

/// Marker proving `observations` was created with the canonical AUTOINCREMENT
/// DDL below; the authority schema contract's AUTOINCREMENT invariant consumes
/// it. It is recorded at creation, never by rewriting an existing table.
pub(super) const OBSERVATION_SCHEMA_MIGRATION: &str = "observations-v2-canonical-autoincrement";

/// Identity of the native-source scheme the committed observations were
/// written under. It is *content* identity, not table shape: since
/// `ff5c895ae` a Cline/Roo/Kilo task's `ui_messages.json` is its own native
/// source (`<task>:ui_messages`, its own generation, in-file ordinals) rather
/// than sharing the API history's combined `<task>` source. A store holding
/// rows written under the old scheme would re-admit every one of those native
/// UI events a second time under the new source key and silently double-count
/// their usage facts, so admission refuses it with
/// [`ResetRequired`](tracedecay_domain::errors::TraceDecayError::ResetRequired)
/// instead. The marker is recorded for any authority that holds no rows yet,
/// and for one whose retained rows and cursors name no Cline/Roo/Kilo source at
/// all — the scheme change touched only those hosts, so such a store cannot
/// double-count anything (see [`cline_like_sources_present`]). Only stores
/// carrying old-scheme rows from those hosts refuse.
pub const OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION: &str =
    "observations-native-source-scheme-v2-cline-ui-messages";

/// Source-key suffix of the native `ui_messages.json` source a Cline-like task
/// gained under [`OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION`] (the sessions
/// crate's `ui_messages_source_key`).
const CLINE_LIKE_UI_MESSAGES_SOURCE_SUFFIX: &str = ":ui_messages";

/// Rows examined by one native-source census query. The schema transaction's
/// long lease renews after each bounded query completes, so the census must
/// expose progress between pages instead of running one full-table JSON scan.
/// Observation payloads may approach the 1 MiB authority limit; keep their
/// page smaller than the cursor-only page.
pub(super) const OBSERVATION_SOURCE_CENSUS_PAGE_ROWS: i64 = 48;
pub(super) const SOURCE_CURSOR_CENSUS_PAGE_ROWS: i64 = 128;

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

pub(super) const OBSERVATION_SCHEMA_OPERATION: &str = "ensure observation authority schema";

async fn observation_table_exists(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<bool> {
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

/// Whether the authority already carries native-source identity written under
/// whatever scheme was current when it was committed: retained observations,
/// or the source cursors that decide what gets re-offered. Only these stores
/// can double-count when the scheme changes; an empty authority just enrolls.
/// Both tables are created by [`OBSERVATION_AUTHORITY_SCHEMA_SQL`], which runs
/// before every caller of this helper.
async fn observation_authority_populated(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 WHERE EXISTS(SELECT 1 FROM observations)
                        OR EXISTS(SELECT 1 FROM source_cursors)",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

/// Whether any retained observation or admission cursor names a Cline, Roo
/// Code, or Kilo source — the only hosts whose admission scheme changed under
/// [`OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION`]. A populated authority
/// without such rows was written by a scheme that never applied to it, so
/// enrolling it is exact rather than a migration of ambiguous data; one with
/// such rows carries no record of which scheme wrote them and must reset.
#[hotpath::measure(future = true, label = "global_db.observation.native_source_census")]
async fn cline_like_sources_present(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<bool> {
    let providers = [
        NativeHostIdentityV1::Cline.hook_key(),
        NativeHostIdentityV1::RooCode.hook_key(),
        NativeHostIdentityV1::Kilo.hook_key(),
    ];
    let ui_messages_pattern = format!("%{CLINE_LIKE_UI_MESSAGES_SOURCE_SUFFIX}");

    let mut cursor_rowid = i64::MIN;
    loop {
        let mut rows = conn
            .query(
                "SELECT rowid,
                        COALESCE(
                            json_extract(source_json, '$.provider') IN (?1, ?2, ?3)
                            OR json_extract(source_json, '$.source_key') LIKE ?4,
                            0
                        )
                 FROM source_cursors
                 WHERE rowid > ?5 ORDER BY rowid LIMIT ?6",
                params![
                    providers[0],
                    providers[1],
                    providers[2],
                    &ui_messages_pattern,
                    cursor_rowid,
                    SOURCE_CURSOR_CENSUS_PAGE_ROWS
                ],
            )
            .await
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
        {
            page_rows += 1;
            cursor_rowid = row
                .get::<i64>(0)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
            if row
                .get::<i64>(1)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
                != 0
            {
                return Ok(true);
            }
        }
        drop(rows);
        if page_rows < SOURCE_CURSOR_CENSUS_PAGE_ROWS {
            break;
        }
    }

    let mut observation_sequence = 0_i64;
    loop {
        let mut rows = conn
            .query(
                "SELECT sequence,
                        COALESCE(
                            json_extract(observation_json, '$.identity.source.provider')
                                IN (?1, ?2, ?3)
                            OR json_extract(observation_json, '$.identity.source.source_key')
                                LIKE ?4,
                            0
                        )
                 FROM observations
                 WHERE sequence > ?5 ORDER BY sequence LIMIT ?6",
                params![
                    providers[0],
                    providers[1],
                    providers[2],
                    &ui_messages_pattern,
                    observation_sequence,
                    OBSERVATION_SOURCE_CENSUS_PAGE_ROWS
                ],
            )
            .await
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
        {
            page_rows += 1;
            observation_sequence = row
                .get::<i64>(0)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
            if row
                .get::<i64>(1)
                .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
                != 0
            {
                return Ok(true);
            }
        }
        drop(rows);
        if page_rows < OBSERVATION_SOURCE_CENSUS_PAGE_ROWS {
            return Ok(false);
        }
    }
}

pub(super) async fn migration_recorded(
    conn: &impl QueryExecutor,
    migration: &str,
) -> tracedecay_domain::errors::Result<bool> {
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
) -> tracedecay_domain::errors::Result<BTreeSet<String>> {
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
) -> tracedecay_domain::errors::Result<()> {
    if observation_table_exists(conn).await? {
        let columns = table_columns(conn, "observations").await?;
        let recorded = migration_recorded(conn, OBSERVATION_SCHEMA_MIGRATION).await?;
        if columns != canonical_column_set(OBSERVATION_CANONICAL_COLUMNS) || !recorded {
            return Err(tracedecay_domain::errors::TraceDecayError::reset_required(
                OBSERVATION_AUTHORITY,
                "observations carries a pre-release branch-local shape that no \
                     published binary ever wrote; there is no sanctioned migration, \
                     reset the observation authority to recreate it at the canonical \
                     schema",
            ));
        }
    }
    let advances = table_columns(conn, "source_cursor_advances").await?;
    if !advances.is_empty()
        && advances != canonical_column_set(SOURCE_CURSOR_ADVANCES_CANONICAL_COLUMNS)
    {
        return Err(tracedecay_domain::errors::TraceDecayError::reset_required(
            OBSERVATION_AUTHORITY,
            "source_cursor_advances carries a pre-release branch-local shape \
                 that no published binary ever wrote; there is no sanctioned \
                 migration, reset the observation authority to recreate it at the \
                 canonical schema",
        ));
    }
    if observation_authority_populated(conn).await?
        && !migration_recorded(conn, OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION).await?
    {
        return Err(tracedecay_domain::errors::TraceDecayError::reset_required(
            OBSERVATION_AUTHORITY,
            "these observations were committed before a Cline/Roo/Kilo task's \
                 ui_messages.json became its own native source; re-offering that \
                 file under the <task>:ui_messages source would admit every one of \
                 its native UI events a second time and double-count their usage \
                 facts. There is no sanctioned migration, reset the observation \
                 authority so the derived usage and the admission cursors rebuild \
                 together from the preserved transcripts",
        ));
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
            PRIMARY KEY(observation_id, refused_payload_digest),
            FOREIGN KEY(observation_id) REFERENCES observations(observation_id)
        );
        CREATE TRIGGER IF NOT EXISTS observation_admission_refusals_immutable_update
        BEFORE UPDATE ON observation_admission_refusals BEGIN
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
) -> tracedecay_domain::errors::Result<()> {
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
    // The retained marker already certifies the native-source scheme. Reopening
    // an enrolled authority must not scan historical JSON again while holding
    // schema admission's writer transaction.
    // Enroll the native-source scheme wherever it cannot double-count: an
    // authority with no rows, or one whose rows and cursors never came from a
    // Cline-like host. Only a populated authority that does carry such rows is
    // left unmarked, and `require_admitted_observation_shape` refuses it.
    if !migration_recorded(conn, OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION).await?
        && (!observation_authority_populated(conn).await?
            || !cline_like_sources_present(conn).await?)
    {
        conn.execute(
            "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
            params![OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    }
    require_admitted_observation_shape(conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_runtime_core::db::engine::{IntoParams, Rows};

    use super::*;

    struct CountQueries<'a, T> {
        inner: &'a T,
        count: AtomicUsize,
    }

    impl<T: QueryExecutor> QueryExecutor for CountQueries<'_, T> {
        async fn query<P>(
            &self,
            sql: &str,
            params: P,
        ) -> tracedecay_runtime_core::db::engine::Result<Rows>
        where
            P: IntoParams,
        {
            self.count.fetch_add(1, Ordering::Relaxed);
            self.inner.query(sql, params).await
        }
    }

    impl<T: Executor> Executor for CountQueries<'_, T> {
        async fn execute<P>(
            &self,
            sql: &str,
            params: P,
        ) -> tracedecay_runtime_core::db::engine::Result<u64>
        where
            P: IntoParams,
        {
            self.inner.execute(sql, params).await
        }

        async fn execute_batch(
            &self,
            sql: &str,
        ) -> tracedecay_runtime_core::db::engine::Result<()> {
            self.inner.execute_batch(sql).await
        }
    }

    #[tokio::test]
    async fn enrolled_schema_admission_cost_does_not_grow_with_retained_observations() {
        let directory = tempfile::TempDir::new().unwrap();
        let fixture = crate::tests::harness::open_registered_test_fixture(
            &directory.path().join("sessions.db"),
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap();
        let transaction = fixture.database().begin_write_transaction().await.unwrap();
        let measured = CountQueries {
            inner: &transaction,
            count: AtomicUsize::new(0),
        };
        ensure_observation_schema(&measured).await.unwrap();
        let empty_queries = measured.count.swap(0, Ordering::Relaxed);

        for index in 0..=OBSERVATION_SOURCE_CENSUS_PAGE_ROWS * 2 {
            let (observation, cursor) =
                crate::schema_contract::invariants::test_fixture::authority_fixture(
                    index as u64,
                    &format!("enrolled-{index}"),
                );
            let receipt = observation.receipt();
            transaction
                .execute(
                    "INSERT INTO sanitization_receipts
                 (receipt_id, sanitizer_version, payload_digest, receipt_json)
                 VALUES (?1, ?2, ?3, ?4)",
                    params![
                        receipt.receipt().receipt_id().as_str(),
                        receipt.receipt().sanitizer_version().as_str(),
                        observation.payload_reference().digest().as_str(),
                        serde_json::to_string(receipt).unwrap()
                    ],
                )
                .await
                .unwrap();
            transaction.execute(
                "INSERT INTO observations
                 (observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![observation.observation_id().as_str(), observation.payload_reference().digest().as_str(),
                    receipt.receipt().receipt_id().as_str(), serde_json::to_string(&observation).unwrap(),
                    serde_json::to_string(&cursor).unwrap()],
            ).await.unwrap();
        }
        ensure_observation_schema(&measured).await.unwrap();
        let populated_queries = measured.count.swap(0, Ordering::Relaxed);

        transaction
            .execute(
                "DELETE FROM global_schema_migrations WHERE migration = ?1",
                params![OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION],
            )
            .await
            .unwrap();
        ensure_observation_schema(&measured).await.unwrap();
        let unenrolled_queries = measured.count.swap(0, Ordering::Relaxed);
        println!(
            "schema queries: empty={empty_queries}, populated enrolled={populated_queries}, unenrolled={unenrolled_queries}"
        );
        assert!(
            unenrolled_queries > populated_queries,
            "unmarked content must still be inspected"
        );
        assert!(
            populated_queries <= empty_queries + 1,
            "enrolled admission must not page through historical content: empty={empty_queries}, populated={populated_queries}"
        );
        assert!(
            migration_recorded(&transaction, OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION)
                .await
                .unwrap()
        );
        transaction.rollback().await.unwrap();
    }
}
