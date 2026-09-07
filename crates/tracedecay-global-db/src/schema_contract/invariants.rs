use tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION;

use super::super::{global_db_operation_error, global_db_operation_message};
use super::normalize_trigger_sql;
use tracedecay_runtime_core::db::{
    Database, DatabaseWriteTransaction,
    engine::{Executor, QueryExecutor, params},
};

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
    validate_mutable_invariant_rows, validate_observation_authority_page,
    validate_observation_authority_rows, validate_receipt_authority_page,
    validate_receipt_authority_rows, validate_source_cursor_authority_chunk,
    validate_source_cursor_authority_rows,
};
pub use triggers::released_v3_invariant_triggers_intact;
use triggers::{FOREIGN_KEY_AUDIT_QUERY, replace_trigger, trigger_contracts_intact};
pub(super) use triggers::{INVARIANTS, Trigger};
pub(crate) use triggers::{invariant_trigger_names_for_tables, invariant_trigger_sql_for_tables};

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

pub async fn authority_invariant_triggers_intact(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<bool> {
    trigger_contracts_intact(conn).await
}

pub async fn require_foreign_key_audit(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<()> {
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

async fn foreign_key_audit_required(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<bool> {
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

async fn projection_checkpoint(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<i64> {
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

pub async fn ensure_authority_invariant_schema(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<bool> {
    ensure_audit_checkpoint_schema(conn).await?;
    let trigger_contracts_were_intact = trigger_contracts_intact(conn).await?;
    for invariant in INVARIANTS {
        for trigger in invariant.triggers {
            replace_trigger(conn, trigger).await?;
        }
    }
    Ok(trigger_contracts_were_intact)
}

pub async fn ensure_authority_audit_checkpoint_schema(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<()> {
    ensure_audit_checkpoint_schema(conn).await
}

pub(crate) trait AuthorityInvariantTransactionProvider {
    async fn begin_authority_invariant_step(
        &self,
        operation: &'static str,
    ) -> tracedecay_domain::errors::Result<DatabaseWriteTransaction<'_>>;
}

impl AuthorityInvariantTransactionProvider for Database {
    async fn begin_authority_invariant_step(
        &self,
        operation: &'static str,
    ) -> tracedecay_domain::errors::Result<DatabaseWriteTransaction<'_>> {
        self.begin_bulk_write_transaction(operation).await
    }
}

pub(crate) async fn ensure_fresh_authority_invariants(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<()> {
    ensure_authority_invariant_schema(conn).await?;
    write_audit_checkpoint(
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
    .await
}

#[hotpath::measure(future = true, label = "global_db.schema.persist.converge_step")]
async fn authority_invariant_step<P, F, T>(
    provider: &P,
    operation: &'static str,
    step: F,
) -> tracedecay_domain::errors::Result<T>
where
    P: AuthorityInvariantTransactionProvider + Sync,
    F: for<'transaction> AsyncFnOnce(
            &'transaction DatabaseWriteTransaction<'_>,
        ) -> tracedecay_domain::errors::Result<T>
        + Send,
    T: Send,
{
    let transaction = provider.begin_authority_invariant_step(operation).await?;
    match step(&transaction).await {
        Ok(value) => {
            transaction.commit().await?;
            Ok(value)
        }
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(global_db_operation_error(
                "roll back global database authority invariant step",
                std::io::Error::other(format!("{error}; rollback failed: {rollback_error}")),
            )),
        },
    }
}

pub(crate) async fn ensure_authority_invariants(
    provider: &(impl AuthorityInvariantTransactionProvider + Sync),
    force_exhaustive: bool,
    is_fresh: bool,
) -> tracedecay_domain::errors::Result<()> {
    let (trigger_contracts_were_intact, foreign_key_audit_is_required) = authority_invariant_step(
        provider,
        "install authority invariant guards",
        async |conn| {
            let trigger_contracts_were_intact = if is_fresh {
                let intact = authority_invariant_triggers_intact(conn).await?;
                ensure_fresh_authority_invariants(conn).await?;
                intact
            } else {
                ensure_authority_invariant_schema(conn).await?
            };
            let foreign_key_audit_is_required = foreign_key_audit_required(conn).await?;
            Ok((trigger_contracts_were_intact, foreign_key_audit_is_required))
        },
    )
    .await?;
    if is_fresh {
        // This open created the authority schema from an empty database, so
        // every table the row audits below would scan is guaranteed empty. An
        // exhaustive pass over empty tables produces exactly the default
        // (all-zero) checkpoint with zero audited counts, so write that
        // baseline directly and skip the empty-table scans (~40-60ms on every
        // first open). `is_fresh` is never set on reopen, so corruption
        // detection for existing stores is unchanged; triggers were still
        // (re)installed by `ensure_authority_invariant_schema` above.
        return Ok(());
    }
    let force_exhaustive = force_exhaustive || foreign_key_audit_is_required;
    let checkpoint =
        authority_invariant_step(provider, "read authority audit checkpoint", async |conn| {
            if !force_exhaustive && trigger_contracts_were_intact {
                match read_audit_checkpoint(conn).await? {
                    Some(checkpoint) if audit_checkpoint_is_plausible(conn, checkpoint).await? => {
                        Ok(Some(checkpoint))
                    }
                    _ => Ok(None),
                }
            } else {
                Ok(None)
            }
        })
        .await?;
    let exhaustive = checkpoint.is_none_or(|checkpoint| {
        checkpoint.bounded_passes_since_exhaustive == INCOMPLETE_EXHAUSTIVE_PASS
    });
    let checkpoint = checkpoint.unwrap_or_default();

    let mut receipt_rowid = checkpoint.receipt_rowid;
    let mut receipts_audited = 0;
    loop {
        let (next_rowid, page_audited, complete) = authority_invariant_step(
            provider,
            "audit sanitization receipt authority page",
            async |conn| {
                let (next_rowid, page_audited, complete) =
                    validate_receipt_authority_page(conn, receipt_rowid).await?;
                if exhaustive {
                    write_audit_checkpoint(
                        conn,
                        AuditProgress {
                            checkpoint: AuditCheckpoint {
                                receipt_rowid: next_rowid,
                                bounded_passes_since_exhaustive: INCOMPLETE_EXHAUSTIVE_PASS,
                                ..checkpoint
                            },
                            receipts_audited: receipts_audited + page_audited,
                            observations_audited: 0,
                            provenance_audited: 0,
                            dispositions_audited: 0,
                            aliases_audited: 0,
                        },
                    )
                    .await?;
                }
                Ok((next_rowid, page_audited, complete))
            },
        )
        .await?;
        receipt_rowid = next_rowid;
        receipts_audited += page_audited;
        if complete {
            break;
        }
    }

    // The cursor repair and coverage passes below reconcile the observation
    // suffix committed since the last trusted audit, so they must keep the
    // resume watermark this pass started from. `observation_sequence` and the
    // `checkpoint` rebound after the source-cursor loop both carry the
    // *advanced* watermark, which would skip every row this pass just audited.
    let audited_from_sequence = checkpoint.observation_sequence;
    let mut observation_sequence = checkpoint.observation_sequence;
    let mut observations_audited = 0;
    loop {
        let (next_sequence, page_audited, complete) =
            authority_invariant_step(provider, "audit observation authority page", async |conn| {
                let (next_sequence, page_audited, complete) =
                    validate_observation_authority_page(conn, observation_sequence).await?;
                if exhaustive {
                    write_audit_checkpoint(
                        conn,
                        AuditProgress {
                            checkpoint: AuditCheckpoint {
                                receipt_rowid,
                                observation_sequence: next_sequence,
                                bounded_passes_since_exhaustive: INCOMPLETE_EXHAUSTIVE_PASS,
                                ..checkpoint
                            },
                            receipts_audited,
                            observations_audited: observations_audited + page_audited,
                            provenance_audited: 0,
                            dispositions_audited: 0,
                            aliases_audited: 0,
                        },
                    )
                    .await?;
                }
                Ok((next_sequence, page_audited, complete))
            })
            .await?;
        observation_sequence = next_sequence;
        observations_audited += page_audited;
        if complete {
            break;
        }
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
            let (source_cursor_rowid, source_advance_rowid, complete) = authority_invariant_step(
                provider,
                "audit source cursor authority page",
                async |conn| {
                    let (source_cursor_rowid, source_advance_rowid, complete) =
                        validate_source_cursor_authority_chunk(
                            conn,
                            progress.source_cursor_rowid,
                            progress.source_advance_rowid,
                        )
                        .await?;
                    let next = AuditCheckpoint {
                        source_cursor_rowid,
                        source_advance_rowid,
                        ..progress
                    };
                    write_audit_checkpoint(
                        conn,
                        AuditProgress {
                            checkpoint: next,
                            receipts_audited,
                            observations_audited,
                            provenance_audited: 0,
                            dispositions_audited: 0,
                            aliases_audited: 0,
                        },
                    )
                    .await?;
                    Ok((source_cursor_rowid, source_advance_rowid, complete))
                },
            )
            .await?;
            progress.source_cursor_rowid = source_cursor_rowid;
            progress.source_advance_rowid = source_advance_rowid;
            if complete {
                break progress;
            }
        }
    };

    authority_invariant_step(provider, "repair committed source cursors", async |conn| {
        repair_committed_source_cursors(conn, audited_from_sequence).await
    })
    .await?;
    authority_invariant_step(
        provider,
        "validate observation cursor coverage",
        async |conn| validate_observation_cursor_coverage(conn, audited_from_sequence).await,
    )
    .await?;

    authority_invariant_step(
        provider,
        "repair observation projection frontier",
        async |conn| {
            repair_projection_frontier(conn, checkpoint.projection_checkpoint).await?;
            if exhaustive {
                validate_invariant_rows(conn).await
            } else {
                validate_mutable_invariant_rows(conn).await
            }
        },
    )
    .await?;
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
    let (checkpoint, provenance_audited, dispositions_audited, aliases_audited) = if exhaustive {
        let mut progress = projection_start;
        let mut audited_counts = None;
        loop {
            let (next, provenance, dispositions, aliases, complete) = authority_invariant_step(
                provider,
                "audit observation projection authority page",
                async |conn| {
                    let result = validate_projection_authority_chunk(conn, progress).await?;
                    write_audit_checkpoint(
                        conn,
                        AuditProgress {
                            checkpoint: result.0,
                            receipts_audited,
                            observations_audited,
                            provenance_audited: if result.4 { result.1 } else { 0 },
                            dispositions_audited: if result.4 { result.2 } else { 0 },
                            aliases_audited: if result.4 { result.3 } else { 0 },
                        },
                    )
                    .await?;
                    Ok(result)
                },
            )
            .await?;
            audited_counts.get_or_insert((provenance, dispositions, aliases));
            progress = next;
            if complete {
                let (provenance, dispositions, aliases) = audited_counts.unwrap_or((0, 0, 0));
                break (progress, provenance, dispositions, aliases);
            }
        }
    } else {
        authority_invariant_step(
            provider,
            "audit observation projection authority suffix",
            async |conn| validate_projection_authority_suffix(conn, projection_start).await,
        )
        .await?
    };

    if exhaustive && force_exhaustive {
        loop {
            match authority_invariant_step(
                provider,
                "audit global database foreign key table",
                async |conn| audit_next_foreign_key_table(conn).await,
            )
            .await?
            {
                ForeignKeyAuditStep::Continue => {}
                ForeignKeyAuditStep::Complete => break,
                ForeignKeyAuditStep::Violation => {
                    return Err(global_db_operation_message(
                        OPERATION,
                        "global database contains a foreign-key violation",
                    ));
                }
            }
        }
    }

    // A writer between committed pages can only append above a captured
    // watermark: admission-critical triggers guard that row immediately, and
    // this pass's later pages or the next bounded suffix pass audits it. Only
    // the final step below turns the in-progress marker trusted.
    let mut checkpoint = checkpoint;
    checkpoint.bounded_passes_since_exhaustive = if exhaustive {
        0
    } else {
        checkpoint.bounded_passes_since_exhaustive.saturating_add(1)
    };
    authority_invariant_step(
        provider,
        "publish trusted authority audit checkpoint",
        async |conn| {
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
        },
    )
    .await
}

pub(super) async fn validate_invariant_rows(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
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

#[cfg(test)]
async fn foreign_key_violation_exists_resumable(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<bool> {
    loop {
        match audit_next_foreign_key_table(conn).await? {
            ForeignKeyAuditStep::Continue => {}
            ForeignKeyAuditStep::Complete => return Ok(false),
            ForeignKeyAuditStep::Violation => return Ok(true),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForeignKeyAuditStep {
    Continue,
    Complete,
    Violation,
}

async fn audit_next_foreign_key_table(
    conn: &impl Executor,
) -> tracedecay_domain::errors::Result<ForeignKeyAuditStep> {
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
        return Ok(ForeignKeyAuditStep::Complete);
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
        return Ok(ForeignKeyAuditStep::Violation);
    }
    conn.execute(
        "INSERT INTO authority_foreign_key_audit_progress (audit_name, last_table)
         VALUES (?1, ?2)
         ON CONFLICT(audit_name) DO UPDATE SET last_table = excluded.last_table",
        params![FOREIGN_KEY_AUDIT_PROGRESS, table],
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(ForeignKeyAuditStep::Continue)
}

async fn foreign_key_violation_exists_read_only(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<bool> {
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

pub async fn validate_authority_rows_exhaustive(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::TempDir;
    use tokio::sync::{Notify, Semaphore};

    use super::{
        AUDIT_PAGE_ROWS, AuthorityInvariantTransactionProvider, FOREIGN_KEY_AUDIT_PROGRESS,
        INCOMPLETE_EXHAUSTIVE_PASS, OBSERVATION_AUDIT_PAGE_ROWS, ensure_authority_invariants,
        foreign_key_violation_exists_read_only, foreign_key_violation_exists_resumable,
        global_db_operation_error, global_db_operation_message,
    };
    use crate::schema_contract::invariants::test_fixture::{
        authority_fixture, open_registered, seed_observation, write_cursor,
    };
    use tracedecay_runtime_core::db::engine::TestConnection;
    use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor};
    use tracedecay_runtime_core::db::{Database, DatabaseWriteTransaction};

    struct FailingTransactionProvider<'a> {
        database: &'a Database,
        fail_at: usize,
        calls: AtomicUsize,
    }

    impl AuthorityInvariantTransactionProvider for FailingTransactionProvider<'_> {
        async fn begin_authority_invariant_step(
            &self,
            operation: &'static str,
        ) -> tracedecay_domain::errors::Result<DatabaseWriteTransaction<'_>> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if call == self.fail_at {
                return Err(global_db_operation_message(
                    operation,
                    format!("injected transaction-provider failure at step {call}"),
                ));
            }
            self.database.begin_bulk_write_transaction(operation).await
        }
    }

    struct PausingTransactionProvider<'a> {
        database: &'a Database,
        pause_at: usize,
        calls: AtomicUsize,
        paused: Notify,
        release: Semaphore,
    }

    impl PausingTransactionProvider<'_> {
        async fn wait_until_paused(&self) {
            while self.calls.load(Ordering::Acquire) < self.pause_at {
                self.paused.notified().await;
            }
        }

        fn release(&self) {
            self.release.add_permits(1);
        }
    }

    impl AuthorityInvariantTransactionProvider for PausingTransactionProvider<'_> {
        async fn begin_authority_invariant_step(
            &self,
            operation: &'static str,
        ) -> tracedecay_domain::errors::Result<DatabaseWriteTransaction<'_>> {
            let call = self.calls.fetch_add(1, Ordering::AcqRel) + 1;
            if call == self.pause_at {
                self.paused.notify_waiters();
                self.release
                    .acquire()
                    .await
                    .map_err(|error| global_db_operation_error(operation, error))?
                    .forget();
            }
            self.database.begin_bulk_write_transaction(operation).await
        }
    }

    async fn seed_multi_page_authority_fixture(connection: &impl Executor) -> (usize, usize) {
        let observation_rows = usize::try_from(OBSERVATION_AUDIT_PAGE_ROWS).unwrap() * 2 + 1;
        let source_cursor_rows = usize::try_from(AUDIT_PAGE_ROWS).unwrap() * 2 + 1;
        for index in 0..observation_rows {
            let (_, cursor) = seed_observation(
                connection,
                u64::try_from(index).unwrap(),
                &format!("stepped-{index}"),
            )
            .await;
            write_cursor(connection, &cursor).await;
        }
        for index in observation_rows..source_cursor_rows {
            let (_, cursor) =
                authority_fixture(u64::try_from(index).unwrap(), &format!("stepped-{index}"));
            write_cursor(connection, &cursor).await;
        }
        (observation_rows, source_cursor_rows)
    }

    #[tokio::test]
    async fn completed_audit_pages_survive_later_convergence_failure() {
        let (_directory, fixture) = open_registered().await;
        let transaction = fixture
            .database()
            .begin_write_transaction()
            .await
            .expect("begin multi-page authority fixture");
        let (_, source_cursor_rows) = seed_multi_page_authority_fixture(&transaction).await;
        transaction
            .execute("DELETE FROM authority_audit_checkpoints", ())
            .await
            .expect("arm exhaustive audit");
        transaction
            .commit()
            .await
            .expect("commit multi-page authority fixture");

        let provider = FailingTransactionProvider {
            database: fixture.database().runtime_database(),
            fail_at: 10,
            calls: AtomicUsize::new(0),
        };
        let error = ensure_authority_invariants(&provider, true, false)
            .await
            .expect_err("later convergence step must fail");
        assert!(
            error
                .to_string()
                .contains("injected transaction-provider failure at step 10"),
            "unexpected injected failure: {error}"
        );

        let mut rows = fixture
            .query(
                "SELECT source_cursor_rowid, bounded_passes_since_exhaustive
                 FROM authority_audit_checkpoints
                 WHERE audit_name = 'observation-authority'",
                (),
            )
            .await
            .expect("read resumable audit progress");
        let row = rows
            .next()
            .await
            .expect("read resumable audit progress row")
            .expect("completed audit pages must persist before later convergence work");
        assert_eq!(
            row.get::<i64>(0).expect("decode source cursor watermark"),
            i64::try_from(source_cursor_rows).unwrap()
        );
        assert_eq!(
            row.get::<i64>(1).expect("decode exhaustive marker"),
            -1,
            "interrupted convergence must not publish a trusted checkpoint"
        );
    }

    #[tokio::test]
    async fn ordinary_write_completes_between_convergence_steps() {
        let (_directory, fixture) = open_registered().await;
        let transaction = fixture
            .database()
            .begin_write_transaction()
            .await
            .expect("begin multi-page authority fixture");
        seed_multi_page_authority_fixture(&transaction).await;
        transaction
            .execute("DELETE FROM authority_audit_checkpoints", ())
            .await
            .expect("arm exhaustive audit");
        transaction
            .commit()
            .await
            .expect("commit multi-page authority fixture");

        let provider = PausingTransactionProvider {
            database: fixture.database().runtime_database(),
            pause_at: 4,
            calls: AtomicUsize::new(0),
            paused: Notify::new(),
            release: Semaphore::new(0),
        };
        let convergence = ensure_authority_invariants(&provider, true, false);
        tokio::pin!(convergence);
        tokio::select! {
            result = &mut convergence => {
                panic!("convergence completed before the controlled inter-step pause: {result:?}");
            }
            () = provider.wait_until_paused() => {}
        }

        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            fixture
                .database()
                .writer_connection()
                .expect("ordinary registered writer")
                .execute(
                    "INSERT INTO savings_ledger(
                        ts, project_path, tool_name, before_tokens, after_tokens
                     ) VALUES (1, '/writer-availability', 'test', 2, 1)",
                    (),
                )
                .await
                .expect("ordinary write while convergence is between steps");
        })
        .await
        .expect("ordinary write must not wait for the full convergence pass");

        provider.release();
        convergence
            .await
            .expect("convergence resumes after ordinary admission");
    }

    #[tokio::test]
    async fn stepped_convergence_preserves_single_transaction_checkpoint_result() {
        let (_directory, fixture) = open_registered().await;
        let transaction = fixture
            .database()
            .begin_write_transaction()
            .await
            .expect("begin equivalence authority fixture");
        let (observation_rows, source_cursor_rows) =
            seed_multi_page_authority_fixture(&transaction).await;
        transaction
            .execute("DELETE FROM authority_audit_checkpoints", ())
            .await
            .expect("arm exhaustive audit");
        transaction
            .commit()
            .await
            .expect("commit equivalence authority fixture");

        ensure_authority_invariants(fixture.database().runtime_database(), true, false)
            .await
            .expect("stepped convergence");

        let mut rows = fixture
            .query(
                "SELECT receipt_rowid, observation_sequence,
                        source_cursor_rowid, source_advance_rowid,
                        provenance_rowid, disposition_rowid, alias_rowid,
                        projection_checkpoint, last_receipts_audited,
                        last_observations_audited, last_provenance_audited,
                        last_dispositions_audited, last_aliases_audited,
                        bounded_passes_since_exhaustive
                 FROM authority_audit_checkpoints
                 WHERE audit_name = 'observation-authority'",
                (),
            )
            .await
            .expect("read final authority checkpoint");
        let row = rows
            .next()
            .await
            .expect("read final authority checkpoint row")
            .expect("trusted checkpoint");
        let expected_observations = i64::try_from(observation_rows).unwrap();
        let expected_cursors = i64::try_from(source_cursor_rows).unwrap();
        assert_eq!(
            [
                row.get::<i64>(0).unwrap(),
                row.get::<i64>(1).unwrap(),
                row.get::<i64>(2).unwrap(),
                row.get::<i64>(3).unwrap(),
                row.get::<i64>(4).unwrap(),
                row.get::<i64>(5).unwrap(),
                row.get::<i64>(6).unwrap(),
                row.get::<i64>(7).unwrap(),
                row.get::<i64>(8).unwrap(),
                row.get::<i64>(9).unwrap(),
                row.get::<i64>(10).unwrap(),
                row.get::<i64>(11).unwrap(),
                row.get::<i64>(12).unwrap(),
                row.get::<i64>(13).unwrap(),
            ],
            [
                expected_observations,
                expected_observations,
                expected_cursors,
                0,
                0,
                0,
                0,
                0,
                expected_observations,
                expected_observations,
                0,
                0,
                0,
                0,
            ],
            "stepped convergence must retain the pre-fix checkpoint values"
        );
    }

    #[tokio::test]
    async fn corruption_refuses_trust_after_durable_audit_progress() {
        let (_directory, fixture) = open_registered().await;
        let transaction = fixture
            .database()
            .begin_write_transaction()
            .await
            .expect("begin corrupt authority fixture");
        seed_multi_page_authority_fixture(&transaction).await;
        transaction
            .execute(
                "UPDATE source_cursors
                 SET cursor_json = '{}'
                 WHERE rowid = (SELECT MAX(rowid) FROM source_cursors)",
                (),
            )
            .await
            .expect("inject source cursor authority violation");
        transaction
            .execute("DELETE FROM authority_audit_checkpoints", ())
            .await
            .expect("arm exhaustive audit");
        transaction
            .commit()
            .await
            .expect("commit corrupt authority fixture");

        let error = ensure_authority_invariants(fixture.database().runtime_database(), true, false)
            .await
            .expect_err("corrupt source cursor must fail convergence");
        assert!(
            error
                .to_string()
                .contains("invalid source cursor authority JSON"),
            "unexpected corruption error: {error}"
        );
        let mut rows = fixture
            .query(
                "SELECT bounded_passes_since_exhaustive
                 FROM authority_audit_checkpoints
                 WHERE audit_name = 'observation-authority'",
                (),
            )
            .await
            .expect("read interrupted checkpoint");
        assert_eq!(
            rows.next()
                .await
                .expect("read interrupted checkpoint row")
                .expect("completed pages persist before corruption")
                .get::<i64>(0)
                .expect("decode exhaustive marker"),
            INCOMPLETE_EXHAUSTIVE_PASS,
            "corruption must never publish a trusted checkpoint"
        );
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
