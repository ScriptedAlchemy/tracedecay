use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlAttachment, ExactSqlTransaction as RuntimeTransaction,
};

use crate::profiled_lock::{ProfiledMutex, ProfiledMutexGuard};

use super::{
    Error, IntoParams, Result, Rows, Value, WriteStatement,
    connection::{Runtime, statement},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionBehavior {
    #[cfg(any(test, feature = "test-helpers"))]
    Deferred,
    Immediate,
}

pub struct Transaction {
    /// Serializes every statement issued against one open write transaction.
    /// The lock is held across the blocking rusqlite call, so it is where a
    /// transaction that has already won `BEGIN IMMEDIATE` makes its remaining
    /// statements queue behind each other.
    runtime: Arc<ProfiledMutex<Option<RuntimeTransaction>>>,
    #[cfg(any(test, feature = "test-helpers"))]
    connection_runtime: Arc<dyn Runtime>,
}

impl Transaction {
    pub(super) fn from_runtime(
        runtime: RuntimeTransaction,
        connection_runtime: Arc<dyn Runtime>,
    ) -> Self {
        #[cfg(not(any(test, feature = "test-helpers")))]
        let _ = connection_runtime;
        Self {
            runtime: Arc::new(hotpath::mutex!(
                Mutex::new(Some(runtime)),
                label = "runtime_core.db.transaction.lock"
            )),
            #[cfg(any(test, feature = "test-helpers"))]
            connection_runtime,
        }
    }

    #[hotpath::skip]
    pub async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        let runtime = Arc::clone(&self.runtime);
        let statement = statement(sql, params)?;
        tokio::task::spawn_blocking(move || {
            lock_runtime(&runtime)?
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .execute(statement)
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
        .map(|result| result.changed_rows as u64)
    }

    #[hotpath::measure(
        label = "runtime_core.db.transaction.execute_statements",
        future = true
    )]
    pub async fn execute_statements(&self, statements: Vec<WriteStatement>) -> Result<Vec<u64>> {
        let runtime = Arc::clone(&self.runtime);
        let statements = statements
            .into_iter()
            .map(WriteStatement::into_exact)
            .collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            let runtime = lock_runtime(&runtime)?;
            let runtime = runtime.as_ref().ok_or(Error::TransactionClosed)?;
            let mut results = Vec::with_capacity(statements.len());
            for (index, statement) in statements.into_iter().enumerate() {
                let result = runtime
                    .execute(statement)
                    .map_err(Error::from)
                    .map_err(|error| Error::statement_batch(index, error))?;
                results.push(result.changed_rows as u64);
            }
            Ok(results)
        })
        .await
        .map_err(join_error)?
    }

    #[hotpath::skip]
    pub async fn attach_database(&self, path: &Path, database_name: &str) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
        let filename = path.to_str().ok_or_else(|| {
            super::Error::invalid_operation("SQLite attachment path is not valid UTF-8")
        })?;
        let attachment = ExactSqlAttachment::new(filename.to_owned(), database_name.to_owned())?;
        tokio::task::spawn_blocking(move || {
            lock_runtime(&runtime)?
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .attach_database(attachment)
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    #[hotpath::skip]
    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let runtime = Arc::clone(&self.runtime);
        let statement = statement(sql, params)?;
        let rows = tokio::task::spawn_blocking(move || {
            lock_runtime(&runtime)?
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
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

    #[hotpath::skip]
    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            lock_runtime(&runtime)?
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .execute_batch(sql)
                .map(|_| ())
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    /// Executes one separately authorized authority-revalidated batch without the ordinary
    /// statement deadline.
    #[hotpath::skip]
    pub async fn execute_authority_revalidated_batch(&self, sql: &str) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            lock_runtime(&runtime)?
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .execute_authority_revalidated_batch(sql)
                .map(|_| ())
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    #[hotpath::skip]
    pub async fn validate(&self, sql: &str) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
        let statement = statement(sql, ())?;
        tokio::task::spawn_blocking(move || {
            lock_runtime(&runtime)?
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .validate(statement)
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn last_insert_rowid(&self) -> i64 {
        self.connection_runtime.last_insert_rowid()
    }

    #[hotpath::skip]
    pub async fn commit(self) -> Result<()> {
        let runtime = self.take_runtime()?;
        tokio::task::spawn_blocking(move || {
            runtime.commit().map(|_| ()).map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    #[hotpath::skip]
    pub async fn rollback(self) -> Result<()> {
        let runtime = self.take_runtime()?;
        tokio::task::spawn_blocking(move || {
            runtime.rollback().map(|_| ()).map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    fn take_runtime(&self) -> Result<RuntimeTransaction> {
        lock_runtime(&self.runtime)?
            .take()
            .ok_or(super::Error::TransactionClosed)
    }
}

#[hotpath::measure]
fn lock_runtime<T>(runtime: &ProfiledMutex<T>) -> Result<ProfiledMutexGuard<'_, T>> {
    runtime
        .lock()
        .map_err(|_| super::Error::Runtime("exact SQL transaction lock poisoned".to_owned()))
}

#[hotpath::measure]
fn join_error(error: tokio::task::JoinError) -> super::Error {
    super::Error::Runtime(format!("exact SQL transaction task failed: {error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracedecay_rusqlite_runtime::exact_sql::{
        ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
    };

    use super::{
        super::{Error, TestConnection},
        lock_runtime,
    };

    struct AllowWrites;

    impl ExactSqlWriteAuthority for AllowWrites {
        fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
            Ok(())
        }
    }

    #[test]
    fn poisoned_transaction_lock_returns_a_typed_error() {
        let runtime = hotpath::mutex!(Mutex::new(()), label = "runtime_core.db.transaction.lock");
        let _ = std::panic::catch_unwind(|| {
            let _guard = runtime.lock().unwrap();
            panic!("poison transaction lock");
        });

        let result = lock_runtime(&runtime);
        let Err(Error::Runtime(message)) = result else {
            panic!("poisoned transaction lock must return a runtime error");
        };
        assert_eq!(message, "exact SQL transaction lock poisoned");
    }

    #[tokio::test]
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
