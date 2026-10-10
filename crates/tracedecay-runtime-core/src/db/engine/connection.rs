use std::{sync::Arc, time::Duration};

use tracedecay_store::{OperationPriorityV1, StoreRuntimeBindingV1};

use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlHandle, ExactSqlStatement, MemoryReleaseOutcome,
};
pub use tracedecay_rusqlite_runtime::reader::{ReaderPoolSnapshot, ReaderPoolState};

#[cfg(any(test, feature = "test-helpers"))]
use super::Statement;
use super::{
    Error, IntoParams, ReadSnapshot, Result, Rows, Transaction, TransactionBehavior, WriteStatement,
};

const READER_WAIT: Duration = Duration::from_secs(5);
/// Doctor and other health probes must not sit behind a writer or a saturated
/// general lane. The reserved health reader either admits immediately or the
/// caller reports a typed locked/unavailable state.
const HEALTH_READER_WAIT: Duration = Duration::ZERO;

#[derive(Clone)]
enum Runtime {
    Sqlite(Arc<ExactSqlHandle>),
    Native(Arc<super::native::NativeHandle>),
}

#[derive(Clone)]
pub struct Connection {
    runtime: Runtime,
    binding: StoreRuntimeBindingV1,
    /// Priority every read issued through this handle is admitted under.
    ///
    /// Reads default to `Foreground`; a caller that knows it is bulk or
    /// maintenance work opts down with [`ReadConnection::background`], which
    /// keeps a slice of the reader pool's general lane free for interactive
    /// queries.
    read_priority: OperationPriorityV1,
}

#[derive(Clone)]
pub struct ReadConnection {
    connection: Connection,
}

impl ReadConnection {
    pub fn backend_kind(&self) -> super::BackendKind {
        self.connection.backend_kind()
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        self.connection.query(sql, params).await
    }

    pub async fn read_snapshot(&self) -> Result<ReadSnapshot> {
        self.connection.read_snapshot().await
    }

    /// Live reader-pool telemetry for this exact store.
    #[must_use]
    pub fn reader_pool_occupancy(&self) -> Option<ReaderPoolSnapshot> {
        self.connection.reader_pool_occupancy()
    }

    /// The same store, read as background work.
    ///
    /// Use this for bulk sweeps, catch-up ingest, and maintenance scans: they
    /// admit against the unreserved slice of the reader lane, so a saturating
    /// sweep cannot starve an interactive read.
    #[must_use]
    pub fn background(&self) -> Self {
        Self {
            connection: self.connection.background_reads(),
        }
    }
}

impl Connection {
    pub fn attach_native(
        database: tracedecay_turso_runtime::Database,
        binding: StoreRuntimeBindingV1,
        authority: Arc<dyn tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteAuthority>,
    ) -> Self {
        Self {
            runtime: Runtime::Native(Arc::new(super::native::NativeHandle::new(
                database,
                authority,
                tracedecay_store::AdmissionConfigV1::default().readers,
            ))),
            binding,
            read_priority: OperationPriorityV1::Foreground,
        }
    }

    pub fn attach_native_with_reader_budget(
        database: tracedecay_turso_runtime::Database,
        binding: StoreRuntimeBindingV1,
        authority: Arc<dyn tracedecay_rusqlite_runtime::exact_sql::ExactSqlWriteAuthority>,
        budget: tracedecay_store::ReaderBudgetV1,
    ) -> Result<Self> {
        budget
            .validate()
            .map_err(|error| Error::invalid_operation(error.to_string()))?;
        Ok(Self {
            runtime: Runtime::Native(Arc::new(super::native::NativeHandle::new(
                database, authority, budget,
            ))),
            binding,
            read_priority: OperationPriorityV1::Foreground,
        })
    }

    pub fn backend_kind(&self) -> super::BackendKind {
        match &self.runtime {
            Runtime::Sqlite(_) => super::BackendKind::Sqlite,
            Runtime::Native(_) => super::BackendKind::NativeTurso,
        }
    }

    fn sqlite_runtime(&self) -> Result<Arc<ExactSqlHandle>> {
        match &self.runtime {
            Runtime::Sqlite(runtime) => Ok(Arc::clone(runtime)),
            Runtime::Native(_) => Err(Error::invalid_operation(
                "operation requires a SQLite runtime",
            )),
        }
    }

    pub fn attach(runtime: ExactSqlHandle) -> Self {
        Self {
            binding: runtime.binding().clone(),
            runtime: Runtime::Sqlite(Arc::new(runtime)),
            read_priority: OperationPriorityV1::Foreground,
        }
    }

    /// The exact store identity carried by this attached engine connection.
    ///
    /// This stays within the runtime core: typed capabilities may project the
    /// identity needed to validate a purpose, but callers never receive the
    /// underlying exact-SQL runtime or handle.
    pub(crate) fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn read_only(&self) -> ReadConnection {
        ReadConnection {
            connection: self.clone(),
        }
    }

    /// The same store, with every read this handle issues admitted as background work.
    ///
    /// The returned handle shares the attached runtime and its write authority.
    /// Only admission of non-transactional `query`/`read_snapshot` calls changes;
    /// those reads use the unreserved slice of the general lane.
    ///
    /// Background maintenance that runs on a *write* connection needs this:
    /// [`Self::attach`] defaults to `Foreground`, so a bulk sweep driven from
    /// the writer would otherwise contend for the same reserved lane slice as
    /// interactive queries, and, because reader leases are bounded, be the
    /// first thing to fail once it has saturated that lane itself.
    #[must_use]
    pub fn background_reads(&self) -> Self {
        Self {
            runtime: self.runtime.clone(),
            binding: self.binding.clone(),
            read_priority: OperationPriorityV1::Background,
        }
    }

    pub async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime.execute(sql.to_owned(), params.into_params()?).await;
        }
        let statement = statement(sql, params)?;
        let runtime = self.sqlite_runtime()?;
        runtime
            .execute_async(statement)
            .await
            .map(|result| result.changed_rows as u64)
            .map_err(Into::into)
    }

    /// Executes one autocommit write and materializes its own RETURNING rows.
    /// Once admitted to the writer, the statement completes if its waiter drops.
    pub async fn execute_returning<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime
                .execute_returning(sql.to_owned(), params.into_params()?)
                .await;
        }
        let statement = statement(sql, params)?;
        let runtime = self.sqlite_runtime()?;
        runtime
            .execute_returning_async(statement)
            .await
            .map(Rows::from_exact)
            .map_err(Into::into)
    }

    #[tracing::instrument(name = "runtime_core.db.execute_statements", level = "trace", skip_all)]
    pub async fn execute_statements(&self, statements: Vec<WriteStatement>) -> Result<Vec<u64>> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime
                .execute_statements(
                    statements
                        .into_iter()
                        .map(WriteStatement::into_parts)
                        .collect(),
                )
                .await;
        }
        let statements = statements
            .into_iter()
            .map(WriteStatement::into_exact)
            .collect::<Vec<_>>();
        let runtime = self.sqlite_runtime()?;
        // Once admitted, a batch continues through its first error even if its
        // caller stops waiting. Separate dispatches preserve writer interleaving.
        tokio::spawn(async move {
            let mut results = Vec::with_capacity(statements.len());
            for (index, statement) in statements.into_iter().enumerate() {
                let result = runtime
                    .execute_async(statement)
                    .await
                    .map_err(Error::from)
                    .map_err(|error| Error::statement_batch(index, error))?;
                results.push(result.changed_rows as u64);
            }
            Ok(results)
        })
        .await
        .map_err(join_error)?
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime
                .query(sql.to_owned(), params.into_params()?, self.read_priority)
                .await;
        }
        let statement = statement(sql, params)?;
        let runtime = self.sqlite_runtime()?;
        let priority = self.read_priority;
        let rows = tokio::task::spawn_blocking(move || {
            runtime.query_with_priority(statement, priority, READER_WAIT)
        })
        .await
        .map_err(join_error)??;
        Ok(Rows::from_exact(rows))
    }

    pub async fn checkpoint_wal_truncate(&self) -> Result<Rows> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime.checkpoint().await;
        }
        let runtime = self.sqlite_runtime()?;
        let rows = runtime.checkpoint_wal_truncate_async().await?;
        Ok(Rows::from_exact(rows))
    }

    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime.execute_batch(sql.to_owned()).await;
        }
        let runtime = self.sqlite_runtime()?;
        let sql = sql.to_owned();
        runtime
            .execute_batch_async(sql)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    pub async fn release_connection_memory(&self) -> Result<MemoryReleaseOutcome> {
        if matches!(&self.runtime, Runtime::Native(_)) {
            return Err(Error::invalid_operation(
                "native Turso connection memory release is unsupported",
            ));
        }
        let runtime = self.sqlite_runtime()?;
        tokio::spawn(async move { runtime.release_connection_memory_async().await })
            .await
            .map_err(join_error)?
            .map_err(Into::into)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn prepare(&self, sql: &str) -> Result<Statement<'_>> {
        if let Runtime::Native(runtime) = &self.runtime {
            runtime.validate(sql.to_owned()).await?;
            return Statement::for_connection(self, sql);
        }
        let statement = statement(sql, ())?;
        let runtime = self.sqlite_runtime()?;
        runtime.validate_async(statement).await?;
        Statement::for_connection(self, sql)
    }

    /// Live reader-pool occupancy for the store behind this connection.
    ///
    /// Lock-free and lease-free, so it still answers while the pool is
    /// saturated, which is the only moment the numbers matter.
    #[must_use]
    pub fn reader_pool_occupancy(&self) -> Option<ReaderPoolSnapshot> {
        match &self.runtime {
            Runtime::Sqlite(runtime) => runtime.reader_pool_occupancy(),
            Runtime::Native(_) => None,
        }
    }

    #[tracing::instrument(name = "runtime_core.db.snapshot.read", level = "trace", skip_all)]
    pub async fn read_snapshot(&self) -> Result<ReadSnapshot> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime
                .snapshot(self.read_priority)
                .await
                .map(ReadSnapshot::from_native);
        }
        let runtime = self.sqlite_runtime()?;
        let priority = self.read_priority;
        tokio::task::spawn_blocking(move || {
            runtime.begin_read_snapshot_with_priority(priority, READER_WAIT)
        })
        .await
        .map_err(join_error)?
        .map(ReadSnapshot::from_runtime)
        .map_err(Into::into)
    }

    pub(crate) async fn health_read_snapshot(&self) -> Result<ReadSnapshot> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime
                .snapshot(OperationPriorityV1::Health)
                .await
                .map(ReadSnapshot::from_native);
        }
        let runtime = self.sqlite_runtime()?;
        tokio::task::spawn_blocking(move || runtime.begin_health_read_snapshot(HEALTH_READER_WAIT))
            .await
            .map_err(join_error)?
            .map(ReadSnapshot::from_runtime)
            .map_err(Into::into)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn transaction(&self) -> Result<Transaction> {
        self.transaction_with_behavior(TransactionBehavior::Deferred)
            .await
    }

    #[tracing::instrument(name = "runtime_core.db.transaction.begin", level = "trace", skip_all)]
    pub async fn transaction_with_behavior(
        &self,
        behavior: TransactionBehavior,
    ) -> Result<Transaction> {
        if let Runtime::Native(runtime) = &self.runtime {
            let native_behavior = match behavior {
                #[cfg(any(test, feature = "test-helpers"))]
                TransactionBehavior::Deferred => {
                    tracedecay_turso_runtime::TransactionBehavior::Deferred
                }
                TransactionBehavior::Immediate => {
                    tracedecay_turso_runtime::TransactionBehavior::Immediate
                }
            };
            return runtime
                .transaction(false, native_behavior)
                .await
                .map(Transaction::from_native);
        }
        match behavior {
            #[cfg(any(test, feature = "test-helpers"))]
            TransactionBehavior::Deferred => {
                let runtime = self.sqlite_runtime()?;
                runtime
                    .begin_deferred_async()
                    .await
                    .map_err(Error::from)
                    .map(Transaction::from_runtime)
            }
            TransactionBehavior::Immediate => {
                let runtime = self.sqlite_runtime()?;
                runtime
                    .begin_immediate_async()
                    .await
                    .map_err(Error::from)
                    .map(Transaction::from_runtime)
            }
        }
    }

    /// Begins the authority-bound transaction whose lease renews on progress.
    ///
    /// Reserved for schema installation on a fresh or index-less store and for
    /// full-index bulk replacement, writes that legitimately outlive one fixed
    /// lease while continuously making progress. It steps no store forward from
    /// an older shape. Only its explicit authority-revalidated batch may bypass
    /// the ordinary per-statement deadline; all other operations retain
    /// ordinary bounds, and idleness, shutdown, and authority revocation still
    /// cancel.
    #[tracing::instrument(name = "runtime_core.db.txn.long_lease", level = "trace", skip_all)]
    pub async fn authorized_long_lease_transaction(&self) -> Result<Transaction> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime
                .transaction(
                    true,
                    tracedecay_turso_runtime::TransactionBehavior::Immediate,
                )
                .await
                .map(Transaction::from_native);
        }
        let runtime = self.sqlite_runtime()?;
        runtime
            .begin_authorized_long_lease_immediate_async()
            .await
            .map_err(Error::from)
            .map(Transaction::from_runtime)
    }
}

fn join_error(error: tokio::task::JoinError) -> super::Error {
    super::Error::Runtime(format!("exact SQL worker task failed: {error}"))
}

pub(super) fn statement<P>(sql: &str, params: P) -> Result<ExactSqlStatement>
where
    P: IntoParams,
{
    ExactSqlStatement::new(
        sql.to_owned(),
        params.into_params()?.into_iter().map(Into::into).collect(),
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracedecay_rusqlite_runtime::exact_sql::{
        ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
    };

    use super::super::{Error, TestConnection};

    struct AllowWrites;

    impl ExactSqlWriteAuthority for AllowWrites {
        fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn long_lease_entrypoint_requires_attached_write_authority() {
        let directory = tempfile::TempDir::new().unwrap();
        let plain =
            TestConnection::open_without_write_authority(&directory.path().join("plain.sqlite3"));

        let error = match plain.authorized_long_lease_transaction().await {
            Ok(_) => panic!("long-lease transaction must require attached authority"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            Error::InvalidOperation(_) | Error::Runtime(_)
        ));

        let authorized = TestConnection::open_with_write_authority(
            &directory.path().join("authorized.sqlite3"),
            Arc::new(AllowWrites),
        );
        authorized
            .authorized_long_lease_transaction()
            .await
            .unwrap()
            .rollback()
            .await
            .unwrap();
    }
}
