use std::fmt::Write as _;
use std::fs;

use tempfile::TempDir;
use tracedecay_domain::ProjectId;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::engine::TestConnection;
use tracedecay_rusqlite_runtime::workflow::{
    WORKFLOW_SCHEMA_IDENTITY_V1, WORKFLOW_TABLE_CONTRACTS_V1,
};

use crate::tests::harness::open_registered_test_database_fixture;
use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;

async fn assert_workflow_schema_reset_without_mutation(malformed_schema: String) {
    crate::register_registered_schema_installer();
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("project/sessions.db");
    fs::create_dir_all(database_path.parent().unwrap()).unwrap();
    drop(
        open_registered_test_database_fixture(
            &database_path,
            TestDatabaseRuntimeScope::ProjectSessions {
                project_id: ProjectId::new("project.workflow-schema").unwrap(),
            },
        )
        .await
        .unwrap(),
    );
    let database = TestConnection::open(&database_path);
    let connection = (*database).clone();
    let mut drop_workflow_schema = String::new();
    for table in WORKFLOW_TABLE_CONTRACTS_V1.iter().rev() {
        writeln!(drop_workflow_schema, "DROP TABLE IF EXISTS {};", table.name).unwrap();
    }
    connection
        .execute_batch(&drop_workflow_schema)
        .await
        .unwrap();
    drop(connection);
    drop(database);
    {
        let connection = rusqlite::Connection::open(&database_path).unwrap();
        connection.execute_batch(&malformed_schema).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE workflow_reset_canary (value TEXT NOT NULL);
                 INSERT INTO workflow_reset_canary VALUES ('preserve-me');",
            )
            .unwrap();
    }
    let before_bytes = fs::read(&database_path).unwrap();
    let database = TestConnection::open(&database_path);
    let connection = (*database).clone();
    let before_schema = schema_snapshot(&connection).await;
    drop(connection);
    drop(database);

    let (lease, owner) = open_registered_test_database_fixture(
        &database_path,
        TestDatabaseRuntimeScope::ProjectSessions {
            project_id: ProjectId::new("project.workflow-schema").unwrap(),
        },
    )
    .await
    .expect("the store's other authorities stay admissible");
    for refusal in [lease.reset_required(), owner.reset_required()] {
        assert!(
            matches!(
                refusal,
                Some(TraceDecayError::ResetRequired {
                    ref authority,
                    ..
                }) if authority == "workflow"
            ),
            "a malformed workflow schema refuses session features: {refusal:?}"
        );
    }
    drop((lease, owner));
    assert_eq!(
        fs::read(&database_path).unwrap(),
        before_bytes,
        "typed refusal must preserve the exact main database bytes"
    );
    let preserved = TestConnection::open(&database_path);
    assert_eq!(
        schema_snapshot(&preserved).await,
        before_schema,
        "typed refusal must preserve every schema object byte-for-byte"
    );
    let connection = TestConnection::open(&database_path);
    let mut canary = connection
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
async fn short_lived_admission_reclaims_freed_pages_without_daemon_maintenance() {
    crate::register_registered_schema_installer();
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("project/sessions.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let scope = || TestDatabaseRuntimeScope::ProjectSessions {
        project_id: ProjectId::new("project.short-lived-reclaim").unwrap(),
    };

    drop(
        open_registered_test_database_fixture(&path, scope())
            .await
            .unwrap(),
    );
    let connection = rusqlite::Connection::open(&path).unwrap();
    let auto_vacuum: i64 = connection
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        auto_vacuum, 2,
        "new stores must support incremental reclamation"
    );
    connection
        .execute_batch(
            "CREATE TABLE ephemeral_reclaim(payload BLOB);
             INSERT INTO ephemeral_reclaim VALUES (zeroblob(8388608));
             DROP TABLE ephemeral_reclaim;",
        )
        .unwrap();
    let before_pages: i64 = connection
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .unwrap();
    let before_free: i64 = connection
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .unwrap();
    assert!(
        before_free > 0,
        "the fixture must contain reclaimable pages"
    );
    drop(connection);

    // Reopen through the actual short-lived owner: there is no background
    // compactor after this call, so admission must make physical progress.
    let (lease, owner) = open_registered_test_database_fixture(&path, scope())
        .await
        .unwrap();
    let (_, after_pages, after_free) = lease.storage_page_counts().await.unwrap();
    assert!(
        after_pages < before_pages as u64,
        "short-lived admission did not reduce the physical page count: {before_pages} -> {after_pages}"
    );
    assert!(
        after_free < before_free as u64,
        "short-lived admission left the original free-page backlog: {before_free} -> {after_free}"
    );
    drop((lease, owner));
}

#[tokio::test]
async fn partial_workflow_store_requires_reset_without_mutation() {
    assert_workflow_schema_reset_without_mutation(
        "CREATE TABLE workflow_schema (
                         singleton INTEGER PRIMARY KEY,
                         schema_version INTEGER NOT NULL,
                         definition_digest TEXT NOT NULL
                     );
         INSERT INTO workflow_schema VALUES (1, 0, 'partial');"
            .to_owned(),
    )
    .await;
}

#[tokio::test]
async fn name_complete_workflow_store_with_missing_columns_requires_reset_without_mutation() {
    assert_workflow_schema_reset_without_mutation(
        "CREATE TABLE workflow_definitions (definition_id TEXT);
                     CREATE TABLE workflow_effect_journal (idempotency_key TEXT);
                     CREATE TABLE workflow_handoffs (token_digest TEXT);
                     CREATE TABLE workflow_schema (singleton INTEGER PRIMARY KEY);
         INSERT INTO workflow_schema VALUES (1);"
            .to_owned(),
    )
    .await;
}

#[tokio::test]
async fn constraintless_workflow_lookalike_requires_reset_without_mutation() {
    assert_workflow_schema_reset_without_mutation(
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
         );"
        .to_owned(),
    )
    .await;
}

#[tokio::test]
async fn extra_workflow_schema_identity_requires_reset_without_mutation() {
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
    assert_workflow_schema_reset_without_mutation(schema).await;
}

mod published_artifact_ceiling {
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;

    use rusqlite::Connection;
    use tempfile::TempDir;
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_lcm::LCM_SCHEMA_VERSION;
    use tracedecay_runtime_core::cancellation::CancellationToken;
    use tracedecay_runtime_core::db::engine;
    use tracedecay_runtime_core::db::{
        RegisteredTestRuntimeRetirementControlV1, TestDatabaseRuntimeScope,
    };
    use tracedecay_runtime_core::shard_runtime::registry::{
        StoreRuntimeClientLease, StoreRuntimeRetirementOutcome, StoreRuntimeRetirementResult,
    };
    use tracedecay_rusqlite_runtime::workflow::{
        WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1, WORKFLOW_TABLE_CONTRACTS_V1,
    };

    use super::fs;
    use crate::RegisteredGlobalDbOwnerV1;
    use crate::schema_stages::upgrade_published_artifact_ceiling;
    use crate::tests::harness::open_registered_test_database_fixture;

    /// The artifact table as `git show v1.0.0-beta.92` defines it, deliberately
    /// spelled out here so a change to production's copy cannot pass.
    const PUBLISHED_TABLE_SQL: &str = "CREATE TABLE workflow_artifact_payloads (
    payload_digest TEXT NOT NULL PRIMARY KEY,
    byte_length INTEGER NOT NULL CHECK (
        byte_length = length(payload) AND byte_length <= 4194304
    ),
    payload BLOB NOT NULL
) STRICT";
    const PUBLISHED_DIGEST: &str =
        "sha256:a292df6bc47e763f0d20bdb44a4032c0b1d7ac8e4cb83173b66ae7d1ff0d03be";
    const PUBLISHED_CEILING: usize = 4_194_304;

    fn final_table_sql() -> &'static str {
        WORKFLOW_TABLE_CONTRACTS_V1
            .iter()
            .find(|table| table.name == "workflow_artifact_payloads")
            .unwrap()
            .sql
    }

    /// One row of every workflow table a published release could have written,
    /// including artifact rows at both ends of the published range and a rowid
    /// gap a sloppy copy would close.
    const PUBLISHED_ROWS: &str = "
        INSERT INTO workflow_artifact_payloads (rowid, payload_digest, byte_length, payload)
        VALUES
            (3, 'sha256:empty', 0, x''),
            (7, 'sha256:small', 4, x'00ff10ee'),
            (9, 'sha256:at-ceiling', 4194304, zeroblob(4194304));
        INSERT INTO workflow_run_journal VALUES ('run.1', 1, 'command.1', '{\"e\":1}', 'd1');
        INSERT INTO workflow_fan_out_census_journal VALUES ('run.1', 1, 10, '{\"c\":1}', 'd2', 1);
        INSERT INTO workflow_definition_source_journal VALUES ('definition.1', 1, '{}', 'd3');
        INSERT INTO workflow_definition_disposition VALUES ('definition.1', 1, 'active', 2, 11);
        INSERT INTO workflow_definition_transition_journal
            VALUES ('definition.1', 1, 2, 1, 'activate', 'validated', 'active', 12);
        INSERT INTO workflow_effect_journal VALUES (
            'effect.1', 'd4', '{}', 'd5', '{}', 'd6', 'operation', 'committed', '{}', 'd7', 13, 14
        );
        INSERT INTO workflow_handoffs VALUES ('token.1', '{}', 15, 16, 0, '{}', 'd8');";

    fn republish(connection: &Connection, table_sql: &str, digest: &str) {
        connection
            .execute_batch(&format!(
                "DROP TABLE workflow_artifact_payloads;
                 {table_sql};
                 UPDATE workflow_schema SET definition_digest = '{digest}';"
            ))
            .unwrap();
    }

    struct PublishedStore {
        path: PathBuf,
        _directory: TempDir,
    }

    impl PublishedStore {
        async fn create() -> Self {
            Self::create_at("sessions.db", TestDatabaseRuntimeScope::ProfileSessions).await
        }

        async fn create_at(file: &str, scope: TestDatabaseRuntimeScope) -> Self {
            let directory = TempDir::new().unwrap();
            let path = directory.path().join(file);
            drop(
                open_registered_test_database_fixture(&path, scope)
                    .await
                    .unwrap(),
            );
            let mut connection = Connection::open(&path).unwrap();
            // No reader sees fixture assembly, so publish its schema and rows
            // with one durable commit instead of committing each statement.
            let transaction = connection.transaction().unwrap();
            republish(&transaction, PUBLISHED_TABLE_SQL, PUBLISHED_DIGEST);
            transaction.execute_batch(PUBLISHED_ROWS).unwrap();
            transaction.commit().unwrap();
            Self {
                path,
                _directory: directory,
            }
        }

        fn run(&self, sql: &str) {
            Connection::open(&self.path)
                .unwrap()
                .execute_batch(sql)
                .unwrap();
        }

        fn state(&self) -> WorkflowState {
            WorkflowState::read(&self.path)
        }

        async fn admit(&self) -> Option<TraceDecayError> {
            let (lease, owner) = open_registered_test_database_fixture(
                &self.path,
                TestDatabaseRuntimeScope::ProfileSessions,
            )
            .await
            .expect("admission serves the store's other authorities");
            let refusal = owner.reset_required();
            assert_eq!(
                lease.workflow_storage().await.is_ok(),
                refusal.is_none(),
                "the production workflow gate must agree with admission: {refusal:?}"
            );
            refusal
        }
    }

    /// Everything the upgrade promises to preserve or change, read outside
    /// production code.
    #[derive(Debug, PartialEq, Eq, Clone)]
    struct WorkflowState {
        table_sql: String,
        digest: String,
        artifacts: Vec<(i64, String, i64, Vec<u8>)>,
        other_rows: Vec<String>,
        objects_named_after_the_table: Vec<String>,
    }

    impl WorkflowState {
        fn read(path: &Path) -> Self {
            let connection = Connection::open(path).unwrap();
            let table_sql = connection
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE name = 'workflow_artifact_payloads'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let digest = connection
                .query_row("SELECT definition_digest FROM workflow_schema", [], |row| {
                    row.get(0)
                })
                .unwrap();
            let artifacts = connection
                .prepare(
                    "SELECT rowid, payload_digest, byte_length, payload
                     FROM workflow_artifact_payloads ORDER BY rowid",
                )
                .unwrap()
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            let mut other_rows = Vec::new();
            for table in WORKFLOW_TABLE_CONTRACTS_V1 {
                if table.name == "workflow_artifact_payloads" || table.name == "workflow_schema" {
                    continue;
                }
                let columns = table
                    .columns
                    .iter()
                    .map(|column| format!("quote({})", column.name))
                    .collect::<Vec<_>>()
                    .join(" || ',' || ");
                let mut statement = connection
                    .prepare(&format!("SELECT {columns} FROM {}", table.name))
                    .unwrap();
                let rows = statement
                    .query_map([], |row| row.get::<_, String>(0))
                    .unwrap();
                for row in rows {
                    other_rows.push(format!("{}:{}", table.name, row.unwrap()));
                }
            }
            let objects_named_after_the_table = connection
                .prepare(
                    "SELECT name FROM sqlite_master
                     WHERE name LIKE '%workflow_artifact_payloads%' ORDER BY name",
                )
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            Self {
                table_sql,
                digest,
                artifacts,
                other_rows,
                objects_named_after_the_table,
            }
        }
    }

    fn assert_workflow_refusal(refusal: Option<TraceDecayError>) -> String {
        let refusal = refusal.expect("this shape must keep refusing workflow features");
        let Some(("workflow", reason)) = refusal.reset_required_context() else {
            panic!("refused with the wrong typed state: {refusal}");
        };
        assert!(
            !reason.contains("CREATE TABLE"),
            "stored SQL must not reach the refusal: {reason}"
        );
        reason.to_owned()
    }

    #[tokio::test]
    async fn admission_upgrades_the_published_shape_and_keeps_every_record() {
        let store = PublishedStore::create().await;
        let before = store.state();
        assert_eq!(before.artifacts.len(), 3);
        assert_eq!(before.other_rows.len(), 7);

        assert!(store.admit().await.is_none());

        let after = store.state();
        assert_eq!(after.table_sql, final_table_sql());
        assert_eq!(after.digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);
        assert_eq!(after.artifacts, before.artifacts);
        assert_eq!(after.other_rows, before.other_rows);
        assert_eq!(
            after.objects_named_after_the_table,
            vec![
                "sqlite_autoindex_workflow_artifact_payloads_1",
                "workflow_artifact_payloads"
            ]
        );
        let connection = Connection::open(&store.path).unwrap();
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        // The ceiling is gone and the remaining constraint still holds.
        let above_ceiling = PUBLISHED_CEILING + 1;
        connection
            .execute_batch(&format!(
                "INSERT INTO workflow_artifact_payloads
                 VALUES ('sha256:above', {above_ceiling}, zeroblob({above_ceiling}));"
            ))
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO workflow_artifact_payloads VALUES ('sha256:liar', 1, x'0011');",
            )
            .expect_err("the byte-length constraint survives the upgrade");
    }

    #[tokio::test]
    async fn reopening_an_upgraded_store_changes_nothing_and_keeps_later_writes() {
        let store = PublishedStore::create().await;
        assert!(store.admit().await.is_none());
        store.run("INSERT INTO workflow_artifact_payloads VALUES ('sha256:later', 1, x'01');");
        let upgraded = store.state();

        assert!(store.admit().await.is_none());
        assert!(store.admit().await.is_none());

        assert_eq!(store.state(), upgraded);
        assert_eq!(upgraded.artifacts.len(), 4);
    }

    /// A trigger that would block the identity swap is visible before anything
    /// runs, so the census can name it too: the store is refused untouched, and
    /// takes the upgrade once it is gone.
    #[tokio::test]
    async fn identity_blocking_trigger_refuses_until_it_is_gone() {
        let store = PublishedStore::create().await;
        store.run(
            "CREATE TRIGGER block_workflow_identity BEFORE UPDATE ON workflow_schema
             BEGIN SELECT RAISE(ABORT, 'foreign trigger text'); END;",
        );
        let before = store.state();

        let reason = assert_workflow_refusal(store.admit().await);
        assert!(reason.contains("1 other schema object(s)"), "{reason}");
        assert!(!reason.contains("foreign trigger text"), "{reason}");
        assert_eq!(store.state(), before);

        store.run("DROP TRIGGER block_workflow_identity;");
        assert!(store.admit().await.is_none());
        let after = store.state();
        assert_eq!(after.digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);
        assert_eq!(after.artifacts, before.artifacts);
    }

    /// A row STRICT would reject, reachable only by a store edited around it,
    /// under the exact published table text.
    fn written_around_strict_types(values: &str) -> String {
        format!(
            "PRAGMA ignore_check_constraints = ON;
             PRAGMA writable_schema = ON;
             UPDATE sqlite_master SET sql = replace(sql, ') STRICT', ')')
              WHERE name = 'workflow_artifact_payloads';
             PRAGMA writable_schema = RESET;
             INSERT INTO workflow_artifact_payloads VALUES ({values});
             PRAGMA writable_schema = ON;
             UPDATE sqlite_master SET sql = sql || ' STRICT'
              WHERE name = 'workflow_artifact_payloads';
             PRAGMA writable_schema = RESET;"
        )
    }

    /// Each case differs from the proven shape in one way and must be refused
    /// with nothing changed.
    #[tokio::test]
    async fn shapes_other_than_the_published_one_are_refused_untouched() {
        let reformatted =
            PUBLISHED_TABLE_SQL.replace("byte_length <= 4194304", "byte_length<=4194304");
        let doubled_ceiling = PUBLISHED_TABLE_SQL.replace("4194304", "8388608");
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "published table under the final identity",
                format!("UPDATE workflow_schema SET definition_digest = '{WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1}';"),
                "definition differs",
            ),
            (
                "published table under an unknown identity",
                "UPDATE workflow_schema SET definition_digest = 'sha256:unknown';".to_owned(),
                "definition differs",
            ),
            (
                "final table under the published identity",
                format!(
                    "DROP TABLE workflow_artifact_payloads; {};",
                    final_table_sql()
                ),
                "workflow schema identity does not match",
            ),
            (
                "reformatted published table",
                format!("DROP TABLE workflow_artifact_payloads; {reformatted};"),
                "definition differs",
            ),
            (
                "different ceiling",
                format!("DROP TABLE workflow_artifact_payloads; {doubled_ceiling};"),
                "definition differs",
            ),
            (
                "another table drifted as well",
                "DROP TABLE workflow_handoffs;".to_owned(),
                "missing 1 (`workflow_handoffs`)",
            ),
            (
                "a second identity row",
                format!(
                    "PRAGMA ignore_check_constraints = ON;
                     INSERT INTO workflow_schema VALUES (2, 1, '{PUBLISHED_DIGEST}');"
                ),
                "several identity rows",
            ),
            (
                "a trigger on the artifact table",
                "CREATE TRIGGER artifact_audit AFTER INSERT ON workflow_artifact_payloads
                 BEGIN SELECT 1; END;"
                    .to_owned(),
                "1 other schema object(s)",
            ),
            (
                "a view over the artifact table",
                "CREATE VIEW artifact_view AS SELECT payload_digest FROM workflow_artifact_payloads;"
                    .to_owned(),
                "1 other schema object(s)",
            ),
            (
                "a table in the displaced table's name",
                "CREATE TABLE workflow_artifact_payloads_published (value TEXT);".to_owned(),
                "1 other schema object(s)",
            ),
            (
                "a trigger named exactly after the artifact table",
                "CREATE TRIGGER workflow_artifact_payloads AFTER INSERT ON workflow_artifact_payloads
                 BEGIN SELECT 1; END;"
                    .to_owned(),
                "1 other schema object(s)",
            ),
            (
                "a trigger on the identity table",
                "CREATE TRIGGER identity_audit AFTER UPDATE ON workflow_schema
                 BEGIN SELECT 1; END;"
                    .to_owned(),
                "1 other schema object(s)",
            ),
            (
                "an index on the artifact table",
                "CREATE INDEX artifact_length ON workflow_artifact_payloads (byte_length);"
                    .to_owned(),
                "1 other schema object(s)",
            ),
            (
                "a digest written around the strict column type",
                written_around_strict_types("x'00', 1, x'aa'"),
                "1 row(s)",
            ),
            (
                "a byte length written around the strict column type",
                written_around_strict_types("'sha256:text-length', 'one', x'aa'"),
                "1 row(s)",
            ),
            (
                "a payload written around the strict column type",
                written_around_strict_types("'sha256:text-payload', 2, 'ab'"),
                "1 row(s)",
            ),
            (
                "a row whose length disagrees",
                "PRAGMA ignore_check_constraints = ON;
                 INSERT INTO workflow_artifact_payloads VALUES ('sha256:liar', 1, x'0011');"
                    .to_owned(),
                "1 row(s)",
            ),
            (
                "a row above the published ceiling",
                format!(
                    "PRAGMA ignore_check_constraints = ON;
                     INSERT INTO workflow_artifact_payloads VALUES (
                         'sha256:above', {above}, zeroblob({above})
                     );",
                    above = PUBLISHED_CEILING + 1
                ),
                "1 row(s)",
            ),
        ];
        for (case, shape, expected) in cases {
            let store = PublishedStore::create().await;
            if shape.contains("DROP TABLE workflow_artifact_payloads") {
                store.run("DELETE FROM workflow_artifact_payloads;");
            }
            store.run(&shape);
            let before = store.state_tolerating_missing_tables();
            let reason = assert_workflow_refusal(store.admit().await);
            assert!(reason.contains(expected), "{case}: {reason}");
            assert_eq!(
                store.state_tolerating_missing_tables(),
                before,
                "{case} must be left untouched"
            );
        }
    }

    impl PublishedStore {
        /// Schema text and artifact rows, for cases that removed other tables
        /// from under [`WorkflowState::read`].
        fn state_tolerating_missing_tables(
            &self,
        ) -> (Vec<(String, Option<String>)>, usize, String) {
            let connection = Connection::open(&self.path).unwrap();
            let schema = connection
                .prepare(
                    "SELECT name, sql FROM sqlite_master
                     WHERE name LIKE 'workflow%' OR name LIKE 'artifact%' ORDER BY name",
                )
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            let rows = connection
                .query_row(
                    "SELECT count(*) FROM workflow_artifact_payloads",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            let digest = connection
                .query_row(
                    "SELECT group_concat(definition_digest) FROM workflow_schema",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            (schema, usize::try_from(rows).unwrap(), digest)
        }
    }

    /// Another authority's refusal is the store's verdict, so nothing of this
    /// store changes on the way to its reset.
    #[tokio::test]
    async fn store_refused_by_another_authority_keeps_the_published_shape() {
        let store = PublishedStore::create().await;
        let previous = LCM_SCHEMA_VERSION - 1;
        store.run(&format!(
            "UPDATE tracedecay_schema_version SET version = {previous} WHERE component = 'lcm';"
        ));
        let before = store.state();

        let refusal = store.admit().await.expect("an aged LCM refuses the store");
        assert!(
            matches!(refusal, TraceDecayError::ProfileResetRequired { .. }),
            "{refusal}"
        );
        assert_eq!(store.state(), before);
    }

    /// A published store at its profile path, held open for daemon admission.
    struct DaemonPublishedStore {
        store: PublishedStore,
        _scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    }

    impl DaemonPublishedStore {
        async fn create() -> Self {
            let directory = TempDir::new().unwrap();
            let profile_root = directory.path().join("profile");
            tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile_root)
                .unwrap();
            let path = tracedecay_sessions::runtime::user_sessions_db_path(&profile_root);
            drop(
                open_registered_test_database_fixture(
                    &path,
                    TestDatabaseRuntimeScope::ProfileSessions,
                )
                .await
                .unwrap(),
            );
            let mut connection = Connection::open(&path).unwrap();
            // Finish the private fixture before entering its daemon scope.
            let transaction = connection.transaction().unwrap();
            republish(&transaction, PUBLISHED_TABLE_SQL, PUBLISHED_DIGEST);
            transaction.execute_batch(PUBLISHED_ROWS).unwrap();
            transaction.commit().unwrap();
            drop(connection);
            let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
                &profile_root,
                crate::tests::harness::TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed),
                "published workflow shape",
            )
            .unwrap();
            Self {
                store: PublishedStore {
                    path,
                    _directory: directory,
                },
                _scope: scope,
            }
        }
    }

    async fn publish_daemon_runtime(
        path: &Path,
    ) -> tracedecay_runtime_core::db::RegisteredTestRuntimeFixtureV1 {
        let authority = tracedecay_runtime_core::db::DatabaseAuthority::for_owned_runtime(
            path,
            "publish published-shape daemon test runtime",
        )
        .unwrap();
        tracedecay_runtime_core::db::Database::publish_registered_daemon_test_runtime_with_retirement_control(
            path,
            &authority,
            tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Existing,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .unwrap()
    }

    /// The census asks what attach would decide. It must admit a store the
    /// real attach can upgrade, and must not leave that upgrade behind.
    #[tokio::test(flavor = "multi_thread")]
    async fn attach_refusal_inspection_admits_without_upgrading() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        let before = store.state();

        let (database, _runtime, _retirement) =
            publish_daemon_runtime(&store.path).await.into_parts();
        let refusal = RegisteredGlobalDbOwnerV1::attach_reset_refusal(&database)
            .await
            .unwrap();
        assert!(refusal.is_none(), "{refusal:?}");
        drop(database);

        assert_eq!(store.state(), before);
    }

    /// Daemon admission of a published store, with the store's convergence plan
    /// and the runtime parts that keep the store open until they drop.
    async fn admit_for_daemon(
        path: &Path,
    ) -> (
        RegisteredGlobalDbOwnerV1,
        crate::schema_stages::RegisteredSchemaConvergence,
        (
            StoreRuntimeClientLease,
            RegisteredTestRuntimeRetirementControlV1,
        ),
    ) {
        let (database, runtime, retirement) = publish_daemon_runtime(path).await.into_parts();
        let (owner, convergence) = RegisteredGlobalDbOwnerV1::admit_and_attach_for_daemon(
            database,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        (
            owner,
            convergence.expect("an admitted store has a plan"),
            (runtime, retirement),
        )
    }

    async fn assert_workflow_recovering(lease: &crate::RegisteredGlobalDbLeaseV1) {
        let error = lease.workflow_storage().await.err().expect("recovering");
        assert!(error.is_workflow_authority_recovering(), "{error}");
        assert_eq!(
            error
                .project_route_context()
                .map(|(_, retryable, _)| retryable),
            Some(true),
            "a store that is upgrading asks the client to retry"
        );
        assert!(!error.is_store_reset_required(), "{error}");
    }

    /// Daemon admission only decides. It copies nothing however large the
    /// store, answers typed recovering from workflow storage, and keeps every
    /// other authority serving; convergence then completes the move for clients
    /// issued before it committed, and the old table's pages leave the file.
    #[tokio::test(flavor = "multi_thread")]
    async fn daemon_admission_defers_the_copy_to_convergence() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        store.run(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 64)
             INSERT INTO workflow_artifact_payloads
             SELECT 'sha256:large-' || i, 1048576, randomblob(1048576) FROM n;",
        );
        store.run(
            "CREATE TABLE freed_pages(payload BLOB);
             INSERT INTO freed_pages VALUES (zeroblob(2097152));
             DROP TABLE freed_pages;",
        );
        let before = store.state();
        let wal_path = format!("{}-wal", store.path.display());
        let wal_bytes = || fs::metadata(&wal_path).map_or(0, |metadata| metadata.len());

        let (owner, convergence, _held) = admit_for_daemon(&store.path).await;
        assert!(
            wal_bytes() < 1024 * 1024,
            "daemon admission wrote {} WAL bytes, so it copied payloads",
            wal_bytes()
        );
        assert_eq!(
            store.state(),
            before,
            "admission changed the published shape"
        );
        assert!(owner.reset_required().is_none());
        let lease = owner.issue_lease().unwrap();
        let (_, _, free_pages) = lease.storage_page_counts().await.unwrap();
        assert!(
            free_pages > 0,
            "admission reclaimed pages before convergence"
        );
        let issuer = owner.weak_lease_issuer();
        assert_workflow_recovering(&lease).await;
        assert_workflow_recovering(&issuer.issue_lease().unwrap()).await;
        lease
            .runtime_database()
            .execute_write_batch(
                "another authority keeps writing",
                "INSERT INTO parse_offsets VALUES ('during-recovery', 1, 1, 1)",
            )
            .await
            .unwrap();

        lease
            .converge_schema(convergence, &CancellationToken::new())
            .await
            .unwrap();

        lease.workflow_storage().await.unwrap();
        owner
            .issue_lease()
            .unwrap()
            .workflow_storage()
            .await
            .unwrap();
        issuer
            .issue_lease()
            .unwrap()
            .workflow_storage()
            .await
            .unwrap();
        let after = store.state();
        assert_eq!(after.table_sql, final_table_sql());
        assert_eq!(after.digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);
        assert_eq!(after.artifacts, before.artifacts);
        assert_eq!(after.other_rows, before.other_rows);
        assert_eq!(
            after.objects_named_after_the_table,
            vec![
                "sqlite_autoindex_workflow_artifact_payloads_1",
                "workflow_artifact_payloads"
            ],
            "no copy of the old table survives success"
        );
        let (_, _, free_pages) = lease.storage_page_counts().await.unwrap();
        assert_eq!(free_pages, 0, "the dropped table's pages were kept");
    }

    /// Convergence observes shutdown at every statement. At each sampled point
    /// the store is either wholly published or wholly final, a cancelled store
    /// stays typed recovering, and the next open finishes the job.
    #[tokio::test(flavor = "multi_thread")]
    async fn background_upgrade_is_atomic_at_every_cancellation_point() {
        async fn converge_cancelled_at(
            path: &Path,
            cancel_at: usize,
        ) -> (Result<(), TraceDecayError>, usize) {
            let (owner, convergence, (runtime, retirement)) = admit_for_daemon(path).await;
            let lease = owner.issue_lease().unwrap();
            let cancellation = CancellationToken::new();
            let mut recovery = Box::pin(lease.converge_schema(convergence, &cancellation));
            let mut polls = 0;
            let outcome = std::future::poll_fn(|context| {
                polls += 1;
                if polls == cancel_at {
                    cancellation.cancel();
                }
                recovery.as_mut().poll(context)
            })
            .await;
            drop(recovery);
            if let Err(error) = &outcome
                && error.is_store_open_cancelled()
                && WorkflowState::read(path).digest == PUBLISHED_DIGEST
            {
                assert_workflow_recovering(&lease).await;
            }
            // Each sampled cancellation reopens this path. Join the exact
            // published runtime before the next admission can acquire it.
            drop((lease, owner, runtime));
            let mut reservation = match retirement
                .registry()
                .reserve_retirement_batch(vec![retirement.retirement_target()])
            {
                StoreRuntimeRetirementResult::Reserved(reservation) => reservation,
                StoreRuntimeRetirementResult::Blocked(refusal) => {
                    panic!("completed convergence retained runtime clients: {refusal:?}")
                }
            };
            let committed = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
                reservation.commit()
            })
            .await
            .unwrap()
            .unwrap();
            assert!(
                matches!(
                    committed.outcomes(),
                    [StoreRuntimeRetirementOutcome::Closed { .. }]
                ),
                "fixture runtime must close before reopening: {:?}",
                committed.outcomes()
            );
            (outcome, polls)
        }

        let reference = DaemonPublishedStore::create().await;
        reference.store.run(BULK_ROWS);
        let before = reference.store.state();
        let (outcome, total_polls) = converge_cancelled_at(&reference.store.path, usize::MAX).await;
        outcome.unwrap();
        let completed = reference.store.state();
        assert_eq!(completed.digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);
        assert_eq!(completed.artifacts, before.artifacts);

        let stride = (total_polls / 12).max(1);
        let mut cancelled_published = 0;
        for cancel_at in (1..total_polls).step_by(stride) {
            let daemon_store = DaemonPublishedStore::create().await;
            let store = &daemon_store.store;
            store.run(BULK_ROWS);
            let (outcome, _) = converge_cancelled_at(&store.path, cancel_at).await;
            let state = store.state();
            match outcome {
                Ok(()) => assert_eq!(state, completed, "completed at poll {cancel_at}"),
                Err(error) => {
                    assert!(error.is_store_open_cancelled(), "poll {cancel_at}: {error}");
                    assert!(
                        state == before || state == completed,
                        "poll {cancel_at} left a partial upgrade"
                    );
                    if state == before {
                        cancelled_published += 1;
                        let (reopened, _) = converge_cancelled_at(&store.path, usize::MAX).await;
                        reopened.unwrap();
                        assert_eq!(store.state(), completed, "reopen after poll {cancel_at}");
                    }
                }
            }
        }
        assert!(
            cancelled_published > 0,
            "the sweep never cancelled before the commit"
        );
    }

    /// Retirement aborts the worker, which drops the upgrade wherever it is
    /// parked. Nothing of it may survive, and the next open starts over.
    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_background_upgrade_mid_copy_rolls_it_back() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        store.run(BULK_ROWS);
        let before = store.state();

        let (owner, _, held) = admit_for_daemon(&store.path).await;
        let lease = owner.issue_lease().unwrap();
        let transaction = lease
            .runtime_database()
            .begin_bulk_write_transaction("abort")
            .await
            .unwrap();
        let copying = StatementCounter::new(&transaction, "INSERT INTO workflow_artifact_payloads");
        let mut upgrade = Box::pin(upgrade_published_artifact_ceiling(&copying));
        drive_until(&mut upgrade, || copying.seen() >= 2).await;
        drop(upgrade);
        drop(transaction);

        assert_eq!(store.state(), before, "a dropped upgrade left a trace");
        assert_workflow_recovering(&lease).await;
        drop((lease, owner, held));

        let (owner, convergence, _held) = admit_for_daemon(&store.path).await;
        let lease = owner.issue_lease().unwrap();
        assert_workflow_recovering(&lease).await;
        lease
            .converge_schema(convergence, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(store.state().artifacts, before.artifacts);
        assert_eq!(store.state().digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);
    }

    /// WAL readers see the store as it was for as long as the copy runs. Readers
    /// are what the upgrade must never hold up, so this parks it between two
    /// batches with the writer lane held.
    #[tokio::test(flavor = "multi_thread")]
    async fn reads_serve_while_the_upgrade_holds_the_writer() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        store.run(BULK_ROWS);
        store.run("INSERT INTO parse_offsets VALUES ('readable', 7, 1, 1);");
        let before = store.state();

        let (owner, convergence, _held) = admit_for_daemon(&store.path).await;
        let lease = owner.issue_lease().unwrap();
        let reader = owner.issue_read_only_lease().unwrap();
        let transaction = lease
            .runtime_database()
            .begin_bulk_write_transaction("hold")
            .await
            .unwrap();
        let copying = StatementCounter::new(&transaction, "INSERT INTO workflow_artifact_payloads");
        let mut upgrade = Box::pin(upgrade_published_artifact_ceiling(&copying));
        drive_until(&mut upgrade, || copying.seen() >= 2).await;

        let mut rows = reader
            .read_connection()
            .query(
                "SELECT byte_offset FROM parse_offsets WHERE file_path = 'readable'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 7);
        let mut artifacts = reader
            .read_connection()
            .query("SELECT count(*) FROM workflow_artifact_payloads", ())
            .await
            .unwrap();
        let count = artifacts.next().await.unwrap().unwrap();
        assert_eq!(
            usize::try_from(count.get::<i64>(0).unwrap()).unwrap(),
            before.artifacts.len(),
            "a reader saw the transaction's half-copied table"
        );
        assert_workflow_recovering(&reader).await;
        assert_eq!(store.state(), before, "an outside reader saw the copy");

        assert!(upgrade.as_mut().await.unwrap().is_none());
        drop(upgrade);
        transaction.commit().await.unwrap();
        assert_eq!(store.state().digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);

        // A peer already finished, so this recovery proves completion under the
        // write lock and opens workflow storage for its own clients.
        assert_workflow_recovering(&lease).await;
        lease
            .converge_schema(convergence, &CancellationToken::new())
            .await
            .unwrap();
        lease.workflow_storage().await.unwrap();
    }

    /// A store changed between admission and convergence is proven again under
    /// the write lock: it is held in its typed reset state, untouched, and
    /// clients issued before the verdict see it too.
    #[tokio::test(flavor = "multi_thread")]
    async fn background_refusal_holds_the_store_in_its_reset_state() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        let (owner, convergence, _held) = admit_for_daemon(&store.path).await;
        let lease = owner.issue_lease().unwrap();
        store.run(
            "CREATE TRIGGER peer_audit AFTER DELETE ON workflow_artifact_payloads
             BEGIN SELECT 1; END;",
        );
        let before = store.state();

        let refused = lease
            .converge_schema(convergence, &CancellationToken::new())
            .await
            .expect_err("the store changed under its plan");
        assert!(refused.is_store_reset_required(), "{refused}");
        assert!(assert_workflow_refusal(lease.reset_required()).contains("1 other schema object"));
        assert!(assert_workflow_refusal(owner.reset_required()).contains("1 other schema object"));
        assert!(lease.workflow_storage().await.is_err());
        assert_eq!(store.state(), before);
    }

    /// A failure that can clear says nothing about the store: workflow storage
    /// stays recovering, nothing changed, and the next attempt finishes.
    #[tokio::test(flavor = "multi_thread")]
    async fn held_lock_at_convergence_keeps_the_store_recovering() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        let before = store.state();
        let (owner, convergence, held) = admit_for_daemon(&store.path).await;
        let lease = owner.issue_lease().unwrap();
        let peer = Connection::open(&store.path).unwrap();
        peer.execute_batch("BEGIN IMMEDIATE").unwrap();

        let error = lease
            .converge_schema(convergence, &CancellationToken::new())
            .await
            .expect_err("the write lock is held");
        assert!(!error.is_store_reset_required(), "{error}");
        assert!(lease.reset_required().is_none());
        assert_workflow_recovering(&lease).await;
        peer.execute_batch("ROLLBACK").unwrap();
        drop(peer);
        assert_eq!(store.state(), before);

        lease
            .converge_schema(convergence, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(store.state().digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);
        assert_eq!(store.state().artifacts, before.artifacts);
        lease.workflow_storage().await.unwrap();
        drop((lease, owner, held));
    }

    /// `SQLITE_FULL`, which the guarded writer cannot be made to raise here.
    const SQLITE_FULL: i32 = 13;

    /// Rows spread over several copy batches, with rowid gaps and a negative
    /// rowid a batch cursor starting at zero would lose.
    const BULK_ROWS: &str = "
        WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100)
        INSERT INTO workflow_artifact_payloads (rowid, payload_digest, byte_length, payload)
        SELECT i * 3 + 100, 'sha256:bulk-' || i, 8, CAST(printf('%08d', i) AS BLOB) FROM n;
        INSERT INTO workflow_artifact_payloads (rowid, payload_digest, byte_length, payload)
        VALUES (-5, 'sha256:negative-rowid', 1, x'07');";

    /// Counts one kind of statement so a test can park an upgrade mid-copy.
    struct StatementCounter<'a, T> {
        inner: &'a T,
        needle: &'static str,
        seen: AtomicUsize,
    }

    impl<'a, T> StatementCounter<'a, T> {
        fn new(inner: &'a T, needle: &'static str) -> Self {
            Self {
                inner,
                needle,
                seen: AtomicUsize::new(0),
            }
        }

        fn seen(&self) -> usize {
            self.seen.load(Ordering::Relaxed)
        }

        fn note(&self, sql: &str) {
            if sql.contains(self.needle) {
                self.seen.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    impl<T: engine::QueryExecutor + Sync> engine::QueryExecutor for StatementCounter<'_, T> {
        async fn query<P>(&self, sql: &str, params: P) -> engine::Result<engine::Rows>
        where
            P: engine::IntoParams,
        {
            self.note(sql);
            self.inner.query(sql, params).await
        }
    }

    impl<T: engine::Executor + Sync> engine::Executor for StatementCounter<'_, T> {
        async fn execute<P>(&self, sql: &str, params: P) -> engine::Result<u64>
        where
            P: engine::IntoParams,
        {
            self.note(sql);
            self.inner.execute(sql, params).await
        }

        async fn execute_statements(
            &self,
            statements: Vec<engine::WriteStatement>,
        ) -> engine::Result<Vec<u64>> {
            self.inner.execute_statements(statements).await
        }

        async fn execute_batch(&self, sql: &str) -> engine::Result<()> {
            self.note(sql);
            self.inner.execute_batch(sql).await
        }
    }

    /// Polls `future` until `reached`, then leaves it parked where it is.
    async fn drive_until<F: Future + Unpin>(future: &mut F, reached: impl Fn() -> bool) {
        std::future::poll_fn(|context| {
            if reached() {
                return Poll::Ready(());
            }
            match Pin::new(&mut *future).poll(context) {
                Poll::Ready(_) => panic!("the upgrade finished before it was parked"),
                Poll::Pending if reached() => Poll::Ready(()),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
    }

    /// An attached runtime whose statements fail at one chosen occurrence, for
    /// the faults a healthy store never produces.
    struct FailingStatement<'a, T> {
        inner: &'a T,
        needle: &'static str,
        occurrence: AtomicUsize,
        error: fn() -> engine::Error,
    }

    impl<'a, T> FailingStatement<'a, T> {
        fn new(
            inner: &'a T,
            needle: &'static str,
            occurrence: usize,
            error: fn() -> engine::Error,
        ) -> Self {
            Self {
                inner,
                needle,
                occurrence: AtomicUsize::new(occurrence),
                error,
            }
        }

        fn check(&self, sql: &str) -> engine::Result<()> {
            if sql.contains(self.needle) && self.occurrence.fetch_sub(1, Ordering::Relaxed) == 1 {
                return Err((self.error)());
            }
            Ok(())
        }
    }

    impl<T: engine::QueryExecutor + Sync> engine::QueryExecutor for FailingStatement<'_, T> {
        async fn query<P>(&self, sql: &str, params: P) -> engine::Result<engine::Rows>
        where
            P: engine::IntoParams,
        {
            self.check(sql)?;
            self.inner.query(sql, params).await
        }
    }

    impl<T: engine::Executor + Sync> engine::Executor for FailingStatement<'_, T> {
        async fn execute<P>(&self, sql: &str, params: P) -> engine::Result<u64>
        where
            P: engine::IntoParams,
        {
            self.check(sql)?;
            self.inner.execute(sql, params).await
        }

        async fn execute_statements(
            &self,
            statements: Vec<engine::WriteStatement>,
        ) -> engine::Result<Vec<u64>> {
            self.inner.execute_statements(statements).await
        }

        async fn execute_batch(&self, sql: &str) -> engine::Result<()> {
            self.check(sql)?;
            self.inner.execute_batch(sql).await
        }
    }

    /// A step failing anywhere in the upgrade, including between copy batches
    /// and in the final proof, restores everything once the admission
    /// transaction rolls back. A refused statement is the store's typed reset;
    /// a held database or a full disk is not, so a passing condition never asks
    /// for one.
    #[tokio::test(flavor = "multi_thread")]
    async fn failure_at_any_upgrade_step_rolls_back_with_the_right_typed_state() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        store.run(BULK_ROWS);
        let before = store.state();
        let (database, _runtime, _retirement) =
            publish_daemon_runtime(&store.path).await.into_parts();
        let lease = database.issue_lease().unwrap();

        let transient: [fn() -> engine::Error; 2] = [
            || engine::Error::Busy,
            || engine::Error::Sqlite {
                operation: "step",
                code: Some(SQLITE_FULL),
                extended_code: Some(SQLITE_FULL),
                message: "database or disk is full".to_owned(),
            },
        ];
        for fault in transient {
            for (needle, occurrence) in [
                ("ALTER TABLE", 1),
                ("CREATE TABLE workflow_artifact_payloads", 1),
                ("INSERT INTO workflow_artifact_payloads", 1),
                ("INSERT INTO workflow_artifact_payloads", 3),
                ("DROP TABLE", 1),
                ("UPDATE workflow_schema", 1),
                ("table_info", 1),
            ] {
                let transaction = lease.begin_bulk_write_transaction("fault").await.unwrap();
                let failed = upgrade_published_artifact_ceiling(&FailingStatement::new(
                    &transaction,
                    needle,
                    occurrence,
                    fault,
                ))
                .await
                .err()
                .unwrap_or_else(|| panic!("{needle}#{occurrence} must fail the upgrade"));
                assert!(!failed.is_store_reset_required(), "{needle}: {failed}");
                transaction.rollback().await.unwrap();
                assert_eq!(
                    store.state(),
                    before,
                    "{needle}#{occurrence} was not undone"
                );
            }
        }

        for (needle, occurrence) in [
            ("INSERT INTO workflow_artifact_payloads", 2),
            ("DROP TABLE", 1),
            ("UPDATE workflow_schema", 1),
        ] {
            let transaction = lease.begin_bulk_write_transaction("fault").await.unwrap();
            let refused = upgrade_published_artifact_ceiling(&FailingStatement::new(
                &transaction,
                needle,
                occurrence,
                || engine::Error::InvalidOperation("refused".to_owned()),
            ))
            .await
            .err()
            .unwrap_or_else(|| panic!("{needle}#{occurrence} must fail the upgrade"));
            let reason = assert_workflow_refusal(Some(refused));
            assert!(reason.contains("left as found"), "{reason}");
            transaction.rollback().await.unwrap();
            assert_eq!(
                store.state(),
                before,
                "{needle}#{occurrence} was not undone"
            );
        }

        // The same runtime takes the upgrade once nothing interferes, and
        // moves every row of every batch.
        let transaction = lease.begin_bulk_write_transaction("upgrade").await.unwrap();
        assert!(
            upgrade_published_artifact_ceiling(&transaction)
                .await
                .unwrap()
                .is_none()
        );
        transaction.commit().await.unwrap();
        let after = store.state();
        assert_eq!(after.table_sql, final_table_sql());
        assert_eq!(after.artifacts, before.artifacts);
        assert_eq!(after.artifacts.len(), 104);
        assert_eq!(after.artifacts[0].0, -5);
    }

    /// Classification runs before the write lock. A peer that finished the
    /// upgrade in between leaves nothing to do; one that changed the store into
    /// another shape in between gets the refusal, not a blind rewrite.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_write_lock_proof_decides_after_a_stale_classification() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        let (database, _runtime, _retirement) =
            publish_daemon_runtime(&store.path).await.into_parts();
        let lease = database.issue_lease().unwrap();

        store.run(
            "CREATE TRIGGER peer_audit AFTER DELETE ON workflow_artifact_payloads
                   BEGIN SELECT 1; END;",
        );
        let before = store.state();
        let transaction = lease.begin_bulk_write_transaction("stale").await.unwrap();
        let refused = upgrade_published_artifact_ceiling(&transaction)
            .await
            .unwrap()
            .expect("a shape changed under the classification is refused");
        assert!(
            assert_workflow_refusal(Some(refused.error())).contains("1 other schema object(s)")
        );
        transaction.commit().await.unwrap();
        assert_eq!(store.state(), before);
        store.run("DROP TRIGGER peer_audit;");

        for _ in 0..2 {
            let transaction = lease.begin_bulk_write_transaction("peer").await.unwrap();
            assert!(
                upgrade_published_artifact_ceiling(&transaction)
                    .await
                    .unwrap()
                    .is_none()
            );
            transaction.commit().await.unwrap();
        }
        let after = store.state();
        assert_eq!(after.digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);
        assert_eq!(after.artifacts, before.artifacts);
    }

    /// A peer holding the write lock delays admission and says nothing about
    /// the store: the attach fails retryably, and the next one admits.
    #[tokio::test(flavor = "multi_thread")]
    async fn held_write_lock_delays_admission_without_a_reset() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        let before = store.state();
        let peer = Connection::open(&store.path).unwrap();
        peer.execute_batch("BEGIN IMMEDIATE").unwrap();

        let (database, _runtime, _retirement) =
            publish_daemon_runtime(&store.path).await.into_parts();
        let error = RegisteredGlobalDbOwnerV1::admit_and_attach_for_daemon(
            database,
            &CancellationToken::new(),
        )
        .await
        .err()
        .expect("the write lock is held");
        assert!(!error.is_store_reset_required(), "{error}");
        peer.execute_batch("ROLLBACK").unwrap();
        drop(peer);
        assert_eq!(store.state(), before);

        let (owner, convergence, held) = admit_for_daemon(&store.path).await;
        assert!(owner.reset_required().is_none());
        owner
            .issue_lease()
            .unwrap()
            .converge_schema(convergence, &CancellationToken::new())
            .await
            .unwrap();
        drop((owner, held));
        assert_eq!(store.state().digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);
    }

    /// A store whose schema refuses the rename keeps serving its other
    /// authorities, and the census names the same refusal attach reaches.
    #[tokio::test(flavor = "multi_thread")]
    async fn census_and_attach_agree_on_a_schema_that_refuses_the_move() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        store.run("CREATE VIEW broken_view AS SELECT missing FROM nowhere;");
        let before = store.state();

        let (database, _runtime, _retirement) =
            publish_daemon_runtime(&store.path).await.into_parts();
        let census = RegisteredGlobalDbOwnerV1::attach_reset_refusal(&database)
            .await
            .unwrap();
        let census_reason = assert_workflow_refusal(census);
        assert!(census_reason.contains("moving"), "{census_reason}");
        assert_eq!(store.state(), before);

        let (owner, _) = RegisteredGlobalDbOwnerV1::admit_and_attach_for_daemon(
            database,
            &CancellationToken::new(),
        )
        .await
        .expect("the store's other authorities stay available");
        let attach_reason = assert_workflow_refusal(owner.reset_required());
        assert_eq!(attach_reason, census_reason);
        drop(owner);
        assert_eq!(store.state(), before);
    }

    /// Asking whether a store can upgrade costs no payload IO: the census
    /// leaves the WAL at its prior size however much the store holds, while the
    /// same store's convergence still moves every row.
    #[tokio::test(flavor = "multi_thread")]
    async fn census_writes_no_payload_bytes() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        store.run(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 64)
             INSERT INTO workflow_artifact_payloads
             SELECT 'sha256:large-' || i, 1048576, randomblob(1048576) FROM n;",
        );
        let wal_path = format!("{}-wal", store.path.display());
        let wal_bytes = || fs::metadata(&wal_path).map_or(0, |metadata| metadata.len());
        let before = store.state();

        let (database, _runtime, _retirement) =
            publish_daemon_runtime(&store.path).await.into_parts();
        let wal_before = wal_bytes();
        for _ in 0..2 {
            let refusal = RegisteredGlobalDbOwnerV1::attach_reset_refusal(&database)
                .await
                .unwrap();
            assert!(refusal.is_none(), "{refusal:?}");
        }
        assert_eq!(wal_bytes(), wal_before, "the census rewrote the table");
        assert_eq!(store.state(), before);

        let (owner, convergence) = RegisteredGlobalDbOwnerV1::admit_and_attach_for_daemon(
            database,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(owner.reset_required().is_none());
        owner
            .issue_lease()
            .unwrap()
            .converge_schema(
                convergence.expect("an admitted store has a plan"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        drop(owner);
        assert_eq!(store.state().artifacts, before.artifacts);
    }

    /// An observation refusal surfaces late in the stage sequence, after the
    /// workflow stage. It still leaves the published shape in place.
    #[tokio::test]
    async fn observation_refusal_keeps_the_published_shape() {
        let store = PublishedStore::create().await;
        store.run(
            "DELETE FROM tracedecay_schema_version
             WHERE component = 'observations-unified-identity-v1';",
        );
        let before = store.state();

        let refusal = store.admit().await.expect("old observations refuse");
        let Some(("observations", _)) = refusal.reset_required_context() else {
            panic!("refused with the wrong typed state: {refusal}");
        };
        assert_eq!(store.state(), before);
    }

    /// The classification page boundary must not hide a bad row, and a store
    /// past one page still upgrades whole.
    #[tokio::test]
    async fn row_classification_reaches_every_page() {
        const PAGED_ROWS: &str = "
            WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 4200)
            INSERT INTO workflow_artifact_payloads
            SELECT 'sha256:paged-' || i, 1, x'01' FROM n;";
        let store = PublishedStore::create().await;
        store.run(PAGED_ROWS);
        store.run(
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO workflow_artifact_payloads VALUES ('sha256:liar', 1, x'0011');",
        );
        let reason = assert_workflow_refusal(store.admit().await);
        assert!(reason.contains("1 row(s)"), "{reason}");

        let store = PublishedStore::create().await;
        store.run(PAGED_ROWS);
        let before = store.state();
        assert!(store.admit().await.is_none());
        assert_eq!(store.state().artifacts, before.artifacts);
    }

    /// A store that already carries the final contract is complete, with no
    /// upgrade work, whatever version label the binary that wrote it carried.
    #[tokio::test(flavor = "multi_thread")]
    async fn final_shape_store_has_nothing_to_upgrade() {
        let daemon_store = DaemonPublishedStore::create().await;
        let store = &daemon_store.store;
        store.run(&format!(
            "DELETE FROM workflow_artifact_payloads;
             DROP TABLE workflow_artifact_payloads;
             {};
             INSERT INTO workflow_artifact_payloads VALUES ('sha256:final', 1, x'01');
             UPDATE workflow_schema SET definition_digest = '{WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1}';",
            final_table_sql()
        ));
        let before = store.state();
        let (database, _runtime, _retirement) =
            publish_daemon_runtime(&store.path).await.into_parts();
        let lease = database.issue_lease().unwrap();
        let transaction = lease.begin_bulk_write_transaction("final").await.unwrap();
        assert!(
            upgrade_published_artifact_ceiling(&transaction)
                .await
                .unwrap()
                .is_none()
        );
        transaction.commit().await.unwrap();
        assert_eq!(store.state(), before);
    }

    /// The profile authority holds no workflow rows and never reads these
    /// tables, so it keeps them exactly as found.
    #[tokio::test]
    async fn profile_authority_is_not_upgraded() {
        let store = PublishedStore::create_at("global.db", TestDatabaseRuntimeScope::Profile).await;
        let before = store.state();

        let (lease, owner) =
            open_registered_test_database_fixture(&store.path, TestDatabaseRuntimeScope::Profile)
                .await
                .unwrap();
        assert!(owner.reset_required().is_none());
        drop((lease, owner));

        assert_eq!(store.state(), before);
    }
}
