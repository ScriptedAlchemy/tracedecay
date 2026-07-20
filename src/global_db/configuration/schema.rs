//! Additive SQLite schema for the revisioned configuration control plane.

use thiserror::Error;

/// Version of the sealed complete topology value stored by this schema.
pub const TOPOLOGY_POLICY_SCHEMA_VERSION: u16 = 1;
pub const WORK_TOPOLOGY_POLICY_MIGRATION_RECEIPT_NAME: &str = "work-topology-policy";

#[derive(Debug, Error)]
pub enum ConfigurationSchemaError {
    #[error("configuration schema operation failed: {0}")]
    Storage(#[from] libsql::Error),
}

/// Tables are additive and append-only. Registration from the global schema
/// lifecycle is intentionally performed by the shared migration spine.
const CONFIGURATION_SCHEMA_SQL: &str = r"
CREATE TABLE IF NOT EXISTS configuration_revisions (
    revision_id TEXT PRIMARY KEY,
    parent_revision_id TEXT,
    snapshot_id TEXT NOT NULL UNIQUE,
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

CREATE TABLE IF NOT EXISTS configuration_migration_quarantine (
    source_kind TEXT NOT NULL,
    source_key_digest TEXT NOT NULL,
    reason_code TEXT NOT NULL,
    redacted_value_digest TEXT NOT NULL,
    quarantined_at INTEGER NOT NULL,
    PRIMARY KEY(source_kind, source_key_digest, redacted_value_digest)
);

CREATE TABLE IF NOT EXISTS configuration_migration_receipts (
    receipt_name TEXT NOT NULL,
    source_snapshot_digest TEXT NOT NULL,
    initial_snapshot_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(receipt_name, source_snapshot_digest)
);

CREATE TABLE IF NOT EXISTS configuration_credential_references (
    reference_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    reference_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    rotation INTEGER NOT NULL
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

CREATE TRIGGER IF NOT EXISTS configuration_revisions_immutable_update
BEFORE UPDATE ON configuration_revisions
BEGIN SELECT RAISE(ABORT, 'configuration revisions are immutable'); END;
CREATE TRIGGER IF NOT EXISTS configuration_revisions_immutable_delete
BEFORE DELETE ON configuration_revisions
BEGIN SELECT RAISE(ABORT, 'configuration revisions are immutable'); END;
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
";

pub async fn ensure_configuration_schema(
    connection: &libsql::Connection,
) -> Result<(), ConfigurationSchemaError> {
    connection.execute_batch(CONFIGURATION_SCHEMA_SQL).await?;
    Ok(())
}
