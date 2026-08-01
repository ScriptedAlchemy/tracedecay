use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::super::{global_db_operation_error, global_db_operation_message};
use super::definitions::{
    Column, INDEXES, Index, OBSERVATIONS_TABLE_NAME, REGISTRY_TABLE_NAMES, TABLES, Table,
};
use super::normalize_trigger_sql;
use super::pragma::{
    ActualColumn, ActualForeignKey, ActualIndex, read_columns, read_foreign_keys, read_indexes,
};

const OPERATION: &str = "validate global database authority schema";

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

fn index_has_columns(actual: &ActualIndex, expected: &[&str]) -> bool {
    actual.unique
        && actual.origin.eq_ignore_ascii_case("u")
        && !actual.partial
        && actual.columns.len() == expected.len()
        && actual
            .columns
            .iter()
            .zip(expected)
            .all(|(actual, expected)| {
                actual.cid >= 0
                    && !actual.descending
                    && actual.collation.eq_ignore_ascii_case("BINARY")
                    && actual.name.eq_ignore_ascii_case(expected)
            })
}

pub async fn validate_observation_migration_source(
    conn: &impl QueryExecutor,
    has_legacy_idempotency: bool,
) -> tracedecay_runtime_core::errors::Result<()> {
    let Some(contract) = TABLES
        .iter()
        .find(|contract| contract.name == OBSERVATIONS_TABLE_NAME)
    else {
        return Err(global_db_operation_message(
            OPERATION,
            "canonical observations schema contract is not defined",
        ));
    };
    let columns = read_columns(conn, contract.name).await?;
    if columns.len() != contract.columns.len() + usize::from(has_legacy_idempotency)
        || contract.columns.iter().any(|expected| {
            columns
                .get(&expected.name.to_ascii_lowercase())
                .is_none_or(|actual| !column_metadata_matches(actual, expected))
        })
    {
        return Err(global_db_operation_message(
            OPERATION,
            "observations has incompatible metadata for canonical migration",
        ));
    }
    if has_legacy_idempotency {
        let Some(column) = columns.get("idempotency_key") else {
            return Err(global_db_operation_message(
                OPERATION,
                "observations is missing legacy idempotency metadata",
            ));
        };
        if column.hidden != 0
            || !column.declared_type.eq_ignore_ascii_case("TEXT")
            || !column.not_null
            || column.default_value.is_some()
            || column.primary_key_ordinal != 0
        {
            return Err(global_db_operation_message(
                OPERATION,
                "observations has incompatible legacy idempotency metadata",
            ));
        }
    }
    let foreign_keys = read_foreign_keys(conn, contract.name).await?;
    if !foreign_keys_match(&foreign_keys, contract) {
        return Err(global_db_operation_message(
            OPERATION,
            "observations has incompatible foreign keys for canonical migration",
        ));
    }
    let indexes = read_indexes(conn, contract.name).await?;
    let unique = indexes
        .iter()
        .filter(|index| index.unique && !index.origin.eq_ignore_ascii_case("pk"))
        .collect::<Vec<_>>();
    let expected_count = 1 + usize::from(has_legacy_idempotency);
    if unique.len() != expected_count
        || !unique
            .iter()
            .any(|index| index_has_columns(index, &["observation_id"]))
        || (has_legacy_idempotency
            && !unique
                .iter()
                .any(|index| index_has_columns(index, &["idempotency_key"])))
    {
        return Err(global_db_operation_message(
            OPERATION,
            "observations has incompatible unique indexes for canonical migration",
        ));
    }
    Ok(())
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
    for contract in &expected {
        if !actual.iter().any(|index| index_matches(index, contract)) {
            return Err(global_db_operation_message(
                OPERATION,
                format!(
                    "table '{}' is missing required {}index on ({})",
                    contract.table,
                    if contract.unique { "unique " } else { "" },
                    contract.columns.join(", ")
                ),
            ));
        }
    }

    let actual_unique = actual
        .iter()
        .filter(|index| index.unique && !index.origin.eq_ignore_ascii_case("pk"))
        .collect::<Vec<_>>();
    let expected_unique = expected
        .iter()
        .copied()
        .filter(|index| index.unique)
        .collect::<Vec<_>>();
    if actual_unique.len() != expected_unique.len()
        || expected_unique.iter().any(|expected| {
            !actual_unique
                .iter()
                .any(|actual| index_matches(actual, expected))
        })
    {
        return Err(global_db_operation_message(
            OPERATION,
            format!("table '{table}' has incompatible unique-key indexes"),
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

pub async fn validate_registry_schema_contract(
    conn: &impl QueryExecutor,
) -> tracedecay_runtime_core::errors::Result<()> {
    validate_named_tables_and_indexes(conn, REGISTRY_TABLE_NAMES).await
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
    use tracedecay_runtime_core::db::engine::TestConnection;

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
        let connection = TestConnection::open(&directory.path().join("global.db"));
        crate::ensure_registered_schema(&connection).await.unwrap();
        validate_registry_schema_contract(&connection)
            .await
            .unwrap();
    }
}
