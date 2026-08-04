//! Canonical physical schema shared by fact and observation retrieval anchors.
//!
//! An anchor is immutable evidence, while its observation/fact binding remains
//! local to the physical store that owns the referenced record.  Keeping this
//! small schema here prevents those stores from drifting into competing anchor
//! identities.

use crate::db::engine::Executor;
use crate::errors::{Result, TraceDecayError};

/// The canonical anchor DDL lives in `tracedecay-store` because the concrete
/// executors in the rusqlite runtime crate write the same table and must see
/// the same constraints; installing a private copy here is how a fixture ends
/// up weaker than production.
const ANCHORS_SCHEMA: &str = tracedecay_store::RETRIEVAL_ANCHORS_SCHEMA_DDL;

const ALIASES_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS retrieval_anchor_aliases (
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        alias_kind TEXT NOT NULL CHECK(length(alias_kind) > 0),
        locator_digest TEXT NOT NULL CHECK(length(locator_digest) > 0),
        anchor_id TEXT NOT NULL,
        PRIMARY KEY(owner_json, alias_kind, locator_digest),
        UNIQUE(anchor_id, alias_kind, locator_digest),
        FOREIGN KEY(anchor_id, owner_json)
            REFERENCES retrieval_anchors(anchor_id, owner_json)
    );
";

const AUTHORITY_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS retrieval_anchor_dispositions (
        sequence INTEGER PRIMARY KEY AUTOINCREMENT,
        disposition_id TEXT NOT NULL CHECK(length(disposition_id) > 0),
        anchor_id TEXT NOT NULL,
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        state TEXT NOT NULL
            CHECK(state IN (
                'active', 'superseded', 'redacted', 'expired', 'quarantined',
                'deleted', 'unavailable'
            )),
        superseded_by TEXT,
        reason_class TEXT NOT NULL CHECK(reason_class IN (
            'user_request', 'retention', 'redaction', 'quarantine',
            'correction', 'legal_hold', 'source_unavailable'
        )),
        effective_at INTEGER NOT NULL,
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        UNIQUE(owner_json, disposition_id),
        FOREIGN KEY(anchor_id, owner_json)
            REFERENCES retrieval_anchors(anchor_id, owner_json),
        FOREIGN KEY(superseded_by, owner_json)
            REFERENCES retrieval_anchors(anchor_id, owner_json),
        CHECK(
            (state = 'superseded' AND superseded_by IS NOT NULL)
            OR (state <> 'superseded' AND superseded_by IS NULL)
        )
    );
    CREATE INDEX IF NOT EXISTS idx_retrieval_anchor_dispositions_current
        ON retrieval_anchor_dispositions(anchor_id, owner_json, sequence DESC);

    CREATE TABLE IF NOT EXISTS retrieval_anchor_reverse_lineage (
        source_anchor_id TEXT NOT NULL,
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        derivative_kind TEXT NOT NULL
            CHECK(derivative_kind IN ('span', 'contribution', 'finding')),
        derivative_id TEXT NOT NULL CHECK(length(derivative_id) > 0),
        direct_evidence INTEGER NOT NULL CHECK(direct_evidence IN (0, 1)),
        PRIMARY KEY(
            source_anchor_id, owner_json, derivative_kind, derivative_id
        ),
        FOREIGN KEY(source_anchor_id, owner_json)
            REFERENCES retrieval_anchors(anchor_id, owner_json)
    );
    CREATE INDEX IF NOT EXISTS idx_retrieval_anchor_reverse_derivative
        ON retrieval_anchor_reverse_lineage(
            owner_json, derivative_kind, derivative_id, direct_evidence
        );

    CREATE TABLE IF NOT EXISTS retrieval_anchor_derivative_tombstones (
        source_anchor_id TEXT NOT NULL,
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        derivative_kind TEXT NOT NULL
            CHECK(derivative_kind IN ('span', 'contribution', 'finding')),
        derivative_id TEXT NOT NULL CHECK(length(derivative_id) > 0),
        disposition_id TEXT NOT NULL,
        effective_at INTEGER NOT NULL,
        PRIMARY KEY(
            source_anchor_id, owner_json, derivative_kind, derivative_id,
            disposition_id
        ),
        FOREIGN KEY(
            source_anchor_id, owner_json, derivative_kind, derivative_id
        ) REFERENCES retrieval_anchor_reverse_lineage(
            source_anchor_id, owner_json, derivative_kind, derivative_id
        )
    );
";

const IMMUTABILITY_TRIGGERS: &str = "
    CREATE TRIGGER IF NOT EXISTS retrieval_anchors_immutable_update
    BEFORE UPDATE ON retrieval_anchors BEGIN
        SELECT RAISE(ABORT, 'retrieval anchors are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchors_immutable_delete
    BEFORE DELETE ON retrieval_anchors BEGIN
        SELECT RAISE(ABORT, 'retrieval anchors are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_aliases_immutable_update
    BEFORE UPDATE ON retrieval_anchor_aliases BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor aliases are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_aliases_immutable_delete
    BEFORE DELETE ON retrieval_anchor_aliases BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor aliases are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_dispositions_immutable_update
    BEFORE UPDATE ON retrieval_anchor_dispositions BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor dispositions are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_dispositions_immutable_delete
    BEFORE DELETE ON retrieval_anchor_dispositions BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor dispositions are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_reverse_lineage_immutable_update
    BEFORE UPDATE ON retrieval_anchor_reverse_lineage BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor reverse lineage is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_reverse_lineage_immutable_delete
    BEFORE DELETE ON retrieval_anchor_reverse_lineage BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor reverse lineage is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_derivative_tombstones_immutable_update
    BEFORE UPDATE ON retrieval_anchor_derivative_tombstones BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor derivative tombstones are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_derivative_tombstones_immutable_delete
    BEFORE DELETE ON retrieval_anchor_derivative_tombstones BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor derivative tombstones are immutable');
    END;
";

fn database_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation}: {error}"),
        operation: operation.to_owned(),
    }
}

pub async fn install_retrieval_anchor_schema(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<()> {
    conn.execute_batch(ANCHORS_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    conn.execute_batch(ALIASES_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    conn.execute_batch(AUTHORITY_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    conn.execute_batch(IMMUTABILITY_TRIGGERS)
        .await
        .map_err(|error| database_error(operation, error))
}

#[cfg(test)]
mod tests {
    use crate::db::engine::{Executor, TestConnection};

    use super::install_retrieval_anchor_schema;

    #[tokio::test]
    async fn fresh_schema_installs_final_owner_bound_tables() {
        let directory = tempfile::tempdir().expect("create retrieval-anchor schema fixture");
        let connection = TestConnection::open(&directory.path().join("anchors.db"));
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .expect("enable foreign keys");

        install_retrieval_anchor_schema(&connection, "install final retrieval-anchor schema")
            .await
            .expect("install schema");

        for table in [
            "retrieval_anchors",
            "retrieval_anchor_aliases",
            "retrieval_anchor_dispositions",
            "retrieval_anchor_reverse_lineage",
            "retrieval_anchor_derivative_tombstones",
        ] {
            let mut rows = connection
                .query(
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                )
                .await
                .expect("query installed schema");
            assert!(
                rows.next().await.expect("read installed schema").is_some(),
                "missing final table {table}"
            );
        }
    }
}
