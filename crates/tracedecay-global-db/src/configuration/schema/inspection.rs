use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::ConfigurationSchemaError;

const DEFINITION_DIGEST_DOMAIN: &[u8] = b"tracedecay.configuration.schema-definition.v1\0";

#[derive(Debug, Serialize)]
struct SchemaObjectDefinition {
    kind: String,
    name: String,
    table: String,
    definition: Option<String>,
    columns: Vec<TableColumnDefinition>,
    index_columns: Vec<IndexColumnDefinition>,
}

#[derive(Debug, Serialize)]
struct TableColumnDefinition {
    cid: i64,
    name: String,
    declared_type: String,
    not_null: i64,
    default_value: Option<String>,
    primary_key_ordinal: i64,
    hidden: i64,
}

#[derive(Debug, Serialize)]
struct IndexColumnDefinition {
    sequence: i64,
    cid: i64,
    name: Option<String>,
    descending: i64,
    collation: String,
}

fn normalize_definition(sql: &str) -> String {
    sql.trim_end_matches(';').to_owned()
}

async fn table_columns(
    connection: &impl QueryExecutor,
    table: &str,
) -> Result<Vec<TableColumnDefinition>, ConfigurationSchemaError> {
    let mut rows = connection
        .query(
            "SELECT cid, name, type, \"notnull\", dflt_value, pk, hidden
             FROM pragma_table_xinfo(?1)
             ORDER BY cid",
            params![table],
        )
        .await?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await? {
        columns.push(TableColumnDefinition {
            cid: row.get(0)?,
            name: row.get(1)?,
            declared_type: row.get(2)?,
            not_null: row.get(3)?,
            default_value: row.get(4)?,
            primary_key_ordinal: row.get(5)?,
            hidden: row.get(6)?,
        });
    }
    Ok(columns)
}

async fn index_columns(
    connection: &impl QueryExecutor,
    index: &str,
) -> Result<Vec<IndexColumnDefinition>, ConfigurationSchemaError> {
    let mut rows = connection
        .query(
            "SELECT seqno, cid, name, desc, coll
             FROM pragma_index_xinfo(?1)
             WHERE key = 1
             ORDER BY seqno",
            params![index],
        )
        .await?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await? {
        columns.push(IndexColumnDefinition {
            sequence: row.get(0)?,
            cid: row.get(1)?,
            name: row.get(2)?,
            descending: row.get(3)?,
            collation: row.get(4)?,
        });
    }
    Ok(columns)
}

pub(super) async fn configuration_definition_digest(
    connection: &impl QueryExecutor,
) -> Result<Option<String>, ConfigurationSchemaError> {
    let mut rows = connection
        .query(
            "SELECT type, name, tbl_name, sql
             FROM sqlite_master
             WHERE name LIKE 'configuration_%'
                OR name LIKE 'idx_configuration_%'
                OR tbl_name LIKE 'configuration_%'
             ORDER BY type, name",
            (),
        )
        .await?;
    let mut headers = Vec::new();
    while let Some(row) = rows.next().await? {
        headers.push((
            row.get::<String>(0)?,
            row.get::<String>(1)?,
            row.get::<String>(2)?,
            row.get::<Option<String>>(3)?,
        ));
    }
    if headers.is_empty() {
        return Ok(None);
    }

    let mut definitions = Vec::with_capacity(headers.len());
    for (kind, name, table, sql) in headers {
        let columns = if kind == "table" {
            table_columns(connection, &name).await?
        } else {
            Vec::new()
        };
        let index_columns = if kind == "index" {
            index_columns(connection, &name).await?
        } else {
            Vec::new()
        };
        definitions.push(SchemaObjectDefinition {
            kind,
            name,
            table,
            definition: sql.as_deref().map(normalize_definition),
            columns,
            index_columns,
        });
    }

    let canonical =
        serde_json::to_vec(&definitions).map_err(|_| ConfigurationSchemaError::ResetRequired {
            reason: "configuration schema definition could not be canonicalized",
        })?;
    let mut hasher = Sha256::new();
    hasher.update(DEFINITION_DIGEST_DOMAIN);
    hasher.update(canonical);
    Ok(Some(format!("sha256:{}", hex::encode(hasher.finalize()))))
}

pub(super) async fn registered_store_is_empty(
    connection: &impl QueryExecutor,
) -> Result<bool, ConfigurationSchemaError> {
    let mut rows = connection
        .query(
            "SELECT 1
             FROM sqlite_master
             LIMIT 1",
            (),
        )
        .await?;
    Ok(rows.next().await?.is_none())
}
