use super::schema_contract::{
    ensure_authority_invariant_schema, validate_authority_rows_exhaustive,
    validate_authority_schema_contract, validate_registry_schema_contract,
};
use super::{
    configuration, git_index_transactions, global_db_operation_error, observation,
    observation_projection, session_temporal,
};
use tracedecay_runtime_core::db::engine::{Connection, QueryExecutor, TransactionBehavior, params};
use tracedecay_rusqlite_runtime::repository::AUTHORIZED_SCOPE_SET_SCHEMA_V1;
use tracedecay_rusqlite_runtime::work::WORK_SCHEMA_V1;

const REGISTERED_SCHEMA_VERSION: i64 = 1;

const REGISTERED_SCHEMA_STATE: &str = "
    CREATE TABLE tracedecay_schema_state (
        domain TEXT PRIMARY KEY CHECK(domain = 'registered-global'),
        version INTEGER NOT NULL CHECK(version > 0)
    );
    INSERT INTO tracedecay_schema_state(domain, version)
    VALUES ('registered-global', 1);
";

const REGISTRY_SCHEMA: &str = "
    CREATE TABLE projects (
        path TEXT PRIMARY KEY,
        tokens_saved INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE code_projects (
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
    CREATE TABLE project_aliases (
        alias_path TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        last_seen_at INTEGER NOT NULL,
        FOREIGN KEY(project_id) REFERENCES code_projects(project_id) ON DELETE CASCADE
    );
    CREATE TABLE store_instances (
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
    CREATE TABLE graph_scopes (
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
    CREATE TABLE store_artifacts (
        store_id TEXT NOT NULL,
        artifact_kind TEXT NOT NULL,
        relpath TEXT NOT NULL,
        size_bytes INTEGER,
        schema_version TEXT,
        updated_at INTEGER,
        PRIMARY KEY (store_id, artifact_kind, relpath),
        FOREIGN KEY(store_id) REFERENCES store_instances(store_id) ON DELETE CASCADE
    );
    CREATE INDEX idx_project_aliases_project_id
        ON project_aliases(project_id);
    CREATE INDEX idx_store_instances_project_id
        ON store_instances(project_id);
    CREATE INDEX idx_graph_scopes_project_store
        ON graph_scopes(project_id, store_id);
";

const TRANSCRIPT_SCHEMA: &str = "
    CREATE TABLE turns (
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
    CREATE INDEX idx_turns_timestamp ON turns(timestamp);
    CREATE INDEX idx_turns_project ON turns(project_hash);
    CREATE INDEX idx_turns_model ON turns(model);
    CREATE TABLE parse_offsets (
        file_path TEXT PRIMARY KEY,
        byte_offset INTEGER NOT NULL,
        mtime INTEGER NOT NULL,
        file_id INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE savings_ledger (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        ts INTEGER NOT NULL,
        project_path TEXT NOT NULL,
        tool_name TEXT NOT NULL,
        before_tokens INTEGER NOT NULL,
        after_tokens INTEGER NOT NULL
    );
    CREATE INDEX idx_savings_ledger_ts ON savings_ledger(ts);
    CREATE INDEX idx_savings_ledger_project ON savings_ledger(project_path);
    CREATE TABLE analytics_events (
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
    CREATE INDEX idx_analytics_events_provider_project_session
        ON analytics_events(provider, project_id, session_id, timestamp);
    CREATE INDEX idx_analytics_events_kind
        ON analytics_events(event_kind, timestamp);
    CREATE INDEX idx_analytics_events_project_time
        ON analytics_events(project_id, timestamp);
    CREATE INDEX idx_analytics_events_timestamp
        ON analytics_events(timestamp);
    CREATE UNIQUE INDEX idx_observability_event_idempotency
        ON analytics_events(provider, project_id, hint_id)
        WHERE provider = 'tracedecay-observability' AND hint_id IS NOT NULL;
    CREATE TABLE sessions (
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
    CREATE INDEX idx_sessions_project ON sessions(provider, project_key);
    CREATE INDEX idx_sessions_started_at ON sessions(started_at);
    CREATE TABLE session_messages (
        provider TEXT NOT NULL,
        message_id TEXT NOT NULL,
        session_id TEXT NOT NULL,
        role TEXT NOT NULL,
        timestamp INTEGER,
        ordinal INTEGER NOT NULL,
        text TEXT NOT NULL,
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
    CREATE INDEX idx_session_messages_session
        ON session_messages(provider, session_id, ordinal);
    CREATE INDEX idx_session_messages_timestamp
        ON session_messages(timestamp);
    CREATE INDEX idx_session_messages_source
        ON session_messages(source_path);
    CREATE VIRTUAL TABLE session_messages_fts USING fts5(
        text, role, kind, model, tool_names,
        content='session_messages', content_rowid='rowid'
    );
    CREATE TRIGGER session_messages_fts_insert
        AFTER INSERT ON session_messages BEGIN
            INSERT INTO session_messages_fts(rowid, text, role, kind, model, tool_names)
            VALUES (NEW.rowid, NEW.text, NEW.role, NEW.kind, NEW.model, NEW.tool_names);
        END;
    CREATE TRIGGER session_messages_fts_delete
        AFTER DELETE ON session_messages BEGIN
            INSERT INTO session_messages_fts(
                session_messages_fts, rowid, text, role, kind, model, tool_names
            )
            VALUES (
                'delete', OLD.rowid, OLD.text, OLD.role, OLD.kind, OLD.model, OLD.tool_names
            );
        END;
    CREATE TRIGGER session_messages_fts_update
        AFTER UPDATE ON session_messages BEGIN
            INSERT INTO session_messages_fts(
                session_messages_fts, rowid, text, role, kind, model, tool_names
            )
            VALUES (
                'delete', OLD.rowid, OLD.text, OLD.role, OLD.kind, OLD.model, OLD.tool_names
            );
            INSERT INTO session_messages_fts(rowid, text, role, kind, model, tool_names)
            VALUES (NEW.rowid, NEW.text, NEW.role, NEW.kind, NEW.model, NEW.tool_names);
        END;
";

/// Installs the global/session schema at its final shape through the exact
/// registered runtime connection, or verifies that an existing store already
/// carries it. No database path is resolved or reopened, and no store is
/// stepped forward from an older shape.
pub async fn ensure_registered_schema(
    conn: &Connection,
) -> tracedecay_runtime_core::errors::Result<()> {
    ensure_registered_schema_for_admission(conn).await
}

/// Creates the registered schema once at its final shape. Reopening performs
/// bounded schema metadata validation only; any unstamped or incompatible
/// store requires an explicit reset.
pub async fn ensure_registered_schema_for_admission(
    conn: &Connection,
) -> tracedecay_runtime_core::errors::Result<()> {
    const OPERATION: &str = "initialize registered global database schema";
    let is_fresh = database_is_empty(conn).await?;
    if !is_fresh {
        let version = registered_schema_version(conn).await?;
        if version != Some(REGISTERED_SCHEMA_VERSION) {
            return Err(
                tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                    "registered-global",
                    format!(
                        "store is not the registered global schema v{REGISTERED_SCHEMA_VERSION} \
                     created by this binary; remove the store and let TraceDecay create it again"
                    ),
                ),
            );
        }
        validate_authority_schema_contract(conn).await.map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                "registered-global",
                format!(
                    "store does not match registered global schema v{REGISTERED_SCHEMA_VERSION}: \
                     {error}; remove the store and let TraceDecay create it again"
                ),
            )
        })?;
        return configuration::validate_configuration_schema(conn)
            .await
            .map_err(map_configuration_schema_validation_error);
    }

    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;

    let installation = async {
        transaction
            .execute_batch(REGISTRY_SCHEMA)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize global project registry", error)
            })?;
        validate_registry_schema_contract(&transaction).await?;

        configuration::ensure_configuration_schema(&transaction)
            .await
            .map_err(map_configuration_schema_installation_error)?;
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
        transaction
            .execute_batch(REGISTERED_SCHEMA_STATE)
            .await
            .map_err(|error| {
                global_db_operation_error("record registered global schema version", error)
            })?;
        tracedecay_runtime_core::errors::Result::Ok(())
    }
    .await;

    match installation {
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

fn map_configuration_schema_validation_error(
    error: configuration::ConfigurationSchemaError,
) -> tracedecay_runtime_core::errors::TraceDecayError {
    match error {
        configuration::ConfigurationSchemaError::ResetRequired { message } => {
            tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                "configuration",
                message,
            )
        }
        configuration::ConfigurationSchemaError::Storage(error) => {
            tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                "configuration",
                format!("configuration final schema could not be validated: {error}"),
            )
        }
    }
}

fn map_configuration_schema_installation_error(
    error: configuration::ConfigurationSchemaError,
) -> tracedecay_runtime_core::errors::TraceDecayError {
    match error {
        configuration::ConfigurationSchemaError::ResetRequired { message } => {
            tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                "configuration",
                message,
            )
        }
        configuration::ConfigurationSchemaError::Storage(error) => {
            global_db_operation_error("initialize configuration schema", error)
        }
    }
}

async fn table_exists(
    conn: &impl QueryExecutor,
    table: &str,
) -> tracedecay_runtime_core::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            params![table],
        )
        .await
        .map_err(|error| global_db_operation_error("inspect registered global schema", error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error("inspect registered global schema", error))
}

async fn database_is_empty(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%'
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error("inspect registered global schema", error))?;
    rows.next()
        .await
        .map(|row| row.is_none())
        .map_err(|error| global_db_operation_error("inspect registered global schema", error))
}

async fn registered_schema_version(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<Option<i64>> {
    if !table_exists(conn, "tracedecay_schema_state").await? {
        return Ok(None);
    }
    let mut rows = conn
        .query(
            "SELECT version FROM tracedecay_schema_state
             WHERE domain = 'registered-global'",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error("read registered schema version", error))?;
    rows.next()
        .await
        .map_err(|error| global_db_operation_error("read registered schema version", error))?
        .map(|row| {
            row.get::<i64>(0)
                .map_err(|error| global_db_operation_error("read registered schema version", error))
        })
        .transpose()
}

pub async fn validate_observation_authority_connection(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_authority_schema_contract(conn).await?;
    validate_authority_rows_exhaustive(conn).await
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::ensure_registered_schema;
    use tracedecay_runtime_core::db::engine::{QueryExecutor, TestConnection};

    #[tokio::test]
    async fn registered_store_reopen_rejects_missing_configuration_schema_without_healing() {
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
                .execute_batch("DROP TABLE configuration_schema_metadata;")
                .expect("remove configuration schema identity");
        }

        let connection = TestConnection::open(&database_path);
        let error = ensure_registered_schema(&connection)
            .await
            .expect_err("reopen must reject an incompatible configuration schema");
        assert_eq!(
            error
                .reset_required_context()
                .map(|(authority, _)| authority),
            Some("configuration")
        );
        let mut rows = connection
            .query(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name = 'configuration_schema_metadata'",
                (),
            )
            .await
            .unwrap();
        let marker_exists = rows.next().await.unwrap().is_some();
        assert!(
            !marker_exists,
            "read-only validation must not heal the marker"
        );
    }
}
