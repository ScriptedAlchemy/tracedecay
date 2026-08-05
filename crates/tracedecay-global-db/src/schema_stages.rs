use super::schema_contract::{
    ensure_authority_audit_checkpoint_schema, ensure_authority_invariant_schema,
    validate_authority_rows_exhaustive, validate_authority_schema_contract,
    validate_registry_schema_contract,
};
use super::{
    configuration, git_index_transactions, global_db_operation_error, global_db_operation_message,
    observation, observation_projection, session_temporal,
};
use tracedecay_runtime_core::db::engine::{Connection, QueryExecutor, TransactionBehavior, params};
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

const CONFIGURATION_OBJECTS: &[(&str, &str)] = &[
    ("table", "configuration_revisions"),
    ("table", "configuration_entries"),
    ("table", "configuration_topology_policies"),
    ("table", "configuration_topology_roots"),
    ("table", "configuration_topology_protected_refs"),
    ("table", "configuration_source_bindings"),
    ("table", "configuration_access_rules"),
    ("table", "configuration_change_plans"),
    ("table", "configuration_change_plan_operations"),
    ("table", "configuration_change_plan_events"),
    ("table", "configuration_mutation_receipts"),
    ("table", "configuration_audit_events"),
    ("table", "configuration_audit_redaction_keys"),
    ("table", "configuration_credential_references"),
    ("table", "configuration_component_activation_events"),
    ("index", "idx_configuration_revision_parent"),
    ("index", "idx_configuration_entry_key"),
    ("index", "idx_configuration_topology_root_id"),
    ("index", "idx_configuration_topology_root_locator"),
    ("index", "idx_configuration_topology_protected_ref"),
    ("index", "idx_configuration_audit_occurred_at"),
    ("index", "idx_configuration_component_activation_latest"),
    ("trigger", "configuration_revisions_immutable_update"),
    ("trigger", "configuration_revisions_immutable_delete"),
    ("trigger", "configuration_entries_immutable_update"),
    ("trigger", "configuration_entries_immutable_delete"),
    ("trigger", "configuration_topology_policy_immutable_update"),
    ("trigger", "configuration_topology_policy_immutable_delete"),
    ("trigger", "configuration_topology_roots_immutable_update"),
    ("trigger", "configuration_topology_roots_immutable_delete"),
    (
        "trigger",
        "configuration_topology_protected_refs_immutable_update",
    ),
    (
        "trigger",
        "configuration_topology_protected_refs_immutable_delete",
    ),
    ("trigger", "configuration_source_bindings_immutable_update"),
    ("trigger", "configuration_source_bindings_immutable_delete"),
    ("trigger", "configuration_access_rules_immutable_update"),
    ("trigger", "configuration_access_rules_immutable_delete"),
    ("trigger", "configuration_change_plans_immutable_update"),
    ("trigger", "configuration_change_plans_immutable_delete"),
    (
        "trigger",
        "configuration_change_plan_operations_immutable_update",
    ),
    (
        "trigger",
        "configuration_change_plan_operations_immutable_delete",
    ),
    (
        "trigger",
        "configuration_change_plan_events_immutable_update",
    ),
    (
        "trigger",
        "configuration_change_plan_events_immutable_delete",
    ),
    (
        "trigger",
        "configuration_mutation_receipts_immutable_update",
    ),
    (
        "trigger",
        "configuration_mutation_receipts_immutable_delete",
    ),
    ("trigger", "configuration_audit_events_immutable_update"),
    ("trigger", "configuration_audit_events_immutable_delete"),
    (
        "trigger",
        "configuration_audit_redaction_keys_immutable_update",
    ),
    (
        "trigger",
        "configuration_audit_redaction_keys_immutable_delete",
    ),
    (
        "trigger",
        "configuration_credential_references_immutable_update",
    ),
    (
        "trigger",
        "configuration_credential_references_immutable_delete",
    ),
    (
        "trigger",
        "configuration_component_activation_events_immutable_update",
    ),
    (
        "trigger",
        "configuration_component_activation_events_immutable_delete",
    ),
];

const CONFIGURATION_COLUMNS: &[(&str, &[&str])] = &[
    (
        "configuration_revisions",
        &[
            "revision_id",
            "parent_revision_id",
            "snapshot_id",
            "effective_behavior_digest",
            "resolution_provenance_digest",
            "actor_id",
            "operation_kind",
            "created_at",
        ],
    ),
    (
        "configuration_source_bindings",
        &[
            "revision_id",
            "binding_id",
            "source_kind",
            "locator_digest",
            "authority_kind",
            "project_id",
            "user_profile_id",
            "provenance_digest",
        ],
    ),
    (
        "configuration_audit_events",
        &[
            "event_id",
            "actor_id",
            "idempotency_key",
            "operation_kind",
            "base_revision_id",
            "result_revision_id",
            "sealed_target_reference",
            "event_scoped_target_commitment",
            "receipt_digest",
            "correlation_id",
            "safe_reason_code",
            "occurred_at",
        ],
    ),
];

const TRANSCRIPT_OBJECTS: &[(&str, &str)] = &[
    ("table", "turns"),
    ("table", "parse_offsets"),
    ("table", "savings_ledger"),
    ("table", "analytics_events"),
    ("table", "sessions"),
    ("table", "session_messages"),
    ("table", "session_messages_fts"),
    ("index", "idx_turns_timestamp"),
    ("index", "idx_turns_project"),
    ("index", "idx_turns_model"),
    ("index", "idx_savings_ledger_ts"),
    ("index", "idx_savings_ledger_project"),
    ("index", "idx_analytics_events_provider_project_session"),
    ("index", "idx_analytics_events_kind"),
    ("index", "idx_analytics_events_project_time"),
    ("index", "idx_analytics_events_timestamp"),
    ("index", "idx_observability_event_idempotency"),
    ("index", "idx_sessions_project"),
    ("index", "idx_sessions_started_at"),
    ("index", "idx_session_messages_session"),
    ("index", "idx_session_messages_timestamp"),
    ("index", "idx_session_messages_source"),
    ("trigger", "session_messages_fts_insert"),
    ("trigger", "session_messages_fts_delete"),
    ("trigger", "session_messages_fts_update"),
];

const TRANSCRIPT_COLUMNS: &[(&str, &[&str])] = &[
    (
        "sessions",
        &[
            "provider",
            "session_id",
            "project_key",
            "project_path",
            "title",
            "started_at",
            "ended_at",
            "transcript_path",
            "metadata_json",
            "parent_session_id",
            "is_subagent",
            "agent_id",
            "parent_tool_use_id",
        ],
    ),
    (
        "session_messages",
        &[
            "provider",
            "message_id",
            "session_id",
            "role",
            "timestamp",
            "ordinal",
            "text",
            "kind",
            "model",
            "tool_names",
            "source_path",
            "source_offset",
            "metadata_json",
        ],
    ),
];

const LCM_OBJECTS: &[(&str, &str)] = &[
    ("table", "session_schema_migrations"),
    ("table", "lcm_raw_messages"),
    ("table", "lcm_external_payloads"),
    ("table", "lcm_gc_marks"),
    ("table", "lcm_gc_meta"),
    ("table", "lcm_summary_nodes"),
    ("table", "lcm_summary_sources"),
    ("table", "lcm_lifecycle_state"),
    ("table", "lcm_maintenance_debt"),
    ("table", "lcm_raw_messages_fts"),
    ("table", "lcm_summary_nodes_fts"),
    ("index", "idx_lcm_raw_session_order"),
    ("index", "idx_lcm_raw_session_id"),
    ("index", "idx_lcm_external_payloads_owner"),
    ("index", "idx_lcm_summary_nodes_session_depth_time"),
    ("index", "idx_lcm_summary_nodes_codex_pending_session_order"),
    ("index", "idx_lcm_summary_nodes_codex_pending_root_order"),
    ("index", "idx_lcm_summary_sources_source"),
    ("index", "idx_lcm_maintenance_debt_kind"),
    ("trigger", "lcm_raw_messages_fts_insert"),
    ("trigger", "lcm_raw_messages_fts_delete"),
    ("trigger", "lcm_raw_messages_fts_update"),
    ("trigger", "lcm_summary_nodes_fts_insert"),
    ("trigger", "lcm_summary_nodes_fts_delete"),
    ("trigger", "lcm_summary_nodes_fts_update"),
];

const LCM_COLUMNS: &[(&str, &[&str])] = &[
    (
        "session_schema_migrations",
        &["name", "version", "applied_at"],
    ),
    (
        "lcm_raw_messages",
        &[
            "provider",
            "message_id",
            "session_id",
            "store_id",
            "role",
            "ordinal",
            "timestamp",
            "content",
            "content_hash",
            "storage_kind",
            "payload_ref",
            "snippet_text",
            "index_text",
            "legacy_source",
            "legacy_truncated",
            "metadata_json",
        ],
    ),
    (
        "lcm_lifecycle_state",
        &[
            "provider",
            "conversation_id",
            "current_session_id",
            "last_finalized_session_id",
            "current_frontier_store_id",
            "last_finalized_frontier_store_id",
            "rollover_at",
            "reset_at",
            "maintenance_at",
            "boundary_skip_at",
            "updated_at",
        ],
    ),
];

const WORKFLOW_OBJECTS: &[(&str, &str)] = &[
    ("table", "workflow_runs"),
    ("table", "workflow_agents"),
    ("table", "workflow_index_meta"),
    ("index", "idx_workflow_runs_parent"),
    ("index", "idx_workflow_agents_run"),
];

const WORKFLOW_COLUMNS: &[(&str, &[&str])] = &[
    (
        "workflow_runs",
        &[
            "run_id",
            "parent_session_id",
            "name",
            "description",
            "phase_json",
            "status",
            "started_ts",
            "ended_ts",
            "result_summary",
            "agent_count",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "workflow_agents",
        &[
            "run_id",
            "agent_label",
            "agent_id",
            "phase",
            "transcript_path",
            "agent_session_id",
            "status",
            "model",
            "tokens",
            "started_ts",
            "ended_ts",
            "created_at",
            "updated_at",
        ],
    ),
    ("workflow_index_meta", &["key", "value", "updated_at"]),
];

/// Installs the global/session schema at its final shape through the exact
/// registered runtime connection, or verifies that an existing store already
/// carries it. No database path is resolved or reopened, and no store is
/// stepped forward from an older shape.
pub async fn ensure_registered_schema(
    conn: &Connection,
) -> tracedecay_runtime_core::errors::Result<()> {
    let convergence = ensure_registered_schema_for_admission(conn).await?;
    converge_registered_schema(conn, convergence).await
}

#[derive(Clone, Copy, Debug)]
pub struct RegisteredSchemaConvergence {
    _validated: (),
}

/// Installs the minimum schema and write guards required before a registered
/// runtime may be published. Historical convergence remains separately
/// resumable so daemon admission never waits for whole-store scans.
pub async fn ensure_registered_schema_for_admission(
    conn: &Connection,
) -> tracedecay_runtime_core::errors::Result<RegisteredSchemaConvergence> {
    const OPERATION: &str = "initialize registered global database schema";
    if registered_object_exists(conn).await? {
        validate_registered_schema_connection(conn)
            .await
            .map_err(|error| reset_required(error.to_string()))?;
        return Ok(RegisteredSchemaConvergence { _validated: () });
    }
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;

    let creation = async {
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

    match creation {
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
    validate_registered_schema_connection(conn).await?;
    Ok(RegisteredSchemaConvergence { _validated: () })
}

/// Stores are created at the final schema by
/// [`ensure_registered_schema_for_admission`], so there is nothing here to step
/// an older shape forward: the historical projection-anchor binding, retrieval
/// anchor, repository provenance, projector version, and session project-path
/// conversion passes were one-time upgrades and have been removed.
pub async fn converge_registered_schema(
    _conn: &Connection,
    _convergence: RegisteredSchemaConvergence,
) -> tracedecay_runtime_core::errors::Result<()> {
    Ok(())
}

fn reset_required(actual: String) -> tracedecay_runtime_core::errors::TraceDecayError {
    tracedecay_runtime_core::errors::TraceDecayError::ResetRequired {
        store: "registered global/session database".to_owned(),
        expected: "exact-final registered schema and authority rows".to_owned(),
        actual,
    }
}

async fn registered_object_exists(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1
             FROM sqlite_master
             WHERE type IN ('table', 'view', 'index', 'trigger')
               AND name NOT LIKE 'sqlite_%'
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error("inspect registered global schema", error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error("inspect registered global schema", error))
}

pub async fn validate_registered_schema_connection(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_registry_schema_contract(conn).await?;
    validate_schema_domain(
        conn,
        "configuration",
        CONFIGURATION_OBJECTS,
        CONFIGURATION_COLUMNS,
    )
    .await?;
    validate_schema_domain(conn, "transcript", TRANSCRIPT_OBJECTS, TRANSCRIPT_COLUMNS).await?;
    validate_schema_marker(
        conn,
        "lcm",
        tracedecay_sessions::runtime::lcm::schema::LCM_SCHEMA_VERSION,
    )
    .await?;
    validate_schema_domain(conn, "LCM", LCM_OBJECTS, LCM_COLUMNS).await?;
    if tracedecay_sessions::runtime::lcm::schema::raw_fts_structure_is_current(conn).await
        != Some(true)
    {
        return Err(global_db_operation_message(
            "validate registered schema",
            "LCM raw-message FTS synchronization objects are not exact-final",
        ));
    }
    tracedecay_sessions::runtime::git_correlation::validate_git_correlation_schema(conn)
        .await
        .map_err(|error| global_db_operation_message("validate registered schema", error))?;
    validate_schema_marker(
        conn,
        "workflow_indexing",
        tracedecay_sessions::runtime::workflow_index::WORKFLOW_INDEX_SCHEMA_VERSION,
    )
    .await?;
    validate_schema_domain(conn, "workflow", WORKFLOW_OBJECTS, WORKFLOW_COLUMNS).await?;
    validate_authority_schema_contract(conn).await?;
    validate_authority_rows_exhaustive(conn).await
}

async fn validate_schema_marker(
    conn: &impl QueryExecutor,
    name: &str,
    expected: i64,
) -> tracedecay_runtime_core::errors::Result<()> {
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            params![name],
        )
        .await
        .map_err(|error| global_db_operation_error("validate registered schema marker", error))?;
    let actual = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error("validate registered schema marker", error))?
        .map(|row| row.get::<i64>(0))
        .transpose()
        .map_err(|error| global_db_operation_error("validate registered schema marker", error))?;
    if actual != Some(expected) {
        return Err(global_db_operation_message(
            "validate registered schema marker",
            format!("schema marker '{name}' is {actual:?}, expected {expected}"),
        ));
    }
    Ok(())
}

async fn validate_schema_domain(
    conn: &impl QueryExecutor,
    domain: &str,
    objects: &[(&str, &str)],
    columns: &[(&str, &[&str])],
) -> tracedecay_runtime_core::errors::Result<()> {
    for &(kind, name) in objects {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
                params![kind, name],
            )
            .await
            .map_err(|error| global_db_operation_error("validate registered schema", error))?;
        let present = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error("validate registered schema", error))?
            .is_some_and(|row| row.get::<i64>(0).ok() == Some(1));
        if !present {
            return Err(global_db_operation_message(
                "validate registered schema",
                format!("{domain} schema is missing required {kind} '{name}'"),
            ));
        }
    }
    for &(table, expected) in columns {
        let mut rows = conn
            .query(&format!("PRAGMA table_xinfo({table})"), ())
            .await
            .map_err(|error| global_db_operation_error("validate registered schema", error))?;
        let mut actual = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error("validate registered schema", error))?
        {
            let hidden = row
                .get::<i64>(6)
                .map_err(|error| global_db_operation_error("validate registered schema", error))?;
            if hidden == 0 {
                actual.push(row.get::<String>(1).map_err(|error| {
                    global_db_operation_error("validate registered schema", error)
                })?);
            }
        }
        if actual != expected {
            return Err(global_db_operation_message(
                "validate registered schema",
                format!("{domain} table '{table}' has columns {actual:?}, expected {expected:?}"),
            ));
        }
    }
    Ok(())
}

pub async fn validate_observation_authority_connection(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_authority_schema_contract(conn).await?;
    validate_authority_rows_exhaustive(conn).await
}

#[cfg(test)]
mod tests {
    use super::ensure_registered_schema_for_admission;
    use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, TestConnection};
    use tracedecay_runtime_core::errors::TraceDecayError;

    async fn schema_inventory(connection: &TestConnection) -> Vec<(String, String, String)> {
        let mut rows = connection
            .query(
                "SELECT type, name, COALESCE(sql, '')
                 FROM sqlite_master
                 WHERE name NOT LIKE 'sqlite_%'
                 ORDER BY type, name",
                (),
            )
            .await
            .unwrap();
        let mut inventory = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            inventory.push((
                row.get(0).unwrap(),
                row.get(1).unwrap(),
                row.get(2).unwrap(),
            ));
        }
        inventory
    }

    async fn assert_nonfinal_store_is_rejected_without_mutation(schema: &str) {
        let directory = tempfile::tempdir().unwrap();
        let connection = TestConnection::open(&directory.path().join("registered.db"));
        connection.execute_batch(schema).await.unwrap();
        let before = schema_inventory(&connection).await;

        let error = ensure_registered_schema_for_admission(&connection)
            .await
            .expect_err("a nonempty non-final store must require reset");

        assert!(matches!(error, TraceDecayError::ResetRequired { .. }));
        assert_eq!(schema_inventory(&connection).await, before);
    }

    #[tokio::test]
    async fn arbitrary_user_object_is_not_treated_as_a_fresh_store() {
        assert_nonfinal_store_is_rejected_without_mutation(
            "CREATE TABLE user_object(id INTEGER PRIMARY KEY, payload TEXT NOT NULL);
             INSERT INTO user_object(payload) VALUES ('preserve-me');",
        )
        .await;
    }

    #[tokio::test]
    async fn legacy_projects_only_store_requires_reset_without_overlay() {
        assert_nonfinal_store_is_rejected_without_mutation(
            "CREATE TABLE projects (
                path TEXT PRIMARY KEY,
                tokens_saved INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO projects(path, tokens_saved) VALUES ('/legacy', 41);",
        )
        .await;
    }
}
