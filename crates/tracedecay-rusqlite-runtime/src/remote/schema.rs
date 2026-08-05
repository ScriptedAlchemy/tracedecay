pub const REMOTE_NODE_LOCAL_SCHEMA: &str = "
CREATE TABLE remote_store_contract (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    contract_id TEXT NOT NULL CHECK (
        contract_id = 'tracedecay.remote-node.final-v2'
    )
) STRICT;
INSERT INTO remote_store_contract (singleton, contract_id)
VALUES (1, 'tracedecay.remote-node.final-v2');

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
    "remote_query_policies",
    "remote_replay_policies",
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
    ("remote_query_policies", "scope_digest"),
    ("remote_query_policies", "policy_revision"),
    ("remote_query_policies", "record_json"),
    ("remote_replay_policies", "scope_digest"),
    ("remote_replay_policies", "policy_revision"),
    ("remote_replay_policies", "evidence_json"),
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

/// Canonical repository-store fragment for replay identity and sequencing.
///
/// The registered final-schema authority consumes this exact fragment. It is
/// deliberately absent from [`REMOTE_NODE_LOCAL_SCHEMA`].
pub const REMOTE_OBSERVATION_EVENTS_SCHEMA: &str = "
CREATE TABLE remote_observation_events (
    event_id TEXT PRIMARY KEY,
    frame_digest TEXT NOT NULL,
    enrollment_id TEXT NOT NULL,
    enrollment_revision INTEGER NOT NULL CHECK (enrollment_revision > 0),
    node_id TEXT NOT NULL,
    policy_revision INTEGER NOT NULL CHECK (policy_revision > 0),
    capture_sequence INTEGER NOT NULL CHECK (capture_sequence > 0),
    previous_event_id TEXT REFERENCES remote_observation_events(event_id),
    observation_id TEXT NOT NULL UNIQUE REFERENCES observations(observation_id),
    writer_fence_json TEXT NOT NULL CHECK (json_valid(writer_fence_json)),
    captured_at INTEGER NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    command_digest TEXT NOT NULL,
    UNIQUE (enrollment_id, node_id, capture_sequence)
) STRICT;
";
