use super::schema_contract::{
    ensure_authority_audit_checkpoint_schema, ensure_authority_invariant_schema,
    restore_immutability_after_canonical_repair, suspend_immutability_for_canonical_repair,
    validate_authority_rows_exhaustive, validate_authority_schema_contract,
    validate_registry_schema_contract,
};
use super::{
    configuration, git_index_transactions, global_db_operation_error, observation,
    observation_projection, session_temporal,
};
use tracedecay_runtime_core::db::engine::{
    Connection, Executor, QueryExecutor, TransactionBehavior,
};
use tracedecay_rusqlite_runtime::repository::AUTHORIZED_SCOPE_SET_SCHEMA_V1;
use tracedecay_rusqlite_runtime::work::WORK_SCHEMA_V1;

const REGISTRY_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS projects (
        path TEXT PRIMARY KEY,
        tokens_saved INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS code_projects (
        project_id TEXT PRIMARY KEY,
        canonical_root TEXT NOT NULL,
        display_root TEXT NOT NULL,
        primary_root_platform TEXT,
        primary_root_bytes BLOB,
        primary_root_last_seen_at INTEGER,
        git_common_dir TEXT,
        git_remote_url TEXT,
        default_branch TEXT,
        created_at INTEGER NOT NULL,
        last_seen_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS project_aliases (
        alias_path TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        last_seen_at INTEGER NOT NULL,
        FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE
    );
    CREATE TABLE IF NOT EXISTS store_instances (
        store_id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        store_kind TEXT NOT NULL,
        storage_mode TEXT NOT NULL,
        store_relpath TEXT NOT NULL,
        manifest_relpath TEXT,
        created_at INTEGER NOT NULL,
        last_verified_at INTEGER,
        last_write_at INTEGER,
        FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE
    );
    CREATE TABLE IF NOT EXISTS graph_scopes (
        graph_scope_id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        store_id TEXT NOT NULL,
        branch_name TEXT NOT NULL,
        db_relpath TEXT NOT NULL,
        parent_scope_id TEXT,
        last_synced_at INTEGER,
        writable INTEGER NOT NULL DEFAULT 1,
        FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE,
        FOREIGN KEY(store_id) REFERENCES store_instances(store_id) ON DELETE CASCADE
    );
    CREATE TABLE IF NOT EXISTS store_artifacts (
        store_id TEXT NOT NULL,
        artifact_kind TEXT NOT NULL,
        relpath TEXT NOT NULL,
        size_bytes INTEGER,
        schema_version TEXT,
        updated_at INTEGER,
        PRIMARY KEY (store_id, artifact_kind, relpath),
        FOREIGN KEY(store_id) REFERENCES store_instances(store_id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_project_aliases_project_id
        ON project_aliases(project_id);
    CREATE INDEX IF NOT EXISTS idx_store_instances_project_id
        ON store_instances(project_id);
    CREATE INDEX IF NOT EXISTS idx_graph_scopes_project_store
        ON graph_scopes(project_id, store_id);
";

const TRANSCRIPT_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS turns (
        message_id TEXT PRIMARY KEY,
        project_hash TEXT NOT NULL,
        session_id TEXT NOT NULL,
        model TEXT NOT NULL,
        timestamp INTEGER NOT NULL,
        input_tokens INTEGER NOT NULL,
        output_tokens INTEGER NOT NULL,
        cache_write_tokens INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens INTEGER NOT NULL DEFAULT 0,
        cost_usd REAL NOT NULL,
        category TEXT NOT NULL,
        tool_names TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX IF NOT EXISTS idx_turns_timestamp ON turns(timestamp);
    CREATE INDEX IF NOT EXISTS idx_turns_project ON turns(project_hash);
    CREATE INDEX IF NOT EXISTS idx_turns_model ON turns(model);
    CREATE TABLE IF NOT EXISTS parse_offsets (
        file_path TEXT PRIMARY KEY,
        byte_offset INTEGER NOT NULL,
        mtime INTEGER NOT NULL,
        file_id INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS savings_ledger (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts INTEGER NOT NULL,
        project_path TEXT NOT NULL,
        tool_name TEXT NOT NULL,
        before_tokens INTEGER NOT NULL,
        after_tokens INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_savings_ledger_ts ON savings_ledger(ts);
    CREATE INDEX IF NOT EXISTS idx_savings_ledger_project ON savings_ledger(project_path);
    CREATE TABLE IF NOT EXISTS analytics_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        provider TEXT NOT NULL,
        project_id TEXT NOT NULL,
        session_id TEXT,
        timestamp INTEGER NOT NULL,
        event_kind TEXT NOT NULL,
        hook_name TEXT,
        tool_name TEXT,
        tool_category TEXT,
        skill_name TEXT,
        hint_category TEXT,
        hint_id TEXT,
        outcome TEXT,
        metadata_json TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_analytics_events_provider_project_session
        ON analytics_events(provider, project_id, session_id, timestamp);
    CREATE INDEX IF NOT EXISTS idx_analytics_events_kind
        ON analytics_events(event_kind, timestamp);
    CREATE INDEX IF NOT EXISTS idx_analytics_events_project_time
        ON analytics_events(project_id, timestamp);
    CREATE INDEX IF NOT EXISTS idx_analytics_events_timestamp
        ON analytics_events(timestamp);
    CREATE UNIQUE INDEX IF NOT EXISTS idx_observability_event_idempotency
        ON analytics_events(provider, project_id, hint_id)
        WHERE provider = 'tracedecay-observability' AND hint_id IS NOT NULL;
    CREATE TABLE IF NOT EXISTS sessions (
        provider TEXT NOT NULL,
        session_id TEXT NOT NULL,
        project_key TEXT NOT NULL,
        project_path TEXT NOT NULL,
        title TEXT,
        started_at INTEGER,
        ended_at INTEGER,
        transcript_path TEXT,
        metadata_json TEXT,
        parent_session_id TEXT,
        is_subagent INTEGER NOT NULL DEFAULT 0,
        agent_id TEXT,
        parent_tool_use_id TEXT,
        PRIMARY KEY(provider, session_id)
    );
    CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(provider, project_key);
    CREATE INDEX IF NOT EXISTS idx_sessions_started_at ON sessions(started_at);
    CREATE TABLE IF NOT EXISTS session_messages (
        provider TEXT NOT NULL,
        message_id TEXT NOT NULL,
        session_id TEXT NOT NULL,
        role TEXT NOT NULL,
        timestamp INTEGER,
        ordinal INTEGER NOT NULL,
        occurrence_id TEXT,
        snippet_text TEXT NOT NULL,
        index_text TEXT NOT NULL,
        kind TEXT,
        model TEXT,
        tool_names TEXT,
        source_path TEXT,
        source_offset INTEGER,
        metadata_json TEXT,
        PRIMARY KEY(provider, message_id),
        FOREIGN KEY(provider, session_id)
            REFERENCES sessions(provider, session_id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS idx_session_messages_session
        ON session_messages(provider, session_id, ordinal);
    CREATE INDEX IF NOT EXISTS idx_session_messages_timestamp
        ON session_messages(timestamp);
    CREATE INDEX IF NOT EXISTS idx_session_messages_source
        ON session_messages(source_path);
";

/// Installs the final Registered schema into an empty sibling staging store.
///
/// Existing stores are never sent through this path. The runtime registry
/// validates their exact catalog read-only and returns `ResetRequired` for any
/// other shape.
pub async fn ensure_registered_schema(
    conn: &Connection,
) -> tracedecay_runtime_core::errors::Result<()> {
    const OPERATION: &str = "initialize registered global database schema";
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;

    let migration = async {
        transaction
            .execute_batch(REGISTRY_SCHEMA)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize global project registry", error)
            })?;
        validate_registry_schema_contract(&transaction).await?;

        configuration::ensure_configuration_schema(&transaction)
            .await
            .map_err(|error| global_db_operation_error("initialize configuration schema", error))?;
        git_index_transactions::ensure_git_index_transaction_schema(&transaction).await?;

        transaction
            .execute_batch(TRANSCRIPT_SCHEMA)
            .await
            .map_err(|error| global_db_operation_error("initialize transcript schema", error))?;
        transaction
            .execute_batch(WORK_SCHEMA_V1)
            .await
            .map_err(|error| global_db_operation_error("initialize Work schema", error))?;
        transaction
            .execute_batch(AUTHORIZED_SCOPE_SET_SCHEMA_V1)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize authorized scope-set schema", error)
            })?;
        ensure_authority_audit_checkpoint_schema(&transaction).await?;
        session_temporal::ensure_session_temporal_schema(&transaction).await?;
        observation::ensure_observation_schema(&transaction).await?;
        observation_projection::ensure_observation_projection_schema(&transaction)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize observation projection", error)
            })?;
        tracedecay_runtime_core::db::install_external_source_schema(
            &transaction,
            "initialize registered external source state",
        )
        .await?;
        ensure_authority_invariant_schema(&transaction).await?;

        tracedecay_sessions::runtime::lcm::schema::ensure_lcm_schema_in_transaction(&transaction)
            .await
            .map_err(|error| global_db_operation_error("initialize LCM schema", error))?;
        tracedecay_sessions::runtime::git_correlation::ensure_git_correlation_schema_in_transaction(
            &transaction,
        )
        .await
        .map_err(|error| global_db_operation_error("initialize git correlation schema", error))?;
        tracedecay_sessions::runtime::workflow_index::ensure_workflow_index_schema(&transaction)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize workflow index schema", error)
            })?;
        tracedecay_runtime_core::errors::Result::Ok(())
    }
    .await;

    match migration {
        Ok(()) => transaction
            .commit()
            .await
            .map_err(|error| global_db_operation_error("commit registered global schema", error))?,
        Err(error) => {
            return match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(global_db_operation_error(
                    "roll back registered global schema",
                    std::io::Error::other(format!("{error}; rollback failed: {rollback_error}")),
                )),
            };
        }
    }

    observation_projection::ensure_observation_projection_performance_indexes(conn)
        .await
        .map_err(|error| {
            global_db_operation_error("initialize observation projection indexes", error)
        })?;
    validate_authority_schema_contract(conn).await?;
    Ok(())
}

pub async fn validate_observation_authority_connection(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_authority_schema_contract(conn).await?;
    validate_authority_rows_exhaustive(conn).await
}

pub async fn begin_observation_authority_canonical_repair(
    conn: &impl Executor,
) -> tracedecay_runtime_core::errors::Result<()> {
    suspend_immutability_for_canonical_repair(conn).await
}

pub async fn finish_observation_authority_canonical_repair(
    conn: &impl Executor,
) -> tracedecay_runtime_core::errors::Result<()> {
    restore_immutability_after_canonical_repair(conn).await
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::ensure_registered_schema;
    use tracedecay_runtime_core::db::engine::TestConnection;

    #[tokio::test]
    async fn late_audit_failure_preserves_completed_idempotent_repairs() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        {
            let connection = TestConnection::open(&database_path);
            ensure_registered_schema(&connection)
                .await
                .expect("initialize authority schema");
        }
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch(
                    "PRAGMA foreign_keys = OFF;
                 DROP TRIGGER IF EXISTS projection_queue_identity_insert_v1;
                 DROP TRIGGER IF EXISTS session_query_cursor_keys_insert_guard_v1;
                 DROP TRIGGER IF EXISTS session_query_cursor_keys_retire_update_v1;
                 DROP TRIGGER IF EXISTS session_query_cursor_keys_rotate_insert_v1;
                 INSERT INTO projection_queue(observation_id, observation_sequence)
                 VALUES ('orphaned-observation', 1);
                 INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES
                    ('cursor-a', 1, X'01', 100, NULL),
                    ('cursor-b', 2, X'02', 200, NULL);
                 DELETE FROM authority_audit_checkpoints;",
                )
                .expect("seed a repair followed by a late audit failure");
        }

        let connection = TestConnection::open(&database_path);
        let error = ensure_registered_schema(&connection)
            .await
            .expect_err("corrupt cursor keys must fail the full offline audit");
        assert!(
            error
                .to_string()
                .contains("session cursor key rotation state is invalid"),
            "unexpected audit failure: {error}"
        );

        let mut rows = connection
            .query("SELECT COUNT(*) FROM projection_queue", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0,
            "an idempotent repair completed before a later audit failure must remain committed"
        );
        drop(rows);
        let mut rows = connection
            .query(
                "SELECT bounded_passes_since_exhaustive
                 FROM authority_audit_checkpoints
                 WHERE audit_name = 'observation-authority'",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            -1,
            "validated exhaustive-audit frontiers must remain resumable after a late failure"
        );
    }

    #[tokio::test]
    async fn foreign_key_failure_remains_blocking_after_trigger_repair() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        {
            let connection = TestConnection::open(&database_path);
            ensure_registered_schema(&connection)
                .await
                .expect("initialize authority schema");
        }
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch(
                    "PRAGMA foreign_keys = OFF;
                     DROP TRIGGER IF EXISTS projection_queue_identity_insert_v1;
                     CREATE TABLE audit_parent (id INTEGER PRIMARY KEY);
                     CREATE TABLE audit_child (
                        id INTEGER PRIMARY KEY,
                        parent_id INTEGER NOT NULL REFERENCES audit_parent(id)
                     );
                     INSERT INTO audit_child(id, parent_id) VALUES (1, 99);",
                )
                .expect("seed a foreign-key violation behind a broken trigger");
        }

        for attempt in 1..=2 {
            let connection = TestConnection::open(&database_path);
            let error = ensure_registered_schema(&connection)
                .await
                .expect_err("an observed foreign-key violation must keep admission closed");
            assert!(
                error
                    .to_string()
                    .contains("global database contains a foreign-key violation"),
                "open attempt {attempt} returned an unexpected error: {error}"
            );
        }
    }
}
