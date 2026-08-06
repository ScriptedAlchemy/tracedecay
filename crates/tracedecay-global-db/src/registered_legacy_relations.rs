use tracedecay_runtime_core::db::engine::QueryExecutor;
use tracedecay_runtime_core::errors::TraceDecayError;
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardScopeV1};

const LEGACY_SESSION_RELATION_TABLES: [&str; 5] = [
    "session_summary_sources",
    "session_summary_successors",
    "session_logical_copy_edges",
    "session_thread_hierarchy_edges",
    "session_agent_hierarchy_edges",
];

pub(crate) async fn reject_legacy_session_relation_shape(
    connection: &impl QueryExecutor,
    binding: &StoreRuntimeBindingV1,
) -> tracedecay_runtime_core::errors::Result<()> {
    if !matches!(
        &binding.shard_id.scope,
        StoreShardScopeV1::ProjectSessions { .. } | StoreShardScopeV1::ProfileSessions
    ) {
        return Ok(());
    }
    for table in LEGACY_SESSION_RELATION_TABLES {
        let mut rows = connection
            .query(
                "SELECT 1
                 FROM sqlite_master
                 WHERE type = 'table' AND name = ?1
                 LIMIT 1",
                [table],
            )
            .await
            .map_err(|error| inspection_error(error))?;
        if rows
            .next()
            .await
            .map_err(|error| inspection_error(error))?
            .is_some()
        {
            return Err(TraceDecayError::reset_required(
                "registered session relation store",
                format!(
                    "registered session store contains retired relational authority \
                     '{table}'; reset this session shard before daemon admission"
                ),
            ));
        }
    }
    Ok(())
}

fn inspection_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: "inspect legacy session relation shape".to_owned(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;
    use tracedecay_runtime_core::db::{
        Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
        enter_daemon_database_scope,
    };

    use super::*;
    use crate::RegisteredGlobalDb;

    fn schema_snapshot(path: &Path) -> Vec<(String, String, String)> {
        let connection = rusqlite::Connection::open(path).expect("open schema snapshot");
        let mut statement = connection
            .prepare(
                "SELECT type, name, COALESCE(sql, '')
                 FROM sqlite_master
                 ORDER BY type, name",
            )
            .expect("prepare schema snapshot");
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query schema snapshot")
            .collect::<Result<Vec<_>, _>>()
            .expect("read schema snapshot")
    }

    #[tokio::test]
    async fn attach_paths_require_typed_reset_without_mutating_legacy_profile_shape() {
        for daemon_attach in [false, true] {
            crate::register_test_schema_installer();
            let directory = TempDir::new().expect("temporary profile");
            let database_path = directory.path().join("sessions.db");
            {
                let connection =
                    rusqlite::Connection::open(&database_path).expect("create legacy profile");
                connection
                    .execute_batch(
                        "CREATE TABLE session_summary_sources (
                             summary_id TEXT NOT NULL,
                             source_ordinal INTEGER NOT NULL
                         );
                         INSERT INTO session_summary_sources VALUES ('legacy-summary', 0);",
                    )
                    .expect("legacy relation shape");
            }
            let _scope =
                enter_daemon_database_scope(directory.path(), 1, "legacy-relation-shape-test")
                    .expect("database scope");
            let authority = DatabaseAuthority::acquire_test(
                &database_path,
                "legacy relation shape test runtime",
            )
            .expect("database authority");
            let (database, _) = Database::publish_registered_test_runtime(
                &database_path,
                &authority,
                TestDatabaseRuntimeMode::Existing,
                TestDatabaseRuntimeScope::ProfileSessions,
            )
            .await
            .expect("existing registered runtime");
            // The runtime open itself configures the journal mode, so the
            // untouched-shape contract is captured after publication: the
            // attach refusal below must not migrate, write, or checkpoint.
            let before_schema = schema_snapshot(&database_path);
            let before_bytes = fs::read(&database_path).expect("legacy database bytes");
            let before_len = fs::metadata(&database_path)
                .expect("legacy database metadata")
                .len();
            let runtime = database.retained_runtime().clone();
            let expected_binding = runtime.binding().clone();
            let expected_locator = runtime.locator().verified().clone();
            let attach_authority = runtime
                .database_authority("reject legacy relation shape")
                .expect("attach authority");
            let error = if daemon_attach {
                RegisteredGlobalDb::migrate_and_attach_for_daemon(
                    runtime,
                    expected_binding,
                    expected_locator,
                    attach_authority,
                )
                .await
                .err()
                .expect("daemon attach must reject legacy relation shape")
            } else {
                RegisteredGlobalDb::migrate_and_attach(
                    runtime,
                    expected_binding,
                    expected_locator,
                    attach_authority,
                )
                .await
                .err()
                .expect("attach must reject legacy relation shape")
            };

            assert!(matches!(error, TraceDecayError::ResetRequired { .. }));
            assert_eq!(schema_snapshot(&database_path), before_schema);
            assert_eq!(
                fs::metadata(&database_path)
                    .expect("post-refusal database metadata")
                    .len(),
                before_len
            );
            assert_eq!(
                fs::read(&database_path).expect("post-refusal database bytes"),
                before_bytes
            );
        }
    }
}
