//! In-place upgrade of the one published workflow shape the final contract
//! left behind.
//!
//! Tagged releases up to `v1.0.0-beta.92` created the same nine workflow tables
//! byte for byte and recorded the same identity digest. `v1.0.0-beta.93` dropped
//! the artifact byte ceiling from `workflow_artifact_payloads`, which changed
//! that one table's `CHECK` and the identity digest, with no way for an existing
//! store to follow. The digest is an opaque published label rather than a hash
//! of the stored SQL, so this recognizes the shape from the exact stored text
//! together with the recorded identity. Anything else, however close, keeps the
//! typed refusal. A binary's version label is no evidence either way: a build
//! labelled with one of those versions from a later source revision writes the
//! final shape, so only the store's own text decides.
//!
//! The exact-SQL guard denies catalog edits on the registered writer, so the
//! change is a real table move: it costs one copy of the artifact rows, once.
//! Deciding whether a store can take it costs no copy, so admission only decides
//! and the copy is historical convergence: a daemon runs it in its bounded
//! background maintenance while the store's other authorities serve and workflow
//! storage answers typed recovering. One transaction carries the whole move, so
//! interruption at any point leaves the published store, and the next open
//! recognizes it and starts over. For as long as that transaction runs it holds
//! the store's writer lane, and its pages live in the WAL beside the old table's,
//! so the store's other writers queue behind it and the store briefly takes
//! about twice the payload's space. The dropped table's pages are reclaimed
//! afterwards.

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::{self, Executor, QueryExecutor};
use tracedecay_rusqlite_runtime::workflow::{
    WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1, WORKFLOW_SCHEMA_VERSION_V1, WORKFLOW_TABLE_CONTRACTS_V1,
};

use super::{
    CancellableSchemaTransaction, WorkflowSchemaAdmission, inspect_workflow_schema_for_admission,
    read_recorded_workflow_identity, schema_install_checkpoint, settle_schema_transaction,
    workflow_schema_refusal,
};
use crate::registered::RefusedAuthorityV1;
use crate::{global_db_operation_error, global_db_operation_message};

const WORKFLOW_ARTIFACT_RECLAIM_PAGES: u64 = 4096;
const ARTIFACT_TABLE: &str = "workflow_artifact_payloads";

const PUBLISHED_ARTIFACT_TABLE_SQL: &str = "CREATE TABLE workflow_artifact_payloads (
    payload_digest TEXT NOT NULL PRIMARY KEY,
    byte_length INTEGER NOT NULL CHECK (
        byte_length = length(payload) AND byte_length <= 4194304
    ),
    payload BLOB NOT NULL
) STRICT";

const PUBLISHED_WORKFLOW_DIGEST: &str =
    "sha256:a292df6bc47e763f0d20bdb44a4032c0b1d7ac8e4cb83173b66ae7d1ff0d03be";

/// The final contract's SQL is stored byte-exact only when `CREATE TABLE` runs
/// under the final name, so the published table moves aside first rather than
/// renaming a new table into place, which would quote the stored name.
const DISPLACED_ARTIFACT_TABLE: &str = "workflow_artifact_payloads_published";

/// Objects other than the two tables the upgrade changes that could observe or
/// block it. A trigger may share a table's name, so the exclusions name the
/// object type too.
const DEPENDENT_OBJECT_COUNT_SQL: &str = "SELECT count(*) FROM sqlite_master
     WHERE NOT (type = 'table' AND name IN ('workflow_artifact_payloads', 'workflow_schema'))
       AND NOT (type = 'index' AND name = 'sqlite_autoindex_workflow_artifact_payloads_1')
       AND (tbl_name IN ('workflow_artifact_payloads', 'workflow_schema')
            OR instr(lower(sql), 'workflow_artifact_payloads') > 0)";

/// Rows per classification page. `typeof` and `length` read the record header
/// rather than a payload's overflow pages, so one page stays cheap however large
/// the artifacts are, as long as the predicate stays in the inner select: a
/// `payload` column crossing the subquery boundary would load every blob.
const ROW_CLASSIFICATION_PAGE_ROWS: i64 = 4096;

/// Rows the published constraints would have rejected can only come from a
/// store written around them, so they are not the proven shape. This is what
/// makes the copy unable to refuse a row. An exhausted page has no `max`.
const NONCONFORMING_ROW_PAGE_SQL: &str = "SELECT max(row_id), coalesce(sum(nonconforming), 0)
     FROM (
         SELECT
             rowid AS row_id,
             typeof(payload_digest) <> 'text'
                 OR typeof(byte_length) <> 'integer'
                 OR typeof(payload) <> 'blob'
                 OR byte_length IS NOT length(payload)
                 OR byte_length > 4194304 AS nonconforming
         FROM workflow_artifact_payloads
         WHERE rowid > ?1
         ORDER BY rowid
         LIMIT ?2
     )";

/// Rows per copy statement. Published rows hold at most 4 MiB, so one statement
/// moves at most 128 MiB. The transaction lease renews and cancellation is
/// observed between statements, so a store of any size can finish, and shutdown
/// never waits for more than one batch.
const COPY_BATCH_ROWS: i64 = 32;

const COPY_ROW_BATCH_SQL: &str = "INSERT INTO workflow_artifact_payloads (
         rowid, payload_digest, byte_length, payload
     )
     SELECT rowid, payload_digest, byte_length, payload
     FROM workflow_artifact_payloads_published
     WHERE rowid > ?1
     ORDER BY rowid
     LIMIT ?2";

const LAST_COPIED_ROWID_SQL: &str = "SELECT max(rowid) FROM workflow_artifact_payloads";

const PUBLISHED_ROW_COUNT_SQL: &str = "SELECT count(*) FROM workflow_artifact_payloads_published";

/// What a store whose workflow tables differ from the final contract turned
/// out to be.
pub(super) enum PublishedArtifactCeilingShape {
    /// Exactly the published shape, with conforming rows.
    Upgradable,
    /// The published tables and identity, but with something the proven shape
    /// does not include. The reason names only contract objects and counts.
    Refused(String),
    /// Some other shape, reported by the ordinary drift refusal.
    Other,
}

#[tracing::instrument(
    name = "global_db.schema.workflow_artifact_ceiling.classify",
    level = "trace",
    skip_all
)]
pub(super) async fn classify_published_artifact_ceiling(
    conn: &impl QueryExecutor,
    workflow_tables: &[(&str, Option<&str>)],
) -> Result<PublishedArtifactCeilingShape> {
    if !tables_match_published_shape(workflow_tables) || !records_published_identity(conn).await? {
        return Ok(PublishedArtifactCeilingShape::Other);
    }
    let dependents = single_count(conn, DEPENDENT_OBJECT_COUNT_SQL, "inspect objects").await?;
    if dependents > 0 {
        return Ok(PublishedArtifactCeilingShape::Refused(format!(
            "workflow tables carry the published artifact ceiling, but {dependents} other schema \
             object(s) depend on `{ARTIFACT_TABLE}` or `workflow_schema`, so it cannot be upgraded \
             in place"
        )));
    }
    let nonconforming = count_nonconforming_rows(conn).await?;
    if nonconforming > 0 {
        return Ok(PublishedArtifactCeilingShape::Refused(format!(
            "workflow tables carry the published artifact ceiling, but {nonconforming} row(s) of \
             `{ARTIFACT_TABLE}` break that table's own constraints, so it cannot be upgraded in \
             place"
        )));
    }
    Ok(PublishedArtifactCeilingShape::Upgradable)
}

fn tables_match_published_shape(workflow_tables: &[(&str, Option<&str>)]) -> bool {
    workflow_tables.len() == WORKFLOW_TABLE_CONTRACTS_V1.len()
        && workflow_tables
            .iter()
            .zip(WORKFLOW_TABLE_CONTRACTS_V1)
            .all(|((name, sql), contract)| {
                let required = if contract.name == ARTIFACT_TABLE {
                    PUBLISHED_ARTIFACT_TABLE_SQL
                } else {
                    contract.sql
                };
                *name == contract.name && *sql == Some(required)
            })
}

async fn records_published_identity(conn: &impl QueryExecutor) -> Result<bool> {
    match read_recorded_workflow_identity(conn).await {
        Ok(Some(identity)) => Ok(identity.singleton == 1
            && identity.version == WORKFLOW_SCHEMA_VERSION_V1
            && identity.digest == PUBLISHED_WORKFLOW_DIGEST
            && !identity.extra),
        Ok(None) => Ok(false),
        Err(error) if error.is_transient_read_failure() => Err(global_db_operation_error(
            "inspect published workflow identity",
            error,
        )),
        // An identity that does not decode is reported by the drift refusal.
        Err(_) => Ok(false),
    }
}

async fn count_nonconforming_rows(conn: &impl QueryExecutor) -> Result<i64> {
    const OPERATION: &str = "inspect published artifact rows";
    let mut after_rowid = i64::MIN;
    let mut nonconforming = 0_i64;
    loop {
        let mut rows = conn
            .query(
                NONCONFORMING_ROW_PAGE_SQL,
                (after_rowid, ROW_CLASSIFICATION_PAGE_ROWS),
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let page = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .ok_or_else(|| global_db_operation_message(OPERATION, "a page returned no row"))?;
        let last_rowid = page
            .get::<Option<i64>>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let Some(last_rowid) = last_rowid else {
            return Ok(nonconforming);
        };
        nonconforming = nonconforming.saturating_add(
            page.get::<i64>(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        );
        after_rowid = last_rowid;
    }
}

async fn single_count(
    conn: &impl QueryExecutor,
    sql: &str,
    operation: &'static str,
) -> Result<i64> {
    let mut rows = conn
        .query(sql, ())
        .await
        .map_err(|error| global_db_operation_error(operation, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(operation, error))?
        .ok_or_else(|| global_db_operation_message(operation, "a count query returned no row"))?;
    row.get::<i64>(0)
        .map_err(|error| global_db_operation_error(operation, error))
}

/// Runs the upgrade in one transaction of its own. `cancellation` is observed
/// before every statement, so shutdown stops the copy within one batch and rolls
/// it back. `Some` is the refusal that stopped it before anything had changed.
#[tracing::instrument(
    name = "global_db.schema.workflow_artifact_ceiling.recover",
    level = "trace",
    skip_all
)]
pub(crate) async fn recover_published_artifact_ceiling(
    database: &Database,
    cancellation: &CancellationToken,
) -> Result<Option<RefusedAuthorityV1>> {
    const OPERATION: &str = "upgrade published workflow artifact table";
    const COMMIT: &str = "commit published workflow artifact upgrade";
    const ROLLBACK: &str = "roll back published workflow artifact upgrade";
    schema_install_checkpoint(cancellation, OPERATION)?;
    let transaction = database.begin_bulk_write_transaction(OPERATION).await?;
    let install = CancellableSchemaTransaction {
        transaction,
        cancellation,
    };
    let upgraded = Box::pin(upgrade_published_artifact_ceiling(&install))
        .await
        .and_then(|refused| schema_install_checkpoint(cancellation, COMMIT).map(|()| refused));
    let settled = tracing::Instrument::instrument(
        settle_schema_transaction(install, upgraded, COMMIT, ROLLBACK),
        tracing::trace_span!("global_db.schema.workflow_artifact_ceiling.settle"),
    )
    .await?;
    if settled.is_none() {
        tracing::info!("the published workflow artifact table now carries the final contract");
    }
    Ok(settled)
}

/// Returns the dropped table's pages to the filesystem one bounded compaction at
/// a time, so readers and writers interleave. A store created without
/// incremental auto-vacuum reports the same free pages after a pass, which ends
/// the loop; SQLite then reuses those pages for later writes.
#[tracing::instrument(
    name = "global_db.schema.workflow_artifact_ceiling.reclaim",
    level = "trace",
    skip_all
)]
pub(crate) async fn reclaim_freed_artifact_pages(
    database: &Database,
    cancellation: &CancellationToken,
) -> Result<()> {
    const OPERATION: &str = "reclaim published workflow artifact pages";
    let mut remaining = u64::MAX;
    loop {
        schema_install_checkpoint(cancellation, OPERATION)?;
        let (_, _, free_pages) = database.storage_page_counts().await?;
        if free_pages == 0 || free_pages >= remaining {
            return Ok(());
        }
        remaining = free_pages;
        database
            .run_incremental_vacuum(WORKFLOW_ARTIFACT_RECLAIM_PAGES)
            .await?;
    }
}

/// Upgrades the published shape inside the caller's transaction, or returns the
/// refusal that stops it before anything of the upgrade has run.
///
/// Classification ran on a read connection, possibly long before, so the shape
/// is proven again under the write lock before anything changes: a peer that
/// upgraded in the meantime makes this a no-op, and one that changed the store
/// into anything else refuses. Every step shares the caller's transaction, so
/// any failure leaves the store exactly as found. Only payload bytes the transaction itself wrote
/// are read back, which SQLite already guarantees, so the copy is checked by row
/// count rather than by reading every payload twice more.
#[tracing::instrument(
    name = "global_db.schema.workflow_artifact_ceiling.upgrade",
    level = "trace",
    skip_all
)]
pub(crate) async fn upgrade_published_artifact_ceiling(
    transaction: &(impl Executor + Sync),
) -> Result<Option<RefusedAuthorityV1>> {
    let proven = Box::pin(inspect_workflow_schema_for_admission(transaction)).await?;
    match proven {
        Ok(WorkflowSchemaAdmission::Complete) => return Ok(None),
        Ok(WorkflowSchemaAdmission::UpgradePublishedArtifactCeiling) => {}
        Ok(WorkflowSchemaAdmission::Create) => {
            return Err(upgrade_refusal(
                "the workflow tables disappeared before the upgrade began",
            ));
        }
        Err(refused) => return Ok(Some(refused)),
    }
    let final_table_sql = WORKFLOW_TABLE_CONTRACTS_V1
        .iter()
        .find(|contract| contract.name == ARTIFACT_TABLE)
        .map(|contract| contract.sql)
        .ok_or_else(|| upgrade_refusal("the final contract has no artifact table"))?;

    if let Some(refused) = move_published_table_aside(transaction).await? {
        return Ok(Some(refused));
    }
    run_step(
        transaction,
        final_table_sql,
        "create the final artifact table",
    )
    .await?;
    copy_artifact_rows(transaction).await?;
    tracing::Instrument::instrument(
        run_step(
            transaction,
            &format!("DROP TABLE {DISPLACED_ARTIFACT_TABLE}"),
            "drop the published artifact table",
        ),
        tracing::trace_span!("global_db.schema.workflow_artifact_ceiling.drop"),
    )
    .await?;
    record_final_identity(transaction).await?;

    let upgraded = Box::pin(inspect_workflow_schema_for_admission(transaction)).await?;
    match upgraded {
        Ok(WorkflowSchemaAdmission::Complete) => Ok(None),
        Ok(_) => Err(upgrade_refusal(
            "the upgraded workflow tables did not reach the final contract",
        )),
        Err(refused) => Err(refused.error()),
    }
}

/// What a store classified upgradable would decide if it attached now, for a
/// caller that will roll this transaction back.
///
/// `ALTER TABLE` re-parses the whole schema, so an unrelated view or trigger
/// that no longer resolves refuses the move. No read-only query predicts that
/// verdict. The rename edits only the catalog, so rehearsing it reads no row. The
/// other steps cannot refuse what classification proved, so they do not run.
pub(super) async fn rehearse_published_artifact_ceiling_upgrade(
    transaction: &(impl Executor + Sync),
) -> Result<Option<RefusedAuthorityV1>> {
    move_published_table_aside(transaction).await
}

/// A rename that cannot run changes nothing, so the store stays refused for its
/// workflow features like every other shape this binary will not read.
async fn move_published_table_aside(
    transaction: &(impl Executor + Sync),
) -> Result<Option<RefusedAuthorityV1>> {
    const STEP: &str = "move the published artifact table aside";
    let renamed = transaction
        .execute_batch(&format!(
            "ALTER TABLE {ARTIFACT_TABLE} RENAME TO {DISPLACED_ARTIFACT_TABLE}"
        ))
        .await;
    match renamed {
        Ok(()) => Ok(None),
        Err(error) if is_store_refusal(&error) => Ok(Some(workflow_schema_refusal(
            "workflow tables carry the published artifact ceiling, but the schema of this store \
             refused moving `workflow_artifact_payloads` aside, so it cannot be upgraded in place",
        ))),
        Err(error) => Err(global_db_operation_error(STEP, error)),
    }
}

async fn run_step(
    transaction: &(impl Executor + Sync),
    sql: &str,
    step: &'static str,
) -> Result<()> {
    transaction
        .execute_batch(sql)
        .await
        .map_err(|error| upgrade_step_error(step, error))
}

#[tracing::instrument(
    name = "global_db.schema.workflow_artifact_ceiling.copy",
    level = "trace",
    skip_all
)]
async fn copy_artifact_rows(transaction: &(impl Executor + Sync)) -> Result<()> {
    const STEP: &str = "copy artifact rows";
    let published = single_count(transaction, PUBLISHED_ROW_COUNT_SQL, STEP).await?;
    let mut after_rowid = i64::MIN;
    let mut copied = 0_i64;
    loop {
        let batch = transaction
            .execute(COPY_ROW_BATCH_SQL, (after_rowid, COPY_BATCH_ROWS))
            .await
            .map_err(|error| upgrade_step_error(STEP, error))?;
        if batch == 0 {
            break;
        }
        copied = copied.saturating_add(i64::try_from(batch).unwrap_or(i64::MAX));
        after_rowid = single_count(transaction, LAST_COPIED_ROWID_SQL, STEP).await?;
    }
    if copied != published {
        return Err(upgrade_refusal(
            "the copied artifact rows differ from the published rows",
        ));
    }
    Ok(())
}

async fn record_final_identity(transaction: &(impl Executor + Sync)) -> Result<()> {
    const STEP: &str = "record the final workflow identity";
    let updated = transaction
        .execute(
            "UPDATE workflow_schema SET definition_digest = ?1
             WHERE singleton = 1 AND schema_version = ?2 AND definition_digest = ?3",
            (
                WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1,
                WORKFLOW_SCHEMA_VERSION_V1,
                PUBLISHED_WORKFLOW_DIGEST,
            ),
        )
        .await
        .map_err(|error| upgrade_step_error(STEP, error))?;
    if updated != 1 {
        return Err(upgrade_refusal(
            "the published workflow identity changed during the upgrade",
        ));
    }
    Ok(())
}

/// `SQLITE_ERROR`: the statement itself cannot run against this schema, for
/// example an unrelated broken trigger failing the rename's schema check.
const SQLITE_ERROR: i32 = 1;

fn is_store_refusal(error: &engine::Error) -> bool {
    error.is_deterministic_refusal() || error.sqlite_code() == Some(SQLITE_ERROR)
}

/// A failure that will recur on every open means this store cannot take the
/// upgrade, which is the store's typed refusal. One that can clear by itself,
/// such as a held lock or a full disk, stays an operation failure the store's
/// convergence task retries, so a passing condition never tells an operator to
/// reset.
fn upgrade_step_error(step: &'static str, error: engine::Error) -> TraceDecayError {
    if is_store_refusal(&error) {
        upgrade_refusal(&format!("the store refused the step to {step}"))
    } else {
        global_db_operation_error(step, error)
    }
}

fn upgrade_refusal(detail: &str) -> TraceDecayError {
    TraceDecayError::reset_required(
        "workflow",
        format!(
            "the published artifact ceiling could not be upgraded and the store was left as \
             found: {detail}"
        ),
    )
}
