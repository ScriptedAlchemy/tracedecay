//! Additive external-source state schema owned by the canonical database.

use crate::db::engine::{Executor, QueryExecutor};
use tracedecay_domain::errors::{Result, TraceDecayError};

/// The three tables that once carried their own copy of `mutation_json`,
/// paired with the statement that moves each row into its digest-referencing
/// successor. `json_extract` reads the digest the encoding already carried,
/// so no row needs the history table to be migrated.
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
        connection.execute_batch(migration).await.map_err(|error| {
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
            .begin_write_transaction("seed retired shape")
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
                     '{\"mutation_digest\":\"sha256:mut1\"}');",
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
                'external_source_projection_effects_v1')",
        )
        .await;
        assert_eq!(retired, 0, "every retired table is dropped");
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
