use std::collections::BTreeSet;

use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use crate::configuration::FreshConfigurationStoreEvidence;
use crate::schema_contract::{
    SESSION_RELATION_RECEIPT_RECOVERY_COLUMNS, starts_with_ignore_ascii_case,
    validate_session_graph_publication_schema_contract,
    validate_session_relation_receipts_without_recovery_contract,
    validate_session_temporal_schema_contract,
};
use crate::{global_db_operation_error, global_db_operation_message};

use super::{
    MIGRATION_NAME, OPERATION, SESSION_TEMPORAL_AUTHORITY, SESSION_TEMPORAL_SCHEMA_VERSION, TEMPORAL_FTS_CONTRACTS,
    TEMPORAL_TABLE_COLUMNS,
};

const TEMPORAL_FTS_SHADOW_TABLES: &[&str] = &[
    "session_occurrences_fts_config",
    "session_occurrences_fts_content",
    "session_occurrences_fts_data",
    "session_occurrences_fts_docsize",
    "session_occurrences_fts_idx",
    "session_summary_nodes_fts_config",
    "session_summary_nodes_fts_content",
    "session_summary_nodes_fts_data",
    "session_summary_nodes_fts_docsize",
    "session_summary_nodes_fts_idx",
];

const SESSION_RELATION_RECEIPTS_TABLE: &str = "session_relation_receipts";

// `session_relation_receipts` as published by every v3 release and carried
// unchanged into the v4 stores persisted before receipt recovery added its
// columns.
const SESSION_RELATION_RECEIPTS_WITHOUT_RECOVERY_DIGEST: &str =
    "867dc83c80264f4b13aeab7f1ac51572a88ee5d614739a701ebddbb8dcb84a80";

/// Read-only admission result for the final session-temporal schema.
///
/// Non-final shapes, including every earlier version and the unreleased
/// pre-recovery receipt table, are not variants: admission returns
/// `ResetRequired` before any conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionTemporalSchemaAdmission {
    /// The persisted schema and its objects exactly match the final contract.
    Current,
    /// The registered store is proven empty and may receive the final contract.
    Fresh,
}

/// Classifies a store without changing its schema or retained session state.
#[hotpath::measure(future = true, label = "session_temporal.schema.admit")]
pub(crate) async fn require_admissible_session_temporal_schema(
    conn: &impl QueryExecutor,
    fresh_store: Option<&FreshConfigurationStoreEvidence>,
) -> tracedecay_domain::errors::Result<SessionTemporalSchemaAdmission> {
    let version = schema_version(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    match version {
        Some(SESSION_TEMPORAL_SCHEMA_VERSION) => {
            if session_relation_receipts_lack_recovery_columns(conn).await? {
                validate_without_receipt_recovery_session_temporal_schema(conn).await?;
                return Err(session_temporal_reset_required(
                    "session_relation_receipts carries the unreleased pre-recovery v4 shape; \
                     that shape never shipped as its own version and there is no sanctioned \
                     conversion, reset the session temporal authority to recreate the final schema",
                ));
            }
            validate_current_session_temporal_schema(conn).await?;
            Ok(SessionTemporalSchemaAdmission::Current)
        }
        Some(version) => Err(session_temporal_reset_required(format!(
            "persisted schema version {version} does not match final version {SESSION_TEMPORAL_SCHEMA_VERSION}"
        ))),
        None if fresh_store.is_some() => Ok(SessionTemporalSchemaAdmission::Fresh),
        None => Err(session_temporal_reset_required(
            "a nonempty store does not carry the final schema marker",
        )),
    }
}

pub(super) async fn validate_current_session_temporal_schema(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    let tables = TEMPORAL_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .filter(|table| !table.ends_with("_fts"))
        .collect::<Vec<_>>();
    validate_session_temporal_schema_contract(conn, &tables)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_namespace_and_fts(conn).await
}

/// True only when the persisted `session_relation_receipts` column list is
/// exactly the final list minus its trailing recovery columns. Every other
/// shape, including a partially added recovery set, is left for the final
/// contract to refuse with its precise typed reason.
async fn session_relation_receipts_lack_recovery_columns(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<bool> {
    let Some(expected) = TEMPORAL_TABLE_COLUMNS
        .iter()
        .find(|(table, _)| *table == SESSION_RELATION_RECEIPTS_TABLE)
        .and_then(|(_, columns)| columns.strip_suffix(SESSION_RELATION_RECEIPT_RECOVERY_COLUMNS))
    else {
        return Err(global_db_operation_message(
            OPERATION,
            "session relation receipt recovery columns are not the trailing contract columns",
        ));
    };
    let mut rows = conn
        .query(
            "SELECT name FROM pragma_table_info(?1) ORDER BY cid",
            params![SESSION_RELATION_RECEIPTS_TABLE],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut actual = Vec::with_capacity(expected.len());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        actual.push(
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        );
    }
    Ok(actual
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied()))
}

pub(super) async fn validate_without_receipt_recovery_session_temporal_schema(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    let tables = TEMPORAL_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .filter(|table| !table.ends_with("_fts") && *table != SESSION_RELATION_RECEIPTS_TABLE)
        .collect::<Vec<_>>();
    validate_session_temporal_schema_contract(conn, &tables)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_session_relation_receipts_without_recovery_contract(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_table_definition_digest(
        conn,
        SESSION_RELATION_RECEIPTS_TABLE,
        SESSION_RELATION_RECEIPTS_WITHOUT_RECOVERY_DIGEST,
    )
    .await?;
    validate_temporal_namespace_and_fts(conn).await
}

async fn validate_temporal_namespace_and_fts(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    validate_temporal_namespace_tables(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_session_graph_publication_schema_contract(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_fts_contracts(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_fts_match(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))
}

/// Pins a persisted CREATE TABLE definition by normalized digest so CHECK
/// expressions, which the PRAGMA contract cannot observe, are admitted exactly.
async fn validate_temporal_table_definition_digest(
    conn: &impl QueryExecutor,
    table: &str,
    expected_digest: &str,
) -> tracedecay_domain::errors::Result<()> {
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    else {
        return Err(session_temporal_reset_required(format!(
            "temporal table '{table}' is missing"
        )));
    };
    let sql = row
        .get::<String>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let digest = sha256_hex(normalize_schema_sql(&sql).as_bytes());
    if digest != expected_digest {
        return Err(session_temporal_reset_required(format!(
            "temporal table '{table}' has an incompatible CREATE TABLE contract"
        )));
    }
    Ok(())
}

async fn validate_temporal_namespace_tables(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    let expected = TEMPORAL_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .chain(TEMPORAL_FTS_SHADOW_TABLES.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let name = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if belongs_to_temporal_namespace(&name) && !expected.contains(name.as_str()) {
            return Err(global_db_operation_message(
                OPERATION,
                format!("unexpected session temporal table or view '{name}'"),
            ));
        }
    }
    Ok(())
}

fn belongs_to_temporal_namespace(name: &str) -> bool {
    [
        "session_agent_",
        "session_agents",
        "session_assertion",
        "session_current_entit",
        "session_derived_evidence",
        "session_external_payload",
        "session_logical_copy",
        "session_occurrence",
        "session_query_cursor",
        "session_refresh",
        "session_relation",
        "session_summary_availability",
        "session_summary_",
        "session_summary_nodes",
        "session_temporal",
        "session_thread",
        "session_turn",
    ]
    .iter()
    .any(|prefix| starts_with_ignore_ascii_case(name, prefix))
}

pub(super) fn session_temporal_reset_required(
    reason: impl Into<String>,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::reset_required(SESSION_TEMPORAL_AUTHORITY, reason)
}

pub(super) async fn validate_temporal_fts_contracts(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    for (table, expected_sql) in TEMPORAL_FTS_CONTRACTS {
        let mut rows = conn
            .query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![*table],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        else {
            return Err(global_db_operation_message(
                OPERATION,
                format!("temporal FTS table '{table}' is missing"),
            ));
        };
        let sql = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if normalize_fts_sql(&sql) != *expected_sql {
            return Err(global_db_operation_message(
                OPERATION,
                format!("table '{table}' has an incompatible temporal FTS contract"),
            ));
        }
    }
    Ok(())
}

fn normalize_fts_sql(sql: &str) -> String {
    normalize_schema_sql(sql)
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_whitespace() && *character != ';')
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .replace("ifnotexists", "")
}

pub(super) async fn validate_temporal_fts_match(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    for (table, _) in TEMPORAL_FTS_CONTRACTS {
        conn.query(
            &format!("SELECT rowid FROM {table} WHERE {table} MATCH ?1 LIMIT 1"),
            params!["__tracedecay_temporal_fts_probe__"],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }
    Ok(())
}

async fn schema_version(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<Option<i64>> {
    let mut tables = conn
        .query(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'session_temporal_schema_migrations'",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if tables
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .is_none()
    {
        return Ok(None);
    }

    let mut rows = conn
        .query(
            "SELECT name, version FROM session_temporal_schema_migrations ORDER BY name",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    else {
        return Err(global_db_operation_message(
            OPERATION,
            "session temporal schema marker is missing",
        ));
    };
    let name = row
        .get::<String>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let version = row
        .get::<i64>(1)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if name != MIGRATION_NAME
        || rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .is_some()
    {
        return Err(global_db_operation_message(
            OPERATION,
            "session temporal schema marker is not the exact final singleton",
        ));
    }
    Ok(Some(version))
}
