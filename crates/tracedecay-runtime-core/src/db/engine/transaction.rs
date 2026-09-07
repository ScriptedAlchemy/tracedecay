use std::{path::Path, sync::Arc};

use tokio::sync::Mutex;

use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlAttachment, ExactSqlHandle, ExactSqlTransaction as RuntimeTransaction,
};

use super::{Error, IntoParams, Result, Rows, Value, WriteStatement, connection::statement};

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
    #[cfg(any(test, feature = "test-helpers"))]
    connection_runtime: Arc<ExactSqlHandle>,
}

impl Transaction {
    pub(super) fn from_runtime(
        runtime: RuntimeTransaction,
        connection_runtime: Arc<ExactSqlHandle>,
    ) -> Self {
        #[cfg(not(any(test, feature = "test-helpers")))]
        let _ = connection_runtime;
        Self {
            runtime: Arc::new(Mutex::new(Some(runtime))),
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

    #[hotpath::skip]
    pub async fn attach_database(&self, path: &Path, database_name: &str) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
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

    #[hotpath::skip]
    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let runtime = Arc::clone(&self.runtime);
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
    #[hotpath::skip]
    pub async fn execute_authority_revalidated_batch(&self, sql: &str) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
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

    #[hotpath::skip]
    pub async fn validate(&self, sql: &str) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
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

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn last_insert_rowid(&self) -> i64 {
        self.connection_runtime.last_insert_rowid()
    }

    #[hotpath::skip]
    pub async fn commit(self) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
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

    #[hotpath::skip]
    pub async fn rollback(self) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
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
