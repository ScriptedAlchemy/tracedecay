//! Convergence for a v34-stamped project store whose inventory is free of
//! retired projections.
//!
//! Every release from v0.1.0-beta.25 through v0.1.0-beta.37 created one
//! byte-identical `tracedecay.db` stamped `user_version` 34 — the exact SQL
//! lives in `tests/fixtures/project-store-released-v34.sql`, whose header
//! carries the tag-to-inventory table. The current contract differs from it in
//! objects that hold no data of their own (two absent indexes, a renamed
//! external-source mutation family, the runtime-writer ledger) and in two
//! diagnostics tables that gained a `publication_revision` column and a wider
//! primary key. Those differences are convergeable and are converged here.
//!
//! Every released store also carries the `semantic_vector_*` staging family
//! that v36 retired with dense code retrieval. That family has no forward
//! path, so a store carrying it is refused before this module writes anything,
//! the same way [`super::final_shape`] refuses shapes no release ever wrote.

use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::db::engine::{Executor, QueryExecutor, params};

const OPERATION: &str = "converge released project schema";

/// One table the released shape carries in a form `SQLite` cannot alter in
/// place: widening a primary key, adding a `NOT NULL` column with no default,
/// and relaxing a `CHECK` all require a rebuild.
///
/// A table is rebuilt when its stored DDL differs from the one the current
/// contract expects, so this list needs no record of which release changed
/// what — [`super::final_shape`] stays the single authority on the expected
/// shape.
struct ReleasedTableRebuild {
    table: &'static str,
    /// The columns the released table carried, written verbatim into the
    /// canonical table so the rebuild is a copy rather than a re-derivation.
    released_columns: &'static str,
    /// The column the canonical shape added, with the value every released
    /// row takes. `None` when only a constraint changed.
    added_column: Option<(&'static str, &'static str)>,
}

/// A canonical DDL batch and every released table it recreates.
///
/// The batch owns the table's indexes and triggers too, which the rebuild's
/// `DROP TABLE` removes, so each group drops all of its tables before
/// replaying the batch once.
struct ReleasedRebuildGroup {
    canonical: &'static str,
    tables: &'static [ReleasedTableRebuild],
}

/// Diagnostics rows published before revisions existed are the first revision
/// of their generation.
const RELEASED_V34_REBUILDS: &[ReleasedRebuildGroup] = &[ReleasedRebuildGroup {
    canonical: tracedecay_store::GENERATION_DIAGNOSTICS_SCHEMA_DDL,
    tables: &[
        ReleasedTableRebuild {
            table: "diagnostic_generation_publications",
            released_columns: "generation_id, record_state, state_generation, published_at",
            added_column: Some(("publication_revision", "1")),
        },
        ReleasedTableRebuild {
            table: "generation_diagnostics",
            released_columns: "diagnostic_anchor, generation_id, repository, worktree, \
                               reference, source_revision, file_occurrence_id, \
                               content_digest, symbol_occurrence_id, span_start, span_end, \
                               code, severity, message, message_digest, producer_kind, \
                               producer, analyzer_revision, configuration_revision, \
                               sanitization_receipt, evidence_class, collected_at, \
                               record_state, state_generation, persisted_at",
            added_column: Some(("publication_revision", "1")),
        },
    ],
}];

fn failure(message: String) -> TraceDecayError {
    TraceDecayError::Database {
        message,
        operation: OPERATION.to_owned(),
    }
}

/// Converges a store stamped with the released version to the shape this
/// binary creates, carrying every row forward.
///
/// A store still carrying a retired projection is refused first, inside the
/// caller's transaction, so the refusal leaves the store byte-identical.
/// Runs before the payload-digest step, whose own admission check requires the
/// current shape everywhere but the digest objects. Idempotent by
/// construction: the rebuilds are selected by the released column being
/// absent, the schema installs are `CREATE ... IF NOT EXISTS`, and the row
/// moves are the same resumable statements the registered stores converge
/// with.
pub(super) async fn converge_released_project_schema(conn: &(impl Executor + Sync)) -> Result<()> {
    super::require_no_retired_sqlite_projection_object(conn).await?;
    for group in RELEASED_V34_REBUILDS {
        rebuild_released_group(conn, group).await?;
    }
    // Replaces the released alias-immutability trigger, which guarded every
    // update instead of only the fields that must not change.
    crate::db::retrieval_anchor_schema::install_retrieval_anchor_schema(conn, OPERATION).await?;
    crate::db::external_source::install_external_source_schema(conn, OPERATION).await?;
    crate::db::external_source::retire_mutation_copies_in_transaction(conn).await?;
    super::install_runtime_writer_ledger(conn, OPERATION).await
}

/// Rebuilds every table in one group whose stored DDL is not the one the
/// current contract expects.
///
/// The released rows are copied aside, the tables dropped with their indexes
/// and triggers, the canonical batch replayed, and the rows written back
/// through the canonical column list. A table's own triggers are dropped again
/// before the rows return: a census trigger that fired per copied row would
/// count work its authority already records. Replaying the batch afterwards
/// restores them, which is sound because every statement in these batches
/// creates its object only if it is absent.
///
/// A store this binary created has no pending table and pays one catalog probe
/// per listed table.
async fn rebuild_released_group(
    conn: &(impl Executor + Sync),
    group: &ReleasedRebuildGroup,
) -> Result<()> {
    let mut pending = Vec::new();
    for rebuild in group.tables {
        if released_table_pending(conn, rebuild.table).await? {
            pending.push(rebuild);
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    let mut triggers = Vec::new();
    for rebuild in &pending {
        triggers.extend(table_triggers(conn, rebuild.table).await?);
    }
    // Children of a rebuilt table are valid again before this transaction
    // commits, which is when deferred enforcement checks them.
    batch(conn, "PRAGMA defer_foreign_keys = ON;").await?;
    for rebuild in &pending {
        let scratch = scratch_table(rebuild.table);
        batch(
            conn,
            &format!(
                "CREATE TABLE {scratch} AS SELECT * FROM {table};
                 DROP TABLE {table};",
                table = rebuild.table
            ),
        )
        .await?;
    }
    batch(conn, group.canonical).await?;
    for trigger in &triggers {
        batch(conn, &format!("DROP TRIGGER IF EXISTS {trigger};")).await?;
    }
    for rebuild in &pending {
        let scratch = scratch_table(rebuild.table);
        let columns = rebuild.released_columns;
        let (added, value) = match rebuild.added_column {
            Some((added, value)) => (format!("{added}, "), format!("{value}, ")),
            None => (String::new(), String::new()),
        };
        batch(
            conn,
            &format!(
                "INSERT INTO {table}({added}{columns})
                 SELECT {value}{columns} FROM {scratch};
                 DROP TABLE {scratch};",
                table = rebuild.table
            ),
        )
        .await?;
    }
    batch(conn, group.canonical).await
}

/// Reports whether a table exists carrying DDL other than the one this binary
/// creates.
async fn released_table_pending(conn: &impl QueryExecutor, table: &str) -> Result<bool> {
    let Some(expected) = super::final_shape::expected_object_sql(table)? else {
        return Err(failure(format!(
            "'{table}' is not part of the shape this binary creates"
        )));
    };
    Ok(stored_object_sql(conn, table)
        .await?
        .is_some_and(|stored| stored != expected))
}

/// Names the copy a rebuild reads its released rows out of. The copy lives and
/// dies inside the caller's transaction, so an interrupted convergence leaves
/// neither it nor a half-rebuilt table behind.
fn scratch_table(table: &str) -> String {
    format!("{table}_released_v34")
}

async fn batch(conn: &impl Executor, sql: &str) -> Result<()> {
    conn.execute_batch(sql)
        .await
        .map_err(|error| failure(format!("failed to converge released schema: {error}")))
}

async fn stored_object_sql(conn: &impl QueryExecutor, name: &str) -> Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(sql, '') FROM sqlite_master WHERE name = ?1",
            params![name],
        )
        .await
        .map_err(|error| failure(format!("failed to read the stored DDL of {name}: {error}")))?;
    let Some(row) = rows.next().await.map_err(|error| {
        failure(format!(
            "failed to decode the stored DDL of {name}: {error}"
        ))
    })?
    else {
        return Ok(None);
    };
    row.get::<String>(0).map(Some).map_err(|error| {
        failure(format!(
            "failed to decode the stored DDL of {name}: {error}"
        ))
    })
}

/// Every trigger defined on one table, in catalog order.
async fn table_triggers(conn: &impl QueryExecutor, table: &str) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master
             WHERE type = 'trigger' AND tbl_name = ?1 ORDER BY name",
            params![table],
        )
        .await
        .map_err(|error| failure(format!("failed to list the triggers of {table}: {error}")))?;
    let mut triggers = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| failure(format!("failed to read the triggers of {table}: {error}")))?
    {
        triggers
            .push(row.get::<String>(0).map_err(|error| {
                failure(format!("failed to decode a {table} trigger: {error}"))
            })?);
    }
    Ok(triggers)
}
