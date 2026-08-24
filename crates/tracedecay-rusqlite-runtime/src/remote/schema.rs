pub const REMOTE_NODE_LOCAL_SCHEMA: &str = "
CREATE TABLE remote_store_contract (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    contract_id TEXT NOT NULL CHECK (
        contract_id = 'tracedecay.remote-node.final-v2'
    )
) STRICT;
INSERT INTO remote_store_contract (singleton, contract_id)
VALUES (1, 'tracedecay.remote-node.final-v2');

CREATE TABLE remote_node_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    brain_id TEXT NOT NULL,
    profile_id TEXT NOT NULL,
    node_id TEXT NOT NULL
) STRICT;

CREATE TABLE remote_authorities (
    brain_id TEXT PRIMARY KEY,
    runtime_binding_json TEXT NOT NULL,
    authority_state_json TEXT NOT NULL,
    writer_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE remote_enrollment_grants (
    grant_id TEXT PRIMARY KEY,
    credential_fingerprint TEXT NOT NULL UNIQUE,
    grant_json TEXT NOT NULL,
    admission_json TEXT NOT NULL,
    consumed_at INTEGER
) STRICT;

CREATE TABLE remote_enrollments (
    enrollment_id TEXT PRIMARY KEY,
    brain_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    credential_fingerprint TEXT NOT NULL,
    enrollment_json TEXT NOT NULL,
    commit_receipt_json TEXT NOT NULL,
    UNIQUE (credential_fingerprint),
    UNIQUE (brain_id, node_id, revision)
) STRICT;

CREATE TABLE remote_replay_policies (
    scope_digest TEXT PRIMARY KEY,
    policy_revision INTEGER NOT NULL CHECK (policy_revision > 0),
    evidence_json TEXT NOT NULL CHECK (json_valid(evidence_json))
) STRICT;

CREATE TABLE remote_query_policies (
    scope_digest TEXT PRIMARY KEY,
    policy_revision INTEGER NOT NULL CHECK (policy_revision > 0),
    record_json TEXT NOT NULL CHECK (json_valid(record_json))
) STRICT;

CREATE TABLE remote_recovery_authorities (
    authority_key TEXT PRIMARY KEY,
    authority_json TEXT NOT NULL CHECK (json_valid(authority_json)),
    frontier_sequence INTEGER NOT NULL CHECK (frontier_sequence >= 0),
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE remote_recovery_operations (
    operation_id TEXT PRIMARY KEY,
    operation_kind TEXT NOT NULL CHECK (
        operation_kind IN ('backup', 'restore', 'promotion')
    ),
    request_digest TEXT NOT NULL,
    expected_authority_key TEXT NOT NULL,
    pre_state_digest TEXT NOT NULL,
    context_json TEXT NOT NULL CHECK (json_valid(context_json)),
    state TEXT NOT NULL CHECK (
        state IN (
            'executing', 'completed', 'cancelled', 'timed_out',
            'rolled_back', 'forward_recovery_required'
        )
    ),
    output_json TEXT CHECK (output_json IS NULL OR json_valid(output_json)),
    receipt_json TEXT CHECK (receipt_json IS NULL OR json_valid(receipt_json)),
    started_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE remote_recovery_sink_installations (
    operation_id TEXT NOT NULL REFERENCES remote_recovery_operations(operation_id),
    sink_id TEXT NOT NULL,
    installed_epoch INTEGER NOT NULL CHECK (installed_epoch > 0),
    installed_at INTEGER NOT NULL,
    PRIMARY KEY (operation_id, sink_id)
) STRICT;

CREATE TABLE remote_replay_recovery_lease (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    lease_id TEXT NOT NULL,
    acquired_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > acquired_at)
) STRICT;

CREATE TABLE remote_spool_frames (
    event_id TEXT PRIMARY KEY,
    enrollment_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    previous_event_id TEXT,
    frame_digest TEXT NOT NULL,
    key_revision INTEGER NOT NULL CHECK (key_revision > 0),
    nonce BLOB NOT NULL CHECK (length(nonce) = 12),
    ciphertext BLOB NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN (
            'pending', 'admitted', 'duplicate', 'acknowledged',
            'rejected', 'quarantined', 'garbage_collection_eligible'
        )
    ),
    last_attempt INTEGER NOT NULL DEFAULT 0 CHECK (last_attempt >= 0),
    attempt_started_at INTEGER,
    receipt_json TEXT,
    finding TEXT,
    captured_at INTEGER NOT NULL,
    UNIQUE (enrollment_id, sequence)
) STRICT;

";

pub(super) const REMOTE_NODE_LOCAL_TABLES: &[&str] = &[
    "remote_authorities",
    "remote_enrollment_grants",
    "remote_enrollments",
    "remote_node_identity",
    "remote_query_policies",
    "remote_recovery_authorities",
    "remote_recovery_operations",
    "remote_recovery_sink_installations",
    "remote_replay_policies",
    "remote_replay_recovery_lease",
    "remote_spool_frames",
    "remote_store_contract",
];

pub(super) const REMOTE_NODE_LOCAL_COLUMNS: &[(&str, &str)] = &[
    ("remote_authorities", "brain_id"),
    ("remote_authorities", "runtime_binding_json"),
    ("remote_authorities", "authority_state_json"),
    ("remote_authorities", "writer_json"),
    ("remote_authorities", "updated_at"),
    ("remote_enrollment_grants", "grant_id"),
    ("remote_enrollment_grants", "credential_fingerprint"),
    ("remote_enrollment_grants", "grant_json"),
    ("remote_enrollment_grants", "admission_json"),
    ("remote_enrollment_grants", "consumed_at"),
    ("remote_enrollments", "enrollment_id"),
    ("remote_enrollments", "brain_id"),
    ("remote_enrollments", "node_id"),
    ("remote_enrollments", "revision"),
    ("remote_enrollments", "credential_fingerprint"),
    ("remote_enrollments", "enrollment_json"),
    ("remote_enrollments", "commit_receipt_json"),
    ("remote_node_identity", "singleton"),
    ("remote_node_identity", "brain_id"),
    ("remote_node_identity", "profile_id"),
    ("remote_node_identity", "node_id"),
    ("remote_query_policies", "scope_digest"),
    ("remote_query_policies", "policy_revision"),
    ("remote_query_policies", "record_json"),
    ("remote_recovery_authorities", "authority_key"),
    ("remote_recovery_authorities", "authority_json"),
    ("remote_recovery_authorities", "frontier_sequence"),
    ("remote_recovery_authorities", "updated_at"),
    ("remote_recovery_operations", "operation_id"),
    ("remote_recovery_operations", "operation_kind"),
    ("remote_recovery_operations", "request_digest"),
    ("remote_recovery_operations", "expected_authority_key"),
    ("remote_recovery_operations", "pre_state_digest"),
    ("remote_recovery_operations", "context_json"),
    ("remote_recovery_operations", "state"),
    ("remote_recovery_operations", "output_json"),
    ("remote_recovery_operations", "receipt_json"),
    ("remote_recovery_operations", "started_at"),
    ("remote_recovery_operations", "updated_at"),
    ("remote_recovery_sink_installations", "operation_id"),
    ("remote_recovery_sink_installations", "sink_id"),
    ("remote_recovery_sink_installations", "installed_epoch"),
    ("remote_recovery_sink_installations", "installed_at"),
    ("remote_replay_policies", "scope_digest"),
    ("remote_replay_policies", "policy_revision"),
    ("remote_replay_policies", "evidence_json"),
    ("remote_replay_recovery_lease", "singleton"),
    ("remote_replay_recovery_lease", "lease_id"),
    ("remote_replay_recovery_lease", "acquired_at"),
    ("remote_replay_recovery_lease", "expires_at"),
    ("remote_spool_frames", "event_id"),
    ("remote_spool_frames", "enrollment_id"),
    ("remote_spool_frames", "sequence"),
    ("remote_spool_frames", "previous_event_id"),
    ("remote_spool_frames", "frame_digest"),
    ("remote_spool_frames", "key_revision"),
    ("remote_spool_frames", "nonce"),
    ("remote_spool_frames", "ciphertext"),
    ("remote_spool_frames", "state"),
    ("remote_spool_frames", "last_attempt"),
    ("remote_spool_frames", "attempt_started_at"),
    ("remote_spool_frames", "receipt_json"),
    ("remote_spool_frames", "finding"),
    ("remote_spool_frames", "captured_at"),
    ("remote_store_contract", "singleton"),
    ("remote_store_contract", "contract_id"),
];
