use std::future::Future;
use std::sync::Arc;

use asupersync::Budget;
use asupersync::sync::{Mutex, OwnedMutexGuard};

use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlJobTransaction, ExactSqlTransaction as RuntimeTransaction,
};

use super::{
    Error, Executor, IntoParams, QueryExecutor, Result, Rows, WriteStatement, connection::statement,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionBehavior {
    #[cfg(any(test, feature = "test-helpers"))]
    Deferred,
    Immediate,
}

pub struct Transaction {
    /// An owned async operation holds this gate until the writer acknowledges
    /// its command. Dropping the caller cannot release serialization early or
    /// truncate an already admitted statement batch.
    runtime: Arc<Mutex<Option<RuntimeTransaction>>>,
}

impl Transaction {
    pub(super) fn from_runtime(runtime: RuntimeTransaction) -> Self {
        Self {
            runtime: Arc::new(Mutex::new(Some(runtime))),
        }
    }

    /// Runs one operation that holds the transaction gate. Dropping the
    /// caller's future detaches, never truncates, it.
    async fn settle<T, F, Fut>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(TransactionGate) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let runtime = Arc::clone(&self.runtime);
        crate::native_runtime::settle(async move {
            let gate = match OwnedMutexGuard::try_lock(Arc::clone(&runtime)) {
                Ok(gate) => gate,
                Err(_) => {
                    // Not the caller's `Cx`: cancelling the caller must not
                    // cancel an operation it already handed over.
                    let cx = crate::native_runtime::handle()
                        .ok_or(Error::RuntimeContextUnavailable)?
                        .try_request_cx_with_budget(Budget::INFINITE)
                        .map_err(|error| {
                            Error::Runtime(format!("failed to admit SQL settlement: {error}"))
                        })?;
                    OwnedMutexGuard::lock(runtime, &cx).await.map_err(|error| {
                        Error::Runtime(format!("SQL transaction gate failed: {error}"))
                    })?
                }
            };
            operation(gate).await
        })
        .await
    }

    pub async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        let statement = statement(sql, params)?;
        self.settle(move |gate| async move {
            let result = open(&gate)?.execute_async(statement).await?;
            Ok(result.changed_rows as u64)
        })
        .await
    }

    #[tracing::instrument(
        name = "runtime_core.db.transaction.execute_statements",
        level = "trace",
        skip_all
    )]
    pub async fn execute_statements(&self, statements: Vec<WriteStatement>) -> Result<Vec<u64>> {
        let statements = statements
            .into_iter()
            .map(WriteStatement::into_exact)
            .collect::<Vec<_>>();
        self.settle(move |gate| async move {
            let runtime = open(&gate)?;
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
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let statement = statement(sql, params)?;
        self.settle(move |gate| async move {
            Ok(Rows::from_exact(open(&gate)?.query_async(statement).await?))
        })
        .await
    }

    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        let sql = sql.to_owned();
        self.settle(move |gate| async move {
            open(&gate)?.execute_batch_async(sql).await?;
            Ok(())
        })
        .await
    }

    /// Runs one query under the authority-revalidated policy, without the
    /// ordinary statement deadline.
    pub async fn query_authority_revalidated<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let statement = statement(sql, params)?;
        self.settle(move |gate| async move {
            Ok(Rows::from_exact(
                open(&gate)?
                    .query_authority_revalidated_async(statement)
                    .await?,
            ))
        })
        .await
    }

    /// Executes one separately authorized authority-revalidated batch without the ordinary
    /// statement deadline.
    pub async fn execute_authority_revalidated_batch(&self, sql: &str) -> Result<()> {
        let sql = sql.to_owned();
        self.settle(move |gate| async move {
            open(&gate)?
                .execute_authority_revalidated_batch_async(sql)
                .await?;
            Ok(())
        })
        .await
    }

    pub async fn validate(&self, sql: &str) -> Result<()> {
        let statement = statement(sql, ())?;
        self.settle(move |gate| async move { Ok(open(&gate)?.validate_async(statement).await?) })
            .await
    }

    pub async fn commit(self) -> Result<()> {
        self.settle(|mut gate| async move {
            let transaction = gate.take().ok_or(Error::TransactionClosed)?;
            transaction.commit_async().await?;
            Ok(())
        })
        .await
    }

    /// The writer owns completion resources even if this future is dropped.
    pub async fn commit_with_on_commit<F>(self, on_commit: F) -> Result<()>
    where
        F: FnOnce() + Send + 'static,
    {
        self.settle(move |mut gate| async move {
            let transaction = gate.take().ok_or(Error::TransactionClosed)?;
            transaction.commit_async_with_on_commit(on_commit).await?;
            Ok(())
        })
        .await
    }

    pub async fn rollback(self) -> Result<()> {
        self.settle(|mut gate| async move {
            let transaction = gate.take().ok_or(Error::TransactionClosed)?;
            transaction.rollback_async().await?;
            Ok(())
        })
        .await
    }
}

/// One write job's transaction, seen through the executor traits so the
/// async helpers written against them run inside the job unchanged. Each
/// statement runs synchronously on the writer, so these futures are ready on
/// their first poll.
pub struct WriteTx<'a, 'b> {
    job: &'a ExactSqlJobTransaction<'b>,
}

impl<'a, 'b> WriteTx<'a, 'b> {
    pub(super) const fn new(job: &'a ExactSqlJobTransaction<'b>) -> Self {
        Self { job }
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "A statement must run when its future is polled inside the job, not when it is built."
)]
impl QueryExecutor for WriteTx<'_, '_> {
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        Ok(Rows::from_exact(self.job.query(statement(sql, params)?)?))
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "A statement must run when its future is polled inside the job, not when it is built."
)]
impl Executor for WriteTx<'_, '_> {
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        Ok(self.job.execute(statement(sql, params)?)?.changed_rows as u64)
    }

    async fn execute_batch(&self, sql: &str) -> Result<()> {
        self.job.execute_batch(sql.to_owned())?;
        Ok(())
    }
}

type TransactionGate = OwnedMutexGuard<Option<RuntimeTransaction>>;

fn open(gate: &TransactionGate) -> Result<&RuntimeTransaction> {
    gate.as_ref().ok_or(Error::TransactionClosed)
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

    #[asupersync::test]
    async fn only_long_lease_transaction_exposes_authority_revalidated_batches() {
        let directory = tempfile::TempDir::new().unwrap();
        let connection = TestConnection::open_with_write_authority(
            &directory.path().join("engine.sqlite3"),
            Arc::new(AllowWrites),
        );
        let ordinary = connection.transaction().await.unwrap();

        let error = ordinary
            .execute_authority_revalidated_batch("CREATE TABLE forbidden (id INTEGER)")
            .await
            .unwrap_err();

        assert!(matches!(error, Error::InvalidOperation(_)));
        ordinary.rollback().await.unwrap();

        let long_lease = connection
            .authorized_long_lease_transaction()
            .await
            .unwrap();
        long_lease
            .execute_authority_revalidated_batch("CREATE TABLE allowed (id INTEGER)")
            .await
            .unwrap();
        long_lease.commit().await.unwrap();
    }
}
