use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor};

use super::{
    ConfigurationSchemaError, ensure_configuration_schema, fresh_configuration_store_evidence,
};

async fn final_connection() -> (
    tempfile::TempDir,
    tracedecay_runtime_core::db::engine::TestConnection,
) {
    let directory = tempfile::tempdir().unwrap();
    let connection = tracedecay_runtime_core::db::engine::TestConnection::open(
        &directory.path().join("configuration-admission.db"),
    );
    let fresh = fresh_configuration_store_evidence(&*connection)
        .await
        .unwrap()
        .expect("new database has fresh-store evidence");
    ensure_configuration_schema(&*connection, Some(&fresh))
        .await
        .unwrap();
    (directory, connection)
}

fn assert_reset_required(result: Result<(), ConfigurationSchemaError>) {
    assert!(matches!(
        result,
        Err(ConfigurationSchemaError::ResetRequired { .. })
    ));
}

#[tokio::test]
async fn same_named_table_with_extra_column_requires_reset() {
    let (_directory, connection) = final_connection().await;
    connection
        .execute_batch("ALTER TABLE configuration_entries ADD COLUMN malformed TEXT;")
        .await
        .unwrap();

    assert_reset_required(ensure_configuration_schema(&*connection, None).await);
}

#[tokio::test]
async fn same_named_index_with_wrong_columns_requires_reset() {
    let (_directory, connection) = final_connection().await;
    connection
        .execute_batch(
            "DROP INDEX idx_configuration_entry_key;
             CREATE INDEX idx_configuration_entry_key
                 ON configuration_entries(revision_id);",
        )
        .await
        .unwrap();

    assert_reset_required(ensure_configuration_schema(&*connection, None).await);
}

#[tokio::test]
async fn same_named_immutable_trigger_with_wrong_definition_requires_reset() {
    let (_directory, connection) = final_connection().await;
    connection
        .execute_batch(
            "DROP TRIGGER configuration_entries_immutable_update;
             CREATE TRIGGER configuration_entries_immutable_update
             BEFORE UPDATE ON configuration_entries
             BEGIN SELECT 1; END;",
        )
        .await
        .unwrap();

    assert_reset_required(ensure_configuration_schema(&*connection, None).await);
}

#[tokio::test]
async fn trigger_string_literal_case_is_part_of_the_exact_definition() {
    let (_directory, connection) = final_connection().await;
    connection
        .execute_batch(
            "DROP TRIGGER configuration_entries_immutable_update;
             CREATE TRIGGER configuration_entries_immutable_update
             BEFORE UPDATE ON configuration_entries
             BEGIN
                 SELECT RAISE(ABORT, 'Configuration entries are immutable');
             END;",
        )
        .await
        .unwrap();

    assert_reset_required(ensure_configuration_schema(&*connection, None).await);
}

#[tokio::test]
async fn arbitrary_named_index_attached_to_configuration_table_requires_reset() {
    let (_directory, connection) = final_connection().await;
    connection
        .execute_batch(
            "CREATE INDEX extra_entry_revision
                 ON configuration_entries(revision_id);",
        )
        .await
        .unwrap();

    assert_reset_required(ensure_configuration_schema(&*connection, None).await);
}

#[tokio::test]
async fn arbitrary_named_trigger_attached_to_configuration_table_requires_reset() {
    let (_directory, connection) = final_connection().await;
    connection
        .execute_batch(
            "CREATE TRIGGER extra_entry_guard
             BEFORE DELETE ON configuration_entries
             BEGIN SELECT RAISE(ABORT, 'extra'); END;",
        )
        .await
        .unwrap();

    assert_reset_required(ensure_configuration_schema(&*connection, None).await);
}

#[tokio::test]
async fn stale_fresh_evidence_cannot_create_configuration_schema() {
    let directory = tempfile::tempdir().unwrap();
    let connection = tracedecay_runtime_core::db::engine::TestConnection::open(
        &directory.path().join("stale-fresh.db"),
    );
    let fresh = fresh_configuration_store_evidence(&*connection)
        .await
        .unwrap()
        .expect("initially fresh");
    connection
        .execute_batch(
            "CREATE TABLE registered_identity (value TEXT NOT NULL);
             INSERT INTO registered_identity VALUES ('preserve-after-race');",
        )
        .await
        .unwrap();

    assert_reset_required(ensure_configuration_schema(&*connection, Some(&fresh)).await);
    let mut rows = connection
        .query("SELECT value FROM registered_identity", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "preserve-after-race"
    );
}

#[tokio::test]
async fn non_fresh_store_missing_configuration_schema_is_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let connection = tracedecay_runtime_core::db::engine::TestConnection::open(
        &directory.path().join("registered.db"),
    );
    connection
        .execute_batch(
            "CREATE TABLE registered_identity (value TEXT NOT NULL);
             INSERT INTO registered_identity VALUES ('preserve-byte-state');",
        )
        .await
        .unwrap();
    assert!(
        fresh_configuration_store_evidence(&*connection)
            .await
            .unwrap()
            .is_none()
    );

    let before = sqlite_objects(&connection).await;
    assert_reset_required(ensure_configuration_schema(&*connection, None).await);
    assert_eq!(sqlite_objects(&connection).await, before);

    let mut rows = connection
        .query("SELECT value FROM registered_identity", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "preserve-byte-state"
    );
}

async fn sqlite_objects(
    connection: &tracedecay_runtime_core::db::engine::TestConnection,
) -> Vec<(String, String, String)> {
    let mut rows = connection
        .query(
            "SELECT type, name, COALESCE(sql, '')
             FROM sqlite_master
             ORDER BY type, name",
            (),
        )
        .await
        .unwrap();
    let mut objects = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        objects.push((
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
        ));
    }
    objects
}
