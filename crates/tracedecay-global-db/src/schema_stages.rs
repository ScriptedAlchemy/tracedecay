use super::schema_contract::{
    authority_invariant_triggers_intact, ensure_authority_audit_checkpoint_schema,
    ensure_authority_invariant_schema, ensure_authority_invariants, require_foreign_key_audit,
    restore_immutability_after_canonical_repair, suspend_immutability_for_canonical_repair,
    suspend_session_invariants_for_schema_upgrade, validate_authority_rows_exhaustive,
    validate_authority_schema_contract, validate_registry_schema_contract,
    validate_remote_deletion_schema_contract,
};
use super::{
    configuration, ensure_code_project_native_root_columns, ensure_parse_offset_columns,
    ensure_session_parent_columns, git_index_transactions, global_db_operation_error, observation,
    observation_projection, project_registry, session_temporal,
};
use tracedecay_runtime_core::db::engine::{
    Connection, Executor, QueryExecutor, TransactionBehavior,
};
use tracedecay_rusqlite_runtime::repository::AUTHORIZED_SCOPE_SET_SCHEMA_V1;
use tracedecay_rusqlite_runtime::work::{
    WORK_PRODUCT_SCHEMA_V1 as WORK_PRODUCT_GRAPH_JOURNAL_SCHEMA_V1,
    WORK_SCHEMA_V1 as WORK_EVENT_JOURNAL_SCHEMA_V1,
};
use tracedecay_rusqlite_runtime::workflow::{
    WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1, WORKFLOW_SCHEMA_IDENTITY_V1, WORKFLOW_SCHEMA_VERSION_V1,
    WORKFLOW_TABLE_CONTRACTS_V1,
};

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

const REMOTE_DELETION_SCHEMA: &str = "
    CREATE TABLE remote_deletion_tombstones (
        profile_id TEXT NOT NULL,
        target_kind TEXT NOT NULL,
        project_id TEXT NOT NULL,
        tombstone_id TEXT NOT NULL,
        recorded_at_micros INTEGER NOT NULL,
        cleanup_status TEXT NOT NULL,
        failure_code TEXT,
        failure_phase TEXT,
        retryable INTEGER,
        PRIMARY KEY (profile_id, target_kind, project_id),
        CHECK (length(profile_id) BETWEEN 1 AND 256),
        CHECK (target_kind IN ('account', 'project')),
        CHECK (
            (target_kind = 'account' AND project_id = '')
            OR (target_kind = 'project' AND length(project_id) BETWEEN 1 AND 256)
        ),
        CHECK (length(tombstone_id) BETWEEN 1 AND 256),
        CHECK (recorded_at_micros > 0),
        CHECK (cleanup_status IN ('pending', 'settling', 'partial', 'deleted')),
        CHECK (
            (cleanup_status IN ('pending', 'deleted')
                AND failure_code IS NULL AND failure_phase IS NULL AND retryable IS NULL)
            OR (cleanup_status IN ('settling', 'partial')
                AND failure_code IS NOT NULL AND failure_phase IS NOT NULL
                AND retryable IN (0, 1))
        )
    );
";

const TRANSCRIPT_SCHEMA: &str = "
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
    CREATE INDEX IF NOT EXISTS idx_session_messages_session_activity
        ON session_messages(
            provider, session_id, timestamp, ordinal, message_id,
            kind, tool_names, metadata_json
        );
    CREATE INDEX IF NOT EXISTS idx_session_messages_timestamp
        ON session_messages(timestamp);
    CREATE INDEX IF NOT EXISTS idx_session_messages_source
        ON session_messages(source_path);
    CREATE TABLE IF NOT EXISTS session_backfill_meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL,
        updated_at INTEGER NOT NULL DEFAULT (unixepoch())
    );
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

#[derive(Clone, Copy)]
pub struct RegisteredSchemaConvergence {
    force_exhaustive: bool,
    is_fresh: bool,
}

/// Installs the minimum schema and write guards required before a registered
/// runtime may be published. Historical convergence remains separately
/// resumable so daemon admission never waits for whole-store scans.
pub async fn ensure_registered_schema_for_admission(
    conn: &Connection,
) -> tracedecay_runtime_core::errors::Result<RegisteredSchemaConvergence> {
    const OPERATION: &str = "initialize registered global database schema";
    // The LCM authority classifies profile content first: a legacy or
    // version-skewed session store must surface its own ProfileResetRequired
    // state instead of being masked by the coarser workflow/configuration
    // schema resets, which would also flag a store those features were simply
    // never installed in.
    tracedecay_sessions::runtime::lcm::schema::require_admissible_lcm_schema(conn)
        .await
        .map_err(|error| match error {
            tracedecay_sessions::runtime::lcm::LcmError::ProfileResetRequired {
                found_version,
                required_version,
            } => tracedecay_runtime_core::errors::TraceDecayError::ProfileResetRequired {
                component: "LCM",
                found_version,
                required_version,
            },
            error => global_db_operation_error("classify LCM schema admission", error),
        })?;
    let workflow_admission = inspect_workflow_schema_for_admission(conn).await?;
    let configuration_fresh = configuration::fresh_configuration_store_evidence(conn)
        .await
        .map_err(|error| match error {
            configuration::ConfigurationSchemaError::ResetRequired { reason } => {
                tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                    "configuration",
                    reason,
                )
            }
            configuration::ConfigurationSchemaError::Storage(error) => {
                global_db_operation_error("inspect configuration schema freshness", error)
            }
        })?;
    configuration::admit_configuration_schema(conn, configuration_fresh.as_ref())
        .await
        .map_err(|error| match error {
            configuration::ConfigurationSchemaError::ResetRequired { reason } => {
                tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                    "configuration",
                    reason,
                )
            }
            configuration::ConfigurationSchemaError::Storage(error) => {
                global_db_operation_error("admit configuration schema", error)
            }
        })?;
    let is_fresh = configuration_fresh.is_some();
    // An existing catalog whose remote-deletion tombstone table drifted from the
    // contract cannot be trusted to gate replay or admission, so admission fails
    // closed with the tip's typed reset authority rather than silently
    // continuing on a shape that no longer proves deletion state.
    if !is_fresh && let Err(error) = validate_remote_deletion_schema_contract(conn).await {
        return Err(
            tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                "remote deletion tombstones",
                error.to_string(),
            ),
        );
    }
    let force_exhaustive = !authority_invariant_triggers_intact(conn).await?;
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;

    let migration = async {
        configuration::ensure_configuration_schema(&transaction, configuration_fresh.as_ref())
            .await
            .map_err(|error| match error {
                configuration::ConfigurationSchemaError::ResetRequired { reason } => {
                    tracedecay_runtime_core::errors::TraceDecayError::reset_required(
                        "configuration",
                        reason,
                    )
                }
                configuration::ConfigurationSchemaError::Storage(error) => {
                    global_db_operation_error("initialize configuration schema", error)
                }
            })?;
        ensure_authority_audit_checkpoint_schema(&transaction).await?;
        if force_exhaustive && !is_fresh {
            // Persist the requirement before later schema work repairs the
            // trigger evidence that armed it. The progress row doubles as the
            // resumable cursor and is removed only by a completed FK sweep.
            require_foreign_key_audit(&transaction).await?;
        }
        transaction
            .execute_batch(REGISTRY_SCHEMA)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize global project registry", error)
            })?;
        if is_fresh {
            transaction
                .execute_batch(REMOTE_DELETION_SCHEMA)
                .await
                .map_err(|error| {
                    global_db_operation_error("initialize remote deletion catalog", error)
                })?;
        }
        ensure_code_project_native_root_columns(&transaction)
            .await
            .map_err(|error| global_db_operation_error("ensure native project roots", error))?;
        project_registry::migrate_project_rows_to_canonical_keys(&transaction)
            .await
            .map_err(|error| global_db_operation_error("migrate canonical project keys", error))?;
        validate_registry_schema_contract(&transaction).await?;

        git_index_transactions::ensure_git_index_transaction_schema(&transaction).await?;

        transaction
            .execute_batch(TRANSCRIPT_SCHEMA)
            .await
            .map_err(|error| global_db_operation_error("initialize transcript schema", error))?;
        if workflow_admission == WorkflowSchemaAdmission::Create {
            for table in WORKFLOW_TABLE_CONTRACTS_V1 {
                transaction
                    .execute_batch(table.sql)
                    .await
                    .map_err(|error| {
                        global_db_operation_error("initialize workflow schema", error)
                    })?;
            }
            transaction
                .execute_batch(WORKFLOW_SCHEMA_IDENTITY_V1)
                .await
                .map_err(|error| global_db_operation_error("initialize workflow schema", error))?;
        }
        transaction
            .execute_batch(WORK_EVENT_JOURNAL_SCHEMA_V1)
            .await
            .map_err(|error| global_db_operation_error("initialize Work event journal", error))?;
        // The Work product graph authority is its own admission stage, not a
        // continuation of the task journal above: it is owner-scoped rather
        // than WorkAuthority-scoped, so a store that carries one and not the
        // other is a legible state, and its failure names itself.
        transaction
            .execute_batch(WORK_PRODUCT_GRAPH_JOURNAL_SCHEMA_V1)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize Work product graph journal", error)
            })?;
        transaction
            .execute_batch(AUTHORIZED_SCOPE_SET_SCHEMA_V1)
            .await
            .map_err(|error| {
                global_db_operation_error("initialize authorized scope-set schema", error)
            })?;
        ensure_session_parent_columns(&transaction)
            .await
            .map_err(|error| global_db_operation_error("ensure session parent columns", error))?;
        ensure_parse_offset_columns(&transaction)
            .await
            .map_err(|error| global_db_operation_error("ensure parse offset columns", error))?;

        ensure_authority_audit_checkpoint_schema(&transaction).await?;
        suspend_session_invariants_for_schema_upgrade(&transaction).await?;
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
            .map_err(|error| match error {
                tracedecay_sessions::runtime::lcm::LcmError::ProfileResetRequired {
                    found_version,
                    required_version,
                } => tracedecay_runtime_core::errors::TraceDecayError::ProfileResetRequired {
                    component: "LCM",
                    found_version,
                    required_version,
                },
                error => global_db_operation_error("initialize LCM schema", error),
            })?;
        tracedecay_sessions::runtime::git_correlation::ensure_git_correlation_receipt_schema_in_transaction(
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
    Ok(RegisteredSchemaConvergence {
        force_exhaustive,
        is_fresh,
    })
}

/// Completes resumable authority convergence after the registered runtime is
/// available. Every stage retains its existing durable checkpoint semantics.
///
/// Stores are created at the final schema by
/// [`ensure_registered_schema_for_admission`], so there is nothing here to step
/// an older shape forward: the historical projection-anchor binding, retrieval
/// anchor, repository provenance, projector version migration, and session
/// project-path passes were all one-time legacy upgrades and have been removed.
/// Only the authority invariant audit remains, and it stays out of line because
/// it pages real authority rows on a large store.
pub async fn converge_registered_schema(
    conn: &Connection,
    convergence: RegisteredSchemaConvergence,
) -> tracedecay_runtime_core::errors::Result<()> {
    // The invariant pass pages historical authority rows and can legitimately
    // outlive an ordinary open on a large store. The admission phase has
    // already installed and validated its guard triggers, so daemon reads and
    // guarded writes may proceed while these idempotent repairs advance.
    // Completed repairs survive interruption, while the trusted checkpoint is
    // still written only after every audit succeeds.
    ensure_authority_invariants(conn, convergence.force_exhaustive, convergence.is_fresh).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkflowSchemaAdmission {
    Create,
    Complete,
}

async fn inspect_workflow_schema_for_admission(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<WorkflowSchemaAdmission> {
    let mut rows = conn
        .query(
            "SELECT type, name, sql FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error("inspect workflow schema tables", error))?;
    let mut tables = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error("read workflow schema tables", error))?
    {
        tables.push((
            row.get::<String>(0).map_err(|error| {
                global_db_operation_error("decode workflow schema object type", error)
            })?,
            row.get::<String>(1).map_err(|error| {
                global_db_operation_error("decode workflow schema object name", error)
            })?,
            row.get::<Option<String>>(2).map_err(|error| {
                global_db_operation_error("decode workflow schema object SQL", error)
            })?,
        ));
    }
    if tables.is_empty() {
        return Ok(WorkflowSchemaAdmission::Create);
    }

    let actual_workflow_tables = tables
        .iter()
        .filter(|(object_type, name, _)| {
            object_type == "table"
                && WORKFLOW_TABLE_CONTRACTS_V1
                    .iter()
                    .any(|contract| contract.name == name.as_str())
        })
        .map(|(_, name, sql)| (name.as_str(), sql.as_deref()))
        .collect::<Vec<_>>();
    let expected_workflow_tables = WORKFLOW_TABLE_CONTRACTS_V1
        .iter()
        .map(|contract| (contract.name, Some(contract.sql)))
        .collect::<Vec<_>>();
    if actual_workflow_tables != expected_workflow_tables {
        return Err(workflow_schema_reset_required(
            "workflow tables are absent, incomplete, or not exact",
        ));
    }

    let mut schema = conn
        .query(
            "SELECT singleton, schema_version, definition_digest FROM workflow_schema
             ORDER BY singleton",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error("inspect workflow schema identity", error))?;
    let Some(identity) = schema
        .next()
        .await
        .map_err(|error| global_db_operation_error("read workflow schema identity", error))?
    else {
        return Err(workflow_schema_reset_required(
            "workflow schema identity is missing",
        ));
    };
    let singleton = identity
        .get::<i64>(0)
        .map_err(|error| global_db_operation_error("decode workflow schema singleton", error))?;
    let schema_version = identity
        .get::<i64>(1)
        .map_err(|error| global_db_operation_error("decode workflow schema version", error))?;
    let definition_digest = identity
        .get::<String>(2)
        .map_err(|error| global_db_operation_error("decode workflow schema digest", error))?;
    let extra_identity = schema
        .next()
        .await
        .map_err(|error| global_db_operation_error("read workflow schema identity", error))?
        .is_some();
    if singleton != 1
        || schema_version != WORKFLOW_SCHEMA_VERSION_V1
        || definition_digest != WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1
        || extra_identity
    {
        return Err(workflow_schema_reset_required(
            "workflow schema identity does not match the final contract",
        ));
    }

    for table in WORKFLOW_TABLE_CONTRACTS_V1 {
        let mut columns = conn
            .query(&format!("PRAGMA table_info({})", table.name), ())
            .await
            .map_err(|error| global_db_operation_error("inspect workflow table columns", error))?;
        let mut actual_columns = Vec::new();
        while let Some(row) = columns
            .next()
            .await
            .map_err(|error| global_db_operation_error("read workflow table columns", error))?
        {
            actual_columns.push((
                row.get::<String>(1).map_err(|error| {
                    global_db_operation_error("decode workflow column name", error)
                })?,
                row.get::<String>(2).map_err(|error| {
                    global_db_operation_error("decode workflow column type", error)
                })?,
                row.get::<i64>(3).map_err(|error| {
                    global_db_operation_error("decode workflow column nullability", error)
                })?,
                row.get::<i64>(5).map_err(|error| {
                    global_db_operation_error("decode workflow column key", error)
                })?,
            ));
        }
        let exact = actual_columns.len() == table.columns.len()
            && actual_columns
                .iter()
                .zip(table.columns)
                .all(|(actual, expected)| {
                    actual.0 == expected.name
                        && actual.1 == expected.sql_type
                        && actual.2 == expected.not_null
                        && actual.3 == expected.primary_key
                });
        if !exact {
            return Err(workflow_schema_reset_required(
                "workflow table columns do not match the final contract",
            ));
        }
    }

    Ok(WorkflowSchemaAdmission::Complete)
}

fn workflow_schema_reset_required(
    reason: &str,
) -> tracedecay_runtime_core::errors::TraceDecayError {
    tracedecay_runtime_core::errors::TraceDecayError::reset_required("workflow", reason)
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
    use tracedecay_runtime_core::db::engine::{QueryExecutor, TestConnection};

    #[tokio::test]
    async fn existing_registry_without_remote_deletion_catalog_requires_typed_reset() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        {
            let connection = TestConnection::open(&database_path);
            ensure_registered_schema(&connection)
                .await
                .expect("initialize final V2 authority schema");
        }
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch("DROP TABLE remote_deletion_tombstones")
                .expect("remove required final V2 catalog");
        }

        let connection = TestConnection::open(&database_path);
        let error = ensure_registered_schema(&connection)
            .await
            .expect_err("an existing catalog must not migrate in remote deletion state");
        let Some((authority, reason)) = error.reset_required_context() else {
            panic!("missing final V2 catalog returned the wrong typed problem: {error}");
        };
        assert_eq!(authority, "remote deletion tombstones");
        assert!(
            reason.contains("remote_deletion_tombstones"),
            "reset problem must identify the missing final catalog: {reason}"
        );
        let mut rows = connection
            .query(
                "SELECT 1 FROM sqlite_schema
                 WHERE type = 'table' AND name = 'remote_deletion_tombstones'",
                (),
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_none(),
            "rejected catalog must not be silently migrated"
        );
    }

    #[tokio::test]
    async fn existing_registry_with_mismatched_remote_deletion_catalog_requires_typed_reset() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("sessions.db");
        {
            let connection = TestConnection::open(&database_path);
            ensure_registered_schema(&connection)
                .await
                .expect("initialize final V2 authority schema");
        }
        {
            let connection = rusqlite::Connection::open(&database_path).unwrap();
            connection
                .execute_batch(
                    "ALTER TABLE remote_deletion_tombstones
                     ADD COLUMN incompatible_branch_catalog TEXT",
                )
                .expect("make the required final V2 catalog incompatible");
        }

        let connection = TestConnection::open(&database_path);
        let error = ensure_registered_schema(&connection)
            .await
            .expect_err("an incompatible catalog must not be converged");
        assert!(
            matches!(
                error,
                tracedecay_runtime_core::errors::TraceDecayError::ResetRequired { .. }
            ),
            "incompatible final V2 catalog returned the wrong typed problem: {error}"
        );
        let mut rows = connection
            .query(
                "SELECT 1 FROM pragma_table_xinfo('remote_deletion_tombstones')
                 WHERE name = 'incompatible_branch_catalog'",
                (),
            )
            .await
            .unwrap();
        assert!(
            rows.next().await.unwrap().is_some(),
            "rejected catalog must not be silently converged"
        );
    }

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
