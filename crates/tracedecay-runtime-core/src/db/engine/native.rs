use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tracedecay_rusqlite_runtime::exact_sql::{ExactSqlWriteAuthority, ExactSqlWriteIntent};
use tracedecay_store::{OperationPriorityV1, ReaderBudgetV1};
use tracedecay_turso_runtime::{Access, Database, ExecutionGuard};

use super::{Error, Result, Rows, Value};

const STATEMENT_LIMIT: Duration = Duration::from_secs(30);
const TRANSACTION_LIMIT: Duration = Duration::from_secs(120);
const IDLE_LIMIT: Duration = Duration::from_secs(30);
const READER_WAIT: Duration = Duration::from_secs(5);
const FOREGROUND_RESERVED_READERS: u16 = 2;

pub(super) struct NativeHandle {
    database: Database,
    authority: Arc<dyn ExactSqlWriteAuthority>,
    general_readers: Arc<Semaphore>,
    health_reader: Arc<Semaphore>,
    reader_released: Arc<Notify>,
    background_reservation: u16,
}

impl NativeHandle {
    pub(super) fn new(
        database: Database,
        authority: Arc<dyn ExactSqlWriteAuthority>,
        budget: ReaderBudgetV1,
    ) -> Self {
        Self {
            database,
            authority,
            general_readers: Arc::new(Semaphore::new(usize::from(budget.max_per_hot_shard))),
            health_reader: Arc::new(Semaphore::new(1)),
            reader_released: Arc::new(Notify::new()),
            background_reservation: FOREGROUND_RESERVED_READERS
                .min(budget.max_per_hot_shard.saturating_sub(1)),
        }
    }

    async fn reader_lease(&self, priority: OperationPriorityV1) -> Result<NativeReaderLease> {
        let permit = match priority {
            OperationPriorityV1::Health => Arc::clone(&self.health_reader)
                .try_acquire_owned()
                .map_err(|_| Error::Busy)?,
            OperationPriorityV1::Foreground => tokio::time::timeout(
                READER_WAIT,
                Arc::clone(&self.general_readers).acquire_owned(),
            )
            .await
            .map_err(|_| Error::Busy)?
            .map_err(|_| Error::invalid_operation("native reader admission is closed"))?,
            OperationPriorityV1::Background => {
                let deadline = tokio::time::Instant::now() + READER_WAIT;
                loop {
                    // A background waiter must not queue a multi-permit acquire
                    // ahead of a foreground read. Try the reservation atomically
                    // and wait for a released lease outside the semaphore queue.
                    let notification = self.reader_released.notified();
                    tokio::pin!(notification);
                    notification.as_mut().enable();
                    if let Ok(mut reservation) = Arc::clone(&self.general_readers)
                        .try_acquire_many_owned(u32::from(self.background_reservation) + 1)
                    {
                        let permit = reservation.split(1).ok_or_else(|| {
                            Error::invalid_operation("native reader reservation was empty")
                        })?;
                        drop(reservation);
                        break permit;
                    }
                    tokio::time::timeout_at(deadline, notification)
                        .await
                        .map_err(|_| Error::Busy)?;
                }
            }
        };
        Ok(NativeReaderLease {
            permit: Some(permit),
            released: Arc::clone(&self.reader_released),
        })
    }

    pub(super) async fn query(
        self: &Arc<Self>,
        sql: String,
        params: Vec<Value>,
        priority: OperationPriorityV1,
    ) -> Result<Rows> {
        let lease = self.reader_lease(priority).await?;
        let this = Arc::clone(self);
        run(move |cancelled| {
            let _lease = lease;
            let mut connection = this.database.connect(Access::Reader)?;
            let guard = this.guard(ExactSqlWriteIntent::Query, cancelled, true);
            connection
                .query(&sql, &validate_and_convert_params(&sql, params)?, &guard)
                .map(Rows::from_native)
                .map_err(Into::into)
        })
        .await
    }

    pub(super) async fn execute(self: &Arc<Self>, sql: String, params: Vec<Value>) -> Result<u64> {
        let this = Arc::clone(self);
        run(move |cancelled| {
            let mut connection = this.database.connect(Access::Writer)?;
            let guard = this.guard(ExactSqlWriteIntent::Execute, cancelled, true);
            connection
                .execute(&sql, &validate_and_convert_params(&sql, params)?, &guard)
                .map_err(Into::into)
        })
        .await
    }

    pub(super) async fn execute_returning(
        self: &Arc<Self>,
        sql: String,
        params: Vec<Value>,
    ) -> Result<Rows> {
        let parameters = validate_and_convert_params(&sql, params)?;
        self.authority.verify(ExactSqlWriteIntent::Execute)?;
        let this = Arc::clone(self);
        run_admitted_write("native returning-write worker failed", async move {
            run(move |cancelled| {
                let mut connection = this.database.connect(Access::Writer)?;
                let guard = this.guard(ExactSqlWriteIntent::Execute, cancelled, true);
                connection.begin(
                    tracedecay_turso_runtime::TransactionBehavior::Deferred,
                    &guard,
                )?;
                let result = connection
                    .query(&sql, &parameters, &guard)
                    .and_then(|rows| {
                        connection.commit(&guard)?;
                        Ok(rows)
                    });
                match result {
                    Ok(rows) => Ok(Rows::from_native(rows)),
                    Err(error) => {
                        connection.rollback()?;
                        Err(error.into())
                    }
                }
            })
            .await
        })
        .await
    }

    pub(super) async fn execute_statements(
        self: &Arc<Self>,
        statements: Vec<(String, Vec<Value>)>,
    ) -> Result<Vec<u64>> {
        let this = Arc::clone(self);
        run_admitted_write("native write batch worker failed", async move {
            let mut results = Vec::with_capacity(statements.len());
            for (index, (sql, params)) in statements.into_iter().enumerate() {
                results.push(
                    this.execute(sql, params)
                        .await
                        .map_err(|error| Error::statement_batch(index, error))?,
                );
            }
            Ok(results)
        })
        .await
    }

    pub(super) async fn execute_batch(self: &Arc<Self>, sql: String) -> Result<()> {
        let this = Arc::clone(self);
        run(move |cancelled| {
            let mut connection = this.database.connect(Access::Writer)?;
            let guard = this.guard(ExactSqlWriteIntent::ExecuteBatch, cancelled, true);
            connection.execute_batch(&sql, &guard).map_err(Into::into)
        })
        .await
    }

    pub(super) async fn checkpoint(self: &Arc<Self>) -> Result<Rows> {
        let this = Arc::clone(self);
        run(move |cancelled| {
            let guard = this.guard(ExactSqlWriteIntent::Query, cancelled, true);
            this.database
                .connect(Access::Writer)?
                .checkpoint(&guard)
                .map(Rows::from_native)
                .map_err(Into::into)
        })
        .await
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub(super) async fn validate(self: &Arc<Self>, sql: String) -> Result<()> {
        let this = Arc::clone(self);
        run(move |cancelled| {
            let mut connection = this.database.connect(Access::Writer)?;
            let guard = this.guard(ExactSqlWriteIntent::Validate, cancelled, true);
            connection.validate(&sql, &guard).map_err(Into::into)
        })
        .await
    }

    pub(super) async fn transaction(
        self: &Arc<Self>,
        long_lease: bool,
        behavior: tracedecay_turso_runtime::TransactionBehavior,
    ) -> Result<Arc<NativeTransaction>> {
        let this = Arc::clone(self);
        let transaction = run(move |cancelled| {
            let mut connection = this.database.connect(Access::Writer)?;
            let guard = this.guard(ExactSqlWriteIntent::BeginTransaction, cancelled, true);
            connection.begin(behavior, &guard)?;
            Ok(Arc::new(NativeTransaction {
                connection: Mutex::new(Some(connection)),
                handle: this,
                started: Instant::now(),
                last_progress: Arc::new(Mutex::new(Instant::now())),
                long_lease,
                expired: AtomicBool::new(false),
            }))
        })
        .await?;
        NativeTransaction::watch_and_release_expired_transaction(&transaction);
        Ok(transaction)
    }

    pub(super) async fn snapshot(
        self: &Arc<Self>,
        priority: OperationPriorityV1,
    ) -> Result<Arc<NativeSnapshot>> {
        let lease = self.reader_lease(priority).await?;
        let this = Arc::clone(self);
        run(move |cancelled| {
            let mut connection = this.database.connect(Access::Reader)?;
            let guard = this.guard(ExactSqlWriteIntent::Query, cancelled, true);
            connection.begin(
                tracedecay_turso_runtime::TransactionBehavior::Deferred,
                &guard,
            )?;
            // BEGIN alone may defer snapshot acquisition until the first read.
            // Establish it before returning the retained snapshot to its caller.
            connection.query("SELECT name FROM sqlite_schema LIMIT 1", &[], &guard)?;
            Ok(Arc::new(NativeSnapshot {
                state: Mutex::new(NativeSnapshotState {
                    connection: Some(connection),
                    lease: Some(lease),
                }),
                handle: this,
            }))
        })
        .await
    }

    fn guard(
        &self,
        intent: ExactSqlWriteIntent,
        cancelled: Arc<AtomicBool>,
        bounded: bool,
    ) -> ExecutionGuard {
        let authority = Arc::clone(&self.authority);
        ExecutionGuard::new(
            bounded.then(|| Instant::now() + STATEMENT_LIMIT),
            cancelled,
            Some(Arc::new(move || {
                authority.verify(intent).map_err(|error| error.to_string())
            })),
        )
    }
}

pub(super) struct NativeSnapshot {
    state: Mutex<NativeSnapshotState>,
    handle: Arc<NativeHandle>,
}

struct NativeSnapshotState {
    connection: Option<tracedecay_turso_runtime::Connection>,
    lease: Option<NativeReaderLease>,
}

impl NativeSnapshot {
    pub(super) async fn query(self: &Arc<Self>, sql: String, params: Vec<Value>) -> Result<Rows> {
        let this = Arc::clone(self);
        run(move |cancelled| {
            let guard = this
                .handle
                .guard(ExactSqlWriteIntent::Query, cancelled, true);
            let mut state = this.state.lock().map_err(lock_error)?;
            let connection = state.connection.as_mut().ok_or(Error::TransactionClosed)?;
            let result =
                connection.query(&sql, &validate_and_convert_params(&sql, params)?, &guard);
            if connection.is_autocommit() || result.as_ref().is_err_and(terminal_transaction_error)
            {
                state.connection.take();
                state.lease.take();
            }
            result.map(Rows::from_native).map_err(Into::into)
        })
        .await
    }
}

pub(super) struct NativeTransaction {
    connection: Mutex<Option<tracedecay_turso_runtime::Connection>>,
    handle: Arc<NativeHandle>,
    started: Instant,
    last_progress: Arc<Mutex<Instant>>,
    long_lease: bool,
    expired: AtomicBool,
}

impl NativeTransaction {
    fn watch_and_release_expired_transaction(transaction: &Arc<Self>) {
        let transaction = Arc::downgrade(transaction);
        tokio::spawn(async move {
            loop {
                let Some(current) = transaction.upgrade() else {
                    return;
                };
                let idle_remaining = match current.last_progress.lock() {
                    Ok(progress) => IDLE_LIMIT.saturating_sub(progress.elapsed()),
                    Err(_) => Duration::ZERO,
                };
                let remaining = if current.long_lease {
                    idle_remaining
                } else {
                    idle_remaining.min(TRANSACTION_LIMIT.saturating_sub(current.started.elapsed()))
                };
                if remaining.is_zero() {
                    current.expired.store(true, Ordering::Release);
                    let cleanup = tokio::task::spawn_blocking(move || {
                        let mut connection = current.connection.lock().map_err(lock_error)?;
                        connection.take();
                        Ok::<(), Error>(())
                    })
                    .await;
                    match cleanup {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            tracing::warn!(%error, "native expired transaction cleanup failed");
                        }
                        Err(error) => {
                            tracing::warn!(%error, "native transaction expiry worker failed");
                        }
                    }
                    return;
                }
                drop(current);
                tokio::time::sleep(remaining).await;
            }
        });
    }
    fn operation<T>(
        &self,
        intent: ExactSqlWriteIntent,
        cancelled: Arc<AtomicBool>,
        bounded: bool,
        operation: impl FnOnce(
            &mut tracedecay_turso_runtime::Connection,
            &ExecutionGuard,
        ) -> tracedecay_turso_runtime::Result<T>,
    ) -> Result<T> {
        let mut owned = self.connection.lock().map_err(lock_error)?;
        let guard = match self.guard(intent, cancelled, bounded) {
            Ok(guard) => guard,
            Err(error) => {
                owned.take();
                return Err(error);
            }
        };
        let connection = owned.as_mut().ok_or(Error::TransactionClosed)?;
        let result = operation(connection, &guard);
        let ended = connection.is_autocommit();
        if ended || result.as_ref().is_err_and(terminal_transaction_error) {
            owned.take();
        }
        result.map_err(Into::into)
    }

    pub(super) async fn execute_statements(
        self: &Arc<Self>,
        statements: Vec<(String, Vec<Value>)>,
    ) -> Result<Vec<u64>> {
        let this = Arc::clone(self);
        run_admitted_write("native transaction batch worker failed", async move {
            run(move |cancelled| {
                let mut owned = this.connection.lock().map_err(lock_error)?;
                let mut results = Vec::with_capacity(statements.len());
                for (index, (sql, params)) in statements.into_iter().enumerate() {
                    let guard = match this.guard(
                        ExactSqlWriteIntent::Execute,
                        Arc::clone(&cancelled),
                        true,
                    ) {
                        Ok(guard) => guard,
                        Err(error) => {
                            owned.take();
                            return Err(Error::statement_batch(index, error));
                        }
                    };
                    let connection = owned.as_mut().ok_or(Error::TransactionClosed)?;
                    let result = connection.execute(
                        &sql,
                        &validate_and_convert_params(&sql, params)?,
                        &guard,
                    );
                    let ended = connection.is_autocommit();
                    if ended || result.as_ref().is_err_and(terminal_transaction_error) {
                        owned.take();
                    }
                    results.push(
                        result
                            .map_err(Error::from)
                            .map_err(|error| Error::statement_batch(index, error))?,
                    );
                }
                Ok(results)
            })
            .await
        })
        .await
    }
    fn guard(
        &self,
        intent: ExactSqlWriteIntent,
        cancelled: Arc<AtomicBool>,
        bounded: bool,
    ) -> Result<ExecutionGuard> {
        if self.expired.load(Ordering::Acquire)
            || self.last_progress.lock().map_err(lock_error)?.elapsed() >= IDLE_LIMIT
            || (!self.long_lease && self.started.elapsed() >= TRANSACTION_LIMIT)
        {
            return Err(Error::TransactionExpired);
        }
        let authority = Arc::clone(&self.handle.authority);
        let progress = Arc::clone(&self.last_progress);
        let deadline = if bounded {
            Some(Instant::now() + STATEMENT_LIMIT)
        } else {
            None
        };
        let deadline = if self.long_lease {
            deadline
        } else {
            Some(deadline.map_or(self.started + TRANSACTION_LIMIT, |limit| {
                limit.min(self.started + TRANSACTION_LIMIT)
            }))
        };
        Ok(ExecutionGuard::new(
            deadline,
            cancelled,
            Some(Arc::new(move || {
                authority
                    .verify(intent)
                    .map_err(|error| error.to_string())?;
                *progress
                    .lock()
                    .map_err(|_| "native transaction progress lock poisoned".to_owned())? =
                    Instant::now();
                Ok(())
            })),
        ))
    }

    pub(super) async fn execute(self: &Arc<Self>, sql: String, params: Vec<Value>) -> Result<u64> {
        let this = Arc::clone(self);
        run(move |cancelled| {
            this.operation(
                ExactSqlWriteIntent::Execute,
                cancelled,
                true,
                |connection, guard| {
                    connection.execute(&sql, &validate_and_convert_params(&sql, params)?, guard)
                },
            )
        })
        .await
    }

    pub(super) async fn query(self: &Arc<Self>, sql: String, params: Vec<Value>) -> Result<Rows> {
        let this = Arc::clone(self);
        run(move |cancelled| {
            this.operation(
                ExactSqlWriteIntent::Query,
                cancelled,
                true,
                |connection, guard| {
                    connection.query(&sql, &validate_and_convert_params(&sql, params)?, guard)
                },
            )
            .map(Rows::from_native)
        })
        .await
    }

    pub(super) async fn execute_batch(
        self: &Arc<Self>,
        sql: String,
        authority_revalidated: bool,
    ) -> Result<()> {
        if authority_revalidated && !self.long_lease {
            return Err(Error::invalid_operation(
                "authority-revalidated batch requires a long-lease transaction",
            ));
        }
        let this = Arc::clone(self);
        run(move |cancelled| {
            this.operation(
                ExactSqlWriteIntent::ExecuteBatch,
                cancelled,
                !authority_revalidated,
                |connection, guard| connection.execute_batch(&sql, guard),
            )
        })
        .await
    }

    pub(super) async fn validate(self: &Arc<Self>, sql: String) -> Result<()> {
        let this = Arc::clone(self);
        run(move |cancelled| {
            this.operation(
                ExactSqlWriteIntent::Validate,
                cancelled,
                true,
                |connection, guard| connection.validate(&sql, guard),
            )
        })
        .await
    }

    pub(super) async fn finish(self: &Arc<Self>, commit: bool) -> Result<()> {
        let this = Arc::clone(self);
        run(move |cancelled| {
            let mut connection = this
                .connection
                .lock()
                .map_err(lock_error)?
                .take()
                .ok_or(Error::TransactionClosed)?;
            if commit {
                let guard = this.guard(ExactSqlWriteIntent::Commit, cancelled, true)?;
                connection.commit(&guard).map_err(Into::into)
            } else {
                connection.rollback().map_err(Into::into)
            }
        })
        .await
    }
}

fn validate_and_convert_params(
    sql: &str,
    values: Vec<Value>,
) -> tracedecay_turso_runtime::Result<Vec<tracedecay_turso_runtime::Value>> {
    let request = tracedecay_rusqlite_runtime::exact_sql::ExactSqlStatement::new(
        sql.to_owned(),
        values.into_iter().map(Into::into).collect(),
    )
    .map_err(|error| match error {
        tracedecay_rusqlite_runtime::exact_sql::ExactSqlError::RequestLimitExceeded => {
            tracedecay_turso_runtime::Error::RequestLimitExceeded
        }
        error => tracedecay_turso_runtime::Error::InvalidOperation(error.to_string()),
    })?;
    Ok(request
        .params
        .into_iter()
        .map(Value::from)
        .map(Into::into)
        .collect())
}

fn lock_error<T>(_error: std::sync::PoisonError<T>) -> Error {
    Error::Runtime("native connection lock poisoned".to_owned())
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

async fn run<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(Arc<AtomicBool>) -> Result<T> + Send + 'static,
{
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation = CancelOnDrop(Arc::clone(&cancelled));
    let result = tokio::task::spawn_blocking(move || operation(cancelled))
        .await
        .map_err(|error| Error::Runtime(format!("native SQL worker failed: {error}")))?;
    drop(cancellation);
    result
}

async fn run_admitted_write<T, F>(worker_error: &str, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: Future<Output = Result<T>> + Send + 'static,
{
    tokio::spawn(operation)
        .await
        .map_err(|error| Error::Runtime(format!("{worker_error}: {error}")))?
}

struct NativeReaderLease {
    permit: Option<OwnedSemaphorePermit>,
    released: Arc<Notify>,
}

impl Drop for NativeReaderLease {
    fn drop(&mut self) {
        self.permit.take();
        self.released.notify_waiters();
    }
}

fn terminal_transaction_error(error: &tracedecay_turso_runtime::Error) -> bool {
    matches!(
        error,
        tracedecay_turso_runtime::Error::Authority(_)
            | tracedecay_turso_runtime::Error::Cancelled
            | tracedecay_turso_runtime::Error::DeadlineExceeded
    ) || error.requires_transaction_retry()
}
