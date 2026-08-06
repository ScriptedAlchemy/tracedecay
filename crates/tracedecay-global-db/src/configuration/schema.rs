//! Exact final `SQLite` schema for the revisioned configuration control plane.

use std::collections::BTreeSet;
use thiserror::Error;

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor};

/// Version of the sealed complete topology value stored by this schema.
pub const TOPOLOGY_POLICY_SCHEMA_VERSION: u16 = 1;
const FINAL_CONFIGURATION_SCHEMA_REVISION: i64 = 1;

#[derive(Debug, Error)]
pub enum ConfigurationSchemaError {
    #[error("configuration schema operation failed: {0}")]
    Storage(#[from] tracedecay_runtime_core::db::engine::Error),
    #[error("configuration store is not the exact final shape: {message}")]
    ResetRequired { message: String },
}

const FINAL_CONFIGURATION_TABLES: [&str; 16] = [
    "configuration_access_rules",
    "configuration_audit_events",
    "configuration_audit_redaction_keys",
    "configuration_change_plan_events",
    "configuration_change_plan_operations",
    "configuration_change_plans",
    "configuration_component_activation_events",
    "configuration_credential_references",
    "configuration_entries",
    "configuration_mutation_receipts",
    "configuration_revisions",
    "configuration_schema_metadata",
    "configuration_source_bindings",
    "configuration_topology_policies",
    "configuration_topology_protected_refs",
    "configuration_topology_roots",
];

/// Canonical schema installed only when no configuration object exists.
const CONFIGURATION_SCHEMA_SQL: &str = r"
CREATE TABLE configuration_schema_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    final_schema_revision INTEGER NOT NULL CHECK (final_schema_revision = 1)
);
INSERT INTO configuration_schema_metadata (singleton, final_schema_revision) VALUES (1, 1);

CREATE TABLE IF NOT EXISTS configuration_revisions (
    revision_id TEXT PRIMARY KEY,
    parent_revision_id TEXT,
    -- A forward rollback can intentionally reproduce a prior immutable
    -- snapshot under a new revision, so snapshot identity is not revision
    -- identity and must not be globally unique.
    snapshot_id TEXT NOT NULL,
    effective_behavior_digest TEXT NOT NULL,
    resolution_provenance_digest TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY(parent_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_entries (
    revision_id TEXT NOT NULL,
    key TEXT NOT NULL,
    layer_kind TEXT NOT NULL,
    layer_id TEXT,
    schema_revision INTEGER NOT NULL,
    typed_value TEXT NOT NULL,
    PRIMARY KEY(revision_id, key, layer_kind, layer_id),
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_topology_policies (
    revision_id TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    topology_policy_digest TEXT NOT NULL,
    placement_kind TEXT NOT NULL,
    default_cross_merge_mode TEXT NOT NULL,
    allow_cross_repository INTEGER NOT NULL CHECK (allow_cross_repository IN (0, 1)),
    cleanliness_kind TEXT NOT NULL,
    review_kind TEXT NOT NULL,
    require_fresh_preflight INTEGER NOT NULL CHECK (require_fresh_preflight IN (0, 1)),
    maximum_preflight_age_seconds INTEGER NOT NULL,
    history_rewrite_kind TEXT NOT NULL CHECK (history_rewrite_kind = 'forbid_force_and_rebase'),
    escalation_kind TEXT NOT NULL,
    automatic_gc_kind TEXT NOT NULL,
    notification_level TEXT NOT NULL,
    sealed_policy_value BLOB NOT NULL,
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_topology_roots (
    revision_id TEXT NOT NULL,
    root_ordinal INTEGER NOT NULL,
    root_id TEXT NOT NULL,
    locator_digest TEXT NOT NULL,
    repository_scope_digest TEXT NOT NULL,
    maximum_active_worktrees INTEGER NOT NULL,
    PRIMARY KEY(revision_id, root_ordinal),
    UNIQUE(revision_id, root_id),
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_topology_protected_refs (
    revision_id TEXT NOT NULL,
    rule_ordinal INTEGER NOT NULL,
    selector_kind TEXT NOT NULL,
    selector_digest TEXT NOT NULL,
    disposition TEXT NOT NULL,
    PRIMARY KEY(revision_id, rule_ordinal),
    UNIQUE(revision_id, selector_digest),
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_source_bindings (
    revision_id TEXT NOT NULL,
    binding_id TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    locator_digest TEXT NOT NULL,
    authority_kind TEXT NOT NULL CHECK (authority_kind IN ('project', 'projectless_hermes')),
    project_id TEXT,
    user_profile_id TEXT,
    provenance_digest TEXT NOT NULL,
    PRIMARY KEY(revision_id, binding_id),
    UNIQUE(revision_id, source_kind, locator_digest),
    CHECK (
        (authority_kind = 'project' AND project_id IS NOT NULL AND user_profile_id IS NULL)
        OR
        (authority_kind = 'projectless_hermes' AND project_id IS NULL AND user_profile_id IS NOT NULL)
    ),
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_access_rules (
    revision_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    subject_kind TEXT NOT NULL,
    subject_id TEXT,
    actor_kind TEXT,
    actor_id TEXT,
    operation_kind TEXT,
    source_kind TEXT,
    authority_kind TEXT NOT NULL CHECK (authority_kind IN ('project', 'projectless_hermes')),
    project_id TEXT,
    user_profile_id TEXT,
    capability_encoding TEXT NOT NULL,
    effect TEXT NOT NULL CHECK (effect IN ('allow', 'deny')),
    expires_at INTEGER,
    PRIMARY KEY(revision_id, rule_id),
    CHECK (
        (authority_kind = 'project' AND project_id IS NOT NULL AND user_profile_id IS NULL)
        OR
        (authority_kind = 'projectless_hermes' AND project_id IS NULL AND user_profile_id IS NOT NULL)
    ),
    FOREIGN KEY(revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_change_plans (
    plan_id TEXT PRIMARY KEY,
    actor_id TEXT NOT NULL,
    base_revision_id TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    resolved_scope_digest TEXT NOT NULL,
    membership_digest TEXT,
    authorization_policy_digest TEXT NOT NULL,
    policy_epoch INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY(base_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_change_plan_operations (
    plan_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    payload_schema_revision INTEGER NOT NULL,
    sealed_typed_operation BLOB NOT NULL,
    operation_digest TEXT NOT NULL,
    PRIMARY KEY(plan_id, sequence),
    FOREIGN KEY(plan_id) REFERENCES configuration_change_plans(plan_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_change_plan_events (
    plan_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    event_kind TEXT NOT NULL,
    safe_reason_code TEXT,
    occurred_at INTEGER NOT NULL,
    PRIMARY KEY(plan_id, sequence),
    FOREIGN KEY(plan_id) REFERENCES configuration_change_plans(plan_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_mutation_receipts (
    receipt_id TEXT PRIMARY KEY,
    plan_id TEXT,
    actor_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    base_revision_id TEXT NOT NULL,
    result_revision_id TEXT NOT NULL,
    operation_digest TEXT NOT NULL,
    authorization_policy_digest TEXT NOT NULL,
    activation_status TEXT NOT NULL,
    receipt_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(actor_id, idempotency_key),
    UNIQUE(plan_id, idempotency_key),
    FOREIGN KEY(plan_id) REFERENCES configuration_change_plans(plan_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY(base_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY(result_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS configuration_audit_events (
    event_id TEXT PRIMARY KEY,
    actor_id TEXT NOT NULL,
    idempotency_key TEXT,
    operation_kind TEXT NOT NULL,
    base_revision_id TEXT NOT NULL,
    result_revision_id TEXT,
    sealed_target_reference BLOB,
    event_scoped_target_commitment TEXT NOT NULL,
    receipt_digest TEXT,
    correlation_id TEXT,
    safe_reason_code TEXT,
    occurred_at INTEGER NOT NULL,
    FOREIGN KEY(base_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY(result_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

-- One store-local key makes event-scoped target commitments resistant to
-- offline guessing. It is never returned by the configuration APIs.
CREATE TABLE IF NOT EXISTS configuration_audit_redaction_keys (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    key_material BLOB NOT NULL CHECK (length(key_material) = 32),
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS configuration_credential_references (
    reference_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    reference_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    rotation INTEGER NOT NULL
);

-- Activation is append-only evidence. The latest row per component exposes
-- desired versus observed state, while a failed activation keeps the prior
-- last-working revision rather than rewriting runtime history.
CREATE TABLE IF NOT EXISTS configuration_component_activation_events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT,
    component TEXT NOT NULL,
    desired_revision_id TEXT NOT NULL,
    observed_revision_id TEXT,
    last_working_revision_id TEXT,
    restart_required INTEGER NOT NULL CHECK (restart_required IN (0, 1)),
    activation_error_code TEXT,
    occurred_at INTEGER NOT NULL,
    FOREIGN KEY(desired_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY(observed_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT,
    FOREIGN KEY(last_working_revision_id) REFERENCES configuration_revisions(revision_id)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS idx_configuration_revision_parent
    ON configuration_revisions(parent_revision_id);
CREATE INDEX IF NOT EXISTS idx_configuration_entry_key
    ON configuration_entries(key);
CREATE INDEX IF NOT EXISTS idx_configuration_topology_root_id
    ON configuration_topology_roots(root_id);
CREATE INDEX IF NOT EXISTS idx_configuration_topology_root_locator
    ON configuration_topology_roots(locator_digest);
CREATE INDEX IF NOT EXISTS idx_configuration_topology_protected_ref
    ON configuration_topology_protected_refs(selector_digest);
CREATE INDEX IF NOT EXISTS idx_configuration_audit_occurred_at
    ON configuration_audit_events(occurred_at, event_id);
CREATE INDEX IF NOT EXISTS idx_configuration_component_activation_latest
    ON configuration_component_activation_events(component, event_id DESC);

CREATE TRIGGER IF NOT EXISTS configuration_schema_metadata_immutable_update
BEFORE UPDATE ON configuration_schema_metadata
BEGIN SELECT RAISE(ABORT, 'configuration schema metadata is immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_schema_metadata_immutable_delete
BEFORE DELETE ON configuration_schema_metadata
BEGIN SELECT RAISE(ABORT, 'configuration schema metadata is immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_revisions_immutable_update
BEFORE UPDATE ON configuration_revisions
BEGIN SELECT RAISE(ABORT, 'configuration revisions are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_revisions_immutable_delete
BEFORE DELETE ON configuration_revisions
BEGIN SELECT RAISE(ABORT, 'configuration revisions are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_entries_immutable_update
BEFORE UPDATE ON configuration_entries
BEGIN SELECT RAISE(ABORT, 'configuration entries are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_entries_immutable_delete
BEFORE DELETE ON configuration_entries
BEGIN SELECT RAISE(ABORT, 'configuration entries are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_topology_policy_immutable_update
BEFORE UPDATE ON configuration_topology_policies
BEGIN SELECT RAISE(ABORT, 'configuration topology policies are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_topology_policy_immutable_delete
BEFORE DELETE ON configuration_topology_policies
BEGIN SELECT RAISE(ABORT, 'configuration topology policies are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_topology_roots_immutable_update
BEFORE UPDATE ON configuration_topology_roots
BEGIN SELECT RAISE(ABORT, 'configuration topology roots are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_topology_roots_immutable_delete
BEFORE DELETE ON configuration_topology_roots
BEGIN SELECT RAISE(ABORT, 'configuration topology roots are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_topology_protected_refs_immutable_update
BEFORE UPDATE ON configuration_topology_protected_refs
BEGIN SELECT RAISE(ABORT, 'configuration topology protected refs are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_topology_protected_refs_immutable_delete
BEFORE DELETE ON configuration_topology_protected_refs
BEGIN SELECT RAISE(ABORT, 'configuration topology protected refs are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_source_bindings_immutable_update
BEFORE UPDATE ON configuration_source_bindings
BEGIN SELECT RAISE(ABORT, 'configuration source bindings are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_source_bindings_immutable_delete
BEFORE DELETE ON configuration_source_bindings
BEGIN SELECT RAISE(ABORT, 'configuration source bindings are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_access_rules_immutable_update
BEFORE UPDATE ON configuration_access_rules
BEGIN SELECT RAISE(ABORT, 'configuration access rules are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_access_rules_immutable_delete
BEFORE DELETE ON configuration_access_rules
BEGIN SELECT RAISE(ABORT, 'configuration access rules are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_change_plans_immutable_update
BEFORE UPDATE ON configuration_change_plans
BEGIN SELECT RAISE(ABORT, 'configuration change plans are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_change_plans_immutable_delete
BEFORE DELETE ON configuration_change_plans
BEGIN SELECT RAISE(ABORT, 'configuration change plans are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_change_plan_operations_immutable_update
BEFORE UPDATE ON configuration_change_plan_operations
BEGIN SELECT RAISE(ABORT, 'configuration change operations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_change_plan_operations_immutable_delete
BEFORE DELETE ON configuration_change_plan_operations
BEGIN SELECT RAISE(ABORT, 'configuration change operations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_change_plan_events_immutable_update
BEFORE UPDATE ON configuration_change_plan_events
BEGIN SELECT RAISE(ABORT, 'configuration change plan events are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_change_plan_events_immutable_delete
BEFORE DELETE ON configuration_change_plan_events
BEGIN SELECT RAISE(ABORT, 'configuration change plan events are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_mutation_receipts_immutable_update
BEFORE UPDATE ON configuration_mutation_receipts
BEGIN SELECT RAISE(ABORT, 'configuration mutation receipts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_mutation_receipts_immutable_delete
BEFORE DELETE ON configuration_mutation_receipts
BEGIN SELECT RAISE(ABORT, 'configuration mutation receipts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_audit_events_immutable_update
BEFORE UPDATE ON configuration_audit_events
BEGIN SELECT RAISE(ABORT, 'configuration audit events are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_audit_events_immutable_delete
BEFORE DELETE ON configuration_audit_events
BEGIN SELECT RAISE(ABORT, 'configuration audit events are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_audit_redaction_keys_immutable_update
BEFORE UPDATE ON configuration_audit_redaction_keys
BEGIN SELECT RAISE(ABORT, 'configuration audit redaction keys are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_audit_redaction_keys_immutable_delete
BEFORE DELETE ON configuration_audit_redaction_keys
BEGIN SELECT RAISE(ABORT, 'configuration audit redaction keys are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_credential_references_immutable_update
BEFORE UPDATE ON configuration_credential_references
BEGIN SELECT RAISE(ABORT, 'configuration credential references are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_credential_references_immutable_delete
BEFORE DELETE ON configuration_credential_references
BEGIN SELECT RAISE(ABORT, 'configuration credential references are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_component_activation_events_immutable_update
BEFORE UPDATE ON configuration_component_activation_events
BEGIN SELECT RAISE(ABORT, 'configuration component activation events are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_component_activation_events_immutable_delete
BEFORE DELETE ON configuration_component_activation_events
BEGIN SELECT RAISE(ABORT, 'configuration component activation events are immutable'); END;
";

pub async fn ensure_configuration_schema(
    connection: &(impl Executor + QueryExecutor),
) -> Result<(), ConfigurationSchemaError> {
    let actual = configuration_tables(connection).await?;
    if actual.is_empty() {
        connection.execute_batch(CONFIGURATION_SCHEMA_SQL).await?;
        return Ok(());
    }
    validate_configuration_schema_tables(connection, actual).await
}

/// Validates an already-created store without installing or repairing any
/// configuration object.
pub async fn validate_configuration_schema(
    connection: &impl QueryExecutor,
) -> Result<(), ConfigurationSchemaError> {
    let actual = configuration_tables(connection).await?;
    if actual.is_empty() {
        return Err(ConfigurationSchemaError::ResetRequired {
            message: "configuration final schema is missing".to_owned(),
        });
    }
    validate_configuration_schema_tables(connection, actual).await
}

async fn validate_configuration_schema_tables(
    connection: &impl QueryExecutor,
    actual: BTreeSet<String>,
) -> Result<(), ConfigurationSchemaError> {
    let expected = FINAL_CONFIGURATION_TABLES
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(ConfigurationSchemaError::ResetRequired {
            message: "configuration tables are missing, partial, or from another schema".to_owned(),
        });
    }

    let mut rows = connection
        .query(
            "SELECT final_schema_revision
             FROM configuration_schema_metadata
             WHERE singleton = 1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(ConfigurationSchemaError::ResetRequired {
            message: "configuration final-schema identity is missing".to_owned(),
        });
    };
    let revision = row.get::<i64>(0)?;
    if revision != FINAL_CONFIGURATION_SCHEMA_REVISION || rows.next().await?.is_some() {
        return Err(ConfigurationSchemaError::ResetRequired {
            message: "configuration final-schema identity is incompatible".to_owned(),
        });
    }
    Ok(())
}

async fn configuration_tables(
    connection: &impl QueryExecutor,
) -> Result<BTreeSet<String>, tracedecay_runtime_core::db::engine::Error> {
    let mut rows = connection
        .query(
            "SELECT name
             FROM sqlite_master
             WHERE type = 'table' AND name LIKE 'configuration_%'
             ORDER BY name",
            (),
        )
        .await?;
    let mut tables = BTreeSet::new();
    while let Some(row) = rows.next().await? {
        tables.insert(row.get::<String>(0)?);
    }
    Ok(tables)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn connection() -> (
        tempfile::TempDir,
        tracedecay_runtime_core::db::engine::TestConnection,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let connection = tracedecay_runtime_core::db::engine::TestConnection::open(
            &directory.path().join("configuration.db"),
        );
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .unwrap();
        ensure_configuration_schema(&*connection).await.unwrap();
        (directory, connection)
    }

    #[tokio::test]
    async fn every_revision_owned_and_append_only_table_rejects_update_and_delete() {
        let (_directory, connection) = connection().await;
        connection
            .execute_batch(
                "INSERT INTO configuration_revisions VALUES
                    ('revision.1', NULL, 'snapshot.1', 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     'sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                     'actor.1', 'genesis', 1);
                 INSERT INTO configuration_entries VALUES
                    ('revision.1', 'analyzer.settings.v1', 'project', 'project.1', 1, '{}');
                 INSERT INTO configuration_topology_policies VALUES
                    ('revision.1', 1, 'sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
                     'existing_worktree_only', 'disabled', 0, 'require_clean', 'independent_review',
                     1, 300, 'forbid_force_and_rebase', 'reject', 'disabled', 'critical_only', X'00');
                 INSERT INTO configuration_topology_roots VALUES
                    ('revision.1', 0, 'root.1',
                     'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
                     'sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee', 1);
                 INSERT INTO configuration_topology_protected_refs VALUES
                    ('revision.1', 0, 'native_default_branch',
                     'sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff', 'reject');
                 INSERT INTO configuration_source_bindings VALUES
                    ('revision.1', 'binding.1', 'cursor',
                     'sha256:1111111111111111111111111111111111111111111111111111111111111111',
                     'project', 'project.1', NULL,
                     'sha256:2222222222222222222222222222222222222222222222222222222222222222');
                 INSERT INTO configuration_access_rules VALUES
                    ('revision.1', 'rule.1', 'actor', 'actor.1', 'actor', 'actor.1',
                     'read', 'cursor', 'project', 'project.1', NULL, 'capability.read', 'deny', NULL);
                 INSERT INTO configuration_change_plans VALUES
                    ('plan.1', 'actor.1', 'revision.1',
                     'sha256:3333333333333333333333333333333333333333333333333333333333333333',
                     'sha256:4444444444444444444444444444444444444444444444444444444444444444',
                     NULL,
                     'sha256:5555555555555555555555555555555555555555555555555555555555555555',
                     1, 10, 1);
                 INSERT INTO configuration_change_plan_operations VALUES
                    ('plan.1', 0, 1, X'00',
                     'sha256:3333333333333333333333333333333333333333333333333333333333333333');
                 INSERT INTO configuration_change_plan_events VALUES
                    ('plan.1', 0, 'dry_run_created', NULL, 1);
                 INSERT INTO configuration_mutation_receipts VALUES
                    ('receipt.1', 'plan.1', 'actor.1', 'idempotency.1', 'revision.1', 'revision.1',
                     'sha256:3333333333333333333333333333333333333333333333333333333333333333',
                     'sha256:5555555555555555555555555555555555555555555555555555555555555555',
                     'active',
                     'sha256:6666666666666666666666666666666666666666666666666666666666666666', 1);
                 INSERT INTO configuration_audit_events VALUES
                    ('audit.1', 'actor.1', NULL, 'genesis', 'revision.1', 'revision.1', NULL,
                     'sha256:7777777777777777777777777777777777777777777777777777777777777777',
                      NULL, NULL, NULL, 1);
                  INSERT INTO configuration_audit_redaction_keys VALUES
                     (1, zeroblob(32), 1);
                 INSERT INTO configuration_credential_references VALUES
                    ('credential.1', 'api_token',
                     'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac', 1, 0);
                 INSERT INTO configuration_component_activation_events (
                    component, desired_revision_id, observed_revision_id,
                    last_working_revision_id, restart_required, activation_error_code, occurred_at
                 ) VALUES ('gateway', 'revision.1', 'revision.1', 'revision.1', 0, NULL, 1);",
            )
            .await
            .unwrap();

        let mut rows = connection
            .query(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name LIKE 'configuration_%'
                 ORDER BY name",
                (),
            )
            .await
            .unwrap();
        let mut tables = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            tables.push(row.get::<String>(0).unwrap());
        }
        drop(rows);
        assert!(!tables.is_empty());
        for table in tables {
            assert!(
                connection
                    .execute(&format!("UPDATE {table} SET rowid = rowid"), ())
                    .await
                    .is_err(),
                "{table} accepted an update"
            );
            assert!(
                connection
                    .execute(&format!("DELETE FROM {table}"), ())
                    .await
                    .is_err(),
                "{table} accepted a delete"
            );
        }
    }

    #[tokio::test]
    async fn prior_or_partial_configuration_schema_requires_reset_without_healing() {
        let directory = tempfile::tempdir().unwrap();
        let connection = tracedecay_runtime_core::db::engine::TestConnection::open(
            &directory.path().join("configuration.db"),
        );
        connection
            .execute_batch(
                "CREATE TABLE configuration_revisions (revision_id TEXT PRIMARY KEY);
                 CREATE TABLE configuration_prior_receipts (receipt_name TEXT PRIMARY KEY);",
            )
            .await
            .unwrap();

        assert!(matches!(
            ensure_configuration_schema(&*connection).await,
            Err(ConfigurationSchemaError::ResetRequired { .. })
        ));
        let tables = configuration_tables(&*connection).await.unwrap();
        assert!(tables.contains("configuration_prior_receipts"));
        assert!(!tables.contains("configuration_schema_metadata"));
    }

    #[tokio::test]
    async fn exact_final_configuration_schema_is_idempotent() {
        let (_directory, connection) = connection().await;
        ensure_configuration_schema(&*connection).await.unwrap();
        validate_configuration_schema(&*connection).await.unwrap();
    }

    #[tokio::test]
    async fn validation_never_installs_a_missing_configuration_schema() {
        let directory = tempfile::tempdir().unwrap();
        let connection = tracedecay_runtime_core::db::engine::TestConnection::open(
            &directory.path().join("configuration.db"),
        );

        assert!(matches!(
            validate_configuration_schema(&*connection).await,
            Err(ConfigurationSchemaError::ResetRequired { .. })
        ));
        assert!(configuration_tables(&*connection).await.unwrap().is_empty());
    }
}
