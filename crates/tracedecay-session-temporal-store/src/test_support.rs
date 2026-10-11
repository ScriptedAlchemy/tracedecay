//! Measurement helpers shared by this crate's unit tests.

use std::future::Future;
use std::sync::Arc;
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

pub(crate) struct FailingRegisteredDb<'a> {
    inner: &'a tracedecay_global_db::RegisteredGlobalDb,
    plan: Vec<(&'static str, usize)>,
    matches: Arc<Vec<AtomicUsize>>,
}

impl<'a> FailingRegisteredDb<'a> {
    pub(crate) fn fail_nth_prefixed_execute(
        inner: &'a tracedecay_global_db::RegisteredGlobalDb,
        prefix: &'static str,
        fail_on_match: usize,
    ) -> Self {
        Self::fail_prefixed_executes(inner, &[(prefix, fail_on_match)])
    }

    pub(crate) fn fail_prefixed_executes(
        inner: &'a tracedecay_global_db::RegisteredGlobalDb,
        plan: &[(&'static str, usize)],
    ) -> Self {
        Self {
            inner,
            plan: plan.to_vec(),
            matches: Arc::new(plan.iter().map(|_| AtomicUsize::new(0)).collect()),
        }
    }
}

pub(crate) struct FailingWriteTxn<'a> {
    inner: tracedecay_global_db::RegisteredGlobalDbWriteTransaction<'a>,
    plan: Vec<(&'static str, usize)>,
    matches: Arc<Vec<AtomicUsize>>,
}

impl SessionTemporalQuery for FailingWriteTxn<'_> {
    fn backend_kind(&self) -> tracedecay_runtime_core::db::engine::BackendKind {
        SessionTemporalQuery::backend_kind(&self.inner)
    }

    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        SessionTemporalQuery::query(&self.inner, sql, params)
    }
}

impl SessionTemporalExec for FailingWriteTxn<'_> {
    fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<u64, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        let plan = self.plan.clone();
        let matches = Arc::clone(&self.matches);
        async move {
            for (index, (prefix, fail_on_match)) in plan.iter().enumerate() {
                if sql.starts_with(prefix)
                    && matches[index].fetch_add(1, Ordering::Relaxed) + 1 >= *fail_on_match
                {
                    return Err(EngineError::Runtime(
                        "injected savepoint failure".to_owned(),
                    ));
                }
            }
            SessionTemporalExec::execute(&self.inner, sql, params).await
        }
    }

    fn execute_batch(&self, sql: &str) -> impl Future<Output = Result<(), EngineError>> + Send {
        SessionTemporalExec::execute_batch(&self.inner, sql)
    }
}

impl crate::handle::SessionTemporalWriteTxn for FailingWriteTxn<'_> {
    fn commit(self) -> impl Future<Output = Result<(), EngineError>> + Send {
        crate::handle::SessionTemporalWriteTxn::commit(self.inner)
    }

    fn rollback(self) -> impl Future<Output = Result<(), EngineError>> + Send {
        crate::handle::SessionTemporalWriteTxn::rollback(self.inner)
    }
}

impl crate::handle::SessionTemporalRegisteredDb for FailingRegisteredDb<'_> {
    type WriteTxn<'b>
        = FailingWriteTxn<'b>
    where
        Self: 'b;

    fn read_snapshot(
        &self,
    ) -> impl Future<
        Output = Result<
            tracedecay_runtime_core::db::DatabaseEngineReadSnapshot,
            tracedecay_domain::errors::TraceDecayError,
        >,
    > + Send {
        crate::handle::SessionTemporalRegisteredDb::read_snapshot(self.inner)
    }

    fn health_read_snapshot(
        &self,
    ) -> impl Future<
        Output = Result<tracedecay_runtime_core::db::DatabaseEngineReadSnapshot, EngineError>,
    > + Send {
        crate::handle::SessionTemporalRegisteredDb::health_read_snapshot(self.inner)
    }

    fn begin_write_transaction(
        &self,
    ) -> impl Future<Output = Result<Self::WriteTxn<'_>, tracedecay_domain::errors::TraceDecayError>>
    + Send {
        let plan = self.plan.clone();
        let matches = Arc::clone(&self.matches);
        async move {
            crate::handle::SessionTemporalRegisteredDb::begin_write_transaction(self.inner)
                .await
                .map(|inner| FailingWriteTxn {
                    inner,
                    plan,
                    matches,
                })
        }
    }

    fn db_path(&self) -> &std::path::Path {
        crate::handle::SessionTemporalRegisteredDb::db_path(self.inner)
    }

    fn session_relation_store(
        &self,
    ) -> Result<
        (
            crate::relations::SessionRelationScope,
            crate::relations::SessionRelationGraphStore,
        ),
        tracedecay_domain::errors::TraceDecayError,
    > {
        crate::handle::SessionTemporalRegisteredDb::session_relation_store(self.inner)
    }
}
