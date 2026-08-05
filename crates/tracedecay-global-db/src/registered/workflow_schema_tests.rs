use std::fs;

use tempfile::TempDir;
use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
    engine::QueryExecutor,
};
use tracedecay_runtime_core::errors::TraceDecayError;
use tracedecay_rusqlite_runtime::workflow::{
    WORKFLOW_SCHEMA_IDENTITY_V1, WORKFLOW_TABLE_CONTRACTS_V1,
};

async fn assert_workflow_schema_reset_without_mutation(
    name: &str,
    seed_schema: impl FnOnce(&rusqlite::Connection),
) {
    crate::register_test_schema_installer();
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("project/sessions.db");
    fs::create_dir_all(database_path.parent().unwrap()).unwrap();
    {
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        seed_schema(&connection);
        connection
            .execute_batch(
                "CREATE TABLE workflow_reset_canary (value TEXT NOT NULL);
                 INSERT INTO workflow_reset_canary VALUES ('preserve-me');",
            )
            .unwrap();
    }
    let authority = DatabaseAuthority::acquire_test(&database_path, name).unwrap();
    let (database, _) = Database::publish_registered_test_runtime(
        &database_path,
        &authority,
        TestDatabaseRuntimeMode::Existing,
        TestDatabaseRuntimeScope::ProjectSessions {
            project_id: tracedecay_domain::ProjectId::new(format!("project.{name}")).unwrap(),
        },
    )
    .await
    .unwrap();
    let writer = database
        .writer_connection("inspect rejected workflow shape")
        .await
        .unwrap();
    let before_bytes = fs::read(&database_path).unwrap();
    let before_schema = schema_snapshot(writer.engine_connection()).await;

    let error = match crate::ensure_registered_schema(writer.engine_connection()).await {
        Ok(()) => panic!("malformed workflow schema must not be completed"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        TraceDecayError::ResetRequired {
            ref authority,
            ..
        } if authority == "workflow"
    ));
    assert_eq!(
        fs::read(&database_path).unwrap(),
        before_bytes,
        "typed refusal must preserve the exact main database bytes"
    );
    assert_eq!(
        schema_snapshot(writer.engine_connection()).await,
        before_schema,
        "typed refusal must preserve every schema object byte-for-byte"
    );
    let mut canary = writer
        .engine_connection()
        .query("SELECT value FROM workflow_reset_canary", ())
        .await
        .unwrap();
    assert_eq!(
        canary
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "preserve-me"
    );
}

async fn schema_snapshot(
    connection: &tracedecay_runtime_core::db::engine::Connection,
) -> Vec<(String, String, String, Option<String>)> {
    let mut rows = connection
        .query(
            "SELECT type, name, tbl_name, sql FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
            (),
        )
        .await
        .unwrap();
    let mut snapshot = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        snapshot.push((
            row.get::<String>(0).unwrap(),
            row.get::<String>(1).unwrap(),
            row.get::<String>(2).unwrap(),
            row.get::<Option<String>>(3).unwrap(),
        ));
    }
    snapshot
}

#[tokio::test]
async fn partial_workflow_store_requires_reset_without_mutation() {
    assert_workflow_schema_reset_without_mutation("partial-workflow", |connection| {
        connection
            .execute_batch(
                "CREATE TABLE workflow_schema (
                         singleton INTEGER PRIMARY KEY,
                         schema_version INTEGER NOT NULL,
                         definition_digest TEXT NOT NULL
                     );
                     INSERT INTO workflow_schema VALUES (1, 0, 'partial');",
            )
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn name_complete_workflow_store_with_missing_columns_requires_reset_without_mutation() {
    assert_workflow_schema_reset_without_mutation("name-complete-workflow", |connection| {
        connection
            .execute_batch(
                "CREATE TABLE workflow_definitions (definition_id TEXT);
                     CREATE TABLE workflow_effect_journal (idempotency_key TEXT);
                     CREATE TABLE workflow_handoffs (token_digest TEXT);
                     CREATE TABLE workflow_schema (singleton INTEGER PRIMARY KEY);
                     INSERT INTO workflow_schema VALUES (1);",
            )
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn constraintless_workflow_lookalike_requires_reset_without_mutation() {
    assert_workflow_schema_reset_without_mutation("constraintless-workflow", |connection| {
        connection
            .execute_batch(
                "CREATE TABLE workflow_definitions (
                         definition_id TEXT NOT NULL,
                         definition_version INTEGER NOT NULL,
                         payload TEXT NOT NULL,
                         payload_digest TEXT NOT NULL,
                         PRIMARY KEY (definition_id, definition_version)
                     );
                     CREATE TABLE workflow_effect_journal (
                         idempotency_key TEXT NOT NULL PRIMARY KEY,
                         identity_digest TEXT NOT NULL,
                         identity_payload TEXT NOT NULL,
                         identity_payload_digest TEXT NOT NULL,
                         prepared_payload TEXT NOT NULL,
                         prepared_payload_digest TEXT NOT NULL,
                         operation TEXT NOT NULL,
                         state TEXT NOT NULL,
                         terminal_payload TEXT,
                         terminal_payload_digest TEXT,
                         created_at INTEGER NOT NULL,
                         updated_at INTEGER NOT NULL
                     );
                     CREATE TABLE workflow_handoffs (
                         token_digest TEXT NOT NULL PRIMARY KEY,
                         scope_payload TEXT NOT NULL,
                         issued_at INTEGER NOT NULL,
                         expires_at INTEGER NOT NULL,
                         consumed INTEGER NOT NULL
                     );
                     CREATE TABLE workflow_schema (
                         singleton INTEGER NOT NULL PRIMARY KEY,
                         schema_version INTEGER NOT NULL,
                         definition_digest TEXT NOT NULL
                     );
                     INSERT INTO workflow_schema VALUES (
                         1,
                         1,
                         'sha256:ef3f0fdc0760f91f64f8cc567cee1174dbd94fec69c9de2a39f9683fd8b780da'
                     );",
            )
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn extra_workflow_schema_identity_requires_reset_without_mutation() {
    assert_workflow_schema_reset_without_mutation("extra-workflow-identity", |connection| {
        let mut schema = String::new();
        for table in WORKFLOW_TABLE_CONTRACTS_V1 {
            schema.push_str(table.sql);
            schema.push_str(";\n");
        }
        schema.push_str(WORKFLOW_SCHEMA_IDENTITY_V1);
        schema.push_str(
            ";
             PRAGMA ignore_check_constraints = ON;
             INSERT INTO workflow_schema VALUES (
                 2,
                 1,
                 'sha256:ef3f0fdc0760f91f64f8cc567cee1174dbd94fec69c9de2a39f9683fd8b780da'
             );",
        );
        connection.execute_batch(&schema).unwrap();
    })
    .await;
}
