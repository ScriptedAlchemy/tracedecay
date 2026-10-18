use std::sync::Arc;

use tracedecay_rusqlite_runtime::exact_sql::{ExactSqlReadSnapshot, ExactSqlStatementBudget};

use super::{IntoParams, Result, Rows, connection::statement};

pub struct ReadSnapshot {
    /// One snapshot holds one pooled reader worker for its whole lifetime, and
    /// the runtime snapshot already serializes the statements issued against
    /// that worker. A snapshot handed to several concurrent readers therefore
    /// funnels all of them through it, so a single slow statement stalls the
    /// rest. The measured query path remains the stable boundary.
    runtime: Arc<ExactSqlReadSnapshot>,
    budget: ExactSqlStatementBudget,
}

impl ReadSnapshot {
    pub(super) fn from_runtime(runtime: ExactSqlReadSnapshot) -> Self {
        Self {
            runtime: Arc::new(runtime),
            budget: ExactSqlStatementBudget::Request,
        }
    }

    /// Every statement on this snapshot runs under `budget`.
    #[must_use]
    pub(crate) fn with_budget(self, budget: ExactSqlStatementBudget) -> Self {
        Self { budget, ..self }
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let runtime = Arc::clone(&self.runtime);
        let budget = self.budget;
        let statement = statement(sql, params)?;
        let rows = super::connection::run_blocking("SQL snapshot query", join_error, move || {
            runtime
                .query_with_budget(statement, budget)
                .map_err(super::Error::from)
        })
        .await??;
        Ok(Rows::from_exact(rows))
    }
}

fn join_error(error: asupersync::runtime::JoinError) -> super::Error {
    super::Error::Runtime(format!("exact SQL read snapshot task failed: {error}"))
}
