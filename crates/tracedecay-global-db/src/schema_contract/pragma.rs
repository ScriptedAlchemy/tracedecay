use std::collections::HashMap;

use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::super::global_db_operation_error;

const OPERATION: &str = "validate global database authority schema";

pub(super) struct ActualColumn {
    pub(super) declared_type: String,
    pub(super) not_null: bool,
    pub(super) default_value: Option<String>,
    pub(super) primary_key_ordinal: i64,
    pub(super) hidden: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ActualForeignKey {
    pub(super) _id: i64,
    pub(super) sequence: i64,
    pub(super) from: String,
    pub(super) target_table: String,
    pub(super) target_column: String,
    pub(super) on_update: String,
    pub(super) on_delete: String,
    pub(super) match_mode: String,
}

#[derive(Debug)]
pub(super) struct ActualIndexColumn {
    pub(super) cid: i64,
    pub(super) name: String,
    pub(super) descending: bool,
    pub(super) collation: String,
}

#[derive(Debug)]
pub(super) struct ActualIndex {
    pub(super) name: String,
    pub(super) unique: bool,
    pub(super) origin: String,
    pub(super) partial: bool,
    pub(super) columns: Vec<ActualIndexColumn>,
}

pub(super) async fn read_columns(
    conn: &impl QueryExecutor,
    table: &str,
) -> tracedecay_runtime_core::errors::Result<HashMap<String, ActualColumn>> {
    let mut rows = conn
        .query(
            "SELECT name, type, \"notnull\", dflt_value, pk, hidden
             FROM pragma_table_xinfo(?1) ORDER BY cid",
            params![table],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut columns = HashMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let name = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        columns.insert(
            name.to_ascii_lowercase(),
            ActualColumn {
                declared_type: row
                    .get(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                not_null: row
                    .get::<i64>(2)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?
                    != 0,
                default_value: row
                    .get(3)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                primary_key_ordinal: row
                    .get(4)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                hidden: row
                    .get(5)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            },
        );
    }
    Ok(columns)
}

pub(super) async fn read_foreign_keys(
    conn: &impl QueryExecutor,
    table: &str,
) -> tracedecay_runtime_core::errors::Result<Vec<ActualForeignKey>> {
    let mut rows = conn
        .query(
            "SELECT id, seq, \"from\", \"table\", \"to\", on_update, on_delete, \"match\"
             FROM pragma_foreign_key_list(?1) ORDER BY id, seq",
            params![table],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut foreign_keys = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        foreign_keys.push(ActualForeignKey {
            _id: row
                .get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            sequence: row
                .get(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            from: row
                .get(2)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            target_table: row
                .get(3)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            target_column: row
                .get(4)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            on_update: row
                .get(5)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            on_delete: row
                .get(6)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            match_mode: row
                .get(7)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        });
    }
    Ok(foreign_keys)
}

pub(super) async fn read_indexes(
    conn: &impl QueryExecutor,
    table: &str,
) -> tracedecay_runtime_core::errors::Result<Vec<ActualIndex>> {
    let mut rows = conn
        .query(
            "SELECT name, \"unique\", origin, partial
             FROM pragma_index_list(?1) ORDER BY seq",
            params![table],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut headers = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        headers.push((
            row.get::<String>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<i64>(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?
                != 0,
            row.get::<String>(2)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            row.get::<i64>(3)
                .map_err(|error| global_db_operation_error(OPERATION, error))?
                != 0,
        ));
    }
    let mut indexes = Vec::new();
    for (name, unique, origin, partial) in headers {
        let mut rows = conn
            .query(
                "SELECT cid, name, desc, coll
                 FROM pragma_index_xinfo(?1) WHERE key = 1 ORDER BY seqno",
                params![name.as_str()],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut columns = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            columns.push(ActualIndexColumn {
                cid: row
                    .get(0)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                name: row
                    .get(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                descending: row
                    .get::<i64>(2)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?
                    != 0,
                collation: row
                    .get(3)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            });
        }
        indexes.push(ActualIndex {
            name,
            unique,
            origin,
            partial,
            columns,
        });
    }
    Ok(indexes)
}
