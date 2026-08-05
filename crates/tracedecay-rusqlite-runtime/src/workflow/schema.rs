//! Workflow definition and handoff tables installed on the registered writer.

use rusqlite::Connection;

pub(crate) const WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1: &str =
    "sha256:ef3f0fdc0760f91f64f8cc567cee1174dbd94fec69c9de2a39f9683fd8b780da";

pub const WORKFLOW_SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS workflow_definitions (
    definition_id TEXT NOT NULL,
    definition_version INTEGER NOT NULL CHECK (definition_version > 0),
    payload TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    PRIMARY KEY (definition_id, definition_version)
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_handoffs (
    token_digest TEXT NOT NULL PRIMARY KEY,
    scope_payload TEXT NOT NULL,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > issued_at),
    consumed INTEGER NOT NULL CHECK (consumed IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_effect_journal (
    idempotency_key TEXT NOT NULL PRIMARY KEY,
    identity_digest TEXT NOT NULL,
    identity_payload TEXT NOT NULL,
    identity_payload_digest TEXT NOT NULL,
    prepared_payload TEXT NOT NULL,
    prepared_payload_digest TEXT NOT NULL,
    operation TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN ('before_effect', 'in_flight', 'committed', 'reconciled')
    ),
    terminal_payload TEXT,
    terminal_payload_digest TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS workflow_schema (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    definition_digest TEXT NOT NULL
) STRICT;

INSERT OR IGNORE INTO workflow_schema (singleton, schema_version, definition_digest)
VALUES (
    1,
    1,
    'sha256:ef3f0fdc0760f91f64f8cc567cee1174dbd94fec69c9de2a39f9683fd8b780da'
);
";

pub fn install_workflow_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(WORKFLOW_SCHEMA_V1)
}
