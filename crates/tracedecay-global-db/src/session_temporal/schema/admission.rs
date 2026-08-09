use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use crate::configuration::FreshConfigurationStoreEvidence;
use crate::{global_db_operation_error, global_db_operation_message};

use super::{
    MIGRATION_NAME, OPERATION, SESSION_TEMPORAL_AUTHORITY, SESSION_TEMPORAL_SCHEMA_VERSION,
    TEMPORAL_FTS_CONTRACTS, TEMPORAL_SCHEMA_DDL, TEMPORAL_TABLE_COLUMNS,
    validate_temporal_table_shapes,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct SchemaObject {
    object_type: String,
    table: String,
    sql: String,
}

type SchemaInventory = BTreeMap<String, SchemaObject>;

static EXPECTED_SESSION_TEMPORAL_SCHEMA: LazyLock<Result<SchemaInventory, String>> =
    LazyLock::new(build_expected_session_temporal_schema);

const GRAPH_PUBLICATION_TABLES: &[&str] = &[
    "graph_publication_replay_v1",
    "graph_publication_replay_dependencies_v1",
    "graph_publication_replay_tombstones_v1",
    "graph_publication_replay_tombstone_dependencies_v1",
    "graph_verified_heads_v1",
];

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionTemporalSchemaAdmission {
    /// The persisted schema and its objects exactly match the final contract.
    Current,
    /// The registered store is proven empty and may receive the final contract.
    Fresh,
}

/// Classifies a store without changing its schema or retained session state.
pub(crate) async fn require_admissible_session_temporal_schema(
    conn: &impl QueryExecutor,
    fresh_store: Option<&FreshConfigurationStoreEvidence>,
) -> tracedecay_runtime_core::errors::Result<SessionTemporalSchemaAdmission> {
    let version = schema_version(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    match version {
        Some(SESSION_TEMPORAL_SCHEMA_VERSION) => {
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

async fn validate_current_session_temporal_schema(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_session_temporal_schema_objects(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_table_shapes(conn)
        .await
        .map_err(|error| session_temporal_reset_required(error.to_string()))?;
    validate_temporal_namespace_tables(conn)
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
) -> tracedecay_runtime_core::errors::Result<()> {
    let expected = TEMPORAL_TABLE_COLUMNS
        .iter()
        .map(|(table, _)| *table)
        .chain(TEMPORAL_FTS_SHADOW_TABLES.iter().copied())
        .chain(GRAPH_PUBLICATION_TABLES.iter().copied())
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
        "lcm_summary_",
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
        "graph_publication_",
        "graph_verified_",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

fn build_expected_session_temporal_schema() -> Result<SchemaInventory, String> {
    let connection = rusqlite::Connection::open_in_memory()
        .map_err(|error| format!("failed to open canonical in-memory schema: {error}"))?;
    connection
        .execute_batch(TEMPORAL_SCHEMA_DDL)
        .map_err(|error| format!("failed to install canonical session temporal schema: {error}"))?;
    connection
        .execute_batch(tracedecay_rusqlite_runtime::repository::GRAPH_PUBLICATION_SCHEMA_V1)
        .map_err(|error| {
            format!("failed to install canonical graph publication schema: {error}")
        })?;
    read_rusqlite_inventory(&connection)
}

fn read_rusqlite_inventory(connection: &rusqlite::Connection) -> Result<SchemaInventory, String> {
    let mut statement = connection
        .prepare(
            "SELECT type, name, tbl_name, COALESCE(sql, '')
             FROM sqlite_master
             WHERE type IN ('table', 'index', 'trigger', 'view')
               AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .map_err(|error| format!("failed to prepare canonical schema inventory: {error}"))?;
    let rows = statement
        .query_map((), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| format!("failed to query canonical schema inventory: {error}"))?;
    let mut inventory = SchemaInventory::new();
    for row in rows {
        let (object_type, name, table, sql) =
            row.map_err(|error| format!("failed to read canonical schema object: {error}"))?;
        if inventory
            .insert(
                name.clone(),
                SchemaObject {
                    object_type,
                    table,
                    sql,
                },
            )
            .is_some()
        {
            return Err(format!("canonical schema repeats object '{name}'"));
        }
    }
    Ok(inventory)
}

async fn read_inventory(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<SchemaInventory> {
    let mut rows = conn
        .query(
            "SELECT type, name, tbl_name, COALESCE(sql, '')
             FROM sqlite_master
             WHERE type IN ('table', 'index', 'trigger', 'view')
               AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut inventory = SchemaInventory::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let object_type = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let name = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let table = row
            .get::<String>(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let sql = row
            .get::<String>(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if inventory
            .insert(
                name.clone(),
                SchemaObject {
                    object_type,
                    table,
                    sql,
                },
            )
            .is_some()
        {
            return Err(global_db_operation_message(
                OPERATION,
                format!("schema inventory repeats object '{name}'"),
            ));
        }
    }
    Ok(inventory)
}

pub(super) async fn validate_session_temporal_schema_objects(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    let actual = read_inventory(conn).await?;
    let expected = EXPECTED_SESSION_TEMPORAL_SCHEMA
        .as_ref()
        .map_err(|error| global_db_operation_message(OPERATION, error.clone()))?;
    for (name, expected_object) in expected {
        let Some(actual_object) = actual.get(name) else {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "session temporal schema is missing required {} '{name}'",
                    expected_object.object_type
                ),
            ));
        };
        if actual_object != expected_object {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "session temporal schema has incompatible {} '{name}'",
                    expected_object.object_type
                ),
            ));
        }
    }
    Ok(())
}

fn session_temporal_reset_required(
    reason: impl Into<String>,
) -> tracedecay_runtime_core::errors::TraceDecayError {
    tracedecay_runtime_core::errors::TraceDecayError::reset_required(
        SESSION_TEMPORAL_AUTHORITY,
        reason,
    )
}

pub(super) async fn validate_temporal_fts_contracts(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
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
    sql.chars()
        .filter(|character| !character.is_whitespace() && *character != ';')
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .replace("ifnotexists", "")
}

pub(super) async fn validate_temporal_fts_match(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
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
) -> tracedecay_runtime_core::errors::Result<Option<i64>> {
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
