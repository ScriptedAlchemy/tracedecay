use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

use tracedecay_rusqlite_runtime::migration_sql::{
    MigrationSqlAttachment, MigrationSqlTransaction as RuntimeTransaction,
};

use super::{
    IntoParams, Result, Rows, Value,
    connection::{Runtime, statement},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionBehavior {
    #[cfg(any(test, feature = "test-helpers"))]
    Deferred,
    Immediate,
}

pub struct Transaction {
    runtime: Arc<Mutex<Option<RuntimeTransaction>>>,
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
            runtime: Arc::new(Mutex::new(Some(runtime))),
            #[cfg(any(test, feature = "test-helpers"))]
            connection_runtime,
        }
    }

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

    pub async fn attach_database(&self, path: &Path, database_name: &str) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
        let filename = path.to_str().ok_or_else(|| {
            super::Error::invalid_operation("SQLite attachment path is not valid UTF-8")
        })?;
        let attachment =
            MigrationSqlAttachment::new(filename.to_owned(), database_name.to_owned())?;
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

    /// Executes one separately authorized schema batch without the ordinary
    /// statement deadline.
    pub async fn execute_schema_batch_step(&self, sql: &str) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            lock_runtime(&runtime)?
                .as_ref()
                .ok_or(super::Error::TransactionClosed)?
                .execute_schema_batch_step(sql)
                .map(|_| ())
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

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

    pub async fn commit(self) -> Result<()> {
        let runtime = self.take_runtime()?;
        tokio::task::spawn_blocking(move || {
            runtime.commit().map(|_| ()).map_err(super::Error::from)
        })
        .await
        .map_err(join_error)?
    }

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

fn lock_runtime<T>(runtime: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    runtime
        .lock()
        .map_err(|_| super::Error::Runtime("migration SQL transaction lock poisoned".to_owned()))
}

fn join_error(error: tokio::task::JoinError) -> super::Error {
    super::Error::Runtime(format!("migration SQL transaction task failed: {error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracedecay_rusqlite_runtime::migration_sql::{
        MigrationSqlError, MigrationSqlWriteAuthority, MigrationSqlWriteIntent,
    };

    use super::{
        super::{Error, TestConnection},
        lock_runtime,
    };

    struct AllowWrites;

    impl MigrationSqlWriteAuthority for AllowWrites {
        fn verify(&self, _intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
            Ok(())
        }
    }

    #[test]
    fn poisoned_transaction_lock_returns_a_typed_error() {
        let runtime = Mutex::new(());
        let _ = std::panic::catch_unwind(|| {
            let _guard = runtime.lock().unwrap();
            panic!("poison transaction lock");
        });

        let result = lock_runtime(&runtime);
        let Err(Error::Runtime(message)) = result else {
            panic!("poisoned transaction lock must return a runtime error");
        };
        assert_eq!(message, "migration SQL transaction lock poisoned");
    }

    #[tokio::test]
    async fn only_long_lease_transaction_exposes_long_schema_steps() {
        let directory = tempfile::TempDir::new().unwrap();
        let connection = TestConnection::open_with_write_authority(
            &directory.path().join("engine.sqlite3"),
            Arc::new(AllowWrites),
        );
        let ordinary = connection.transaction().await.unwrap();

        let error = ordinary
            .execute_schema_batch_step("CREATE TABLE forbidden (id INTEGER)")
            .await
            .unwrap_err();

        assert!(matches!(error, Error::InvalidOperation(_)));
        ordinary.rollback().await.unwrap();

        let long_lease = connection
            .authorized_long_lease_transaction()
            .await
            .unwrap();
        long_lease
            .execute_schema_batch_step("CREATE TABLE allowed (id INTEGER)")
            .await
            .unwrap();
        long_lease.commit().await.unwrap();
    }
}
