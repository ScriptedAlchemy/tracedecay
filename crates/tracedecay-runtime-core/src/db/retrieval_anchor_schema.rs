//! Canonical physical schema shared by fact and observation retrieval anchors.
//!
//! An anchor is immutable evidence, while its observation/fact binding remains
//! local to the physical store that owns the referenced record.  Keeping this
//! small schema here prevents those stores from drifting into competing anchor
//! identities.

use std::collections::BTreeSet;

use crate::db::engine::{Executor, params};
use tracedecay_domain::errors::{Result, TraceDecayError};

/// The canonical anchor DDL lives in `tracedecay-store` because the concrete
/// executors in the rusqlite runtime crate write the same table and must see
/// the same constraints; installing a private copy here is how a fixture ends
/// up weaker than production.
const ANCHORS_SCHEMA: &str = tracedecay_store::RETRIEVAL_ANCHORS_SCHEMA_DDL;

pub(super) const ALIASES_SCHEMA: &str = "
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

pub(super) const AUTHORITY_SCHEMA: &str = "
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

/// Delete guards on the anchor identity and alias tables.
///
/// A scoped observation-authority reset drops exactly these two inside its
/// maintenance transaction, removes the anchors the reset observation stream
/// bound, and reinstalls every guard from
/// [`RETRIEVAL_ANCHOR_IMMUTABILITY_TRIGGERS_SQL`] before it commits.
pub const RETRIEVAL_ANCHOR_DELETE_GUARD_TRIGGERS: &[&str] = &[
    "retrieval_anchors_immutable_delete",
    "retrieval_anchor_aliases_immutable_delete",
];

/// Idempotent DDL for every retrieval-anchor immutability trigger; the single
/// authority both schema installation and scoped maintenance reinstall from.
pub const RETRIEVAL_ANCHOR_IMMUTABILITY_TRIGGERS_SQL: &str = "
    CREATE TRIGGER IF NOT EXISTS retrieval_anchors_immutable_update
    BEFORE UPDATE ON retrieval_anchors BEGIN
        SELECT RAISE(ABORT, 'retrieval anchors are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchors_immutable_delete
    BEFORE DELETE ON retrieval_anchors BEGIN
        SELECT RAISE(ABORT, 'retrieval anchors are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_aliases_immutable_update
    BEFORE UPDATE ON retrieval_anchor_aliases
    WHEN NEW.owner_json != OLD.owner_json
      OR NEW.alias_kind != OLD.alias_kind
      OR NEW.locator_digest != OLD.locator_digest
      OR NOT EXISTS (
          SELECT 1 FROM retrieval_anchor_dispositions AS disposition
          WHERE disposition.anchor_id = OLD.anchor_id
            AND disposition.state = 'superseded'
            AND disposition.reason_class = 'correction'
            AND disposition.superseded_by = NEW.anchor_id
            AND disposition.sequence = (
                SELECT MAX(latest.sequence) FROM retrieval_anchor_dispositions AS latest
                WHERE latest.anchor_id = OLD.anchor_id
            )
      )
    BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor alias requires exact supersession');
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

fn schema_error(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation}: {}", message.into()),
        operation: operation.to_owned(),
    }
}

async fn table_columns(
    conn: &(impl Executor + Sync),
    table: &str,
    operation: &str,
) -> Result<BTreeSet<String>> {
    let mut rows = conn
        .query("SELECT name FROM pragma_table_xinfo(?1)", params![table])
        .await
        .map_err(|error| database_error(operation, error))?;
    let mut columns = BTreeSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| database_error(operation, error))?
    {
        columns.insert(
            row.get::<String>(0)
                .map_err(|error| database_error(operation, error))?,
        );
    }
    Ok(columns)
}

async fn validate_anchor_table_columns(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<()> {
    let required = [
        "anchor_id",
        "anchor_json",
        "owner_json",
        "projection_generation",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    let columns = table_columns(conn, "retrieval_anchors", operation).await?;
    if required.is_subset(&columns) {
        return Ok(());
    }
    Err(schema_error(
        operation,
        "retrieval_anchors is missing canonical anchor columns",
    ))
}

/// Installs the physical schema for immutable, owner-bound retrieval anchors.
///
/// The caller owns its local binding table (for example observation-to-anchor
/// or fact-evidence-to-anchor) and should invoke this before creating a table
/// with a composite foreign key to `retrieval_anchors(anchor_id, owner_json)`.
#[hotpath::measure(label = "runtime_core.db.anchor_schema_install")]
pub async fn install_retrieval_anchor_schema(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<()> {
    conn.execute_batch(ANCHORS_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    validate_anchor_table_columns(conn, operation).await?;
    conn.execute_batch(ALIASES_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    conn.execute_batch(AUTHORITY_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    conn.execute_batch(RETRIEVAL_ANCHOR_IMMUTABILITY_TRIGGERS_SQL)
        .await
        .map_err(|error| database_error(operation, error))
}

#[cfg(test)]
mod tests {
    use crate::db::engine::{Executor, QueryExecutor, TestConnection, params};

    use super::install_retrieval_anchor_schema;

    async fn connection() -> (tempfile::TempDir, TestConnection) {
        let directory = tempfile::tempdir().expect("create retrieval-anchor schema fixture");
        let connection = TestConnection::open(&directory.path().join("anchors.db"));
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .expect("enable foreign keys");
        (directory, connection)
    }

    async fn insert_anchor(conn: &TestConnection, owner: &str) {
        conn.execute(
            "INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES ('anchor-1', '{\"target\":\"fixture\"}', ?1, 'generation-1')",
            params![owner],
        )
        .await
        .expect("insert anchor");
    }

    #[tokio::test]
    async fn installs_owner_bound_aliases_and_immutable_records() {
        let (_directory, conn) = connection().await;
        install_retrieval_anchor_schema(&conn, "test retrieval-anchor schema")
            .await
            .expect("install schema");
        insert_anchor(&conn, "{\"owner\":\"one\"}").await;
        conn.execute(
            "INSERT INTO retrieval_anchor_aliases (
                owner_json, alias_kind, locator_digest, anchor_id
             ) VALUES (?1, 'fixture', 'digest-1', 'anchor-1')",
            params!["{\"owner\":\"one\"}"],
        )
        .await
        .expect("insert owner-bound alias");

        assert!(
            conn.execute(
                "INSERT INTO retrieval_anchor_aliases (
                    owner_json, alias_kind, locator_digest, anchor_id
                 ) VALUES (?1, 'fixture', 'digest-2', 'anchor-1')",
                params!["{\"owner\":\"other\"}"],
            )
            .await
            .is_err()
        );
        assert!(
            conn.execute(
                "UPDATE retrieval_anchors
                 SET projection_generation = 'generation-2'
                 WHERE anchor_id = 'anchor-1'",
                (),
            )
            .await
            .is_err()
        );
        assert!(
            conn.execute(
                "DELETE FROM retrieval_anchor_aliases WHERE anchor_id = 'anchor-1'",
                (),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn alias_target_promotion_requires_exact_current_supersession() {
        let (_directory, conn) = connection().await;
        install_retrieval_anchor_schema(&conn, "install alias transition fixture")
            .await
            .unwrap();
        conn.execute(
            "INSERT INTO retrieval_anchors(anchor_id, anchor_json, owner_json, projection_generation)
             VALUES ('old', '{}', '{}', 'g'), ('new', '{}', '{}', 'g'), ('other', '{}', '{}', 'g')", (),
        ).await.unwrap();
        conn.execute(
            "INSERT INTO retrieval_anchor_aliases(owner_json, alias_kind, locator_digest, anchor_id)
             VALUES ('{}', 'native', 'digest', 'old')", (),
        ).await.unwrap();
        let promote =
            "UPDATE retrieval_anchor_aliases SET anchor_id = 'new' WHERE anchor_id = 'old'";
        assert!(conn.execute(promote, ()).await.is_err());
        conn.execute(
            "INSERT INTO retrieval_anchor_dispositions(disposition_id, anchor_id, owner_json,
                state, superseded_by, reason_class, effective_at, record_json)
             VALUES ('wrong', 'old', '{}', 'superseded', 'other', 'correction', 1, '{}')",
            (),
        )
        .await
        .unwrap();
        assert!(conn.execute(promote, ()).await.is_err());
        conn.execute(
            "INSERT INTO retrieval_anchor_dispositions(disposition_id, anchor_id, owner_json,
                state, superseded_by, reason_class, effective_at, record_json)
             VALUES ('exact', 'old', '{}', 'superseded', 'new', 'correction', 2, '{}')",
            (),
        )
        .await
        .unwrap();
        assert!(conn.execute(
            "UPDATE retrieval_anchor_aliases SET anchor_id = 'new', locator_digest = 'changed' WHERE anchor_id = 'old'", (),
        ).await.is_err());
        assert_eq!(conn.execute(promote, ()).await.unwrap(), 1);
        assert!(
            conn.execute("DELETE FROM retrieval_anchor_aliases", ())
                .await
                .is_err()
        );
        install_retrieval_anchor_schema(&conn, "reopen promoted aliases")
            .await
            .unwrap();
        let mut rows = conn
            .query("SELECT anchor_id FROM retrieval_anchor_aliases", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn disposition_identity_is_owner_scoped() {
        let (_directory, conn) = connection().await;
        install_retrieval_anchor_schema(&conn, "test retrieval-anchor schema")
            .await
            .expect("install schema");
        conn.execute(
            "INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES
                ('anchor-one', '{\"target\":\"fixture\"}', '{\"owner\":\"one\"}', 'generation-1'),
                ('anchor-two', '{\"target\":\"fixture\"}', '{\"owner\":\"two\"}', 'generation-1')",
            (),
        )
        .await
        .expect("insert owner-scoped anchors");
        for (anchor_id, owner) in [
            ("anchor-one", "{\"owner\":\"one\"}"),
            ("anchor-two", "{\"owner\":\"two\"}"),
        ] {
            conn.execute(
                "INSERT INTO retrieval_anchor_dispositions (
                    disposition_id, anchor_id, owner_json, state, superseded_by,
                    reason_class, effective_at, record_json
                 ) VALUES ('shared-disposition', ?1, ?2, 'active', NULL,
                           'correction', 1, '{}')",
                params![anchor_id, owner],
            )
            .await
            .expect("insert owner-scoped disposition");
        }
        let mut rows = conn
            .query(
                "SELECT count(*) FROM retrieval_anchor_dispositions
                 WHERE disposition_id = 'shared-disposition'",
                (),
            )
            .await
            .expect("count owner-scoped dispositions");
        assert_eq!(
            rows.next()
                .await
                .expect("read disposition count")
                .expect("disposition count row")
                .get::<i64>(0)
                .expect("disposition count"),
            2
        );
    }
}
