use std::fs;

use tempfile::TempDir;
use tracedecay_domain::ProjectId;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;

use crate::tests::harness::open_registered_test_database_fixture;

/// A store recorded at the whole-projection Git evidence schema (version 5)
/// is refused with the typed reset and left byte-for-byte untouched: nothing
/// converts or copies the older shape.
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
    rusqlite::Connection::open(&database_path)
        .unwrap()
        .execute(
            "UPDATE session_schema_migrations SET version = 5 WHERE name = 'git_correlation'",
            (),
        )
        .unwrap();
    let before = fs::read(&database_path).unwrap();

    let error = match open_registered_test_database_fixture(&database_path, scope()).await {
        Ok(_) => panic!("an older Git correlation schema must not be admitted"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        TraceDecayError::ProfileResetRequired {
            component: "git correlation",
            found_version: Some(5),
            required_version: 6,
        }
    ));
    assert_eq!(
        fs::read(&database_path).unwrap(),
        before,
        "typed refusal must preserve the exact main database bytes"
    );
}
