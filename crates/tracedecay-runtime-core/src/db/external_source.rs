//! Additive external-source state schema owned by the canonical database.

use crate::db::engine::{Executor, QueryExecutor};
use tracedecay_domain::errors::{Result, TraceDecayError};

/// Retired tables that carried their own copy of payloads the history tables
/// already hold, paired with the statement that moves each row into its
/// digest-referencing successor. `json_extract` reads every digest out of the
/// retired row's own encoding, so no row needs another table to be migrated
/// first.
const RETIRED_MUTATION_COPY_TABLES: &[(&str, &str)] = &[
    (
        "external_source_objects_v1",
        "INSERT OR IGNORE INTO external_source_objects_v2 (
            binding_id, native_object_digest, partition_digest, mutation_digest
         )
         SELECT binding_id, native_object_digest, partition_digest, mutation_digest
         FROM external_source_objects_v1;
         DROP TABLE external_source_objects_v1;",
    ),
    (
        "external_source_projected_objects_v1",
        "INSERT OR IGNORE INTO external_source_projected_objects_v2 (
            binding_id, native_object_digest, mutation_digest
         )
         SELECT binding_id, native_object_digest,
                json_extract(mutation_json, '$.mutation_digest')
         FROM external_source_projected_objects_v1
         WHERE json_extract(mutation_json, '$.mutation_digest') IS NOT NULL;
         DROP TABLE external_source_projected_objects_v1;",
    ),
    (
        "external_source_projection_effects_v1",
        "INSERT OR IGNORE INTO external_source_projection_effects_v2 (
            binding_id, projection_digest, effect_index,
            native_object_digest, mutation_digest, effect_json
         )
         SELECT binding_id, projection_digest, effect_index, native_object_digest,
                json_extract(mutation_json, '$.mutation_digest'), effect_json
         FROM external_source_projection_effects_v1
         WHERE json_extract(mutation_json, '$.mutation_digest') IS NOT NULL;
         DROP TABLE external_source_projection_effects_v1;",
    ),
    // Receipts embedded their mutations and aggregate frontiers. The slim
    // shape keeps mutation digests in place of mutations, frontier digests in
    // place of frontiers (the payloads move to `external_source_frontiers_v1`),
    // and, for projections, an empty effects list that hydrates from the
    // effects table. Every replacement value is read out of the retired row's
    // own encoding.
    (
        "external_source_commit_receipts_v1",
        "INSERT OR IGNORE INTO external_source_frontiers_v1 (
            binding_id, frontier_digest, frontier_json
         )
         SELECT binding_id,
                json_extract(receipt_json, '$.source_frontier.digest'),
                json_extract(receipt_json, '$.source_frontier')
         FROM external_source_commit_receipts_v1
         WHERE json_extract(receipt_json, '$.source_frontier.digest') IS NOT NULL;
         INSERT OR IGNORE INTO external_source_frontiers_v1 (
            binding_id, frontier_digest, frontier_json
         )
         SELECT binding_id,
                json_extract(receipt_json, '$.prior_source_frontier.digest'),
                json_extract(receipt_json, '$.prior_source_frontier')
         FROM external_source_commit_receipts_v1
         WHERE json_extract(receipt_json, '$.prior_source_frontier.digest') IS NOT NULL;
         INSERT OR IGNORE INTO external_source_commit_receipts_v2 (
            binding_id, idempotency_key, request_digest, definition_revision,
            binding_revision, predecessor_frontier_digest, successor_frontier_digest,
            receipt_digest, receipt_json
         )
         SELECT binding_id, idempotency_key, request_digest, definition_revision,
                binding_revision, predecessor_frontier_digest, successor_frontier_digest,
                receipt_digest,
                json_set(
                    receipt_json,
                    '$.mutations', json((
                        SELECT json_group_array(json_extract(mutation.value, '$.mutation_digest'))
                        FROM json_each(receipt_json, '$.mutations') AS mutation
                    )),
                    '$.source_frontier', json_extract(receipt_json, '$.source_frontier.digest'),
                    '$.prior_source_frontier',
                    json_extract(receipt_json, '$.prior_source_frontier.digest')
                )
         FROM external_source_commit_receipts_v1;
         DROP TABLE external_source_commit_receipts_v1;",
    ),
    (
        "external_source_projection_publications_v1",
        "INSERT OR IGNORE INTO external_source_frontiers_v1 (
            binding_id, frontier_digest, frontier_json
         )
         SELECT binding_id,
                json_extract(receipt_json, '$.source_frontier.digest'),
                json_extract(receipt_json, '$.source_frontier')
         FROM external_source_projection_publications_v1
         WHERE json_extract(receipt_json, '$.source_frontier.digest') IS NOT NULL;
         INSERT OR IGNORE INTO external_source_frontiers_v1 (
            binding_id, frontier_digest, frontier_json
         )
         SELECT binding_id,
                json_extract(receipt_json, '$.expected_projection_frontier.digest'),
                json_extract(receipt_json, '$.expected_projection_frontier')
         FROM external_source_projection_publications_v1
         WHERE json_extract(receipt_json, '$.expected_projection_frontier.digest') IS NOT NULL;
         INSERT OR IGNORE INTO external_source_projection_publications_v2 (
            binding_id, projection_digest, source_receipt_digest,
            predecessor_frontier_digest, successor_frontier_digest, receipt_json
         )
         SELECT binding_id, projection_digest, source_receipt_digest,
                predecessor_frontier_digest, successor_frontier_digest,
                json_set(
                    receipt_json,
                    '$.mutations', json((
                        SELECT json_group_array(json_extract(mutation.value, '$.mutation_digest'))
                        FROM json_each(receipt_json, '$.mutations') AS mutation
                    )),
                    '$.effects', json('[]'),
                    '$.source_frontier', json_extract(receipt_json, '$.source_frontier.digest'),
                    '$.expected_projection_frontier',
                    json_extract(receipt_json, '$.expected_projection_frontier.digest')
                )
         FROM external_source_projection_publications_v1;
         DROP TABLE external_source_projection_publications_v1;",
    ),
];

pub async fn install_external_source_schema(
    connection: &impl Executor,
    operation: &str,
) -> Result<()> {
    let failure = |message: String| TraceDecayError::Database {
        message,
        operation: operation.to_owned(),
    };
    connection
        .execute_batch(tracedecay_rusqlite_runtime::repository::EXTERNAL_SOURCE_SCHEMA_V1)
        .await
        .map_err(|error| {
            failure(format!(
                "{operation}: failed to install external source state: {error}"
            ))
        })?;
    // A store created before the current-state tables referenced mutations
    // by digest still carries its byte-identical `mutation_json` copies. Move
    // each retired table into its successor and drop it; a store at the
    // current shape has none of them and this is a single catalog probe.
    for (retired_table, migration) in RETIRED_MUTATION_COPY_TABLES {
        if !table_exists(connection, retired_table)
            .await
            .map_err(|error| {
                failure(format!(
                    "{operation}: failed to probe for retired table {retired_table}: {error}"
                ))
            })?
        {
            continue;
        }
        connection
            .execute_bulk_migration_batch(migration)
            .await
            .map_err(|error| {
                failure(format!(
                    "{operation}: failed to migrate {retired_table} to its digest-referencing \
                     successor: {error}"
                ))
            })?;
    }
    Ok(())
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
    use super::install_external_source_schema;
    use crate::db::engine::QueryExecutor;
    use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

    async fn count(connection: &impl QueryExecutor, sql: &str) -> i64 {
        let mut rows = connection.query(sql, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    /// A store still carrying the tables that duplicated `mutation_json` is
    /// moved to the digest-referencing shape in place: every current row
    /// survives with the digest its own encoding carried, the retired tables
    /// are dropped, and a second install is a no-op.
    #[tokio::test]
    async fn install_migrates_retired_mutation_copy_tables() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority =
            DatabaseAuthority::acquire_test(&path, "external source migration").unwrap();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        let writer = db
            .begin_bulk_write_transaction("seed retired shape")
            .await
            .unwrap();
        // The fixture runtime already installed the current schema; recreate
        // the retired tables beside it exactly as an older binary left them.
        writer
            .execute_batch(
                "CREATE TABLE external_source_objects_v1 (
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
                 INSERT INTO external_source_objects_v1 VALUES
                    ('b', 'sha256:obj', 'sha256:part', 'sha256:mut1', '{\"mutation_digest\":\"sha256:mut1\"}');
                 INSERT INTO external_source_projected_objects_v1 VALUES
                    ('b', 'sha256:obj', '{\"x\":1,\"mutation_digest\":\"sha256:mut1\"}');
                 INSERT INTO external_source_projection_effects_v1 VALUES
                    ('b', 'sha256:proj', 0, 'sha256:obj', '{\"effect\":true}',
                     '{\"mutation_digest\":\"sha256:mut1\"}');
                 CREATE TABLE external_source_commit_receipts_v1 (
                    binding_id TEXT NOT NULL, idempotency_key TEXT NOT NULL,
                    request_digest TEXT NOT NULL, definition_revision INTEGER NOT NULL,
                    binding_revision INTEGER NOT NULL, predecessor_frontier_digest TEXT NOT NULL,
                    successor_frontier_digest TEXT NOT NULL, receipt_digest TEXT NOT NULL,
                    receipt_json TEXT NOT NULL,
                    PRIMARY KEY (binding_id, idempotency_key));
                 INSERT INTO external_source_commit_receipts_v1 VALUES
                    ('b', 'sha256:key', 'sha256:req', 1, 1, 'root', 'sha256:front1', 'sha256:rcpt',
                     '{\"idempotency_key\":\"sha256:key\",\"prior_source_frontier\":null,'
                     || '\"source_frontier\":{\"binding\":{\"id\":\"b\"},\"partitions\":{},\"digest\":\"sha256:front1\"},'
                     || '\"mutations\":[{\"x\":1,\"mutation_digest\":\"sha256:mut1\"},{\"x\":2,\"mutation_digest\":\"sha256:mut2\"}],'
                     || '\"receipt_digest\":\"sha256:rcpt\"}');",
            )
            .await
            .unwrap();

        install_external_source_schema(&writer, "migration test")
            .await
            .unwrap();

        let retired = count(
            &writer,
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN (
                'external_source_objects_v1', 'external_source_projected_objects_v1',
                'external_source_projection_effects_v1', 'external_source_commit_receipts_v1')",
        )
        .await;
        assert_eq!(retired, 0, "every retired table is dropped");
        // The receipt is slim: mutation digests where mutations were, the
        // frontier digest where the frontier was, and the frontier itself
        // stored once under its digest.
        assert_eq!(
            count(
                &writer,
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
                &writer,
                "SELECT COUNT(*) FROM external_source_frontiers_v1
                 WHERE binding_id = 'b' AND frontier_digest = 'sha256:front1'
                   AND json_extract(frontier_json, '$.binding.id') = 'b'"
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &writer,
                "SELECT COUNT(*) FROM external_source_objects_v2
                 WHERE binding_id = 'b' AND mutation_digest = 'sha256:mut1'"
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &writer,
                "SELECT COUNT(*) FROM external_source_projected_objects_v2
                 WHERE binding_id = 'b' AND mutation_digest = 'sha256:mut1'"
            )
            .await,
            1,
            "the projected object's digest is read out of its retired encoding"
        );
        assert_eq!(
            count(
                &writer,
                "SELECT COUNT(*) FROM external_source_projection_effects_v2
                 WHERE binding_id = 'b' AND mutation_digest = 'sha256:mut1'
                   AND effect_json = '{\"effect\":true}'"
            )
            .await,
            1
        );

        install_external_source_schema(&writer, "migration test")
            .await
            .unwrap();
        assert_eq!(
            count(&writer, "SELECT COUNT(*) FROM external_source_objects_v2").await,
            1,
            "re-installing on a migrated store changes nothing"
        );
        writer.commit().await.unwrap();
    }
}
