//! Canonical physical schema shared by fact and observation retrieval anchors.
//!
//! An anchor is immutable evidence, while its observation/fact binding remains
//! local to the physical store that owns the referenced record.  Keeping this
//! small schema here prevents those stores from drifting into competing anchor
//! identities.

use std::collections::BTreeSet;

use libsql::{Connection, params};

use crate::errors::{Result, TraceDecayError};

const ALIASES_TABLE: &str = "retrieval_anchor_aliases";
const LEGACY_ALIASES_TABLE: &str = "retrieval_anchor_aliases_owner_unbound_v1";

const ANCHORS_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS retrieval_anchors (
        anchor_id TEXT PRIMARY KEY CHECK(length(anchor_id) > 0),
        anchor_json TEXT NOT NULL CHECK(json_valid(anchor_json)),
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        projection_generation TEXT NOT NULL CHECK(length(projection_generation) > 0)
    );
    -- SQLite requires an exact unique parent key for the composite owner-bound
    -- alias and evidence foreign keys, even though anchor_id is itself unique.
    CREATE UNIQUE INDEX IF NOT EXISTS idx_retrieval_anchors_owner
        ON retrieval_anchors(anchor_id, owner_json);
";

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
        disposition_id TEXT NOT NULL UNIQUE CHECK(length(disposition_id) > 0),
        anchor_id TEXT NOT NULL,
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        state TEXT NOT NULL
            CHECK(state IN ('active', 'superseded', 'deleted', 'unavailable')),
        superseded_by TEXT,
        reason_class TEXT NOT NULL CHECK(reason_class IN (
            'user_request', 'retention', 'redaction', 'quarantine',
            'correction', 'legal_hold', 'source_unavailable'
        )),
        effective_at INTEGER NOT NULL,
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
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

fn schema_error(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation}: {}", message.into()),
        operation: operation.to_owned(),
    }
}

async fn table_exists(conn: &Connection, table: &str, operation: &str) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
        )
        .await
        .map_err(|error| database_error(operation, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| database_error(operation, error))
}

async fn table_columns(
    conn: &Connection,
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

async fn aliases_have_owner_bound_foreign_key(conn: &Connection, operation: &str) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT \"from\", \"to\"
             FROM pragma_foreign_key_list('retrieval_anchor_aliases')
             WHERE \"table\" = 'retrieval_anchors'
             ORDER BY id, seq",
            (),
        )
        .await
        .map_err(|error| database_error(operation, error))?;
    let mut columns = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| database_error(operation, error))?
    {
        columns.push((
            row.get::<String>(0)
                .map_err(|error| database_error(operation, error))?,
            row.get::<String>(1)
                .map_err(|error| database_error(operation, error))?,
        ));
    }
    Ok(columns
        == [
            ("anchor_id".to_owned(), "anchor_id".to_owned()),
            ("owner_json".to_owned(), "owner_json".to_owned()),
        ])
}

async fn validate_alias_table_columns(
    conn: &Connection,
    table: &str,
    operation: &str,
) -> Result<()> {
    let expected = ["owner_json", "alias_kind", "locator_digest", "anchor_id"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let columns = table_columns(conn, table, operation).await?;
    if columns == expected {
        return Ok(());
    }
    Err(schema_error(
        operation,
        format!("{table} has unsupported columns: {columns:?}"),
    ))
}

async fn validate_anchor_table_columns(conn: &Connection, operation: &str) -> Result<()> {
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

async fn validate_legacy_alias_ownership(conn: &Connection, operation: &str) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT aliases.anchor_id
             FROM retrieval_anchor_aliases_owner_unbound_v1 AS aliases
             LEFT JOIN retrieval_anchors AS anchors
               ON anchors.anchor_id = aliases.anchor_id
              AND anchors.owner_json = aliases.owner_json
             WHERE anchors.anchor_id IS NULL
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| database_error(operation, error))?;
    if rows
        .next()
        .await
        .map_err(|error| database_error(operation, error))?
        .is_some()
    {
        return Err(schema_error(
            operation,
            "legacy retrieval-anchor alias has no anchor with the same owner",
        ));
    }
    Ok(())
}

async fn validate_alias_copy_conflicts(conn: &Connection, operation: &str) -> Result<()> {
    for sql in [
        "SELECT 1
         FROM retrieval_anchor_aliases_owner_unbound_v1 AS legacy
         JOIN retrieval_anchor_aliases AS current
           ON current.owner_json = legacy.owner_json
          AND current.alias_kind = legacy.alias_kind
          AND current.locator_digest = legacy.locator_digest
         WHERE current.anchor_id <> legacy.anchor_id
         LIMIT 1",
        "SELECT 1
         FROM retrieval_anchor_aliases_owner_unbound_v1 AS legacy
         JOIN retrieval_anchor_aliases AS current
           ON current.anchor_id = legacy.anchor_id
          AND current.alias_kind = legacy.alias_kind
          AND current.locator_digest = legacy.locator_digest
         WHERE current.owner_json <> legacy.owner_json
         LIMIT 1",
    ] {
        let mut rows = conn
            .query(sql, ())
            .await
            .map_err(|error| database_error(operation, error))?;
        if rows
            .next()
            .await
            .map_err(|error| database_error(operation, error))?
            .is_some()
        {
            return Err(schema_error(
                operation,
                "retrieval-anchor alias migration conflicts with canonical aliases",
            ));
        }
    }
    Ok(())
}

async fn restore_legacy_aliases(conn: &Connection, operation: &str) -> Result<()> {
    if !table_exists(conn, LEGACY_ALIASES_TABLE, operation).await? {
        return Ok(());
    }
    validate_alias_table_columns(conn, LEGACY_ALIASES_TABLE, operation).await?;
    validate_legacy_alias_ownership(conn, operation).await?;
    validate_alias_copy_conflicts(conn, operation).await?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO retrieval_anchor_aliases (
             owner_json, alias_kind, locator_digest, anchor_id
         )
         SELECT owner_json, alias_kind, locator_digest, anchor_id
         FROM retrieval_anchor_aliases_owner_unbound_v1;",
    )
    .await
    .map_err(|error| database_error(operation, error))?;

    let mut rows = conn
        .query(
            "SELECT 1
             FROM retrieval_anchor_aliases_owner_unbound_v1 AS legacy
             LEFT JOIN retrieval_anchor_aliases AS current
               ON current.owner_json = legacy.owner_json
              AND current.alias_kind = legacy.alias_kind
              AND current.locator_digest = legacy.locator_digest
              AND current.anchor_id = legacy.anchor_id
             WHERE current.anchor_id IS NULL
             LIMIT 1",
            (),
        )
        .await
        .map_err(|error| database_error(operation, error))?;
    if rows
        .next()
        .await
        .map_err(|error| database_error(operation, error))?
        .is_some()
    {
        return Err(schema_error(
            operation,
            "retrieval-anchor alias migration did not preserve every legacy row",
        ));
    }
    drop(rows);
    conn.execute_batch("DROP TABLE retrieval_anchor_aliases_owner_unbound_v1;")
        .await
        .map(|_| ())
        .map_err(|error| database_error(operation, error))
}

async fn upgrade_aliases_if_needed(conn: &Connection, operation: &str) -> Result<()> {
    let aliases_exist = table_exists(conn, ALIASES_TABLE, operation).await?;
    let legacy_exists = table_exists(conn, LEGACY_ALIASES_TABLE, operation).await?;
    if aliases_exist && !aliases_have_owner_bound_foreign_key(conn, operation).await? {
        if legacy_exists {
            return Err(schema_error(
                operation,
                "both legacy and noncanonical retrieval-anchor alias tables exist",
            ));
        }
        validate_alias_table_columns(conn, ALIASES_TABLE, operation).await?;
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS retrieval_anchor_aliases_immutable_update;
             DROP TRIGGER IF EXISTS retrieval_anchor_aliases_immutable_delete;
             DROP TRIGGER IF EXISTS retrieval_anchor_aliases_no_update;
             DROP TRIGGER IF EXISTS retrieval_anchor_aliases_no_delete;
             ALTER TABLE retrieval_anchor_aliases
             RENAME TO retrieval_anchor_aliases_owner_unbound_v1;",
        )
        .await
        .map_err(|error| database_error(operation, error))?;
    }

    conn.execute_batch(ALIASES_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    restore_legacy_aliases(conn, operation).await
}

/// Installs the physical schema for immutable, owner-bound retrieval anchors.
///
/// The caller owns its local binding table (for example observation-to-anchor
/// or fact-evidence-to-anchor) and should invoke this before creating a table
/// with a composite foreign key to `retrieval_anchors(anchor_id, owner_json)`.
/// Existing one-column alias foreign keys are upgraded with a resumable,
/// validated copy; conflicting or ownerless rows are retained and reported
/// rather than discarded.
pub(crate) async fn install_retrieval_anchor_schema(
    conn: &Connection,
    operation: &str,
) -> Result<()> {
    conn.execute_batch(ANCHORS_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    validate_anchor_table_columns(conn, operation).await?;
    upgrade_aliases_if_needed(conn, operation).await?;
    conn.execute_batch(AUTHORITY_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS retrieval_anchors_no_update;
         DROP TRIGGER IF EXISTS retrieval_anchors_no_delete;
         DROP TRIGGER IF EXISTS retrieval_anchor_aliases_no_update;
         DROP TRIGGER IF EXISTS retrieval_anchor_aliases_no_delete;",
    )
    .await
    .map_err(|error| database_error(operation, error))?;
    conn.execute_batch(IMMUTABILITY_TRIGGERS)
        .await
        .map(|_| ())
        .map_err(|error| database_error(operation, error))
}

#[cfg(test)]
mod tests {
    use libsql::{Builder, params};

    use super::install_retrieval_anchor_schema;

    async fn connection() -> (tempfile::TempDir, libsql::Connection) {
        let directory = tempfile::tempdir().expect("create retrieval-anchor schema fixture");
        let database = Builder::new_local(directory.path().join("anchors.db"))
            .build()
            .await
            .expect("open retrieval-anchor schema fixture");
        let connection = database
            .connect()
            .expect("connect retrieval-anchor schema fixture");
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .await
            .expect("enable foreign keys");
        (directory, connection)
    }

    async fn insert_anchor(conn: &libsql::Connection, owner: &str) {
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
    async fn upgrades_legacy_aliases_without_losing_rows() {
        let (_directory, conn) = connection().await;
        conn.execute_batch(
            "CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                anchor_json TEXT NOT NULL,
                owner_json TEXT NOT NULL,
                projection_generation TEXT NOT NULL
             );
             CREATE TABLE retrieval_anchor_aliases (
                owner_json TEXT NOT NULL,
                alias_kind TEXT NOT NULL,
                locator_digest TEXT NOT NULL,
                anchor_id TEXT NOT NULL,
                PRIMARY KEY(owner_json, alias_kind, locator_digest),
                UNIQUE(anchor_id, alias_kind, locator_digest),
                FOREIGN KEY(anchor_id) REFERENCES retrieval_anchors(anchor_id)
             );",
        )
        .await
        .expect("create legacy schema");
        insert_anchor(&conn, "{\"owner\":\"one\"}").await;
        conn.execute(
            "INSERT INTO retrieval_anchor_aliases (
                owner_json, alias_kind, locator_digest, anchor_id
             ) VALUES (?1, 'fixture', 'digest-1', 'anchor-1')",
            params!["{\"owner\":\"one\"}"],
        )
        .await
        .expect("insert legacy alias");

        install_retrieval_anchor_schema(&conn, "upgrade retrieval-anchor schema")
            .await
            .expect("upgrade schema");
        install_retrieval_anchor_schema(&conn, "upgrade retrieval-anchor schema")
            .await
            .expect("replay upgrade");

        let mut rows = conn
            .query("SELECT count(*) FROM retrieval_anchor_aliases", ())
            .await
            .expect("count aliases");
        let count = rows
            .next()
            .await
            .expect("read alias count")
            .expect("alias count row")
            .get::<i64>(0)
            .expect("decode alias count");
        assert_eq!(count, 1);
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
    }
}
