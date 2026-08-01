//! Resumable backfills that attach derived rows to every historical
//! observation.
//!
//! Both passes here once ran as single unpaged statements inside the
//! schema-upgrade transaction, and neither consulted its own completion
//! marker. On a large store that combination could not converge: the scan took
//! minutes, the recurring project warmup interrupted the connection once its
//! request deadline passed, the whole transaction rolled back, and the next
//! open started over — including on stores where every row was already
//! attached and the marker was already recorded. Each pass now checks its
//! marker first and otherwise advances through bounded, individually committed
//! pages recorded in `observation_backfill_watermarks`.

use tracedecay_domain::{EvidenceAvailabilityV1, ProjectionGenerationId, UtcMicros};
use tracedecay_store::{
    RepositoryProvenanceAttachmentV1, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

use crate::db::engine::{Connection, Executor, QueryExecutor, TransactionBehavior, params};

use super::super::{global_db_operation_error, global_db_operation_message};
use super::persist::persist_observation_retrieval_anchor;
use super::schema::{
    LEGACY_OBSERVATION_PROJECTION_GENERATION, OBSERVATION_ANCHOR_SCHEMA_MIGRATION,
    OBSERVATION_SCHEMA_OPERATION, migration_recorded,
};

/// Completion marker for the repository-provenance backfill. Public so a
/// writer that appends observations the backfill has already passed -- the
/// consolidator merges a source tail above the target frontier -- can clear it
/// and re-arm convergence.
pub const OBSERVATION_PROVENANCE_SCHEMA_MIGRATION: &str =
    "observation-repository-provenance-v1";

/// Observations covered per committed backfill page. Each page runs in its
/// own transaction so an interrupted open — the project warmup deadline
/// cancels in-flight statements — loses at most one page of work instead of
/// rolling the whole table scan back.
const BACKFILL_PAGE_SIZE: i64 = 512;

/// Anchor pages decode and re-verify a receipt per observation, so they stay
/// smaller than the pure-SQL provenance pages.
const ANCHOR_BACKFILL_PAGE_SIZE: i64 = 64;

/// Watermark keys in `observation_backfill_watermarks`, one per pass.
const PROVENANCE: &str = OBSERVATION_PROVENANCE_SCHEMA_MIGRATION;
const ANCHORS: &str = OBSERVATION_ANCHOR_SCHEMA_MIGRATION;

/// One bounded backfill page: either it advanced the committed watermark
/// (more pages may remain; the next call or the next open continues), or the
/// backfill is complete and its migration marker is recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BackfillPageOutcome {
    Advanced,
    Completed,
}

/// Converges the repository-provenance backfill through bounded, individually
/// committed pages. Must run outside the schema-upgrade mega-transaction so
/// each page's progress survives a cancelled open; converged stores return
/// after a single marker probe.
pub async fn converge_observation_repository_provenance(
    conn: &Connection,
) -> crate::errors::Result<()> {
    if migration_recorded(conn, OBSERVATION_PROVENANCE_SCHEMA_MIGRATION).await? {
        return Ok(());
    }
    let availability_json = default_availability_json()?;
    loop {
        if backfill_page(conn, &availability_json).await? == BackfillPageOutcome::Completed {
            return Ok(());
        }
    }
}

/// Converges the retrieval-anchor backfill through bounded, individually
/// committed pages, under the same constraints as the provenance pass above.
pub async fn converge_observation_retrieval_anchors(
    conn: &Connection,
) -> crate::errors::Result<()> {
    if migration_recorded(conn, OBSERVATION_ANCHOR_SCHEMA_MIGRATION).await? {
        return Ok(());
    }
    loop {
        if anchor_backfill_page(conn).await? == BackfillPageOutcome::Completed {
            return Ok(());
        }
    }
}

fn default_availability_json() -> crate::errors::Result<String> {
    serde_json::to_string(
        RepositoryProvenanceAttachmentV1::new(EvidenceAvailabilityV1::Unknown, None)
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
            .availability(),
    )
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

pub(super) async fn backfill_page(
    conn: &Connection,
    availability_json: &str,
) -> crate::errors::Result<BackfillPageOutcome> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    let outcome = backfill_page_transaction(&transaction, availability_json).await?;
    transaction
        .commit()
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    Ok(outcome)
}

async fn backfill_page_transaction(
    transaction: &impl Executor,
    availability_json: &str,
) -> crate::errors::Result<BackfillPageOutcome> {
    if migration_recorded(transaction, OBSERVATION_PROVENANCE_SCHEMA_MIGRATION).await? {
        return Ok(BackfillPageOutcome::Completed);
    }
    let backfilled_through = read_backfill_watermark(transaction, PROVENANCE).await?;
    let Some(page_upper) =
        read_page_upper_bound(transaction, backfilled_through, BACKFILL_PAGE_SIZE).await?
    else {
        // The watermark row deliberately survives completion: it records the
        // sequence through which provenance is attached, so a later merge that
        // appends observations above it (see the consolidator, which clears
        // this migration's marker) resumes from here instead of rescanning
        // every already-attached row.
        record_completion(transaction, PROVENANCE).await?;
        return Ok(BackfillPageOutcome::Completed);
    };
    transaction
        .execute(
            "INSERT OR IGNORE INTO observation_repository_provenance (
                observation_id, availability_json, capture_json, retrieval_anchor_id, owner_json
             )
             SELECT observation_id, ?1, NULL, NULL, NULL FROM observations
             WHERE sequence > ?2 AND sequence <= ?3",
            params![availability_json, backfilled_through, page_upper],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    let mut rows = transaction
        .query(
            "SELECT 1
             FROM observations AS observation
             LEFT JOIN observation_repository_provenance AS provenance
               ON provenance.observation_id = observation.observation_id
             WHERE observation.sequence > ?1 AND observation.sequence <= ?2
               AND provenance.observation_id IS NULL
             LIMIT 1",
            params![backfilled_through, page_upper],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    if rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
        .is_some()
    {
        return Err(global_db_operation_message(
            OBSERVATION_SCHEMA_OPERATION,
            "repository provenance backfill left an observation without an attachment",
        ));
    }
    drop(rows);
    advance_backfill_watermark(transaction, PROVENANCE, backfilled_through, page_upper).await?;
    Ok(BackfillPageOutcome::Advanced)
}

async fn anchor_backfill_page(conn: &Connection) -> crate::errors::Result<BackfillPageOutcome> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    let outcome = anchor_backfill_page_transaction(&transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    Ok(outcome)
}

async fn anchor_backfill_page_transaction(
    transaction: &(impl Executor + Sync),
) -> crate::errors::Result<BackfillPageOutcome> {
    if migration_recorded(transaction, OBSERVATION_ANCHOR_SCHEMA_MIGRATION).await? {
        return Ok(BackfillPageOutcome::Completed);
    }
    let backfilled_through = read_backfill_watermark(transaction, ANCHORS).await?;
    let Some(page_upper) =
        read_page_upper_bound(transaction, backfilled_through, ANCHOR_BACKFILL_PAGE_SIZE).await?
    else {
        record_completion(transaction, ANCHORS).await?;
        return Ok(BackfillPageOutcome::Completed);
    };
    let mut rows = transaction
        .query(
            "SELECT observation.observation_json, observation.receipt_id,
                    receipt.receipt_json
             FROM observations AS observation
             LEFT JOIN sanitization_receipts AS receipt
               ON receipt.receipt_id = observation.receipt_id
             LEFT JOIN observation_retrieval_anchors AS anchor
               ON anchor.observation_id = observation.observation_id
             WHERE observation.sequence > ?1 AND observation.sequence <= ?2
               AND anchor.observation_id IS NULL
             ORDER BY observation.sequence",
            params![backfilled_through, page_upper],
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
        attach_legacy_observation_anchor(
            transaction,
            &observation_json,
            &receipt_id,
            receipt_json.as_deref(),
        )
        .await?;
    }
    advance_backfill_watermark(transaction, ANCHORS, backfilled_through, page_upper).await?;
    Ok(BackfillPageOutcome::Advanced)
}

async fn attach_legacy_observation_anchor(
    conn: &(impl Executor + Sync),
    observation_json: &str,
    receipt_id: &str,
    receipt_json: Option<&str>,
) -> crate::errors::Result<()> {
    let receipt_json = receipt_json.ok_or_else(|| {
        global_db_operation_message(
            OBSERVATION_SCHEMA_OPERATION,
            "legacy observation receipt is unavailable for anchor backfill",
        )
    })?;
    let observation: tracedecay_domain::DurableObservationV1 =
        serde_json::from_str(observation_json)
            .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    let receipt: tracedecay_domain::SanitizationReceiptV1 = serde_json::from_str(receipt_json)
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
    let authorization =
        build_observation_resolution_authorization_v1(&observation, "legacy-observation-import.v1")
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
            alias_kind = ?collision.alias.kind(),
            existing_anchor_id = collision.existing_anchor_id.as_str(),
            candidate_anchor_id = collision.candidate_anchor_id.as_str(),
            "anchor backfill preserved alias binding; candidate stays reachable by id only"
        );
    }
    Ok(())
}

/// Highest observation sequence in the next page above `backfilled_through`,
/// or `None` once no observation remains — the signal that the pass converged.
async fn read_page_upper_bound(
    conn: &impl QueryExecutor,
    backfilled_through: i64,
    page_size: i64,
) -> crate::errors::Result<Option<i64>> {
    let mut rows = conn
        .query(
            "SELECT MAX(sequence) FROM (
                SELECT sequence FROM observations
                WHERE sequence > ?1
                ORDER BY sequence
                LIMIT ?2
             )",
            params![backfilled_through, page_size],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    let page_upper = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
        .map(|row| row.get::<Option<i64>>(0))
        .transpose()
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
        .flatten();
    Ok(page_upper)
}

async fn record_completion(
    conn: &impl Executor,
    migration: &'static str,
) -> crate::errors::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO global_schema_migrations(migration) VALUES (?1)",
        params![migration],
    )
    .await
    .map(|_| ())
    .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

async fn read_backfill_watermark(
    conn: &impl QueryExecutor,
    migration: &'static str,
) -> crate::errors::Result<i64> {
    let mut rows = conn
        .query(
            "SELECT backfilled_through FROM observation_backfill_watermarks
             WHERE migration = ?1",
            params![migration],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?
    else {
        return Ok(0);
    };
    row.get::<i64>(0)
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))
}

async fn advance_backfill_watermark(
    conn: &impl Executor,
    migration: &'static str,
    previous: i64,
    next: i64,
) -> crate::errors::Result<()> {
    let changed = conn
        .execute(
            "INSERT INTO observation_backfill_watermarks (migration, backfilled_through)
             VALUES (?1, ?3)
             ON CONFLICT(migration) DO UPDATE SET backfilled_through = excluded.backfilled_through
             WHERE observation_backfill_watermarks.backfilled_through = ?2",
            params![migration, previous, next],
        )
        .await
        .map_err(|error| global_db_operation_error(OBSERVATION_SCHEMA_OPERATION, error))?;
    if changed != 1 {
        return Err(global_db_operation_message(
            OBSERVATION_SCHEMA_OPERATION,
            "observation backfill watermark compare-and-swap failed",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::engine::TestConnection;

    /// Seeds `observations` rows with raw `SQLite` before the runtime writer
    /// attaches. Row-at-a-time inserts through the writer actor cost seconds
    /// per thousand rows, which makes a store large enough to reproduce the
    /// real convergence failure untestable.
    fn seed_bulk_observations(path: &std::path::Path, observations: usize) -> Result<(), String> {
        let conn = rusqlite::Connection::open(path).map_err(|err| format!("open bulk: {err}"))?;
        conn.execute_batch("BEGIN IMMEDIATE;")
            .map_err(|err| format!("begin bulk: {err}"))?;
        {
            let mut statement = conn
                .prepare(
                    "INSERT INTO observations(observation_id, payload_digest, receipt_id,
                         observation_json, committed_cursor_json)
                     VALUES (?1, 'digest', 'receipt-1', '{}', '{}')",
                )
                .map_err(|err| format!("prepare bulk: {err}"))?;
            for index in 0..observations {
                statement
                    .execute(rusqlite::params![format!("obs-{index}")])
                    .map_err(|err| format!("bulk insert {index}: {err}"))?;
            }
        }
        conn.execute_batch("COMMIT;")
            .map_err(|err| format!("commit bulk: {err}"))
    }

    async fn seeded_store(
        observations: usize,
    ) -> Result<(tempfile::TempDir, TestConnection), String> {
        let temp = tempfile::tempdir().map_err(|err| format!("tempdir: {err}"))?;
        let conn = TestConnection::open(&temp.path().join("obs.db"));
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .map_err(|err| format!("enable fks: {err}"))?;
        super::super::schema::ensure_observation_schema(&*conn)
            .await
            .map_err(|err| format!("ensure observation schema: {err}"))?;
        conn.execute(
            "INSERT INTO sanitization_receipts(receipt_id, sanitizer_version,
                 payload_digest, receipt_json)
             VALUES ('receipt-1', 'v1', 'digest', '{}')",
            (),
        )
        .await
        .map_err(|err| format!("insert receipt: {err}"))?;
        for index in 0..observations {
            conn.execute(
                "INSERT INTO observations(observation_id, payload_digest, receipt_id,
                     observation_json, committed_cursor_json)
                 VALUES (?1, 'digest', 'receipt-1', '{}', '{}')",
                params![format!("obs-{index}")],
            )
            .await
            .map_err(|err| format!("insert observation {index}: {err}"))?;
        }
        Ok((temp, conn))
    }

    async fn count(conn: &TestConnection, sql: &'static str) -> Result<i64, String> {
        let mut rows = conn.query(sql, ()).await.map_err(|err| format!("{err}"))?;
        rows.next()
            .await
            .map_err(|err| format!("{err}"))?
            .ok_or_else(|| "count query returned no row".to_string())?
            .get::<i64>(0)
            .map_err(|err| format!("{err}"))
    }

    async fn marker_recorded(conn: &TestConnection) -> Result<bool, String> {
        migration_recorded(&**conn, OBSERVATION_PROVENANCE_SCHEMA_MIGRATION)
            .await
            .map_err(|err| format!("{err}"))
    }

    /// The state the real 16GB store was in: every observation already
    /// attached and both markers recorded, yet the old code re-ran the full
    /// scans on every open and the warmup interrupt killed them mid-flight. A
    /// recorded marker must make both passes no-ops.
    #[tokio::test]
    async fn recorded_markers_skip_both_passes_entirely() -> Result<(), String> {
        let (_temp, conn) = seeded_store(4).await?;
        converge_observation_repository_provenance(&conn)
            .await
            .map_err(|err| format!("converge provenance: {err}"))?;
        conn.execute(
            "INSERT OR REPLACE INTO global_schema_migrations(migration) VALUES (?1)",
            params![OBSERVATION_ANCHOR_SCHEMA_MIGRATION],
        )
        .await
        .map_err(|err| format!("record anchor marker: {err}"))?;

        // These observations carry placeholder JSON that could never decode
        // into an observation, so any pass that actually scanned them would
        // fail rather than return.
        converge_observation_retrieval_anchors(&conn)
            .await
            .map_err(|err| format!("converge anchors: {err}"))?;
        converge_observation_repository_provenance(&conn)
            .await
            .map_err(|err| format!("re-converge provenance: {err}"))?;
        let anchors = count(&conn, "SELECT COUNT(*) FROM observation_retrieval_anchors").await?;
        assert_eq!(anchors, 0, "the skipped anchor pass attached nothing");
        Ok(())
    }

    #[tokio::test]
    async fn anchor_pass_records_its_marker_on_an_empty_observation_table() -> Result<(), String> {
        let (_temp, conn) = seeded_store(0).await?;
        assert!(
            !migration_recorded(&*conn, OBSERVATION_ANCHOR_SCHEMA_MIGRATION)
                .await
                .map_err(|err| format!("{err}"))?
        );

        converge_observation_retrieval_anchors(&conn)
            .await
            .map_err(|err| format!("converge anchors: {err}"))?;

        assert!(
            migration_recorded(&*conn, OBSERVATION_ANCHOR_SCHEMA_MIGRATION)
                .await
                .map_err(|err| format!("{err}"))?,
            "an empty table converges the anchor pass"
        );
        Ok(())
    }

    #[tokio::test]
    async fn paged_backfill_converges_across_multiple_pages() -> Result<(), String> {
        let total = usize::try_from(BACKFILL_PAGE_SIZE).unwrap() * 2 + 3;
        let (_temp, conn) = seeded_store(total).await?;

        converge_observation_repository_provenance(&conn)
            .await
            .map_err(|err| format!("converge: {err}"))?;

        let attached = count(
            &conn,
            "SELECT COUNT(*) FROM observation_repository_provenance",
        )
        .await?;
        assert_eq!(attached, i64::try_from(total).unwrap());
        assert!(marker_recorded(&conn).await?, "migration marker recorded");
        let watermark = count(
            &conn,
            "SELECT backfilled_through FROM observation_backfill_watermarks",
        )
        .await?;
        assert_eq!(
            watermark,
            i64::try_from(total).unwrap(),
            "completion retains the attached-through watermark"
        );
        Ok(())
    }

    #[tokio::test]
    async fn interrupted_backfill_resumes_from_committed_watermark() -> Result<(), String> {
        let total = usize::try_from(BACKFILL_PAGE_SIZE).unwrap() + 5;
        let (_temp, conn) = seeded_store(total).await?;
        let availability_json =
            default_availability_json().map_err(|err| format!("availability: {err}"))?;

        // First page commits, then the open is "interrupted" (no further pages).
        let outcome = backfill_page(&conn, &availability_json)
            .await
            .map_err(|err| format!("first page: {err}"))?;
        assert_eq!(outcome, BackfillPageOutcome::Advanced);
        let attached = count(
            &conn,
            "SELECT COUNT(*) FROM observation_repository_provenance",
        )
        .await?;
        assert_eq!(attached, BACKFILL_PAGE_SIZE, "one committed page persists");
        assert!(!marker_recorded(&conn).await?);

        // The next open resumes from the committed watermark and converges.
        converge_observation_repository_provenance(&conn)
            .await
            .map_err(|err| format!("resume: {err}"))?;
        let attached = count(
            &conn,
            "SELECT COUNT(*) FROM observation_repository_provenance",
        )
        .await?;
        assert_eq!(attached, i64::try_from(total).unwrap());
        assert!(marker_recorded(&conn).await?);
        Ok(())
    }

    /// The store size that made the unpaged backfill diverge in production:
    /// a single full-table statement ran for minutes, and every recurring
    /// project warmup interrupted it before it could commit. Run with
    /// `cargo test --all-features --lib -- --ignored provenance_backfill`.
    #[tokio::test]
    #[ignore = "large-store convergence check; seeds 500k observations"]
    async fn repeatedly_interrupted_opens_converge_on_a_large_store() -> Result<(), String> {
        const TOTAL: usize = 500_000;
        let temp = tempfile::tempdir().map_err(|err| format!("tempdir: {err}"))?;
        let path = temp.path().join("large.db");
        {
            let conn = TestConnection::open(&path);
            super::super::schema::ensure_observation_schema(&*conn)
                .await
                .map_err(|err| format!("ensure observation schema: {err}"))?;
            conn.execute(
                "INSERT INTO sanitization_receipts(receipt_id, sanitizer_version,
                     payload_digest, receipt_json)
                 VALUES ('receipt-1', 'v1', 'digest', '{}')",
                (),
            )
            .await
            .map_err(|err| format!("insert receipt: {err}"))?;
        }
        seed_bulk_observations(&path, TOTAL)?;

        // Each iteration is one "open": it gets a short deadline, exactly as
        // the project warmup does, and is cancelled mid-page when the deadline
        // passes. Convergence must come from committed pages surviving those
        // cancellations, never from any single open finishing the whole table.
        let conn = TestConnection::open(&path);
        let mut previous_watermark = 0_i64;
        let mut interrupted_opens = 0_usize;
        let mut opens = 0_usize;
        loop {
            opens += 1;
            assert!(opens < 20_000, "convergence must not need unbounded opens");
            let deadline =
                std::time::Duration::from_millis(if opens.is_multiple_of(3) { 2 } else { 60 });
            let open =
                tokio::time::timeout(deadline, converge_observation_repository_provenance(&conn))
                    .await;
            let watermark = read_backfill_watermark(&*conn, PROVENANCE)
                .await
                .map_err(|err| format!("read watermark: {err}"))?;
            match open {
                Ok(result) => {
                    result.map_err(|err| format!("converge open {opens}: {err}"))?;
                    break;
                }
                Err(_) => {
                    interrupted_opens += 1;
                    assert!(
                        watermark >= previous_watermark,
                        "open {opens}: an interrupted open must never lose committed progress \
                         (watermark {watermark} < {previous_watermark})"
                    );
                    assert!(
                        watermark > previous_watermark || opens.is_multiple_of(3),
                        "open {opens}: a 60ms open must commit at least one page"
                    );
                    previous_watermark = watermark;
                }
            }
        }
        assert!(
            interrupted_opens > 0,
            "the check is only meaningful if opens were actually interrupted"
        );

        let attached = count(
            &conn,
            "SELECT COUNT(*) FROM observation_repository_provenance",
        )
        .await?;
        assert_eq!(attached, i64::try_from(TOTAL).unwrap());
        assert!(marker_recorded(&conn).await?, "migration marker recorded");
        Ok(())
    }

    #[tokio::test]
    async fn converged_store_skips_on_marker_alone() -> Result<(), String> {
        let (_temp, conn) = seeded_store(3).await?;
        converge_observation_repository_provenance(&conn)
            .await
            .map_err(|err| format!("converge: {err}"))?;

        // Post-marker writers persist provenance atomically with each
        // observation, so a second converge must return on the marker probe
        // alone without touching the watermark table again.
        converge_observation_repository_provenance(&conn)
            .await
            .map_err(|err| format!("second converge: {err}"))?;
        assert!(marker_recorded(&conn).await?);
        Ok(())
    }
}
