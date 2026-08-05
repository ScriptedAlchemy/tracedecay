//! Workflow definition and handoff tables installed on the registered writer.

use rusqlite::Connection;

/// Stable description hashed into `workflow_schema.definition_digest`.
const WORKFLOW_SCHEMA_DEFINITION_V1: &str = "workflow_definitions(definition_id text not null,definition_version integer not null check >0,payload text not null,payload_digest text not null,primary key(definition_id,definition_version));workflow_handoffs(token_digest text primary key,scope_payload text not null,issued_at integer not null,expires_at integer not null check expires_at>issued_at,consumed integer not null check in(0,1));workflow_schema(singleton integer primary key check =1,schema_version integer check =1,definition_digest text not null);";
pub(crate) const WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1: &str =
    "sha256:4160b0bb90476a3e868774eef0a95d57b75bd6c586a577fcae693d2bbf1aca5c";

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

CREATE TABLE IF NOT EXISTS workflow_schema (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    definition_digest TEXT NOT NULL
) STRICT;

INSERT OR IGNORE INTO workflow_schema (singleton, schema_version, definition_digest)
VALUES (
    1,
    1,
    'sha256:4160b0bb90476a3e868774eef0a95d57b75bd6c586a577fcae693d2bbf1aca5c'
);
";

pub fn install_workflow_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(WORKFLOW_SCHEMA_V1)
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::{WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1, WORKFLOW_SCHEMA_DEFINITION_V1};

    #[test]
    fn workflow_schema_digest_matches_canonical_definition() {
        assert_eq!(
            WORKFLOW_SCHEMA_DEFINITION_DIGEST_V1,
            format!(
                "sha256:{}",
                hex::encode(Sha256::digest(WORKFLOW_SCHEMA_DEFINITION_V1))
            )
        );
    }
}
