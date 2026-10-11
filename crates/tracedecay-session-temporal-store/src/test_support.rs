//! Measurement helpers shared by this crate's unit tests.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_runtime_core::db::engine::{
    Error as EngineError, Executor, IntoParams, QueryExecutor, Result as EngineResult, Rows,
};

use crate::handle::{SessionTemporalExec, SessionTemporalQuery};

/// Connection adapter that counts the read statements a code path issues, so
/// round-trip amplification is measured rather than inferred.
pub(crate) struct QueryCountingConnection<'a, T> {
    inner: &'a T,
    queries: AtomicUsize,
}

impl<'a, T> QueryCountingConnection<'a, T> {
    pub(crate) fn new(inner: &'a T) -> Self {
        Self {
            inner,
            queries: AtomicUsize::new(0),
        }
    }

    pub(crate) fn query_count(&self) -> usize {
        self.queries.load(Ordering::Relaxed)
    }
}

impl<T: QueryExecutor> QueryExecutor for QueryCountingConnection<'_, T> {
    fn backend_kind(&self) -> tracedecay_runtime_core::db::engine::BackendKind {
        QueryExecutor::backend_kind(self.inner)
    }

    async fn query<P>(&self, sql: &str, params: P) -> EngineResult<Rows>
    where
        P: IntoParams,
    {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.inner.query(sql, params).await
    }
}

impl<T: Executor> Executor for QueryCountingConnection<'_, T> {
    async fn execute<P>(&self, sql: &str, params: P) -> EngineResult<u64>
    where
        P: IntoParams,
    {
        self.inner.execute(sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> EngineResult<()> {
        self.inner.execute_batch(sql).await
    }
}

impl<T: SessionTemporalQuery> SessionTemporalQuery for QueryCountingConnection<'_, T> {
    fn backend_kind(&self) -> tracedecay_runtime_core::db::engine::BackendKind {
        SessionTemporalQuery::backend_kind(self.inner)
    }

    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        self.queries.fetch_add(1, Ordering::Relaxed);
        SessionTemporalQuery::query(self.inner, sql, params)
    }
}

impl<T: SessionTemporalExec> SessionTemporalExec for QueryCountingConnection<'_, T> {
    fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<u64, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        SessionTemporalExec::execute(self.inner, sql, params)
    }

    fn execute_batch(&self, sql: &str) -> impl Future<Output = Result<(), EngineError>> + Send {
        SessionTemporalExec::execute_batch(self.inner, sql)
    }
}

/// The engine's VM steps for running `sql` to completion on a raw driver
/// connection: a work measure that grows with the rows a statement visits.
pub(crate) fn statement_vm_steps(
    connection: &tracedecay_sql::Connection,
    sql: &str,
    params: impl tracedecay_sql::Params,
) -> u64 {
    use tracedecay_sql::StatementControl;

    let mut statement = connection.prepare(sql).expect("prepare measured statement");
    statement.take_counters();
    let mut rows = statement.query(params).expect("run measured statement");
    while rows.next().expect("step measured statement").is_some() {}
    drop(rows);
    statement.take_counters().vm_steps
}
