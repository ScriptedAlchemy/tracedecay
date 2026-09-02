use std::collections::HashMap;

use tracedecay_runtime_core::db::engine::{QueryExecutor, params_from_iter};

use super::super::{global_db_operation_error, global_db_operation_message};

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

#[derive(Default)]
pub(super) struct ActualTableMetadata {
    pub(super) columns: HashMap<String, ActualColumn>,
    pub(super) foreign_keys: Vec<ActualForeignKey>,
    pub(super) indexes: Vec<ActualIndex>,
}

fn requested_values(count: usize) -> String {
    (1..=count)
        .map(|index| format!("(?{index})"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[hotpath::measure(
    future = true,
    label = "global_db.schema_contract.query.table_metadata"
)]
pub(super) async fn read_table_metadata(
    conn: &impl QueryExecutor,
    tables: &[&str],
) -> tracedecay_domain::errors::Result<HashMap<String, ActualTableMetadata>> {
    let mut metadata = tables
        .iter()
        .map(|table| ((*table).to_owned(), ActualTableMetadata::default()))
        .collect::<HashMap<_, _>>();
    if tables.is_empty() {
        return Ok(metadata);
    }
    let values = requested_values(tables.len());
    let mut rows = conn
        .query(
            &format!(
                "WITH requested(table_name) AS (VALUES {values})
                 SELECT requested.table_name, p.name, p.type, p.\"notnull\",
                        p.dflt_value, p.pk, p.hidden
                 FROM requested
                 JOIN pragma_table_xinfo(requested.table_name) AS p
                 ORDER BY requested.table_name, p.cid"
            ),
            params_from_iter(tables.iter().copied()),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let table = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let name = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let table = metadata.get_mut(&table).ok_or_else(|| {
            global_db_operation_message(OPERATION, "table metadata escaped the requested inventory")
        })?;
        table.columns.insert(
            name.to_ascii_lowercase(),
            ActualColumn {
                declared_type: row
                    .get(2)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                not_null: row
                    .get::<i64>(3)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?
                    != 0,
                default_value: row
                    .get(4)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                primary_key_ordinal: row
                    .get(5)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                hidden: row
                    .get(6)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            },
        );
    }

    let mut rows = conn
        .query(
            &format!(
                "WITH requested(table_name) AS (VALUES {values})
                 SELECT requested.table_name, p.id, p.seq, p.\"from\", p.\"table\",
                        p.\"to\", p.on_update, p.on_delete, p.\"match\"
                 FROM requested
                 JOIN pragma_foreign_key_list(requested.table_name) AS p
                 ORDER BY requested.table_name, p.id, p.seq"
            ),
            params_from_iter(tables.iter().copied()),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let table = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let table = metadata.get_mut(&table).ok_or_else(|| {
            global_db_operation_message(
                OPERATION,
                "foreign-key metadata escaped the requested inventory",
            )
        })?;
        table.foreign_keys.push(ActualForeignKey {
            _id: row
                .get(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            sequence: row
                .get(2)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            from: row
                .get(3)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            target_table: row
                .get(4)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            target_column: row
                .get(5)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            on_update: row
                .get(6)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            on_delete: row
                .get(7)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            match_mode: row
                .get(8)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        });
    }

    let mut rows = conn
        .query(
            &format!(
                "WITH requested(table_name) AS (VALUES {values}),
                 headers AS (
                     SELECT requested.table_name, p.seq, p.name, p.\"unique\",
                            p.origin, p.partial
                     FROM requested
                     JOIN pragma_index_list(requested.table_name) AS p
                 )
                 SELECT headers.table_name, headers.name, headers.\"unique\",
                        headers.origin, headers.partial, x.cid, x.name, x.desc, x.coll
                 FROM headers
                 JOIN pragma_index_xinfo(headers.name) AS x
                 WHERE x.key = 1
                 ORDER BY headers.table_name, headers.seq, x.seqno"
            ),
            params_from_iter(tables.iter().copied()),
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut current: Option<(String, ActualIndex)> = None;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let table = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let name = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let unique = row
            .get::<i64>(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            != 0;
        let origin = row
            .get::<String>(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let partial = row
            .get::<i64>(4)
            .map_err(|error| global_db_operation_error(OPERATION, error))?
            != 0;
        if current
            .as_ref()
            .is_some_and(|(current_table, index)| current_table != &table || index.name != name)
            && let Some((current_table, index)) = current.take()
        {
            metadata
                .get_mut(&current_table)
                .ok_or_else(|| {
                    global_db_operation_message(
                        OPERATION,
                        "index metadata escaped the requested inventory",
                    )
                })?
                .indexes
                .push(index);
        }
        let (_, index) = current.get_or_insert_with(|| {
            (
                table,
                ActualIndex {
                    name,
                    unique,
                    origin,
                    partial,
                    columns: Vec::new(),
                },
            )
        });
        index.columns.push(ActualIndexColumn {
            cid: row
                .get(5)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            name: row
                .get(6)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
            descending: row
                .get::<i64>(7)
                .map_err(|error| global_db_operation_error(OPERATION, error))?
                != 0,
            collation: row
                .get(8)
                .map_err(|error| global_db_operation_error(OPERATION, error))?,
        });
    }
    if let Some((table, index)) = current {
        metadata
            .get_mut(&table)
            .ok_or_else(|| {
                global_db_operation_message(
                    OPERATION,
                    "index metadata escaped the requested inventory",
                )
            })?
            .indexes
            .push(index);
    }
    Ok(metadata)
}
