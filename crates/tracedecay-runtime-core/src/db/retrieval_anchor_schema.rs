//! Canonical physical schema shared by fact and observation retrieval anchors.
//!
//! An anchor is immutable evidence, while its observation/fact binding remains
//! local to the physical store that owns the referenced record.  Keeping this
//! small schema here prevents those stores from drifting into competing anchor
//! identities.

use std::collections::BTreeSet;

use crate::db::engine::{Executor, params};
use crate::errors::{Result, TraceDecayError};

const ALIASES_TABLE: &str = "retrieval_anchor_aliases";
const LEGACY_ALIASES_TABLE: &str = "retrieval_anchor_aliases_owner_unbound_v1";
const DISPOSITIONS_TABLE: &str = "retrieval_anchor_dispositions";
const LEGACY_DISPOSITIONS_TABLE: &str = "retrieval_anchor_dispositions_terminal_v0";

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

fn schema_error(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation}: {}", message.into()),
        operation: operation.to_owned(),
    }
}

async fn table_exists(conn: &(impl Executor + Sync), table: &str, operation: &str) -> Result<bool> {
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

async fn aliases_have_owner_bound_foreign_key(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<bool> {
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
    conn: &(impl Executor + Sync),
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

async fn validate_legacy_alias_ownership(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<()> {
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

async fn validate_alias_copy_conflicts(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<()> {
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

async fn restore_legacy_aliases(conn: &(impl Executor + Sync), operation: &str) -> Result<()> {
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
        .map_err(|error| database_error(operation, error))
}

async fn upgrade_aliases_if_needed(conn: &(impl Executor + Sync), operation: &str) -> Result<()> {
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

async fn dispositions_support_terminal_states(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![DISPOSITIONS_TABLE],
        )
        .await
        .map_err(|error| database_error(operation, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| database_error(operation, error))?
    else {
        return Ok(false);
    };
    let sql = row
        .get::<String>(0)
        .map_err(|error| database_error(operation, error))?;
    Ok([
        "'active'",
        "'superseded'",
        "'redacted'",
        "'expired'",
        "'quarantined'",
        "'deleted'",
        "'unavailable'",
    ]
    .iter()
    .all(|state| sql.contains(state)))
}

async fn dispositions_have_owner_bound_identity(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_index_list(?1) WHERE \"unique\" = 1 ORDER BY seq",
            params![DISPOSITIONS_TABLE],
        )
        .await
        .map_err(|error| database_error(operation, error))?;
    let mut indexes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| database_error(operation, error))?
    {
        indexes.push(
            row.get::<String>(0)
                .map_err(|error| database_error(operation, error))?,
        );
    }
    drop(rows);
    for index in indexes {
        let mut columns = conn
            .query(
                "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
                params![index],
            )
            .await
            .map_err(|error| database_error(operation, error))?;
        let mut names = Vec::new();
        while let Some(row) = columns
            .next()
            .await
            .map_err(|error| database_error(operation, error))?
        {
            names.push(
                row.get::<String>(0)
                    .map_err(|error| database_error(operation, error))?,
            );
        }
        if names.len() == 2 && names[0] == "owner_json" && names[1] == "disposition_id" {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn validate_disposition_table_columns(
    conn: &(impl Executor + Sync),
    table: &str,
    operation: &str,
) -> Result<()> {
    let expected = [
        "sequence",
        "disposition_id",
        "anchor_id",
        "owner_json",
        "state",
        "superseded_by",
        "reason_class",
        "effective_at",
        "record_json",
    ]
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

async fn restore_legacy_dispositions(conn: &(impl Executor + Sync), operation: &str) -> Result<()> {
    if !table_exists(conn, LEGACY_DISPOSITIONS_TABLE, operation).await? {
        return Ok(());
    }
    validate_disposition_table_columns(conn, LEGACY_DISPOSITIONS_TABLE, operation).await?;
    validate_disposition_rows(conn, LEGACY_DISPOSITIONS_TABLE, operation).await?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO retrieval_anchor_dispositions (
             sequence, disposition_id, anchor_id, owner_json, state, superseded_by,
             reason_class, effective_at, record_json
         )
         SELECT sequence, disposition_id, anchor_id, owner_json, state, superseded_by,
                reason_class, effective_at, record_json
         FROM retrieval_anchor_dispositions_terminal_v0;",
    )
    .await
    .map_err(|error| database_error(operation, error))?;

    let mut rows = conn
        .query(
            "SELECT 1
             FROM retrieval_anchor_dispositions_terminal_v0 AS legacy
             LEFT JOIN retrieval_anchor_dispositions AS current
               ON current.sequence = legacy.sequence
              AND current.disposition_id = legacy.disposition_id
              AND current.anchor_id = legacy.anchor_id
              AND current.owner_json = legacy.owner_json
              AND current.state = legacy.state
              AND current.superseded_by IS legacy.superseded_by
              AND current.reason_class = legacy.reason_class
              AND current.effective_at = legacy.effective_at
              AND current.record_json = legacy.record_json
             WHERE current.sequence IS NULL
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
            "retrieval-anchor disposition migration did not preserve every legacy row",
        ));
    }
    drop(rows);
    conn.execute_batch("DROP TABLE retrieval_anchor_dispositions_terminal_v0;")
        .await
        .map_err(|error| database_error(operation, error))
}

async fn validate_disposition_rows(
    conn: &(impl Executor + Sync),
    table: &str,
    operation: &str,
) -> Result<()> {
    let sql = match table {
        DISPOSITIONS_TABLE => {
            "SELECT disposition_id, anchor_id, owner_json, state, superseded_by,
                    reason_class, effective_at, record_json
             FROM retrieval_anchor_dispositions
             ORDER BY sequence"
        }
        LEGACY_DISPOSITIONS_TABLE => {
            "SELECT disposition_id, anchor_id, owner_json, state, superseded_by,
                    reason_class, effective_at, record_json
             FROM retrieval_anchor_dispositions_terminal_v0
             ORDER BY sequence"
        }
        _ => {
            return Err(schema_error(
                operation,
                "unsupported retrieval-anchor disposition table",
            ));
        }
    };
    let mut rows = conn
        .query(sql, ())
        .await
        .map_err(|error| database_error(operation, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| database_error(operation, error))?
    {
        let disposition_id = row
            .get::<String>(0)
            .map_err(|error| database_error(operation, error))?;
        let anchor_id = row
            .get::<String>(1)
            .map_err(|error| database_error(operation, error))?;
        let owner_json = row
            .get::<String>(2)
            .map_err(|error| database_error(operation, error))?;
        let state = row
            .get::<String>(3)
            .map_err(|error| database_error(operation, error))?;
        let superseded_by = row
            .get::<Option<String>>(4)
            .map_err(|error| database_error(operation, error))?;
        let reason_class = row
            .get::<String>(5)
            .map_err(|error| database_error(operation, error))?;
        let effective_at = row
            .get::<i64>(6)
            .map_err(|error| database_error(operation, error))?;
        let record_json = row
            .get::<String>(7)
            .map_err(|error| database_error(operation, error))?;
        let record = serde_json::from_str::<tracedecay_store::RetrievalAnchorDispositionRecordV1>(
            &record_json,
        )
        .map_err(|error| database_error(operation, error))?;
        record
            .validate()
            .map_err(|error| database_error(operation, error))?;
        let canonical_owner = serde_json::to_string(record.owner())
            .map_err(|error| database_error(operation, error))?;
        if record.disposition_id() != disposition_id
            || record.anchor_id().as_str() != anchor_id
            || canonical_owner != owner_json
            || record.state().as_str() != state
            || record
                .superseded_by()
                .map(tracedecay_domain::RetrievalAnchorId::as_str)
                != superseded_by.as_deref()
            || record.reason_class().as_str() != reason_class
            || record.effective_at().0 != effective_at
        {
            return Err(schema_error(
                operation,
                "legacy retrieval-anchor disposition record does not match its indexed columns",
            ));
        }
    }
    Ok(())
}

async fn upgrade_dispositions_if_needed(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<()> {
    let current_exists = table_exists(conn, DISPOSITIONS_TABLE, operation).await?;
    let legacy_exists = table_exists(conn, LEGACY_DISPOSITIONS_TABLE, operation).await?;
    if current_exists {
        validate_disposition_table_columns(conn, DISPOSITIONS_TABLE, operation).await?;
    }
    if current_exists
        && (!dispositions_support_terminal_states(conn, operation).await?
            || !dispositions_have_owner_bound_identity(conn, operation).await?)
    {
        if legacy_exists {
            return Err(schema_error(
                operation,
                "both legacy and noncanonical retrieval-anchor disposition tables exist",
            ));
        }
        validate_disposition_rows(conn, DISPOSITIONS_TABLE, operation).await?;
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS retrieval_anchor_dispositions_immutable_update;
             DROP TRIGGER IF EXISTS retrieval_anchor_dispositions_immutable_delete;
             DROP INDEX IF EXISTS idx_retrieval_anchor_dispositions_current;
             ALTER TABLE retrieval_anchor_dispositions
             RENAME TO retrieval_anchor_dispositions_terminal_v0;",
        )
        .await
        .map_err(|error| database_error(operation, error))?;
    }

    conn.execute_batch(AUTHORITY_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    restore_legacy_dispositions(conn, operation).await
}

/// Installs the physical schema for immutable, owner-bound retrieval anchors.
///
/// The caller owns its local binding table (for example observation-to-anchor
/// or fact-evidence-to-anchor) and should invoke this before creating a table
/// with a composite foreign key to `retrieval_anchors(anchor_id, owner_json)`.
/// Existing one-column alias foreign keys are upgraded with a resumable,
/// validated copy; conflicting or ownerless rows are retained and reported
/// rather than discarded.
pub async fn install_retrieval_anchor_schema(
    conn: &(impl Executor + Sync),
    operation: &str,
) -> Result<()> {
    conn.execute_batch(ANCHORS_SCHEMA)
        .await
        .map_err(|error| database_error(operation, error))?;
    validate_anchor_table_columns(conn, operation).await?;
    upgrade_aliases_if_needed(conn, operation).await?;
    upgrade_dispositions_if_needed(conn, operation).await?;
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
        .map_err(|error| database_error(operation, error))
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{FactOwnerV1, ProjectId, RetrievalAnchorId, UtcMicros};
    use tracedecay_store::{
        AnchorDispositionReasonClassV1, AnchorDispositionStateV1,
        RetrievalAnchorDispositionRecordV1,
    };

    use crate::db::engine::{Executor, QueryExecutor, TestConnection, params};

    use super::{ANCHORS_SCHEMA, install_retrieval_anchor_schema};

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

    #[tokio::test]
    async fn widens_legacy_dispositions_without_losing_rows() {
        let (_directory, conn) = connection().await;
        let owner = FactOwnerV1::Project {
            project_id: ProjectId::new("project.fixture").expect("project id"),
        };
        let owner_json = serde_json::to_string(&owner).expect("serialize owner");
        let disposition = RetrievalAnchorDispositionRecordV1::new(
            "disposition-1",
            RetrievalAnchorId::new("anchor-1").expect("anchor id"),
            owner,
            AnchorDispositionStateV1::Active,
            None,
            AnchorDispositionReasonClassV1::Correction,
            UtcMicros(1),
        )
        .expect("legacy disposition");
        let disposition_json =
            serde_json::to_string(&disposition).expect("serialize legacy disposition");
        conn.execute_batch(ANCHORS_SCHEMA)
            .await
            .expect("install anchor schema");
        insert_anchor(&conn, &owner_json).await;
        conn.execute_batch(
            "CREATE TABLE retrieval_anchor_dispositions (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                disposition_id TEXT NOT NULL UNIQUE,
                anchor_id TEXT NOT NULL,
                owner_json TEXT NOT NULL,
                state TEXT NOT NULL CHECK(
                    state IN ('active', 'superseded', 'deleted', 'unavailable')
                ),
                superseded_by TEXT,
                reason_class TEXT NOT NULL,
                effective_at INTEGER NOT NULL,
                record_json TEXT NOT NULL
             );",
        )
        .await
        .expect("create legacy disposition schema");
        conn.execute(
            "INSERT INTO retrieval_anchor_dispositions (
                disposition_id, anchor_id, owner_json, state, superseded_by,
                reason_class, effective_at, record_json
             ) VALUES (
                'disposition-1', 'anchor-1', ?1, 'active', NULL,
                'correction', 1, ?2
             )",
            params![owner_json, disposition_json],
        )
        .await
        .expect("insert legacy disposition");

        install_retrieval_anchor_schema(&conn, "upgrade retrieval-anchor dispositions")
            .await
            .expect("upgrade disposition schema");
        install_retrieval_anchor_schema(&conn, "upgrade retrieval-anchor dispositions")
            .await
            .expect("replay disposition upgrade");
        conn.execute(
            "INSERT INTO retrieval_anchor_dispositions (
                disposition_id, anchor_id, owner_json, state, superseded_by,
                reason_class, effective_at, record_json
             ) VALUES (
                'disposition-2', 'anchor-1', ?1, 'redacted', NULL,
                'redaction', 2, '{}'
             )",
            params![serde_json::to_string(disposition.owner()).expect("serialize owner")],
        )
        .await
        .expect("insert evolved disposition");

        let mut rows = conn
            .query("SELECT count(*) FROM retrieval_anchor_dispositions", ())
            .await
            .expect("count dispositions");
        assert_eq!(
            rows.next()
                .await
                .expect("read disposition count")
                .expect("disposition count row")
                .get::<i64>(0)
                .expect("decode disposition count"),
            2
        );
    }

    #[tokio::test]
    async fn refuses_mismatched_legacy_disposition_before_renaming_it() {
        let (_directory, conn) = connection().await;
        let owner = FactOwnerV1::Project {
            project_id: ProjectId::new("project.fixture").expect("project id"),
        };
        let owner_json = serde_json::to_string(&owner).expect("serialize owner");
        let disposition = RetrievalAnchorDispositionRecordV1::new(
            "disposition-1",
            RetrievalAnchorId::new("anchor-1").expect("anchor id"),
            owner,
            AnchorDispositionStateV1::Deleted,
            None,
            AnchorDispositionReasonClassV1::UserRequest,
            UtcMicros(1),
        )
        .expect("legacy disposition");
        conn.execute_batch(ANCHORS_SCHEMA)
            .await
            .expect("install anchor schema");
        insert_anchor(&conn, &owner_json).await;
        conn.execute_batch(
            "CREATE TABLE retrieval_anchor_dispositions (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                disposition_id TEXT NOT NULL UNIQUE,
                anchor_id TEXT NOT NULL,
                owner_json TEXT NOT NULL,
                state TEXT NOT NULL CHECK(
                    state IN ('active', 'superseded', 'deleted', 'unavailable')
                ),
                superseded_by TEXT,
                reason_class TEXT NOT NULL,
                effective_at INTEGER NOT NULL,
                record_json TEXT NOT NULL
             );",
        )
        .await
        .expect("create legacy disposition schema");
        conn.execute(
            "INSERT INTO retrieval_anchor_dispositions (
                disposition_id, anchor_id, owner_json, state, superseded_by,
                reason_class, effective_at, record_json
             ) VALUES (
                'disposition-1', 'anchor-1', ?1, 'active', NULL,
                'user_request', 1, ?2
             )",
            params![
                owner_json,
                serde_json::to_string(&disposition).expect("serialize disposition")
            ],
        )
        .await
        .expect("insert mismatched disposition");

        assert!(
            install_retrieval_anchor_schema(&conn, "reject invalid disposition migration")
                .await
                .is_err()
        );
        assert!(
            super::table_exists(
                &conn,
                super::DISPOSITIONS_TABLE,
                "inspect rejected migration"
            )
            .await
            .expect("inspect current table")
        );
        assert!(
            !super::table_exists(
                &conn,
                super::LEGACY_DISPOSITIONS_TABLE,
                "inspect rejected migration"
            )
            .await
            .expect("inspect legacy table")
        );
    }
}
