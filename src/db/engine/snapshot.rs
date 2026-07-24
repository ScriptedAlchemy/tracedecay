use std::sync::{Arc, Mutex};

use tracedecay_rusqlite_runtime::migration_sql::MigrationSqlReadSnapshot;

use super::{IntoParams, Result, Rows, Value, connection::statement};

pub(crate) struct ReadSnapshot {
    runtime: Arc<Mutex<MigrationSqlReadSnapshot>>,
}

impl ReadSnapshot {
    pub(super) fn from_runtime(runtime: MigrationSqlReadSnapshot) -> Self {
        Self {
            runtime: Arc::new(Mutex::new(runtime)),
        }
    }

    pub(crate) async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let runtime = Arc::clone(&self.runtime);
        let statement = statement(sql, params)?;
        let rows = tokio::task::spawn_blocking(move || {
            runtime
                .lock()
                .expect("migration SQL read snapshot lock")
                .query(statement)
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)??;
        Ok(Rows::from_parts(
            rows.columns,
            rows.rows
                .into_iter()
                .map(|row| {
                    super::Row::from_values(row.values.into_iter().map(Value::from).collect())
                })
                .collect(),
        ))
    }
}

fn join_error(error: tokio::task::JoinError) -> super::Error {
    super::Error::Runtime(format!("migration SQL read snapshot task failed: {error}"))
}
