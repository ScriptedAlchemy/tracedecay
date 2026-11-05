//! In-place upgrade of the one published workflow shape the final contract
//! left behind.
//!
//! Every tagged release through 1.0.0-beta.92 created the same nine workflow
//! tables byte for byte and recorded the same identity digest. 1.0.0-beta.93
//! dropped the artifact byte ceiling from `workflow_artifact_payloads`, which
//! changed that one table's `CHECK` and the identity digest, with no way for an
//! existing store to follow. The digest is an opaque published label rather
//! than a hash of the stored SQL, so this recognizes the shape from the exact
//! stored text together with the recorded identity. Anything else, however
//! close, keeps the typed refusal.

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::db::engine::{self, Executor, QueryExecutor};
use tracedecay_rusqlite_runtime::workflow::{
    WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1, WORKFLOW_SCHEMA_VERSION_V1, WORKFLOW_TABLE_CONTRACTS_V1,
};

use super::{WorkflowSchemaAdmission, inspect_workflow_schema_for_admission};
use crate::{global_db_operation_error, global_db_operation_message};

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

const DEPENDENT_OBJECT_COUNT_SQL: &str = "SELECT count(*) FROM sqlite_master
     WHERE name <> 'workflow_artifact_payloads'
       AND name <> 'sqlite_autoindex_workflow_artifact_payloads_1'
       AND (tbl_name = 'workflow_artifact_payloads'
            OR instr(lower(sql), 'workflow_artifact_payloads') > 0)";

/// Rows the published constraints would have rejected can only come from a
/// store written around them, so they are not the proven shape.
const NONCONFORMING_ROW_COUNT_SQL: &str = "SELECT count(*) FROM workflow_artifact_payloads
     WHERE typeof(payload) <> 'blob'
        OR byte_length <> length(payload)
        OR byte_length > 4194304";

const COPY_ROWS_SQL: &str = "INSERT INTO workflow_artifact_payloads (
         rowid, payload_digest, byte_length, payload
     )
     SELECT rowid, payload_digest, byte_length, payload
     FROM workflow_artifact_payloads_published
     ORDER BY rowid";

const COPY_EVIDENCE_SQL: &str = "SELECT
     (SELECT count(*) FROM workflow_artifact_payloads_published),
     (SELECT count(*) FROM workflow_artifact_payloads),
     (SELECT count(*)
      FROM workflow_artifact_payloads_published AS published
      JOIN workflow_artifact_payloads AS upgraded ON upgraded.rowid = published.rowid
      WHERE upgraded.payload_digest = published.payload_digest
        AND upgraded.byte_length = published.byte_length
        AND upgraded.payload = published.payload)";

/// What a store whose workflow tables differ from the final contract turned
/// out to be.
pub(super) enum PublishedArtifactCeilingShape {
    /// Exactly the published shape, with conforming rows.
    Upgradable,
    /// The published tables and identity, but with something an upgrade could
    /// not carry across. The reason names only contract objects and counts.
    Refused(String),
    /// Some other shape, reported by the ordinary drift refusal.
    Other,
}

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
            "workflow tables carry the artifact ceiling published through 1.0.0-beta.92, but \
             {dependents} other schema object(s) refer to `{ARTIFACT_TABLE}`, so it cannot be \
             upgraded in place"
        )));
    }
    let nonconforming = single_count(conn, NONCONFORMING_ROW_COUNT_SQL, "inspect rows").await?;
    if nonconforming > 0 {
        return Ok(PublishedArtifactCeilingShape::Refused(format!(
            "workflow tables carry the artifact ceiling published through 1.0.0-beta.92, but \
             {nonconforming} row(s) of `{ARTIFACT_TABLE}` break that table's own constraints, so \
             it cannot be upgraded in place"
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
    const OPERATION: &str = "inspect published workflow identity";
    let mut rows = conn
        .query(
            "SELECT singleton, schema_version, definition_digest FROM workflow_schema
             ORDER BY singleton LIMIT 2",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let Some(identity) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    else {
        return Ok(false);
    };
    let singleton = identity
        .get::<i64>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let version = identity
        .get::<i64>(1)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let digest = identity
        .get::<String>(2)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let extra = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .is_some();
    Ok(singleton == 1
        && version == WORKFLOW_SCHEMA_VERSION_V1
        && digest == PUBLISHED_WORKFLOW_DIGEST
        && !extra)
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

/// Upgrades the published shape inside the caller's admission transaction.
///
/// Classification ran on a read connection, so the shape is proven again under
/// the write lock before anything changes: a peer that upgraded in the meantime
/// makes this a no-op, and one that changed the store into anything else
/// refuses. Every step shares the caller's transaction, so any failure leaves
/// the store exactly as found.
pub(super) async fn upgrade_published_artifact_ceiling(
    transaction: &(impl Executor + Sync),
) -> Result<()> {
    let proven = Box::pin(inspect_workflow_schema_for_admission(transaction)).await?;
    match proven {
        Ok(WorkflowSchemaAdmission::Complete) => return Ok(()),
        Ok(WorkflowSchemaAdmission::UpgradePublishedArtifactCeiling) => {}
        Ok(WorkflowSchemaAdmission::Create) => {
            return Err(upgrade_refusal(
                "the workflow tables disappeared before the upgrade began",
            ));
        }
        Err(refused) => return Err(refused.error()),
    }
    let final_table_sql = WORKFLOW_TABLE_CONTRACTS_V1
        .iter()
        .find(|contract| contract.name == ARTIFACT_TABLE)
        .map(|contract| contract.sql)
        .ok_or_else(|| upgrade_refusal("the final contract has no artifact table"))?;

    run_step(
        transaction,
        &format!("ALTER TABLE {ARTIFACT_TABLE} RENAME TO {DISPLACED_ARTIFACT_TABLE}"),
        "move the published artifact table aside",
    )
    .await?;
    run_step(
        transaction,
        final_table_sql,
        "create the final artifact table",
    )
    .await?;
    run_step(transaction, COPY_ROWS_SQL, "copy artifact rows").await?;
    require_every_row_copied(transaction).await?;
    run_step(
        transaction,
        &format!("DROP TABLE {DISPLACED_ARTIFACT_TABLE}"),
        "drop the published artifact table",
    )
    .await?;
    record_final_identity(transaction).await?;

    let upgraded = Box::pin(inspect_workflow_schema_for_admission(transaction)).await?;
    match upgraded {
        Ok(WorkflowSchemaAdmission::Complete) => Ok(()),
        Ok(_) => Err(upgrade_refusal(
            "the upgraded workflow tables did not reach the final contract",
        )),
        Err(refused) => Err(refused.error()),
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

async fn require_every_row_copied(transaction: &(impl Executor + Sync)) -> Result<()> {
    const STEP: &str = "verify copied artifact rows";
    let mut rows = transaction
        .query(COPY_EVIDENCE_SQL, ())
        .await
        .map_err(|error| upgrade_step_error(STEP, error))?;
    let evidence = rows
        .next()
        .await
        .map_err(|error| upgrade_step_error(STEP, error))?
        .ok_or_else(|| upgrade_refusal("the artifact copy evidence returned no row"))?;
    let count = |index| {
        evidence
            .get::<i64>(index)
            .map_err(|error| upgrade_step_error(STEP, error))
    };
    let (published, upgraded, identical) = (count(0)?, count(1)?, count(2)?);
    if published != upgraded || published != identical {
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

/// A failure that will recur on every open means this store cannot take the
/// upgrade, which is the store's typed refusal. One that can clear by itself,
/// such as a held lock or a full disk, stays an operation failure the next
/// open retries, so a passing condition never tells an operator to reset.
fn upgrade_step_error(step: &'static str, error: engine::Error) -> TraceDecayError {
    if error.is_deterministic_refusal() || error.sqlite_code() == Some(SQLITE_ERROR) {
        upgrade_refusal(&format!("the store refused the step to {step}"))
    } else {
        global_db_operation_error(step, error)
    }
}

fn upgrade_refusal(detail: &str) -> TraceDecayError {
    TraceDecayError::reset_required(
        "workflow",
        format!(
            "the artifact ceiling published through 1.0.0-beta.92 could not be upgraded and the \
             store was left as found: {detail}"
        ),
    )
}
