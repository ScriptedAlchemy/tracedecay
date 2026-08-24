use std::collections::BTreeMap;
use std::sync::LazyLock;

use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::super::{global_db_operation_error, global_db_operation_message};
use super::definitions::{Column, INDEXES, Index, REGISTRY_TABLE_NAMES, TABLES, Table};
use super::pragma::{
    ActualColumn, ActualForeignKey, ActualIndex, read_columns, read_foreign_keys, read_indexes,
};
use super::{normalize_trigger_sql, starts_with_ignore_ascii_case};

const OPERATION: &str = "validate global database authority schema";

#[derive(Clone, Debug, PartialEq, Eq)]
struct GraphPublicationSchemaObject {
    object_type: String,
    table: String,
    sql: String,
}

type GraphPublicationSchemaInventory = BTreeMap<String, GraphPublicationSchemaObject>;
type GraphPublicationSchemaBuildResult =
    std::result::Result<GraphPublicationSchemaInventory, String>;

static EXPECTED_GRAPH_PUBLICATION_SCHEMA: LazyLock<GraphPublicationSchemaBuildResult> =
    LazyLock::new(build_expected_graph_publication_schema);

fn build_expected_graph_publication_schema() -> GraphPublicationSchemaBuildResult {
    let connection = rusqlite::Connection::open_in_memory()
        .map_err(|error| format!("failed to open canonical graph publication schema: {error}"))?;
    connection
        .execute_batch(tracedecay_rusqlite_runtime::repository::GRAPH_PUBLICATION_SCHEMA_V1)
        .map_err(|error| {
            format!("failed to install canonical graph publication schema: {error}")
        })?;
    read_rusqlite_graph_publication_inventory(&connection)
}

fn read_rusqlite_graph_publication_inventory(
    connection: &rusqlite::Connection,
) -> std::result::Result<GraphPublicationSchemaInventory, String> {
    let mut statement = connection
        .prepare(
            "SELECT type, name, tbl_name, COALESCE(sql, '')
             FROM sqlite_master
             WHERE type IN ('table', 'index', 'trigger', 'view')
               AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .map_err(|error| format!("failed to prepare canonical graph schema inventory: {error}"))?;
    let rows = statement
        .query_map((), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| format!("failed to query canonical graph schema inventory: {error}"))?;
    let mut inventory = GraphPublicationSchemaInventory::new();
    for row in rows {
        let (object_type, name, table, sql) =
            row.map_err(|error| format!("failed to read canonical graph schema object: {error}"))?;
        if inventory
            .insert(
                name.clone(),
                GraphPublicationSchemaObject {
                    object_type,
                    table,
                    sql,
                },
            )
            .is_some()
        {
            return Err(format!(
                "canonical graph publication schema repeats object '{name}'"
            ));
        }
    }
    Ok(inventory)
}

fn outer_parentheses_enclose_value(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'(') || bytes.last() != Some(&b')') {
        return false;
    }
    let mut depth = 0_i64;
    let mut quote = None;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == active {
                if bytes.get(index + 1) == Some(&active) {
                    index += 1;
                } else {
                    quote = None;
                }
            }
        } else {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 && index + 1 != bytes.len() {
                        return false;
                    }
                    if depth < 0 {
                        return false;
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    depth == 0 && quote.is_none()
}

fn normalize_default(value: Option<&str>) -> Option<String> {
    value.map(|value| {
        let mut value = value.trim();
        while outer_parentheses_enclose_value(value) {
            value = value[1..value.len() - 1].trim();
        }
        value.to_string()
    })
}

async fn validate_table(
    conn: &impl QueryExecutor,
    contract: &Table,
) -> tracedecay_runtime_core::errors::Result<()> {
    let actual = read_columns(conn, contract.name).await?;
    if actual.len() != contract.columns.len() {
        return Err(global_db_operation_message(
            OPERATION,
            format!(
                "table '{}' has an incompatible number of columns",
                contract.name
            ),
        ));
    }
    for column in contract.columns {
        let Some(actual) = actual.get(&column.name.to_ascii_lowercase()) else {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "table '{}' is missing column '{}'",
                    contract.name, column.name
                ),
            ));
        };
        if !column_metadata_matches(actual, column) {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "table '{}' column '{}' has incompatible xinfo metadata",
                    contract.name, column.name
                ),
            ));
        }
    }

    let actual = read_foreign_keys(conn, contract.name).await?;
    if !foreign_keys_match(&actual, contract) {
        return Err(global_db_operation_message(
            OPERATION,
            format!("table '{}' has incompatible foreign keys", contract.name),
        ));
    }
    Ok(())
}

fn column_metadata_matches(actual: &ActualColumn, expected: &Column) -> bool {
    actual.hidden == 0
        && actual
            .declared_type
            .eq_ignore_ascii_case(expected.declared_type)
        && actual.not_null == expected.not_null
        && normalize_default(actual.default_value.as_deref())
            == normalize_default(expected.default_value)
        && actual.primary_key_ordinal == expected.primary_key_ordinal
}

fn foreign_keys_match(actual: &[ActualForeignKey], contract: &Table) -> bool {
    actual.len() == contract.foreign_keys.len()
        && contract.foreign_keys.iter().all(|expected| {
            actual.iter().any(|actual| {
                actual.sequence == expected.sequence
                    && actual.from.eq_ignore_ascii_case(expected.from)
                    && actual
                        .target_table
                        .eq_ignore_ascii_case(expected.target_table)
                    && actual
                        .target_column
                        .eq_ignore_ascii_case(expected.target_column)
                    && actual.on_update.eq_ignore_ascii_case("NO ACTION")
                    && actual.on_delete.eq_ignore_ascii_case(expected.on_delete)
                    && actual.match_mode.eq_ignore_ascii_case("NONE")
            })
        })
}

fn index_matches(actual: &ActualIndex, expected: &Index) -> bool {
    let expected_partial = expected.name.is_some_and(|name| {
        name.eq_ignore_ascii_case("idx_session_temporal_generations_one_active")
            || name.eq_ignore_ascii_case("idx_session_refresh_operations_one_running")
    });
    expected
        .name
        .is_none_or(|name| actual.name.eq_ignore_ascii_case(name))
        && actual.unique == expected.unique
        && actual.origin.eq_ignore_ascii_case(expected.origin)
        && actual.partial == expected_partial
        && actual.columns.len() == expected.columns.len()
        && actual
            .columns
            .iter()
            .zip(expected.columns)
            .all(|(actual, expected)| {
                actual.cid >= 0
                    && !actual.descending
                    && actual.collation.eq_ignore_ascii_case("BINARY")
                    && actual.name.eq_ignore_ascii_case(expected)
            })
}

fn primary_key_index_columns(contract: &Table) -> Option<Vec<&str>> {
    let mut columns = contract
        .columns
        .iter()
        .filter(|column| column.primary_key_ordinal > 0)
        .collect::<Vec<_>>();
    columns.sort_unstable_by_key(|column| column.primary_key_ordinal);
    if columns.is_empty()
        || (columns.len() == 1 && columns[0].declared_type.eq_ignore_ascii_case("INTEGER"))
    {
        return None;
    }
    Some(columns.into_iter().map(|column| column.name).collect())
}

fn primary_key_index_matches(actual: &ActualIndex, expected_columns: &[&str]) -> bool {
    actual.unique
        && actual.origin.eq_ignore_ascii_case("pk")
        && !actual.partial
        && actual.columns.len() == expected_columns.len()
        && actual
            .columns
            .iter()
            .zip(expected_columns)
            .all(|(actual, expected)| {
                actual.cid >= 0
                    && !actual.descending
                    && actual.collation.eq_ignore_ascii_case("BINARY")
                    && actual.name.eq_ignore_ascii_case(expected)
            })
}

async fn validate_indexes_for_table(
    conn: &impl QueryExecutor,
    table: &str,
) -> tracedecay_runtime_core::errors::Result<()> {
    let actual = read_indexes(conn, table).await?;
    let expected = INDEXES
        .iter()
        .filter(|contract| contract.table.eq_ignore_ascii_case(table))
        .collect::<Vec<_>>();
    let mut matched_actual = vec![false; actual.len()];
    for contract in &expected {
        let Some(actual_index) = actual
            .iter()
            .enumerate()
            .position(|(index, actual)| !matched_actual[index] && index_matches(actual, contract))
        else {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "table '{}' is missing required {}index on ({})",
                    contract.table,
                    if contract.unique { "unique " } else { "" },
                    contract.columns.join(", ")
                ),
            ));
        };
        matched_actual[actual_index] = true;
    }
    let table_contract = TABLES
        .iter()
        .find(|contract| contract.name.eq_ignore_ascii_case(table));
    if expected
        .iter()
        .all(|contract| !contract.origin.eq_ignore_ascii_case("pk"))
        && let Some(primary_key_columns) = table_contract.and_then(primary_key_index_columns)
    {
        let Some(actual_index) = actual.iter().enumerate().position(|(index, actual)| {
            !matched_actual[index] && primary_key_index_matches(actual, &primary_key_columns)
        }) else {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "table '{table}' is missing required primary-key index on ({})",
                    primary_key_columns.join(", ")
                ),
            ));
        };
        matched_actual[actual_index] = true;
    }

    if matched_actual.iter().any(|matched| !matched) {
        return Err(global_db_operation_message(
            OPERATION,
            format!("table '{table}' has an incompatible total index inventory"),
        ));
    }
    Ok(())
}

async fn validate_trigger(
    conn: &impl QueryExecutor,
    trigger: &super::invariants::Trigger,
) -> tracedecay_runtime_core::errors::Result<()> {
    let mut rows = conn
        .query(
            "SELECT tbl_name, sql FROM sqlite_master
             WHERE type = 'trigger' AND name = ?1 COLLATE NOCASE",
            params![trigger.name],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let actual = match rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        Some(row) => Some((
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<String>(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        )),
        None => None,
    };
    if actual.as_ref().is_some_and(|(table, sql)| {
        table.eq_ignore_ascii_case(trigger.table)
            && normalize_trigger_sql(sql) == normalize_trigger_sql(trigger.create_sql)
    }) {
        Ok(())
    } else {
        Err(global_db_operation_message(
            OPERATION,
            format!(
                "required trigger '{}' on table '{}' is missing or incompatible",
                trigger.name, trigger.table
            ),
        ))
    }
}

async fn validate_observation_autoincrement(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    let mut rows = conn
        .query(
            "SELECT EXISTS(
                SELECT 1 FROM global_schema_migrations
                WHERE migration = 'observations-v2-canonical-autoincrement'
             ),
             COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'observations'), 0),
             COALESCE((SELECT MAX(sequence) FROM observations), 0)",
            (),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .ok_or_else(|| {
            global_db_operation_message(OPERATION, "AUTOINCREMENT invariant returned no row")
        })?;
    let recorded = row
        .get::<i64>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        != 0;
    let sqlite_sequence = row
        .get::<i64>(1)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let committed_sequence = row
        .get::<i64>(2)
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if recorded && sqlite_sequence >= committed_sequence {
        Ok(())
    } else {
        Err(global_db_operation_message(
            OPERATION,
            "observations is missing its canonical AUTOINCREMENT invariant",
        ))
    }
}

async fn validate_tables_and_indexes(
    conn: &impl QueryExecutor,
    tables: &[Table],
) -> tracedecay_runtime_core::errors::Result<()> {
    for contract in tables {
        validate_table(conn, contract).await?;
        validate_indexes_for_table(conn, contract.name).await?;
    }
    Ok(())
}

async fn validate_named_tables_and_indexes(
    conn: &impl QueryExecutor,
    table_names: &[&str],
) -> tracedecay_runtime_core::errors::Result<()> {
    for table_name in table_names {
        let contract = TABLES
            .iter()
            .find(|contract| contract.name.eq_ignore_ascii_case(table_name))
            .ok_or_else(|| {
                global_db_operation_message(
                    OPERATION,
                    format!("schema contract for table '{table_name}' is not defined"),
                )
            })?;
        validate_table(conn, contract).await?;
        validate_indexes_for_table(conn, contract.name).await?;
    }
    Ok(())
}

pub async fn validate_session_temporal_schema_contract(
    conn: &impl QueryExecutor,
    table_names: &[&str],
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_named_tables_and_indexes(conn, table_names).await
}

pub async fn validate_session_graph_publication_schema_contract(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    let actual = read_graph_publication_inventory(conn).await?;
    let expected = EXPECTED_GRAPH_PUBLICATION_SCHEMA
        .as_ref()
        .map_err(|error| global_db_operation_message(OPERATION, error.clone()))?;

    for (name, expected_object) in expected {
        let Some(actual_object) = actual.get(name) else {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "graph publication schema is missing required {} '{name}'",
                    expected_object.object_type
                ),
            ));
        };
        if actual_object != expected_object {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "graph publication schema has incompatible {} '{name}'",
                    expected_object.object_type
                ),
            ));
        }
    }
    if let Some((name, object)) = actual
        .iter()
        .find(|(name, _object)| !expected.contains_key(*name))
    {
        return Err(global_db_operation_message(
            OPERATION,
            format!(
                "graph publication schema contains unexpected {} '{name}'",
                object.object_type
            ),
        ));
    }
    Ok(())
}

async fn read_graph_publication_inventory(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<GraphPublicationSchemaInventory> {
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
    let mut inventory = GraphPublicationSchemaInventory::new();
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
        if !belongs_to_graph_publication_namespace(&name, &table) {
            continue;
        }
        if inventory
            .insert(
                name.clone(),
                GraphPublicationSchemaObject {
                    object_type,
                    table,
                    sql,
                },
            )
            .is_some()
        {
            return Err(global_db_operation_message(
                OPERATION,
                format!("graph publication schema repeats object '{name}'"),
            ));
        }
    }
    Ok(inventory)
}

fn belongs_to_graph_publication_namespace(name: &str, table: &str) -> bool {
    [name, table].iter().any(|value| {
        starts_with_ignore_ascii_case(value, "graph_publication_")
            || starts_with_ignore_ascii_case(value, "graph_verified_")
    })
}

pub async fn validate_registry_schema_contract(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_named_tables_and_indexes(conn, REGISTRY_TABLE_NAMES).await
}

pub async fn validate_remote_deletion_schema_contract(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_named_tables_and_indexes(conn, &["remote_deletion_tombstones"]).await
}

/// Validates the composed registry, observation-authority, and projection-authority schemas.
///
/// Transcript, LCM, git-correlation, and workflow-index tables are independently owned by their
/// schema modules; this validator intentionally neither claims nor validates those domains.
pub async fn validate_authority_schema_contract(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_tables_and_indexes(conn, TABLES).await?;
    for invariant in super::invariants::INVARIANTS {
        for trigger in invariant.triggers {
            validate_trigger(conn, trigger).await?;
        }
    }
    validate_observation_autoincrement(conn).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{normalize_default, validate_registry_schema_contract};
    use crate::tests::harness::open_registered_test_database_fixture;
    use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;

    #[test]
    fn default_normalization_only_strips_balanced_outer_parentheses() {
        assert_eq!(normalize_default(Some(" ((0)) ")).as_deref(), Some("0"));
        assert_eq!(
            normalize_default(Some("(0) + (1)")).as_deref(),
            Some("(0) + (1)")
        );
        assert_eq!(normalize_default(Some("((0)")).as_deref(), Some("((0)"));
        assert_eq!(normalize_default(Some("(')')")).as_deref(), Some("')'"));
    }

    #[tokio::test]
    async fn registry_contract_validates_through_engine_connection() {
        let directory = tempfile::tempdir().unwrap();
        let (database, _owner) = open_registered_test_database_fixture(
            &directory.path().join("global.db"),
            TestDatabaseRuntimeScope::Profile,
        )
        .await
        .unwrap();
        validate_registry_schema_contract(&database.read_connection())
            .await
            .unwrap();
    }
}
