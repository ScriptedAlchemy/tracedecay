use std::{path::Path, sync::Arc};

use tokio::sync::Mutex;

use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlAttachment, ExactSqlTransaction as RuntimeTransaction,
};

use super::{Error, IntoParams, Result, Rows, WriteStatement, connection::statement};

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
    runtime: Runtime,
}

enum Runtime {
    Sqlite(Arc<Mutex<Option<RuntimeTransaction>>>),
    Native(Arc<super::native::NativeTransaction>),
}

impl Transaction {
    pub(super) fn from_native(runtime: Arc<super::native::NativeTransaction>) -> Self {
        Self {
            runtime: Runtime::Native(runtime),
        }
    }

    pub fn backend_kind(&self) -> super::BackendKind {
        match &self.runtime {
            Runtime::Sqlite(_) => super::BackendKind::Sqlite,
            Runtime::Native(_) => super::BackendKind::NativeTurso,
        }
    }

    fn sqlite_runtime(&self) -> Result<Arc<Mutex<Option<RuntimeTransaction>>>> {
        match &self.runtime {
            Runtime::Sqlite(transaction) => Ok(Arc::clone(transaction)),
            Runtime::Native(_) => Err(Error::invalid_operation(
                "operation requires a SQLite transaction",
            )),
        }
    }

    pub(super) fn from_runtime(runtime: RuntimeTransaction) -> Self {
        Self {
            runtime: Runtime::Sqlite(Arc::new(Mutex::new(Some(runtime)))),
        }
    }

    pub async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime.execute(sql.to_owned(), params.into_params()?).await;
        }
        let runtime = self.sqlite_runtime()?;
        let statement = statement(sql, params)?;
        tokio::spawn(async move {
            runtime
                .lock()
                .await
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .execute_async(statement)
                .await
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
        .map(|result| result.changed_rows as u64)
    }

    #[tracing::instrument(
        name = "runtime_core.db.transaction.execute_statements",
        level = "trace",
        skip_all
    )]
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
        let runtime = self.sqlite_runtime()?;
        let statements = statements
            .into_iter()
            .map(WriteStatement::into_exact)
            .collect::<Vec<_>>();
        tokio::spawn(async move {
            let runtime = runtime.lock().await;
            let runtime = runtime.as_ref().ok_or(Error::TransactionClosed)?;
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

    pub async fn attach_database(&self, path: &Path, database_name: &str) -> Result<()> {
        if matches!(&self.runtime, Runtime::Native(_)) {
            return Err(Error::invalid_operation(
                "native Turso database attachment is unsupported",
            ));
        }
        let runtime = self.sqlite_runtime()?;
        let filename = path.to_str().ok_or_else(|| {
            super::Error::invalid_operation("SQLite attachment path is not valid UTF-8")
        })?;
        let attachment = ExactSqlAttachment::new(filename.to_owned(), database_name.to_owned())?;
        tokio::spawn(async move {
            runtime
                .lock()
                .await
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .attach_database_async(attachment)
                .await
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime.query(sql.to_owned(), params.into_params()?).await;
        }
        let runtime = self.sqlite_runtime()?;
        let statement = statement(sql, params)?;
        let rows = tokio::spawn(async move {
            runtime
                .lock()
                .await
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .query_async(statement)
                .await
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)??;
        Ok(Rows::from_exact(rows))
    }

    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime.execute_batch(sql.to_owned(), false).await;
        }
        let runtime = self.sqlite_runtime()?;
        let sql = sql.to_owned();
        tokio::spawn(async move {
            runtime
                .lock()
                .await
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .execute_batch_async(sql)
                .await
                .map(|_| ())
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    /// Executes one separately authorized authority-revalidated batch without the ordinary
    /// statement deadline.
    pub async fn execute_authority_revalidated_batch(&self, sql: &str) -> Result<()> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime.execute_batch(sql.to_owned(), true).await;
        }
        let runtime = self.sqlite_runtime()?;
        let sql = sql.to_owned();
        tokio::spawn(async move {
            runtime
                .lock()
                .await
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .execute_authority_revalidated_batch_async(sql)
                .await
                .map(|_| ())
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    pub async fn validate(&self, sql: &str) -> Result<()> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime.validate(sql.to_owned()).await;
        }
        let runtime = self.sqlite_runtime()?;
        let statement = statement(sql, ())?;
        tokio::spawn(async move {
            runtime
                .lock()
                .await
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .validate_async(statement)
                .await
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    pub async fn commit(self) -> Result<()> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime.finish(true).await;
        }
        let runtime = self.sqlite_runtime()?;
        tokio::spawn(async move {
            let transaction = runtime
                .lock()
                .await
                .take()
                .ok_or(Error::TransactionClosed)?;
            transaction
                .commit_async()
                .await
                .map(|_| ())
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

    pub async fn rollback(self) -> Result<()> {
        if let Runtime::Native(runtime) = &self.runtime {
            return runtime.finish(false).await;
        }
        let runtime = self.sqlite_runtime()?;
        tokio::spawn(async move {
            let transaction = runtime
                .lock()
                .await
                .take()
                .ok_or(Error::TransactionClosed)?;
            transaction
                .rollback_async()
                .await
                .map(|_| ())
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }
}

fn join_error(error: tokio::task::JoinError) -> super::Error {
    super::Error::Runtime(format!("exact SQL transaction task failed: {error}"))
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
