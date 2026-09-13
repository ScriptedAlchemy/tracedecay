use std::sync::Arc;

use tracedecay_rusqlite_runtime::exact_sql::ExactSqlReadSnapshot;

use super::{IntoParams, Result, Rows, Value, connection::statement};

pub struct ReadSnapshot {
    /// One snapshot holds one pooled reader worker for its whole lifetime, and
    /// the runtime snapshot already serializes the statements issued against
    /// that worker. A snapshot handed to several concurrent readers therefore
    /// funnels all of them through it, so a single slow statement stalls the
    /// rest. The measured query path remains the stable boundary.
    runtime: Arc<ExactSqlReadSnapshot>,
}

impl ReadSnapshot {
    pub(super) fn from_runtime(runtime: ExactSqlReadSnapshot) -> Self {
        hotpath::gauge!("runtime_core.db.snapshots_active").inc(1.0);
        Self {
            runtime: Arc::new(runtime),
        }
    }

    #[hotpath::skip]
    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let runtime = Arc::clone(&self.runtime);
        let statement = statement(sql, params)?;
        let rows = tokio::task::spawn_blocking(move || {
            runtime.query(statement).map_err(super::Error::from)
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

impl Drop for ReadSnapshot {
    fn drop(&mut self) {
        hotpath::gauge!("runtime_core.db.snapshots_active").dec(1.0);
    }
}

fn join_error(error: tokio::task::JoinError) -> super::Error {
    super::Error::Runtime(format!("exact SQL read snapshot task failed: {error}"))
}
