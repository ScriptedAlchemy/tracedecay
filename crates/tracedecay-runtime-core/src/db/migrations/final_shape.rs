//! Query-only admission for the exact relational shape created by this binary.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::db::engine::QueryExecutor;
use crate::errors::{Result, TraceDecayError};

#[derive(Clone, Debug, PartialEq, Eq)]
struct SchemaObject {
    object_type: String,
    table: String,
    sql: String,
}

type SchemaInventory = BTreeMap<String, SchemaObject>;

static EXPECTED_FINAL_SHAPE: LazyLock<std::result::Result<SchemaInventory, String>> =
    LazyLock::new(build_expected_final_shape);

fn build_expected_final_shape() -> std::result::Result<SchemaInventory, String> {
    let connection = rusqlite::Connection::open_in_memory()
        .map_err(|error| format!("failed to open canonical in-memory schema: {error}"))?;
    connection
        .execute_batch(super::ROOT_SCHEMA)
        .map_err(|error| format!("failed to install canonical root schema: {error}"))?;
    connection
        .execute_batch(tracedecay_store::RETRIEVAL_ANCHORS_SCHEMA_DDL)
        .map_err(|error| format!("failed to install canonical retrieval anchors: {error}"))?;
    for schema in [
        crate::db::retrieval_anchor_schema::ALIASES_SCHEMA,
        crate::db::retrieval_anchor_schema::AUTHORITY_SCHEMA,
        crate::db::retrieval_anchor_schema::IMMUTABILITY_TRIGGERS,
    ] {
        connection.execute_batch(schema).map_err(|error| {
            format!("failed to install canonical retrieval-anchor support: {error}")
        })?;
    }
    for schema in crate::db::memory_v2::FINAL_SCHEMA_BATCHES {
        connection
            .execute_batch(schema)
            .map_err(|error| format!("failed to install canonical memory schema: {error}"))?;
    }
    for schema in [
        crate::db::evidence_assembly::EVIDENCE_ASSEMBLY_SCHEMA,
        crate::db::evidence_assembly::EVIDENCE_ASSEMBLY_IMMUTABILITY,
        tracedecay_rusqlite_runtime::repository::EXTERNAL_SOURCE_SCHEMA_V1,
        tracedecay_rusqlite_runtime::repository::GRAPH_PUBLICATION_SCHEMA_V1,
        tracedecay_rusqlite_runtime::repository::SEMANTIC_VECTOR_STAGING_SCHEMA,
    ] {
        connection
            .execute_batch(schema)
            .map_err(|error| format!("failed to install canonical retained schema: {error}"))?;
    }
    read_rusqlite_inventory(&connection)
}

fn read_rusqlite_inventory(
    connection: &rusqlite::Connection,
) -> std::result::Result<SchemaInventory, String> {
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

async fn read_inventory(conn: &impl QueryExecutor) -> Result<SchemaInventory> {
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
        .map_err(|error| database_error(format!("failed to query schema inventory: {error}")))?;
    let mut inventory = SchemaInventory::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| database_error(format!("failed to read schema inventory: {error}")))?
    {
        let object_type = row
            .get::<String>(0)
            .map_err(|error| database_error(format!("failed to decode object type: {error}")))?;
        let name = row
            .get::<String>(1)
            .map_err(|error| database_error(format!("failed to decode object name: {error}")))?;
        let table = row
            .get::<String>(2)
            .map_err(|error| database_error(format!("failed to decode object table: {error}")))?;
        let sql = row
            .get::<String>(3)
            .map_err(|error| database_error(format!("failed to decode object SQL: {error}")))?;
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
            return Err(database_error(format!(
                "schema inventory repeats object '{name}'"
            )));
        }
    }
    Ok(inventory)
}

fn database_error(message: String) -> TraceDecayError {
    TraceDecayError::Database {
        message,
        operation: "verify exact final SQLite schema".to_owned(),
    }
}

fn reset_required(reason: impl Into<String>) -> TraceDecayError {
    TraceDecayError::reset_required(
        "SQLite store",
        format!(
            "{}; remove the store directory and let this binary create the exact final shape",
            reason.into()
        ),
    )
}

pub(super) async fn require_exact_final_shape(conn: &impl QueryExecutor) -> Result<()> {
    let actual = read_inventory(conn).await?;
    let expected = EXPECTED_FINAL_SHAPE
        .as_ref()
        .map_err(|error| database_error(error.clone()))?;

    for (name, expected_object) in expected {
        let Some(actual_object) = actual.get(name) else {
            return Err(reset_required(format!(
                "database schema is missing required {} '{name}'",
                expected_object.object_type
            )));
        };
        if actual_object != expected_object {
            return Err(reset_required(format!(
                "database schema has incompatible {} '{name}'",
                expected_object.object_type
            )));
        }
    }
    if let Some((name, object)) = actual
        .iter()
        .find(|(name, _object)| !expected.contains_key(*name))
    {
        return Err(reset_required(format!(
            "database schema contains unexpected {} '{name}'",
            object.object_type
        )));
    }
    Ok(())
}
