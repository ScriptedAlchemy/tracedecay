use super::schema_contract::{
    authority_invariant_triggers_intact, ensure_authority_invariant_schema,
    ensure_authority_invariants, restore_immutability_after_canonical_repair,
    suspend_immutability_for_canonical_repair, suspend_session_invariants_for_schema_upgrade,
    validate_authority_rows_exhaustive, validate_authority_schema_contract,
    validate_registry_schema_contract,
};
use super::{
    configuration, ensure_code_project_native_root_columns, ensure_parse_offset_columns,
    ensure_session_parent_columns, git_index_transactions, global_db_operation_error, observation,
    observation_projection, project_registry, session_temporal,
};
use crate::db::engine::{Connection, Executor, QueryExecutor, TransactionBehavior, params};

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
    CREATE INDEX IF NOT EXISTS idx_session_messages_session
        ON session_messages(provider, session_id, ordinal);
    CREATE INDEX IF NOT EXISTS idx_session_messages_timestamp
        ON session_messages(timestamp);
    CREATE INDEX IF NOT EXISTS idx_session_messages_source
        ON session_messages(source_path);
    CREATE VIRTUAL TABLE IF NOT EXISTS session_messages_fts USING fts5(
        text, role, kind, model, tool_names,
        content='session_messages', content_rowid='rowid'
    );
    CREATE TRIGGER IF NOT EXISTS session_messages_fts_insert
        AFTER INSERT ON session_messages BEGIN
            INSERT INTO session_messages_fts(rowid, text, role, kind, model, tool_names)
            VALUES (NEW.rowid, NEW.text, NEW.role, NEW.kind, NEW.model, NEW.tool_names);
        END;
    CREATE TRIGGER IF NOT EXISTS session_messages_fts_delete
        AFTER DELETE ON session_messages BEGIN
            INSERT INTO session_messages_fts(
                session_messages_fts, rowid, text, role, kind, model, tool_names
            )
            VALUES (
                'delete', OLD.rowid, OLD.text, OLD.role, OLD.kind, OLD.model, OLD.tool_names
            );
        END;
    CREATE TRIGGER IF NOT EXISTS session_messages_fts_update
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

/// Installs and migrates the global/session schema through the exact
/// registered runtime connection. No database path is resolved or reopened.
pub(crate) async fn ensure_registered_schema(conn: &Connection) -> crate::errors::Result<()> {
    const OPERATION: &str = "initialize registered global database schema";

    let is_fresh = !table_exists(conn, "sessions").await?
        && !table_exists(conn, "observations").await?
        && !table_exists(conn, "code_projects").await?;
    let force_exhaustive = !authority_invariant_triggers_intact(conn).await?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;

    transaction
        .execute_batch(REGISTRY_SCHEMA)
        .await
        .map_err(|error| global_db_operation_error("initialize global project registry", error))?;
    ensure_code_project_native_root_columns(&transaction)
        .await
        .map_err(|error| global_db_operation_error("ensure native project roots", error))?;
    project_registry::migrate_project_rows_to_canonical_keys(&transaction)
        .await
        .map_err(|error| global_db_operation_error("migrate canonical project keys", error))?;
    validate_registry_schema_contract(&transaction).await?;

    configuration::ensure_configuration_schema(&transaction)
        .await
        .map_err(|error| global_db_operation_error("initialize configuration schema", error))?;
    git_index_transactions::ensure_git_index_transaction_schema(&transaction).await?;

    transaction
        .execute_batch(TRANSCRIPT_SCHEMA)
        .await
        .map_err(|error| global_db_operation_error("initialize transcript schema", error))?;
    ensure_session_parent_columns(&transaction)
        .await
        .map_err(|error| global_db_operation_error("ensure session parent columns", error))?;
    ensure_parse_offset_columns(&transaction)
        .await
        .map_err(|error| global_db_operation_error("ensure parse offset columns", error))?;

    suspend_session_invariants_for_schema_upgrade(&transaction).await?;
    session_temporal::ensure_session_temporal_schema(&transaction).await?;
    observation::ensure_observation_schema(&transaction).await?;
    observation_projection::ensure_observation_projection_schema(&transaction)
        .await
        .map_err(|error| global_db_operation_error("initialize observation projection", error))?;
    crate::db::install_external_source_schema(
        &transaction,
        "initialize registered external source state",
    )
    .await?;
    ensure_authority_invariant_schema(&transaction).await?;
    validate_authority_schema_contract(&transaction).await?;

    crate::sessions::lcm::schema::ensure_lcm_schema_in_transaction(&transaction)
        .await
        .map_err(|error| global_db_operation_error("initialize LCM schema", error))?;
    crate::sessions::git_correlation::ensure_git_correlation_schema_in_transaction(&transaction)
        .await
        .map_err(|error| global_db_operation_error("initialize git correlation schema", error))?;
    crate::sessions::workflow_index::ensure_workflow_index_schema(&transaction)
        .await
        .map_err(|error| global_db_operation_error("initialize workflow index schema", error))?;
    if let Err(error) =
        crate::sessions::transcript_backfill::backfill_transcript_facts(&transaction).await
    {
        if !error.atomicity_preserved() {
            return Err(global_db_operation_error(
                "roll back transcript facts backfill",
                error,
            ));
        }
        tracing::warn!(error = %error, "transcript facts backfill deferred until a later open");
    }

    transaction
        .commit()
        .await
        .map_err(|error| global_db_operation_error("commit registered global schema", error))?;

    // Both of these page their own progress through individually committed
    // transactions. They must stay outside the schema-upgrade transaction
    // above so an interrupted open (the project warmup cancels in-flight
    // statements once its deadline passes) keeps the pages it already
    // committed instead of rolling a whole-table scan back and re-arming it.
    observation::converge_observation_retrieval_anchors(conn).await?;
    observation::converge_observation_repository_provenance(conn).await?;
    observation_projection::prepare_projection_version_migration_with_engine(conn)
        .await
        .map_err(|error| global_db_operation_error("prepare observation projection", error))?;

    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| {
            global_db_operation_error("begin observation authority validation", error)
        })?;
    ensure_authority_invariants(&transaction, force_exhaustive, is_fresh).await?;
    transaction.commit().await.map_err(|error| {
        global_db_operation_error("commit observation authority validation", error)
    })
}

async fn table_exists(conn: &impl QueryExecutor, table: &str) -> crate::errors::Result<bool> {
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

pub(crate) async fn validate_observation_authority_connection(
    conn: &impl QueryExecutor,
) -> crate::errors::Result<()> {
    validate_authority_schema_contract(conn).await?;
    validate_authority_rows_exhaustive(conn).await
}

pub(crate) async fn begin_observation_authority_canonical_repair(
    conn: &impl Executor,
) -> crate::errors::Result<()> {
    suspend_immutability_for_canonical_repair(conn).await
}

pub(crate) async fn finish_observation_authority_canonical_repair(
    conn: &impl Executor,
) -> crate::errors::Result<()> {
    restore_immutability_after_canonical_repair(conn).await
}
