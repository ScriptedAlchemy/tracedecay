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

#[asupersync::test]
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

#[asupersync::test]
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

#[asupersync::test]
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

#[asupersync::test]
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
    use std::future::Future as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::task::Poll;

    use rusqlite::Connection;
    use tempfile::TempDir;
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_lcm::LCM_SCHEMA_VERSION;
    use tracedecay_runtime_core::cancellation::CancellationToken;
    use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
    use tracedecay_rusqlite_runtime::workflow::{
        WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1, WORKFLOW_TABLE_CONTRACTS_V1,
    };

    use crate::RegisteredGlobalDbOwnerV1;
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
            let connection = Connection::open(&path).unwrap();
            republish(&connection, PUBLISHED_TABLE_SQL, PUBLISHED_DIGEST);
            connection.execute_batch(PUBLISHED_ROWS).unwrap();
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

    #[asupersync::test]
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

    #[asupersync::test]
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

    /// A failure at the very last step, after the rows moved and the published
    /// table was dropped, must restore all of it, and the store must take the
    /// upgrade once the cause is gone.
    #[asupersync::test]
    async fn failed_upgrade_rolls_back_every_step_and_succeeds_once_unblocked() {
        let store = PublishedStore::create().await;
        store.run(
            "CREATE TRIGGER block_workflow_identity BEFORE UPDATE ON workflow_schema
             BEGIN SELECT RAISE(ABORT, 'foreign trigger text'); END;",
        );
        let before = store.state();

        let error = open_registered_test_database_fixture(
            &store.path,
            TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .err()
        .expect("an upgrade that cannot finish must not publish the store");
        let Some(("workflow", reason)) = error.reset_required_context() else {
            panic!("refused with the wrong typed state: {error}");
        };
        assert!(reason.contains("left as found"), "{reason}");
        assert!(!reason.contains("foreign trigger text"), "{reason}");
        assert_eq!(store.state(), before);

        store.run("DROP TRIGGER block_workflow_identity;");
        assert!(store.admit().await.is_none());
        let after = store.state();
        assert_eq!(after.digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);
        assert_eq!(after.artifacts, before.artifacts);
    }

    /// Each case differs from the proven shape in one way and must be refused
    /// with nothing changed.
    #[asupersync::test]
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
    #[asupersync::test]
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
            let connection = Connection::open(&path).unwrap();
            republish(&connection, PUBLISHED_TABLE_SQL, PUBLISHED_DIGEST);
            connection.execute_batch(PUBLISHED_ROWS).unwrap();
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
    /// real attach can upgrade, and must not perform that upgrade itself.
    #[asupersync::test(flavor = "multi_thread")]
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

    /// Daemon admission observes shutdown at every statement. At each sampled
    /// cancellation point the store is either wholly published or wholly final.
    #[asupersync::test(flavor = "multi_thread")]
    async fn daemon_admission_is_atomic_at_every_cancellation_point() {
        async fn admit_cancelled_at(
            path: &Path,
            cancel_at: usize,
        ) -> (Result<(), TraceDecayError>, usize) {
            let (database, _runtime, _retirement) = publish_daemon_runtime(path).await.into_parts();
            let cancellation = CancellationToken::new();
            let mut admission = Box::pin(RegisteredGlobalDbOwnerV1::admit_and_attach_for_daemon(
                database,
                &cancellation,
            ));
            let mut polls = 0;
            let outcome = std::future::poll_fn(|context| {
                polls += 1;
                if polls == cancel_at {
                    cancellation.cancel();
                }
                match admission.as_mut().poll(context) {
                    Poll::Ready(result) => Poll::Ready(result.map(|(owner, _)| {
                        assert!(owner.reset_required().is_none());
                    })),
                    Poll::Pending => Poll::Pending,
                }
            })
            .await;
            (outcome, polls)
        }

        let reference = DaemonPublishedStore::create().await;
        let before = reference.store.state();
        let (outcome, total_polls) = admit_cancelled_at(&reference.store.path, usize::MAX).await;
        outcome.unwrap();
        let completed = reference.store.state();
        assert_eq!(completed.digest, WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1);
        assert_eq!(completed.artifacts, before.artifacts);

        let stride = (total_polls / 12).max(1);
        let mut cancelled = 0;
        for cancel_at in (1..total_polls).step_by(stride) {
            let daemon_store = DaemonPublishedStore::create().await;
            let store = &daemon_store.store;
            let (outcome, _) = admit_cancelled_at(&store.path, cancel_at).await;
            let state = store.state();
            match outcome {
                Ok(()) => assert_eq!(state, completed, "completed at poll {cancel_at}"),
                Err(error) => {
                    assert!(error.is_store_open_cancelled(), "poll {cancel_at}: {error}");
                    assert!(
                        state == before || state == completed,
                        "poll {cancel_at} left a partial upgrade: {state:?}"
                    );
                    cancelled += 1;
                    let (reopened, _) = admit_cancelled_at(&store.path, usize::MAX).await;
                    reopened.unwrap();
                    assert_eq!(store.state(), completed, "reopen after poll {cancel_at}");
                }
            }
        }
        assert!(cancelled > 0, "the sweep never observed a cancellation");
    }

    /// The profile authority holds no workflow rows and never reads these
    /// tables, so it keeps them exactly as found.
    #[asupersync::test]
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
