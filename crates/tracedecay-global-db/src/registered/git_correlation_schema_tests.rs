use std::fs;

use tempfile::TempDir;
use tracedecay_domain::ProjectId;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
use tracedecay_sessions::runtime::git_correlation::GIT_CORRELATION_SCHEMA_VERSION;

use crate::tests::harness::open_registered_test_database_fixture;

/// A store recorded at the previous Git evidence schema is admitted in its typed reset-required state and left byte-for-byte
/// untouched: nothing converts or copies the older shape.
#[tokio::test]
async fn an_older_git_correlation_schema_is_refused_without_mutation() {
    crate::register_registered_schema_installer();
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("project/sessions.db");
    fs::create_dir_all(database_path.parent().unwrap()).unwrap();
    let scope = || TestDatabaseRuntimeScope::ProjectSessions {
        project_id: ProjectId::new("project.git-correlation-schema").unwrap(),
    };
    drop(
        open_registered_test_database_fixture(&database_path, scope())
            .await
            .unwrap(),
    );
    let previous = GIT_CORRELATION_SCHEMA_VERSION - 1;
    let connection = tracedecay_sql::Connection::open(&database_path).unwrap();
    connection
        .execute(
            "UPDATE session_schema_migrations SET version = ?1 WHERE name = 'git_correlation'",
            [previous],
        )
        .unwrap();
    // The main file holds every commit only once the log is copied back; the
    // native engine does not checkpoint when a connection closes.
    tracedecay_sql::ConnectionControl::checkpoint(
        &connection,
        tracedecay_sql::CheckpointMode::Truncate,
    )
    .unwrap();
    drop(connection);
    let before = fs::read(&database_path).unwrap();

    let (lease, owner) = open_registered_test_database_fixture(&database_path, scope())
        .await
        .expect("the store's other authorities stay admissible");
    for refusal in [lease.reset_required(), owner.reset_required()] {
        assert!(
            matches!(
                refusal,
                Some(TraceDecayError::ProfileResetRequired {
                    component: "git correlation",
                    found_version: Some(found),
                    required_version: GIT_CORRELATION_SCHEMA_VERSION,
                }) if found == previous
            ),
            "an older Git correlation schema refuses session features: {refusal:?}"
        );
    }
    drop((lease, owner));
    assert_eq!(
        fs::read(&database_path).unwrap(),
        before,
        "typed refusal must preserve the exact main database bytes"
    );
}
