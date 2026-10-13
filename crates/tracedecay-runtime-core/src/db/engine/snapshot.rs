use std::sync::Arc;

use tracedecay_rusqlite_runtime::exact_sql::ExactSqlReadSnapshot;

use super::{IntoParams, Result, Rows, connection::statement};

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
        Self {
            runtime: Arc::new(runtime),
        }
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let runtime = Arc::clone(&self.runtime);
        let statement = statement(sql, params)?;
        let cx = crate::native_runtime::cx().ok_or(super::Error::RuntimeContextUnavailable)?;
        let rows = cx
            .spawn_blocking(move |_cx| runtime.query(statement).map_err(super::Error::from))
            .map_err(|error| {
                super::Error::Runtime(format!("failed to admit SQL snapshot query: {error}"))
            })?
            .join(&cx)
            .await
            .map_err(join_error)??;
        Ok(Rows::from_exact(rows))
    }
}

fn join_error(error: asupersync::runtime::JoinError) -> super::Error {
    super::Error::Runtime(format!("exact SQL read snapshot task failed: {error}"))
}
