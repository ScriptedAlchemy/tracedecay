use std::{sync::Arc, time::Duration};

use tracedecay_rusqlite_runtime::migration_sql::{
    MigrationSqlBatchResult, MigrationSqlExecuteResult, MigrationSqlHandle,
    MigrationSqlReadSnapshot, MigrationSqlRows, MigrationSqlStatement,
    MigrationSqlTransaction as RuntimeTransaction,
};

#[cfg(any(test, feature = "test-helpers"))]
use super::Statement;
use super::{IntoParams, ReadSnapshot, Result, Rows, Transaction, TransactionBehavior, Value};

const READER_WAIT: Duration = Duration::from_secs(5);

pub(super) trait Runtime: Send + Sync {
    fn execute(&self, statement: MigrationSqlStatement) -> Result<MigrationSqlExecuteResult>;
    fn query(&self, statement: MigrationSqlStatement) -> Result<MigrationSqlRows>;
    fn checkpoint_wal_truncate(&self) -> Result<MigrationSqlRows>;
    fn execute_batch(&self, sql: String) -> Result<MigrationSqlBatchResult>;
    fn repair_incremental_auto_vacuum(&self) -> Result<()>;
    #[cfg(any(test, feature = "test-helpers"))]
    fn validate(&self, statement: MigrationSqlStatement) -> Result<()>;
    #[cfg(any(test, feature = "test-helpers"))]
    fn last_insert_rowid(&self) -> i64;
    fn begin_read_snapshot(&self) -> Result<MigrationSqlReadSnapshot>;
    fn begin_health_read_snapshot(&self) -> Result<MigrationSqlReadSnapshot>;
    #[cfg(any(test, feature = "test-helpers"))]
    fn begin_deferred(&self) -> Result<RuntimeTransaction>;
    fn begin_immediate(&self) -> Result<RuntimeTransaction>;
    fn begin_schema_migration_immediate(&self) -> Result<RuntimeTransaction>;
}

impl Runtime for MigrationSqlHandle {
    fn execute(&self, statement: MigrationSqlStatement) -> Result<MigrationSqlExecuteResult> {
        self.execute(statement).map_err(Into::into)
    }

    fn query(&self, statement: MigrationSqlStatement) -> Result<MigrationSqlRows> {
        self.query(statement, READER_WAIT).map_err(Into::into)
    }

    fn checkpoint_wal_truncate(&self) -> Result<MigrationSqlRows> {
        self.checkpoint_wal_truncate().map_err(Into::into)
    }

    fn execute_batch(&self, sql: String) -> Result<MigrationSqlBatchResult> {
        self.execute_batch(sql).map_err(Into::into)
    }

    fn repair_incremental_auto_vacuum(&self) -> Result<()> {
        self.repair_incremental_auto_vacuum().map_err(Into::into)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    fn validate(&self, statement: MigrationSqlStatement) -> Result<()> {
        self.validate(statement).map_err(Into::into)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    fn last_insert_rowid(&self) -> i64 {
        self.last_insert_rowid()
    }

    fn begin_read_snapshot(&self) -> Result<MigrationSqlReadSnapshot> {
        self.begin_read_snapshot(READER_WAIT).map_err(Into::into)
    }

    fn begin_health_read_snapshot(&self) -> Result<MigrationSqlReadSnapshot> {
        self.begin_health_read_snapshot(READER_WAIT)
            .map_err(Into::into)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    fn begin_deferred(&self) -> Result<RuntimeTransaction> {
        self.begin_deferred().map_err(Into::into)
    }

    fn begin_immediate(&self) -> Result<RuntimeTransaction> {
        self.begin_immediate().map_err(Into::into)
    }

    fn begin_schema_migration_immediate(&self) -> Result<RuntimeTransaction> {
        self.begin_schema_migration_immediate().map_err(Into::into)
    }
}

#[derive(Clone)]
pub struct Connection {
    runtime: Arc<dyn Runtime>,
}

#[derive(Clone)]
pub struct ReadConnection {
    connection: Connection,
}

impl ReadConnection {
    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        self.connection.query(sql, params).await
    }

    pub async fn read_snapshot(&self) -> Result<ReadSnapshot> {
        self.connection.read_snapshot().await
    }
}

impl Connection {
    pub fn attach(runtime: MigrationSqlHandle) -> Self {
        Self {
            runtime: Arc::new(runtime),
        }
    }

    pub fn read_only(&self) -> ReadConnection {
        ReadConnection {
            connection: self.clone(),
        }
    }

    pub async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        let statement = statement(sql, params)?;
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || runtime.execute(statement))
            .await
            .map_err(join_error)?
            .map(|result| result.changed_rows as u64)
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let statement = statement(sql, params)?;
        let runtime = Arc::clone(&self.runtime);
        let rows = tokio::task::spawn_blocking(move || runtime.query(statement))
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

    pub async fn checkpoint_wal_truncate(&self) -> Result<Rows> {
        let runtime = Arc::clone(&self.runtime);
        let rows = tokio::task::spawn_blocking(move || runtime.checkpoint_wal_truncate())
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
        tokio::task::spawn_blocking(move || runtime.execute_batch(sql))
            .await
            .map_err(join_error)?
            .map(|_| ())
    }

    pub async fn repair_incremental_auto_vacuum(&self) -> Result<()> {
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || runtime.repair_incremental_auto_vacuum())
            .await
            .map_err(join_error)?
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn prepare(&self, sql: &str) -> Result<Statement<'_>> {
        let statement = statement(sql, ())?;
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || runtime.validate(statement))
            .await
            .map_err(join_error)??;
        Statement::for_connection(self, sql)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub fn last_insert_rowid(&self) -> i64 {
        self.runtime.last_insert_rowid()
    }

    pub async fn read_snapshot(&self) -> Result<ReadSnapshot> {
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || runtime.begin_read_snapshot())
            .await
            .map_err(join_error)?
            .map(ReadSnapshot::from_runtime)
    }

    pub(crate) async fn health_read_snapshot(&self) -> Result<ReadSnapshot> {
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || runtime.begin_health_read_snapshot())
            .await
            .map_err(join_error)?
            .map(ReadSnapshot::from_runtime)
    }

    #[cfg(any(test, feature = "test-helpers"))]
    pub async fn transaction(&self) -> Result<Transaction> {
        self.transaction_with_behavior(TransactionBehavior::Deferred)
            .await
    }

    pub async fn transaction_with_behavior(
        &self,
        behavior: TransactionBehavior,
    ) -> Result<Transaction> {
        match behavior {
            #[cfg(any(test, feature = "test-helpers"))]
            TransactionBehavior::Deferred => {
                let runtime = Arc::clone(&self.runtime);
                tokio::task::spawn_blocking(move || runtime.begin_deferred())
                    .await
                    .map_err(join_error)?
                    .map(|transaction| {
                        Transaction::from_runtime(transaction, Arc::clone(&self.runtime))
                    })
            }
            TransactionBehavior::Immediate => {
                let runtime = Arc::clone(&self.runtime);
                tokio::task::spawn_blocking(move || runtime.begin_immediate())
                    .await
                    .map_err(join_error)?
                    .map(|transaction| {
                        Transaction::from_runtime(transaction, Arc::clone(&self.runtime))
                    })
            }
        }
    }

    /// Begins the authority-bound transaction used by schema migration.
    ///
    /// Only its explicit schema-step methods may bypass the ordinary
    /// per-statement deadline. All other operations retain ordinary bounds.
    pub async fn schema_migration_transaction(&self) -> Result<Transaction> {
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || runtime.begin_schema_migration_immediate())
            .await
            .map_err(join_error)?
            .map(|transaction| Transaction::from_runtime(transaction, Arc::clone(&self.runtime)))
    }
}

fn join_error(error: tokio::task::JoinError) -> super::Error {
    super::Error::Runtime(format!("migration SQL worker task failed: {error}"))
}

pub(super) fn statement<P>(sql: &str, params: P) -> Result<MigrationSqlStatement>
where
    P: IntoParams,
{
    MigrationSqlStatement::new(
        sql.to_owned(),
        params.into_params()?.into_iter().map(Into::into).collect(),
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tracedecay_rusqlite_runtime::migration_sql::{
        MigrationSqlError, MigrationSqlWriteAuthority, MigrationSqlWriteIntent,
    };

    use super::super::{Error, TestConnection};

    struct AllowWrites;

    impl MigrationSqlWriteAuthority for AllowWrites {
        fn verify(&self, _intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn schema_migration_entrypoint_requires_attached_write_authority() {
        let directory = tempfile::TempDir::new().unwrap();
        let plain =
            TestConnection::open_without_write_authority(&directory.path().join("plain.sqlite3"));

        let error = match plain.schema_migration_transaction().await {
            Ok(_) => panic!("schema migration must require attached authority"),
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
            .schema_migration_transaction()
            .await
            .unwrap()
            .rollback()
            .await
            .unwrap();
    }
}
