use crate::{
    RegisteredGlobalDb, global_db_operation_error, global_db_operation_message, schema_contract,
    session_temporal,
};

/// Durable stage of the session-temporal store repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTemporalRepairStage {
    RepairState,
    PrepareSchema,
    AuthorityEffects,
    AuthorityReceipts,
    AuthorityCursorKeys,
    AuthorityRefresh,
    AuthorityGenerations,
    AuthorityOwnership,
}

impl SessionTemporalRepairStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::RepairState => "repair_state",
            Self::PrepareSchema => "prepare_schema",
            Self::AuthorityEffects => "authority_effects",
            Self::AuthorityReceipts => "authority_receipts",
            Self::AuthorityCursorKeys => "authority_cursor_keys",
            Self::AuthorityRefresh => "authority_refresh",
            Self::AuthorityGenerations => "authority_generations",
            Self::AuthorityOwnership => "authority_ownership",
        }
    }

    fn parse(value: &str) -> tracedecay_runtime_core::errors::Result<Self> {
        match value {
            "repair_state" => Ok(Self::RepairState),
            "prepare_schema" => Ok(Self::PrepareSchema),
            "authority_effects" => Ok(Self::AuthorityEffects),
            "authority_receipts" => Ok(Self::AuthorityReceipts),
            "authority_cursor_keys" => Ok(Self::AuthorityCursorKeys),
            "authority_refresh" => Ok(Self::AuthorityRefresh),
            "authority_generations" => Ok(Self::AuthorityGenerations),
            "authority_ownership" => Ok(Self::AuthorityOwnership),
            _ => Err(global_db_operation_message(
                "read session temporal repair progress",
                format!("unknown repair stage '{value}'"),
            )),
        }
    }

    fn authority_audit(self) -> Option<usize> {
        match self {
            Self::AuthorityCursorKeys => Some(1),
            Self::AuthorityRefresh => Some(2),
            Self::AuthorityGenerations => Some(3),
            Self::AuthorityOwnership => Some(4),
            Self::RepairState
            | Self::PrepareSchema
            | Self::AuthorityEffects
            | Self::AuthorityReceipts => None,
        }
    }

    fn next(self) -> Option<Self> {
        match self {
            Self::RepairState => Some(Self::PrepareSchema),
            Self::PrepareSchema => Some(Self::AuthorityEffects),
            Self::AuthorityEffects => Some(Self::AuthorityReceipts),
            Self::AuthorityReceipts => Some(Self::AuthorityCursorKeys),
            Self::AuthorityCursorKeys => Some(Self::AuthorityRefresh),
            Self::AuthorityRefresh => Some(Self::AuthorityGenerations),
            Self::AuthorityGenerations => Some(Self::AuthorityOwnership),
            Self::AuthorityOwnership => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTemporalRepairOutcome {
    NotRequired,
    Pending { stage: SessionTemporalRepairStage },
    Complete,
}

const SESSION_TEMPORAL_REPAIR_NAME: &str = "session-temporal-v1";
const SESSION_TEMPORAL_REPAIR_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy)]
struct SessionTemporalRepairCheckpoint {
    stage: SessionTemporalRepairStage,
    cursor: i64,
}

pub async fn enqueue_session_temporal_store_repair(
    database: &RegisteredGlobalDb,
) -> tracedecay_runtime_core::errors::Result<SessionTemporalRepairOutcome> {
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| global_db_operation_error("enqueue session temporal repair", error))?;
    if !connection_table_exists(&transaction, "session_messages").await?
        || !connection_table_exists(&transaction, "observations").await?
    {
        transaction
            .rollback()
            .await
            .map_err(|error| global_db_operation_error("rollback skipped session repair", error))?;
        return Ok(SessionTemporalRepairOutcome::NotRequired);
    }
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS session_temporal_repair_progress (
                repair_name TEXT PRIMARY KEY,
                stage TEXT NOT NULL,
                cursor INTEGER NOT NULL DEFAULT 0 CHECK(cursor >= 0),
                requested_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS session_temporal_repair_receipts (
                repair_name TEXT PRIMARY KEY,
                repair_version INTEGER NOT NULL CHECK(repair_version > 0),
                completed_at INTEGER NOT NULL
             )",
        )
        .await
        .map_err(|error| global_db_operation_error("create session repair checkpoint", error))?;
    let existing = read_session_temporal_repair_checkpoint(&transaction).await?;
    if existing.is_none() && session_temporal_repair_receipt_is_current(&transaction).await? {
        transaction.commit().await.map_err(|error| {
            global_db_operation_error("commit completed session repair request", error)
        })?;
        return Ok(SessionTemporalRepairOutcome::NotRequired);
    }
    transaction
        .execute(
            "INSERT INTO session_temporal_repair_progress (
                repair_name, stage, cursor, requested_at, updated_at
             ) VALUES (?1, ?2, 0, unixepoch(), unixepoch())
             ON CONFLICT(repair_name) DO NOTHING",
            tracedecay_runtime_core::db::engine::params![
                SESSION_TEMPORAL_REPAIR_NAME,
                SessionTemporalRepairStage::RepairState.as_str()
            ],
        )
        .await
        .map_err(|error| global_db_operation_error("enqueue session temporal repair", error))?;
    let checkpoint = read_session_temporal_repair_checkpoint(&transaction)
        .await?
        .ok_or_else(|| {
            global_db_operation_message(
                "enqueue session temporal repair",
                "repair checkpoint disappeared before commit",
            )
        })?;
    transaction
        .commit()
        .await
        .map_err(|error| global_db_operation_error("commit session repair request", error))?;
    Ok(SessionTemporalRepairOutcome::Pending {
        stage: checkpoint.stage,
    })
}

pub async fn session_temporal_store_repair_status(
    database: &RegisteredGlobalDb,
) -> tracedecay_runtime_core::errors::Result<SessionTemporalRepairOutcome> {
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| global_db_operation_error("snapshot session repair status", error))?;
    Ok(
        match read_session_temporal_repair_checkpoint(&snapshot).await? {
            Some(checkpoint) => SessionTemporalRepairOutcome::Pending {
                stage: checkpoint.stage,
            },
            None => SessionTemporalRepairOutcome::NotRequired,
        },
    )
}

pub async fn advance_required_session_temporal_state_repair(
    database: &RegisteredGlobalDb,
) -> tracedecay_runtime_core::errors::Result<SessionTemporalRepairOutcome> {
    let status = session_temporal_store_repair_status(database).await?;
    if status
        == (SessionTemporalRepairOutcome::Pending {
            stage: SessionTemporalRepairStage::RepairState,
        })
    {
        advance_session_temporal_store_repair(database).await
    } else {
        Ok(status)
    }
}

pub async fn advance_session_temporal_store_repair(
    database: &RegisteredGlobalDb,
) -> tracedecay_runtime_core::errors::Result<SessionTemporalRepairOutcome> {
    advance_session_temporal_store_repair_with_page_rows(
        database,
        schema_contract::SESSION_TEMPORAL_REPAIR_AUDIT_PAGE_ROWS,
    )
    .await
}

pub async fn advance_session_temporal_store_repair_with_page_rows(
    database: &RegisteredGlobalDb,
    page_rows: i64,
) -> tracedecay_runtime_core::errors::Result<SessionTemporalRepairOutcome> {
    debug_assert!(page_rows > 0);
    let transaction = database
        .begin_write_transaction()
        .await
        .map_err(|error| global_db_operation_error("advance session temporal repair", error))?;
    let Some(checkpoint) = read_session_temporal_repair_checkpoint(&transaction).await? else {
        transaction
            .rollback()
            .await
            .map_err(|error| global_db_operation_error("rollback idle session repair", error))?;
        return Ok(SessionTemporalRepairOutcome::NotRequired);
    };
    let stage = checkpoint.stage;

    let repair = async {
        let (next_stage, next_cursor) = match stage {
            SessionTemporalRepairStage::RepairState => {
                session_temporal::repair_session_temporal_state(&transaction).await?;
                // Repair state temporarily drops immutable guards. Restore every
                // authority trigger before this batch becomes visible.
                schema_contract::ensure_authority_invariant_schema(&transaction).await?;
                (stage.next(), 0)
            }
            SessionTemporalRepairStage::PrepareSchema => {
                session_temporal::ensure_session_temporal_schema(&transaction).await?;
                schema_contract::ensure_authority_invariant_schema(&transaction).await?;
                (stage.next(), 0)
            }
            SessionTemporalRepairStage::AuthorityEffects => {
                let (cursor, complete) =
                    schema_contract::validate_session_temporal_effect_authority_page_with_limit(
                        &transaction,
                        checkpoint.cursor,
                        page_rows,
                    )
                    .await?;
                (
                    if complete { stage.next() } else { Some(stage) },
                    if complete { 0 } else { cursor },
                )
            }
            SessionTemporalRepairStage::AuthorityReceipts => {
                let (cursor, complete) =
                    schema_contract::validate_session_temporal_receipt_authority_page_with_limit(
                        &transaction,
                        checkpoint.cursor,
                        page_rows,
                    )
                    .await?;
                (
                    if complete { stage.next() } else { Some(stage) },
                    if complete { 0 } else { cursor },
                )
            }
            _ => {
                let audit_index = stage.authority_audit().ok_or_else(|| {
                    global_db_operation_message(
                        "advance session temporal repair",
                        "repair stage has no authority audit",
                    )
                })?;
                schema_contract::validate_session_temporal_repair_authority_audit(
                    &transaction,
                    audit_index,
                )
                .await?;
                (stage.next(), 0)
            }
        };

        if let Some(next) = next_stage {
            transaction
                .execute(
                    "UPDATE session_temporal_repair_progress
                     SET stage = ?1, cursor = ?2, updated_at = unixepoch()
                     WHERE repair_name = ?3",
                    tracedecay_runtime_core::db::engine::params![
                        next.as_str(),
                        next_cursor,
                        SESSION_TEMPORAL_REPAIR_NAME
                    ],
                )
                .await
                .map_err(|error| {
                    global_db_operation_error("checkpoint session temporal repair", error)
                })?;
            Ok(SessionTemporalRepairOutcome::Pending { stage: next })
        } else {
            transaction
                .execute(
                    "INSERT INTO session_temporal_repair_receipts (
                        repair_name, repair_version, completed_at
                     ) VALUES (?1, ?2, unixepoch())
                     ON CONFLICT(repair_name) DO UPDATE SET
                        repair_version = excluded.repair_version,
                        completed_at = excluded.completed_at",
                    tracedecay_runtime_core::db::engine::params![
                        SESSION_TEMPORAL_REPAIR_NAME,
                        SESSION_TEMPORAL_REPAIR_VERSION
                    ],
                )
                .await
                .map_err(|error| {
                    global_db_operation_error("receipt completed session temporal repair", error)
                })?;
            transaction
                .execute(
                    "DELETE FROM session_temporal_repair_progress WHERE repair_name = ?1",
                    tracedecay_runtime_core::db::engine::params![SESSION_TEMPORAL_REPAIR_NAME],
                )
                .await
                .map_err(|error| {
                    global_db_operation_error("complete session temporal repair", error)
                })?;
            Ok(SessionTemporalRepairOutcome::Complete)
        }
    }
    .await;

    match repair {
        Ok(outcome) => {
            transaction.commit().await.map_err(|error| {
                global_db_operation_error("commit session temporal repair batch", error)
            })?;
            Ok(outcome)
        }
        Err(error) => {
            transaction.rollback().await.map_err(|rollback_error| {
                global_db_operation_message(
                    "rollback failed session temporal repair batch",
                    format!("{rollback_error}; original repair failure: {error}"),
                )
            })?;
            Err(error)
        }
    }
}

pub async fn repair_session_temporal_store(
    database: &RegisteredGlobalDb,
) -> tracedecay_runtime_core::errors::Result<()> {
    let mut outcome = enqueue_session_temporal_store_repair(database).await?;
    while matches!(outcome, SessionTemporalRepairOutcome::Pending { .. }) {
        outcome = advance_session_temporal_store_repair(database).await?;
    }
    match outcome {
        SessionTemporalRepairOutcome::NotRequired | SessionTemporalRepairOutcome::Complete => {
            Ok(())
        }
        SessionTemporalRepairOutcome::Pending { .. } => {
            unreachable!("session repair loop exits only on a terminal outcome")
        }
    }
}

async fn session_temporal_repair_receipt_is_current(
    conn: &(impl tracedecay_runtime_core::db::engine::QueryExecutor + ?Sized),
) -> tracedecay_runtime_core::errors::Result<bool> {
    if !connection_table_exists(conn, "session_temporal_repair_receipts").await? {
        return Ok(false);
    }
    let mut rows = tracedecay_runtime_core::db::engine::QueryExecutor::query(
        conn,
        "SELECT repair_version
         FROM session_temporal_repair_receipts
         WHERE repair_name = ?1",
        tracedecay_runtime_core::db::engine::params![SESSION_TEMPORAL_REPAIR_NAME],
    )
    .await
    .map_err(|error| global_db_operation_error("read session repair receipt", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error("read session repair receipt", error))?
    else {
        return Ok(false);
    };
    let version = row
        .get::<i64>(0)
        .map_err(|error| global_db_operation_error("read session repair receipt", error))?;
    Ok(version == SESSION_TEMPORAL_REPAIR_VERSION)
}

async fn read_session_temporal_repair_checkpoint(
    conn: &(impl tracedecay_runtime_core::db::engine::QueryExecutor + ?Sized),
) -> tracedecay_runtime_core::errors::Result<Option<SessionTemporalRepairCheckpoint>> {
    if !connection_table_exists(conn, "session_temporal_repair_progress").await? {
        return Ok(None);
    }
    let mut rows = tracedecay_runtime_core::db::engine::QueryExecutor::query(
        conn,
        "SELECT stage, cursor
         FROM session_temporal_repair_progress
         WHERE repair_name = ?1",
        tracedecay_runtime_core::db::engine::params![SESSION_TEMPORAL_REPAIR_NAME],
    )
    .await
    .map_err(|error| global_db_operation_error("read session temporal repair progress", error))?;
    rows.next()
        .await
        .map_err(|error| global_db_operation_error("read session temporal repair progress", error))?
        .map(|row| {
            let stage = row
                .get::<String>(0)
                .map_err(|error| {
                    global_db_operation_error("read session temporal repair progress", error)
                })
                .and_then(|stage| SessionTemporalRepairStage::parse(&stage))?;
            let cursor = row.get::<i64>(1).map_err(|error| {
                global_db_operation_error("read session temporal repair progress", error)
            })?;
            Ok(SessionTemporalRepairCheckpoint { stage, cursor })
        })
        .transpose()
}

pub(crate) async fn connection_table_exists(
    conn: &(impl tracedecay_runtime_core::db::engine::QueryExecutor + ?Sized),
    table: &str,
) -> tracedecay_runtime_core::errors::Result<bool> {
    let mut rows = tracedecay_runtime_core::db::engine::QueryExecutor::query(
        conn,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        tracedecay_runtime_core::db::engine::params![table],
    )
    .await
    .map_err(|error| global_db_operation_error("inspect global database schema", error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| global_db_operation_error("inspect global database schema", error))?
        .is_some())
}
