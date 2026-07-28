use tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION;

use super::super::{global_db_operation_error, global_db_operation_message};
use super::normalize_trigger_sql;
use crate::db::engine::{Executor, QueryExecutor, params};

mod audit;
mod repair;
mod rows;
#[cfg(test)]
mod test_fixture;
mod triggers;

use audit::{
    AuditCheckpoint, AuditProgress, audit_checkpoint_is_plausible, ensure_audit_checkpoint_schema,
    read_audit_checkpoint, validate_projection_authority_chunk,
    validate_projection_authority_suffix, write_audit_checkpoint,
};
use repair::{
    repair_committed_source_cursors, repair_projection_frontier,
    validate_observation_cursor_coverage,
};
use rows::{
    authority_violation, observation_row_audit_covers, query_has_rows,
    validate_mutable_invariant_rows, validate_observation_authority_rows,
    validate_receipt_authority_rows, validate_source_cursor_authority_chunk,
    validate_source_cursor_authority_rows,
};
use triggers::{FOREIGN_KEY_AUDIT_QUERY, replace_trigger, trigger_contracts_intact};
pub(super) use triggers::{INVARIANTS, Trigger};
pub(in crate::global_db) use triggers::{
    restore_immutability_after_canonical_repair, suspend_immutability_for_canonical_repair,
    suspend_session_invariants_for_schema_upgrade,
};

const OPERATION: &str = "ensure global database authority invariants";
const INCOMPLETE_EXHAUSTIVE_PASS: i64 = -1;
const FOREIGN_KEY_AUDIT_PROGRESS: &str = "authority-invariants";

/// Rows an authority row audit may ask the SQL channel for at once.
///
/// The channel materializes an entire result set before yielding row one and
/// rejects anything past `MAX_QUERY_ROWS` (`10_000`) or 64 MiB. Every audit below
/// walks a table (or a checkpoint suffix of one) whose length grows with the
/// store, so each scan pages with a keyset cursor instead of requesting one
/// unbounded result set. Long-lived daemons also retain allocator arenas sized
/// for the largest page, so keep this comfortably below the hard channel cap;
/// background convergence can afford the additional round trips.
pub(super) const AUDIT_PAGE_ROWS: i64 = 128;

/// Page size for scans that carry a full observation payload.
///
/// Canonical observation records may approach the 1 MiB observation contract
/// ceiling. The audit no longer carries a duplicate receipt JSON payload, so
/// forty-eight rows leave headroom under the channel's 64 MiB materialization
/// limit while avoiding tens of thousands of SQL-channel round trips on a
/// production-sized store.
pub(super) const OBSERVATION_AUDIT_PAGE_ROWS: i64 = 48;
const SESSION_TEMPORAL_REPAIR_AUDITS: &[&str] = &[
    "session temporal receipts or cursor keys are mutable",
    "session cursor key rotation state is invalid",
    "session refresh operation state is invalid",
    "session temporal generation state is invalid",
    "session temporal authority ownership is invalid",
];
pub(in crate::global_db) const SESSION_TEMPORAL_REPAIR_AUDIT_PAGE_ROWS: i64 = 256;

pub(in crate::global_db) async fn authority_invariant_triggers_intact(
    conn: &impl QueryExecutor,
) -> crate::errors::Result<bool> {
    trigger_contracts_intact(conn).await
}

pub(crate) async fn require_foreign_key_audit(conn: &impl Executor) -> crate::errors::Result<()> {
    conn.execute(
        "INSERT INTO authority_foreign_key_audit_progress (audit_name, last_table)
         VALUES (?1, '')
         ON CONFLICT(audit_name) DO NOTHING",
        (FOREIGN_KEY_AUDIT_PROGRESS,),
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(())
}

async fn foreign_key_audit_required(conn: &impl QueryExecutor) -> crate::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM authority_foreign_key_audit_progress
             WHERE audit_name = ?1",
            (FOREIGN_KEY_AUDIT_PROGRESS,),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

async fn projection_checkpoint(conn: &impl QueryExecutor) -> crate::errors::Result<i64> {
    let mut rows = conn
        .query(
            "SELECT COALESCE((
                SELECT last_sequence FROM observation_projection_checkpoints
                WHERE projector_version = ?1
             ), 0)",
            params![SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .ok_or_else(|| authority_violation("projection checkpoint query returned no row"))?
        .get(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

pub(crate) async fn ensure_authority_invariant_schema(
    conn: &impl Executor,
) -> crate::errors::Result<bool> {
    ensure_audit_checkpoint_schema(conn).await?;
    let trigger_contracts_were_intact = trigger_contracts_intact(conn).await?;
    for invariant in INVARIANTS {
        for trigger in invariant.triggers {
            replace_trigger(conn, trigger).await?;
        }
    }
    Ok(trigger_contracts_were_intact)
}

pub(crate) async fn ensure_authority_audit_checkpoint_schema(
    conn: &impl Executor,
) -> crate::errors::Result<()> {
    ensure_audit_checkpoint_schema(conn).await
}

pub(crate) async fn ensure_authority_invariants(
    conn: &impl Executor,
    force_exhaustive: bool,
    is_fresh: bool,
) -> crate::errors::Result<()> {
    let trigger_contracts_were_intact = ensure_authority_invariant_schema(conn).await?;
    if is_fresh {
        // This open created the authority schema from an empty database, so
        // every table the row audits below would scan is guaranteed empty. An
        // exhaustive pass over empty tables produces exactly the default
        // (all-zero) checkpoint with zero audited counts, so write that
        // baseline directly and skip the empty-table scans (~40-60ms on every
        // first open). `is_fresh` is never set on reopen, so corruption
        // detection for existing stores is unchanged; triggers were still
        // (re)installed by `ensure_authority_invariant_schema` above.
        return write_audit_checkpoint(
            conn,
            AuditProgress {
                checkpoint: AuditCheckpoint::default(),
                receipts_audited: 0,
                observations_audited: 0,
                provenance_audited: 0,
                dispositions_audited: 0,
                aliases_audited: 0,
            },
        )
        .await;
    }
    let force_exhaustive = force_exhaustive || foreign_key_audit_required(conn).await?;
    let checkpoint = if !force_exhaustive && trigger_contracts_were_intact {
        match read_audit_checkpoint(conn).await? {
            Some(checkpoint) if audit_checkpoint_is_plausible(conn, checkpoint).await? => {
                Some(checkpoint)
            }
            _ => None,
        }
    } else {
        None
    };
    let exhaustive = checkpoint.is_none_or(|checkpoint| {
        checkpoint.bounded_passes_since_exhaustive == INCOMPLETE_EXHAUSTIVE_PASS
    });
    let checkpoint = checkpoint.unwrap_or_default();
    let (receipt_rowid, receipts_audited) =
        validate_receipt_authority_rows(conn, checkpoint.receipt_rowid).await?;
    if exhaustive {
        write_audit_checkpoint(
            conn,
            AuditProgress {
                checkpoint: AuditCheckpoint {
                    receipt_rowid,
                    bounded_passes_since_exhaustive: INCOMPLETE_EXHAUSTIVE_PASS,
                    ..checkpoint
                },
                receipts_audited,
                observations_audited: 0,
                provenance_audited: 0,
                dispositions_audited: 0,
                aliases_audited: 0,
            },
        )
        .await?;
    }
    // The cursor repair and coverage passes below reconcile the observation
    // suffix committed since the last trusted audit, so they must keep the
    // resume watermark this pass started from. `observation_sequence` and the
    // `checkpoint` rebound after the source-cursor loop both carry the
    // *advanced* watermark, which would skip every row this pass just audited.
    let audited_from_sequence = checkpoint.observation_sequence;
    let (observation_sequence, observations_audited) =
        validate_observation_authority_rows(conn, checkpoint.observation_sequence).await?;
    if exhaustive {
        write_audit_checkpoint(
            conn,
            AuditProgress {
                checkpoint: AuditCheckpoint {
                    receipt_rowid,
                    observation_sequence,
                    bounded_passes_since_exhaustive: INCOMPLETE_EXHAUSTIVE_PASS,
                    ..checkpoint
                },
                receipts_audited,
                observations_audited,
                provenance_audited: 0,
                dispositions_audited: 0,
                aliases_audited: 0,
            },
        )
        .await?;
    }
    let checkpoint = {
        let mut progress = AuditCheckpoint {
            receipt_rowid,
            observation_sequence,
            bounded_passes_since_exhaustive: if exhaustive {
                INCOMPLETE_EXHAUSTIVE_PASS
            } else {
                checkpoint.bounded_passes_since_exhaustive
            },
            ..checkpoint
        };
        loop {
            let (source_cursor_rowid, source_advance_rowid, complete) =
                validate_source_cursor_authority_chunk(
                    conn,
                    progress.source_cursor_rowid,
                    progress.source_advance_rowid,
                )
                .await?;
            progress.source_cursor_rowid = source_cursor_rowid;
            progress.source_advance_rowid = source_advance_rowid;
            write_audit_checkpoint(
                conn,
                AuditProgress {
                    checkpoint: progress,
                    receipts_audited,
                    observations_audited,
                    provenance_audited: 0,
                    dispositions_audited: 0,
                    aliases_audited: 0,
                },
            )
            .await?;
            if complete {
                break progress;
            }
        }
    };
    repair_committed_source_cursors(conn, audited_from_sequence).await?;
    validate_observation_cursor_coverage(conn, audited_from_sequence).await?;

    repair_projection_frontier(conn, checkpoint.projection_checkpoint).await?;
    let projection_start = AuditCheckpoint {
        receipt_rowid,
        observation_sequence,
        bounded_passes_since_exhaustive: if exhaustive {
            INCOMPLETE_EXHAUSTIVE_PASS
        } else {
            checkpoint.bounded_passes_since_exhaustive
        },
        ..checkpoint
    };
    let (mut checkpoint, provenance_audited, dispositions_audited, aliases_audited) = if exhaustive
    {
        let mut progress = projection_start;
        let mut audited_counts = None;
        loop {
            let (next, provenance, dispositions, aliases, complete) =
                validate_projection_authority_chunk(conn, progress).await?;
            audited_counts.get_or_insert((provenance, dispositions, aliases));
            progress = next;
            if complete {
                let (provenance, dispositions, aliases) = audited_counts.unwrap_or((0, 0, 0));
                break (progress, provenance, dispositions, aliases);
            }
            write_audit_checkpoint(
                conn,
                AuditProgress {
                    checkpoint: progress,
                    receipts_audited,
                    observations_audited,
                    provenance_audited: 0,
                    dispositions_audited: 0,
                    aliases_audited: 0,
                },
            )
            .await?;
        }
    } else {
        validate_projection_authority_suffix(conn, projection_start).await?
    };
    if exhaustive {
        write_audit_checkpoint(
            conn,
            AuditProgress {
                checkpoint,
                receipts_audited,
                observations_audited,
                provenance_audited,
                dispositions_audited,
                aliases_audited,
            },
        )
        .await?;
        // Sweeping every foreign key detects corruption, but it cannot detect a
        // violation an authorized write introduced: each runtime connection
        // enables `PRAGMA foreign_keys` and verifies it came back on, so SQLite
        // rejects the offending write itself. The sweep costs what a corruption
        // scan costs — 96 foreign-key tables and roughly ten million child rows
        // at ~59us per row, tens of minutes on a real store — which a cold open
        // pays before admitting its first request. Restrict it to the case that
        // actually implies tampering: guard triggers that were found missing or
        // altered. An ordinary cold open still runs every row audit above.
        if force_exhaustive && foreign_key_violation_exists_resumable(conn).await? {
            return Err(global_db_operation_message(
                OPERATION,
                "global database contains a foreign-key violation",
            ));
        }
        validate_invariant_rows(conn).await?;
    } else {
        validate_mutable_invariant_rows(conn).await?;
    }
    checkpoint.bounded_passes_since_exhaustive = if exhaustive {
        0
    } else {
        checkpoint.bounded_passes_since_exhaustive.saturating_add(1)
    };
    write_audit_checkpoint(
        conn,
        AuditProgress {
            checkpoint,
            receipts_audited,
            observations_audited,
            provenance_audited,
            dispositions_audited,
            aliases_audited,
        },
    )
    .await
}

pub(super) async fn validate_invariant_rows(
    conn: &impl QueryExecutor,
) -> crate::errors::Result<()> {
    for invariant in INVARIANTS {
        if observation_row_audit_covers(invariant) {
            continue;
        }
        if let Some(query) = invariant.audit_query
            && query != FOREIGN_KEY_AUDIT_QUERY
            && query_has_rows(conn, query).await?
        {
            return Err(global_db_operation_message(OPERATION, invariant.violation));
        }
    }
    Ok(())
}

async fn foreign_key_violation_exists_resumable(
    conn: &impl Executor,
) -> crate::errors::Result<bool> {
    loop {
        let mut rows = conn
            .query(
                "SELECT COALESCE((
                    SELECT last_table FROM authority_foreign_key_audit_progress
                    WHERE audit_name = ?1
                 ), '')",
                (FOREIGN_KEY_AUDIT_PROGRESS,),
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let last_table = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .ok_or_else(|| {
                global_db_operation_message(OPERATION, "foreign-key audit cursor disappeared")
            })?
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        drop(rows);

        let mut rows = conn
            .query(
                "SELECT DISTINCT schema.name
                 FROM sqlite_schema AS schema
                 JOIN pragma_foreign_key_list(schema.name) AS foreign_key
                 WHERE schema.type = 'table' AND schema.name > ?1
                 ORDER BY schema.name
                 LIMIT 1",
                (last_table,),
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let table = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .map(|row| row.get::<String>(0))
            .transpose()
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        drop(rows);
        let Some(table) = table else {
            conn.execute(
                "DELETE FROM authority_foreign_key_audit_progress WHERE audit_name = ?1",
                (FOREIGN_KEY_AUDIT_PROGRESS,),
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
            return Ok(false);
        };

        let mut rows = conn
            .query(
                "SELECT 1 FROM pragma_foreign_key_check(?1) LIMIT 1",
                (table.as_str(),),
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let violation = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .is_some();
        drop(rows);
        if violation {
            return Ok(true);
        }
        conn.execute(
            "INSERT INTO authority_foreign_key_audit_progress (audit_name, last_table)
             VALUES (?1, ?2)
             ON CONFLICT(audit_name) DO UPDATE SET last_table = excluded.last_table",
            params![FOREIGN_KEY_AUDIT_PROGRESS, table],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
}

async fn foreign_key_violation_exists_read_only(
    conn: &impl QueryExecutor,
) -> crate::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT schema.name
             FROM sqlite_schema AS schema
             JOIN pragma_foreign_key_list(schema.name) AS foreign_key
             WHERE schema.type = 'table'
             ORDER BY schema.name",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut tables = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        tables.push(
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        );
    }
    drop(rows);

    for table in tables {
        let mut rows = conn
            .query(
                "SELECT 1 FROM pragma_foreign_key_check(?1) LIMIT 1",
                (table,),
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(in crate::global_db) async fn validate_authority_rows_exhaustive(
    conn: &impl QueryExecutor,
) -> crate::errors::Result<()> {
    validate_receipt_authority_rows(conn, 0).await?;
    validate_observation_authority_rows(conn, 0).await?;
    validate_source_cursor_authority_rows(conn).await?;
    validate_observation_cursor_coverage(conn, 0).await?;
    validate_projection_authority_suffix(conn, AuditCheckpoint::default()).await?;
    if foreign_key_violation_exists_read_only(conn).await? {
        return Err(global_db_operation_message(
            OPERATION,
            "global database contains a foreign-key violation",
        ));
    }
    validate_invariant_rows(conn).await
}

pub(in crate::global_db) async fn validate_session_temporal_repair_authority_audit(
    conn: &impl QueryExecutor,
    audit_index: usize,
) -> crate::errors::Result<()> {
    let invariant = INVARIANTS
        .iter()
        .filter(|invariant| SESSION_TEMPORAL_REPAIR_AUDITS.contains(&invariant.violation))
        .nth(audit_index)
        .ok_or_else(|| {
            global_db_operation_message(
                OPERATION,
                format!("unknown session temporal repair authority audit {audit_index}"),
            )
        })?;
    if let Some(query) = invariant.audit_query
        && query_has_rows(conn, query).await?
    {
        return Err(global_db_operation_message(OPERATION, invariant.violation));
    }
    Ok(())
}

pub(in crate::global_db) async fn validate_session_temporal_effect_authority_page(
    conn: &impl QueryExecutor,
    after_rowid: i64,
) -> crate::errors::Result<(i64, bool)> {
    validate_session_temporal_effect_authority_page_with_limit(
        conn,
        after_rowid,
        SESSION_TEMPORAL_REPAIR_AUDIT_PAGE_ROWS,
    )
    .await
}

pub(in crate::global_db) async fn validate_session_temporal_effect_authority_page_with_limit(
    conn: &impl QueryExecutor,
    after_rowid: i64,
    page_rows: i64,
) -> crate::errors::Result<(i64, bool)> {
    debug_assert!(page_rows > 0);
    let mut rows = conn
        .query(
            "SELECT effect.rowid, observation.observation_id IS NULL
             FROM session_temporal_observation_effects AS effect
             LEFT JOIN observations AS observation
               ON observation.observation_id = effect.observation_id
              AND observation.sequence = effect.observation_sequence
              AND observation.receipt_id = effect.receipt_id
             WHERE effect.rowid > ?1
             ORDER BY effect.rowid
             LIMIT ?2",
            params![after_rowid, page_rows],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut last_rowid = after_rowid;
    let mut count = 0_i64;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        last_rowid = row
            .get(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let invalid = row
            .get::<i64>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            != 0;
        if invalid {
            return Err(global_db_operation_message(
                OPERATION,
                SESSION_TEMPORAL_REPAIR_AUDITS[0],
            ));
        }
        count += 1;
    }
    Ok((last_rowid, count < page_rows))
}

pub(in crate::global_db) async fn validate_session_temporal_receipt_authority_page(
    conn: &impl QueryExecutor,
    after_rowid: i64,
) -> crate::errors::Result<(i64, bool)> {
    validate_session_temporal_receipt_authority_page_with_limit(
        conn,
        after_rowid,
        SESSION_TEMPORAL_REPAIR_AUDIT_PAGE_ROWS,
    )
    .await
}

pub(in crate::global_db) async fn validate_session_temporal_receipt_authority_page_with_limit(
    conn: &impl QueryExecutor,
    after_rowid: i64,
    page_rows: i64,
) -> crate::errors::Result<(i64, bool)> {
    debug_assert!(page_rows > 0);
    let mut rows = conn
        .query(
            "SELECT receipt.rowid,
                    generation.session_id IS NULL
                    OR (
                        receipt.batch_ordinal > 0
                        AND NOT EXISTS (
                            SELECT 1
                            FROM session_temporal_projection_receipts AS previous
                            WHERE previous.session_id = receipt.session_id
                              AND previous.generation = receipt.generation
                              AND previous.batch_ordinal = receipt.batch_ordinal - 1
                              AND previous.source_through <= receipt.source_through
                              AND previous.projection_through <= receipt.projection_through
                        )
                    )
             FROM session_temporal_projection_receipts AS receipt
             LEFT JOIN session_temporal_generations AS generation
               ON generation.session_id = receipt.session_id
              AND generation.generation = receipt.generation
              AND generation.frozen_watermarks_json = receipt.frozen_watermarks_json
             WHERE receipt.rowid > ?1
             ORDER BY receipt.rowid
             LIMIT ?2",
            params![after_rowid, page_rows],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut last_rowid = after_rowid;
    let mut count = 0_i64;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        last_rowid = row
            .get(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let invalid = row
            .get::<i64>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            != 0;
        if invalid {
            return Err(global_db_operation_message(
                OPERATION,
                SESSION_TEMPORAL_REPAIR_AUDITS[0],
            ));
        }
        count += 1;
    }
    Ok((last_rowid, count < page_rows))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{
        FOREIGN_KEY_AUDIT_PROGRESS, foreign_key_violation_exists_read_only,
        foreign_key_violation_exists_resumable, validate_session_temporal_effect_authority_page,
        validate_session_temporal_effect_authority_page_with_limit,
    };
    use crate::db::engine::TestConnection;

    #[tokio::test]
    async fn session_temporal_effect_audit_checkpoints_bounded_pages() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        let mut connection = rusqlite::Connection::open(&database_path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE observations (
                    observation_id TEXT NOT NULL,
                    sequence INTEGER NOT NULL,
                    receipt_id TEXT NOT NULL,
                    PRIMARY KEY(observation_id, sequence, receipt_id)
                 );
                 CREATE TABLE session_temporal_observation_effects (
                    observation_id TEXT NOT NULL,
                    observation_sequence INTEGER NOT NULL,
                    receipt_id TEXT NOT NULL
                 );",
            )
            .unwrap();
        let transaction = connection.transaction().unwrap();
        for ordinal in 1..=257 {
            let observation_id = format!("observation-{ordinal}");
            let receipt_id = format!("receipt-{ordinal}");
            transaction
                .execute(
                    "INSERT INTO observations(observation_id, sequence, receipt_id)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![observation_id, ordinal, receipt_id],
                )
                .unwrap();
            transaction
                .execute(
                    "INSERT INTO session_temporal_observation_effects(
                        observation_id, observation_sequence, receipt_id
                     ) VALUES (?1, ?2, ?3)",
                    rusqlite::params![observation_id, ordinal, receipt_id],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
        drop(connection);
        let connection = TestConnection::open(&database_path);

        let (first_cursor, first_complete) =
            validate_session_temporal_effect_authority_page(&connection, 0)
                .await
                .unwrap();
        assert_eq!(first_cursor, 256);
        assert!(!first_complete);
        let (second_cursor, second_complete) =
            validate_session_temporal_effect_authority_page(&connection, first_cursor)
                .await
                .unwrap();
        assert_eq!(second_cursor, 257);
        assert!(second_complete);

        let (adaptive_cursor, adaptive_complete) =
            validate_session_temporal_effect_authority_page_with_limit(&connection, 0, 512)
                .await
                .unwrap();
        assert_eq!(adaptive_cursor, 257);
        assert!(adaptive_complete);
    }

    #[tokio::test]
    async fn foreign_key_audit_finds_violations_by_child_table() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 CREATE TABLE parent (id INTEGER PRIMARY KEY);
                 CREATE TABLE child (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER NOT NULL REFERENCES parent(id)
                 );
                 INSERT INTO child(id, parent_id) VALUES (1, 99);",
            )
            .unwrap();
        drop(connection);
        let connection = TestConnection::open(&database_path);

        assert!(
            foreign_key_violation_exists_read_only(&connection)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn foreign_key_audit_resumes_after_last_durable_table() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 CREATE TABLE authority_foreign_key_audit_progress (
                    audit_name TEXT PRIMARY KEY,
                    last_table TEXT NOT NULL
                 );
                 CREATE TABLE parent (id INTEGER PRIMARY KEY);
                 CREATE TABLE child_a (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER NOT NULL REFERENCES parent(id)
                 );
                 CREATE TABLE child_b (
                    id INTEGER PRIMARY KEY,
                    parent_id INTEGER NOT NULL REFERENCES parent(id)
                 );
                 INSERT INTO parent(id) VALUES (1);
                 INSERT INTO child_a(id, parent_id) VALUES (1, 1);
                 INSERT INTO child_b(id, parent_id) VALUES (1, 99);
                 INSERT INTO authority_foreign_key_audit_progress (audit_name, last_table)
                 VALUES ('authority-invariants', 'child_a');",
            )
            .unwrap();
        drop(connection);
        let connection = TestConnection::open(&database_path);

        assert!(
            foreign_key_violation_exists_resumable(&connection)
                .await
                .unwrap()
        );
        let mut rows = connection
            .query(
                "SELECT last_table FROM authority_foreign_key_audit_progress
                 WHERE audit_name = ?1",
                (FOREIGN_KEY_AUDIT_PROGRESS,),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "child_a"
        );
    }
}
