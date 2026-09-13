//! Additive external-source state schema owned by the canonical database,
//! and the store-sized rewrite that retires its payload-copying predecessors.

use crate::db::engine::{Executor, QueryExecutor, params};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_rusqlite_runtime::repository::{
    RETIRED_MUTATION_COPY_CHUNK_ROWS, RETIRED_MUTATION_COPY_TABLES,
};

/// Installs the external-source state shape. Cheap idempotent DDL only, so it
/// belongs inside a caller's leased schema transaction.
///
/// Retiring a store's payload-copying predecessors is store-sized work and
/// runs separately through [`migrate_retired_mutation_copy_tables`].
pub async fn install_external_source_schema(
    connection: &impl Executor,
    operation: &str,
) -> Result<()> {
    connection
        .execute_batch(tracedecay_rusqlite_runtime::repository::EXTERNAL_SOURCE_SCHEMA_V1)
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("{operation}: failed to install external source state: {error}"),
            operation: operation.to_owned(),
        })
}

/// Rewrites every retired payload-copying table into its digest-referencing
/// successor, in bounded chunks, and drops it once it is empty.
///
/// This is store-sized work: on a store with 189k `external_source_commit_
/// receipts_v1` rows carrying about a gigabyte of `receipt_json`, the whole
/// rewrite measured around ten minutes. Inside the leased schema transaction
/// that made every open of a large store fail its per-statement execution
/// limit, so it runs here instead — after admission, on the writer, as
/// bounded work that never blocks admission or ordinary retrieval.
///
/// The retired table's remaining contents are the durable progress: each
/// chunk moves its rows and removes them from the retired table in one
/// transaction, so an interrupted migration resumes exactly where it stopped,
/// no row is moved twice — the successors are keyed and the moves are
/// `INSERT OR IGNORE` — and none is lost. A store already at the current
/// shape carries none of these tables and pays one catalog probe each.
pub async fn migrate_retired_mutation_copy_tables(conn: &crate::db::Database) -> Result<()> {
    const OPERATION: &str = "migrate retired external source mutation copies";
    for (retired_table, chunk_statements) in RETIRED_MUTATION_COPY_TABLES {
        if !table_exists(&conn.read_connection(), retired_table)
            .await
            .map_err(|error| {
                migration_failure(
                    format!("failed to probe for retired table {retired_table}"),
                    error,
                )
            })?
        {
            continue;
        }
        loop {
            let transaction = conn.begin_bulk_write_transaction(OPERATION).await?;
            let Some(ceiling) = retired_chunk_ceiling(&transaction, retired_table).await? else {
                // Empty: every row has moved, so only the retired table's
                // own identity is left to retire.
                transaction
                    .execute_batch(&format!("DROP TABLE {retired_table}"))
                    .await
                    .map_err(|error| {
                        migration_failure(format!("failed to drop {retired_table}"), error)
                    })?;
                transaction.commit().await?;
                break;
            };
            for statement in *chunk_statements {
                transaction
                    .execute(statement, params![ceiling])
                    .await
                    .map_err(|error| {
                        migration_failure(
                            format!(
                                "failed to move {retired_table} rows through rowid {ceiling} \
                                 into their digest-referencing successor"
                            ),
                            error,
                        )
                    })?;
            }
            transaction.commit().await?;
        }
    }
    Ok(())
}

/// Retires the payload-copying predecessors inside a caller's transaction.
///
/// The released project store carries them and the shape this binary admits
/// does not, so the one-time convergence of such a store has to finish the
/// move before admission rather than after it. Each chunk is still bounded by
/// the same statements [`migrate_retired_mutation_copy_tables`] uses; what a
/// caller gives up is resumability, which a one-shot upgrade transaction does
/// not have anyway.
pub(super) async fn retire_mutation_copies_in_transaction(
    conn: &(impl Executor + Sync),
) -> Result<()> {
    for (retired_table, chunk_statements) in RETIRED_MUTATION_COPY_TABLES {
        if !table_exists(conn, retired_table).await? {
            continue;
        }
        while let Some(ceiling) = retired_chunk_ceiling(conn, retired_table).await? {
            for statement in *chunk_statements {
                conn.execute(statement, params![ceiling])
                    .await
                    .map_err(|error| {
                        migration_failure(
                            format!("failed to move {retired_table} rows through rowid {ceiling}"),
                            error,
                        )
                    })?;
            }
        }
        conn.execute_batch(&format!("DROP TABLE {retired_table}"))
            .await
            .map_err(|error| migration_failure(format!("failed to drop {retired_table}"), error))?;
    }
    Ok(())
}

fn migration_failure(message: String, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{message}: {error}"),
        operation: "migrate retired external source mutation copies".to_owned(),
    }
}

/// The inclusive `rowid` of the last row in the next chunk, or `None` once the
/// retired table is empty.
async fn retired_chunk_ceiling(
    connection: &impl QueryExecutor,
    table: &str,
) -> Result<Option<i64>> {
    let mut rows = connection
        .query(
            &format!(
                "SELECT MAX(rowid) FROM (
                    SELECT rowid FROM {table}
                    ORDER BY rowid
                    LIMIT {RETIRED_MUTATION_COPY_CHUNK_ROWS}
                 )"
            ),
            (),
        )
        .await
        .map_err(|error| {
            migration_failure(format!("failed to read the next {table} chunk"), error)
        })?;
    let Some(row) = rows.next().await.map_err(|error| {
        migration_failure(format!("failed to step the next {table} chunk"), error)
    })?
    else {
        return Err(migration_failure(
            format!("chunk aggregate over {table} returned no row"),
            "aggregate queries always return one row",
        ));
    };
    row.get::<Option<i64>>(0).map_err(|error| {
        migration_failure(format!("failed to decode the next {table} chunk"), error)
    })
}

async fn table_exists(connection: &impl QueryExecutor, table: &str) -> Result<bool> {
    let mut rows = connection
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            (table,),
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

#[cfg(test)]
mod tests {
    use super::{
        RETIRED_MUTATION_COPY_CHUNK_ROWS, install_external_source_schema,
        migrate_retired_mutation_copy_tables,
    };
    use crate::db::engine::QueryExecutor;
    use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

    /// The retired shape exactly as an older binary left it, beside the
    /// current schema the fixture runtime already installed.
    const SEED_RETIRED_SHAPE: &str = "
        CREATE TABLE external_source_objects_v1 (
            binding_id TEXT NOT NULL, native_object_digest TEXT NOT NULL,
            partition_digest TEXT NOT NULL, mutation_digest TEXT NOT NULL,
            mutation_json TEXT NOT NULL,
            PRIMARY KEY (binding_id, native_object_digest));
        CREATE TABLE external_source_projected_objects_v1 (
            binding_id TEXT NOT NULL, native_object_digest TEXT NOT NULL,
            mutation_json TEXT NOT NULL,
            PRIMARY KEY (binding_id, native_object_digest));
        CREATE TABLE external_source_projection_effects_v1 (
            binding_id TEXT NOT NULL, projection_digest TEXT NOT NULL,
            effect_index INTEGER NOT NULL, native_object_digest TEXT NOT NULL,
            effect_json TEXT NOT NULL, mutation_json TEXT NOT NULL,
            PRIMARY KEY (binding_id, projection_digest, effect_index));
        CREATE TABLE external_source_commit_receipts_v1 (
            binding_id TEXT NOT NULL, idempotency_key TEXT NOT NULL,
            request_digest TEXT NOT NULL, definition_revision INTEGER NOT NULL,
            binding_revision INTEGER NOT NULL, predecessor_frontier_digest TEXT NOT NULL,
            successor_frontier_digest TEXT NOT NULL, receipt_digest TEXT NOT NULL,
            receipt_json TEXT NOT NULL,
            PRIMARY KEY (binding_id, idempotency_key));";

    /// One retired receipt whose encoding carries every value the slim shape
    /// keeps: two mutation digests, a source frontier, and a null prior one.
    const SEED_RETIRED_ROWS: &str = "
        INSERT INTO external_source_objects_v1 VALUES
            ('b', 'sha256:obj', 'sha256:part', 'sha256:mut1', '{\"mutation_digest\":\"sha256:mut1\"}');
        INSERT INTO external_source_projected_objects_v1 VALUES
            ('b', 'sha256:obj', '{\"x\":1,\"mutation_digest\":\"sha256:mut1\"}');
        INSERT INTO external_source_projection_effects_v1 VALUES
            ('b', 'sha256:proj', 0, 'sha256:obj', '{\"effect\":true}',
             '{\"mutation_digest\":\"sha256:mut1\"}');
        INSERT INTO external_source_commit_receipts_v1 VALUES
            ('b', 'sha256:key', 'sha256:req', 1, 1, 'root', 'sha256:front1', 'sha256:rcpt',
             '{\"idempotency_key\":\"sha256:key\",\"prior_source_frontier\":null,'
             || '\"source_frontier\":{\"binding\":{\"id\":\"b\"},\"partitions\":{},\"digest\":\"sha256:front1\"},'
             || '\"mutations\":[{\"x\":1,\"mutation_digest\":\"sha256:mut1\"},{\"x\":2,\"mutation_digest\":\"sha256:mut2\"}],'
             || '\"receipt_digest\":\"sha256:rcpt\"}');";

    /// Seeds the retired object table in batches that each fit inside an
    /// ordinary write, so the fixture itself never depends on the limit this
    /// test is about.
    async fn seed_retired_objects(db: &Database, rows: i64) {
        const BATCH: i64 = 250_000;
        let mut seeded = 0;
        while seeded < rows {
            let batch = BATCH.min(rows - seeded);
            commit_batch(
                db,
                "seed retired objects",
                &format!(
                    "INSERT INTO external_source_objects_v1
                     WITH RECURSIVE row_index(index_value) AS (
                        SELECT {start} UNION ALL
                        SELECT index_value + 1 FROM row_index WHERE index_value < {end}
                     )
                     SELECT 'b', 'sha256:obj' || index_value, 'sha256:part',
                            'sha256:mut' || index_value,
                            '{{\"mutation_digest\":\"sha256:mut' || index_value || '\"}}'
                     FROM row_index",
                    start = seeded + 1,
                    end = seeded + batch,
                ),
            )
            .await;
            seeded += batch;
        }
    }

    async fn count(connection: &impl QueryExecutor, sql: &str) -> i64 {
        let mut rows = connection.query(sql, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    async fn open_store(path: &std::path::Path) -> Database {
        let authority = DatabaseAuthority::acquire_test(path, "external source migration").unwrap();
        let (db, _) =
            Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        // Leaking the authority keeps the published runtime's write scope for
        // the whole test; it is dropped with the process.
        std::mem::forget(authority);
        db
    }

    async fn commit_batch(db: &Database, operation: &'static str, sql: &str) {
        let writer = db.begin_write_transaction(operation).await.unwrap();
        writer.execute_batch(sql).await.unwrap();
        writer.commit().await.unwrap();
    }

    /// A store still carrying the tables that duplicated `mutation_json` is
    /// moved to the digest-referencing shape in place: every current row
    /// survives with the digest its own encoding carried, the retired tables
    /// are dropped, and a second run is a no-op.
    #[tokio::test]
    async fn migration_moves_retired_mutation_copies_by_reference() {
        let temp = tempfile::tempdir().unwrap();
        let db = open_store(&temp.path().join("graph.db")).await;
        commit_batch(
            &db,
            "seed retired shape",
            &format!("{SEED_RETIRED_SHAPE}{SEED_RETIRED_ROWS}"),
        )
        .await;

        migrate_retired_mutation_copy_tables(&db).await.unwrap();

        let reader = db.read_connection();
        assert_eq!(
            count(
                &reader,
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN (
                    'external_source_objects_v1', 'external_source_projected_objects_v1',
                    'external_source_projection_effects_v1', 'external_source_commit_receipts_v1')",
            )
            .await,
            0,
            "every retired table is dropped"
        );
        // The receipt is slim: mutation digests where mutations were, the
        // frontier digest where the frontier was, and the frontier itself
        // stored once under its digest.
        assert_eq!(
            count(
                &reader,
                "SELECT COUNT(*) FROM external_source_commit_receipts_v2
                 WHERE binding_id = 'b'
                   AND json_extract(receipt_json, '$.source_frontier') = 'sha256:front1'
                   AND json_type(receipt_json, '$.prior_source_frontier') = 'null'
                   AND json_extract(receipt_json, '$.mutations[0]') = 'sha256:mut1'
                   AND json_extract(receipt_json, '$.mutations[1]') = 'sha256:mut2'
                   AND json_array_length(receipt_json, '$.mutations') = 2
                   AND json_extract(receipt_json, '$.receipt_digest') = 'sha256:rcpt'"
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &reader,
                "SELECT COUNT(*) FROM external_source_frontiers_v1
                 WHERE binding_id = 'b' AND frontier_digest = 'sha256:front1'
                   AND json_extract(frontier_json, '$.binding.id') = 'b'"
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &reader,
                "SELECT COUNT(*) FROM external_source_objects_v2
                 WHERE binding_id = 'b' AND mutation_digest = 'sha256:mut1'"
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &reader,
                "SELECT COUNT(*) FROM external_source_projected_objects_v2
                 WHERE binding_id = 'b' AND mutation_digest = 'sha256:mut1'"
            )
            .await,
            1,
            "the projected object's digest is read out of its retired encoding"
        );
        assert_eq!(
            count(
                &reader,
                "SELECT COUNT(*) FROM external_source_projection_effects_v2
                 WHERE binding_id = 'b' AND mutation_digest = 'sha256:mut1'
                   AND effect_json = '{\"effect\":true}'"
            )
            .await,
            1
        );

        migrate_retired_mutation_copy_tables(&db).await.unwrap();
        assert_eq!(
            count(&reader, "SELECT COUNT(*) FROM external_source_objects_v2").await,
            1,
            "re-running on a migrated store changes nothing"
        );
    }

    /// Installing the schema is cheap idempotent DDL and must leave a retired
    /// table alone: the store-sized rewrite is not the installer's work, and
    /// running it inside the caller's leased schema transaction is what took
    /// the daemon down on a large store.
    #[tokio::test]
    async fn install_leaves_the_store_sized_rewrite_to_the_migration() {
        let temp = tempfile::tempdir().unwrap();
        let db = open_store(&temp.path().join("graph.db")).await;
        commit_batch(
            &db,
            "seed retired shape",
            &format!("{SEED_RETIRED_SHAPE}{SEED_RETIRED_ROWS}"),
        )
        .await;

        let writer = db
            .begin_write_transaction("reinstall schema")
            .await
            .unwrap();
        install_external_source_schema(&writer, "install test")
            .await
            .unwrap();
        writer.commit().await.unwrap();

        let reader = db.read_connection();
        assert_eq!(
            count(
                &reader,
                "SELECT COUNT(*) FROM external_source_commit_receipts_v1"
            )
            .await,
            1,
            "install must not rewrite retired rows"
        );
        assert_eq!(
            count(
                &reader,
                "SELECT COUNT(*) FROM external_source_commit_receipts_v2"
            )
            .await,
            0
        );
    }

    /// A migration killed between chunks leaves the rows it moved in the
    /// successor and the rest in the retired table. Resuming from that exact
    /// on-disk state must move every remaining row once: the successor's row
    /// count and its digest content match the whole seeded set, with no row
    /// duplicated and none lost.
    #[tokio::test]
    async fn migration_resumes_from_a_partially_moved_table() {
        let temp = tempfile::tempdir().unwrap();
        let db = open_store(&temp.path().join("graph.db")).await;
        // Two chunks plus a remainder, so resumption is exercised across a
        // chunk boundary rather than inside a single pass.
        let seeded = RETIRED_MUTATION_COPY_CHUNK_ROWS * 2 + 17;
        commit_batch(&db, "seed retired shape", SEED_RETIRED_SHAPE).await;
        seed_retired_objects(&db, seeded).await;

        // Exactly what a kill after the first committed chunk leaves behind.
        commit_batch(
            &db,
            "replay one committed chunk",
            &format!(
                "INSERT OR IGNORE INTO external_source_objects_v2 (
                    binding_id, native_object_digest, partition_digest, mutation_digest
                 )
                 SELECT binding_id, native_object_digest, partition_digest, mutation_digest
                 FROM external_source_objects_v1
                 WHERE rowid <= {RETIRED_MUTATION_COPY_CHUNK_ROWS};
                 DELETE FROM external_source_objects_v1
                 WHERE rowid <= {RETIRED_MUTATION_COPY_CHUNK_ROWS};"
            ),
        )
        .await;
        let reader = db.read_connection();
        assert_eq!(
            count(&reader, "SELECT COUNT(*) FROM external_source_objects_v1").await,
            seeded - RETIRED_MUTATION_COPY_CHUNK_ROWS,
            "the partially moved store must still hold the unmoved rows"
        );

        migrate_retired_mutation_copy_tables(&db).await.unwrap();

        assert_eq!(
            count(&reader, "SELECT COUNT(*) FROM external_source_objects_v2").await,
            seeded,
            "resumption moves every remaining row exactly once"
        );
        // Content digest over the whole successor, so a duplicated or lost
        // row fails here even when the count happens to agree.
        let mut rows = reader
            .query(
                "SELECT COUNT(*), COUNT(DISTINCT native_object_digest),
                        SUM(CAST(replace(mutation_digest, 'sha256:mut', '') AS INTEGER))
                 FROM external_source_objects_v2",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), seeded);
        assert_eq!(row.get::<i64>(1).unwrap(), seeded, "no row moved twice");
        assert_eq!(
            row.get::<i64>(2).unwrap(),
            seeded * (seeded + 1) / 2,
            "every seeded digest survives exactly once"
        );
        assert_eq!(
            count(
                &reader,
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'external_source_objects_v1'"
            )
            .await,
            0,
            "the retired table is dropped only once it is empty"
        );
    }
}
