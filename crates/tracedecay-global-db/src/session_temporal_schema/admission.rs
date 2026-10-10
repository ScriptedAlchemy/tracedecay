use std::collections::BTreeSet;

use tracedecay_runtime_core::db::engine::{BackendKind, QueryExecutor, params};

use crate::configuration::FreshConfigurationStoreEvidence;
use crate::registered::RefusedAuthorityV1;
use crate::schema_contract::{
    starts_with_ignore_ascii_case, validate_session_graph_publication_schema_contract,
    validate_session_temporal_schema_contract,
};
use crate::{global_db_operation_error, global_db_operation_message};
use tracedecay_runtime_core::db::native_search::{
    SearchIndex, normalize_schema_sql as normalize_native_schema_sql,
};

use super::{
    MIGRATION_NAME, OPERATION, SESSION_TEMPORAL_AUTHORITY, SESSION_TEMPORAL_SCHEMA_VERSION,
    TEMPORAL_FTS_CONTRACTS, TEMPORAL_TABLE_COLUMNS,
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

/// Read-only admission result for the final session-temporal schema.
///
/// Non-final shapes holding session rows are not variants: admission refuses
/// them with a typed reset before any conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionTemporalSchemaAdmission {
    /// The persisted schema and its objects exactly match the final contract.
    Current,
    /// The registered store is proven empty and may receive the final contract.
    Fresh,
    /// An earlier version whose authority holds no rows, as in a store that
    /// installs every registered schema but never records sessions. Nothing
    /// is converted: its objects are dropped and the final contract installed.
    EmptyEarlier,
}

/// Classifies a store without changing its schema or retained session state.
/// Another recorded version over session rows is the scoped refusal of the
/// authority; every other non-final shape is a hard typed reset.
#[tracing::instrument(name = "session_temporal.schema.admit", level = "trace", skip_all)]
pub(crate) async fn require_admissible_session_temporal_schema(
    conn: &impl QueryExecutor,
    fresh_store: Option<&FreshConfigurationStoreEvidence>,
) -> tracedecay_domain::errors::Result<Result<SessionTemporalSchemaAdmission, RefusedAuthorityV1>> {
    let version = schema_version(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    match version {
        Some(SESSION_TEMPORAL_SCHEMA_VERSION) => {
            validate_current_session_temporal_schema(conn).await?;
            Ok(Ok(SessionTemporalSchemaAdmission::Current))
        }
        Some(version)
            if version < SESSION_TEMPORAL_SCHEMA_VERSION
                && authority_holds_no_rows(conn).await? =>
        {
            Ok(Ok(SessionTemporalSchemaAdmission::EmptyEarlier))
        }
        Some(version) => Ok(Err(RefusedAuthorityV1::Version {
            component: SESSION_TEMPORAL_AUTHORITY,
            found_version: Some(version),
            required_version: SESSION_TEMPORAL_SCHEMA_VERSION,
        })),
        None if fresh_store.is_some() => Ok(Ok(SessionTemporalSchemaAdmission::Fresh)),
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

async fn validate_temporal_namespace_tables(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<()> {
    let expected = TEMPORAL_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .filter(|table| conn.backend_kind() == BackendKind::Sqlite || !table.ends_with("_fts"))
        .chain(
            TEMPORAL_FTS_SHADOW_TABLES
                .iter()
                .copied()
                .filter(|_| conn.backend_kind() == BackendKind::Sqlite),
        )
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
    if conn.backend_kind() == BackendKind::NativeTurso {
        for index in [SearchIndex::Occurrence, SearchIndex::Summary] {
            let mut rows = conn.query(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1 AND tbl_name = ?2",
                params![index.index_name(), index.table_name()],
            ).await.map_err(|error| global_db_operation_error(OPERATION, error))?;
            let Some(row) = rows
                .next()
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?
            else {
                return Err(global_db_operation_message(
                    OPERATION,
                    format!("native search index '{}' is missing", index.index_name()),
                ));
            };
            let sql: String = row
                .get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            if normalize_native_schema_sql(&sql) != normalize_native_schema_sql(index.create_sql())
            {
                return Err(global_db_operation_message(
                    OPERATION,
                    format!(
                        "native search index '{}' has an incompatible contract",
                        index.index_name()
                    ),
                ));
            }
        }
        return Ok(());
    }
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
    if conn.backend_kind() == BackendKind::NativeTurso {
        for index in [SearchIndex::Occurrence, SearchIndex::Summary] {
            conn.query(
                &format!(
                    "SELECT rowid FROM {} WHERE fts_match({}, ?1) LIMIT 1",
                    index.table_name(),
                    index.columns().join(", ")
                ),
                params!["__tracedecay_temporal_fts_probe__"],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        }
        return Ok(());
    }
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

/// Whether every row table of the authority is absent or empty. FTS tables
/// index their content tables, and the version marker is not session state.
pub(super) async fn authority_holds_no_rows(
    conn: &impl QueryExecutor,
) -> tracedecay_domain::errors::Result<bool> {
    let mut existing = BTreeSet::new();
    let mut tables = conn
        .query("SELECT name FROM sqlite_master WHERE type = 'table'", ())
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    while let Some(row) = tables
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        existing.insert(
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        );
    }
    drop(tables);
    for (table, _) in TEMPORAL_TABLE_COLUMNS {
        if *table == "session_temporal_schema_migrations"
            || table.ends_with("_fts")
            || !existing.contains(*table)
        {
            continue;
        }
        let mut rows = conn
            .query(&format!("SELECT 1 FROM {table} LIMIT 1"), ())
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            .is_some()
        {
            return Ok(false);
        }
    }
    Ok(true)
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
