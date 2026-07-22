use rusqlite::params;
use tracedecay_sqlite_parity_protocol::{
    ErrorCode, ErrorPayload, FtsMatch, FtsParity, GraphFtsTable, GraphTable, RowParity,
};

use crate::{closed_sql, snapshot::ReadOnlyDriver, snapshot::sqlite_query_error};

impl ReadOnlyDriver {
    pub(crate) fn row_parity(&self, table: GraphTable) -> Result<RowParity, ErrorPayload> {
        let spec = closed_sql::graph_table_spec(table);
        let row_count = if self.table_exists(spec)? {
            let observed = self.count_rows(spec)?;
            Some(u64::try_from(observed).map_err(|_| {
                ErrorPayload::new(
                    ErrorCode::InvalidSqliteValue,
                    format!(
                        "SQLite returned negative row count {observed} for {}",
                        spec.identifier
                    ),
                )
            })?)
        } else {
            None
        };
        Ok(RowParity { table, row_count })
    }

    pub(crate) fn fts_parity(
        &self,
        table: GraphFtsTable,
        query: &str,
        limit: u16,
    ) -> Result<FtsParity, ErrorPayload> {
        let sql = match table {
            GraphFtsTable::Nodes => closed_sql::NODES_FTS_MATCH,
        };
        let mut statement = self.connection.prepare(sql).map_err(sqlite_query_error)?;
        let matches = statement
            .query_map(params![query, i64::from(limit)], |row| {
                Ok(FtsMatch {
                    rowid: row.get(0)?,
                    rank: row.get(1)?,
                    snippet: row.get(2)?,
                })
            })
            .map_err(sqlite_query_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_query_error)?;
        Ok(FtsParity { table, matches })
    }
}
