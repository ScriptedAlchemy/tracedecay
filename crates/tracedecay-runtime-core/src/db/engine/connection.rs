use std::{sync::Arc, time::Duration};

use tracedecay_store::OperationPriorityV1;

use tracedecay_rusqlite_runtime::migration_sql::{
    MigrationSqlBatchResult, MigrationSqlExecuteResult, MigrationSqlHandle,
    MigrationSqlReadSnapshot, MigrationSqlRows, MigrationSqlStatement,
    MigrationSqlTransaction as RuntimeTransaction,
};
pub use tracedecay_rusqlite_runtime::reader::{ReaderPoolSnapshot, ReaderPoolState};

#[cfg(any(test, feature = "test-helpers"))]
use super::Statement;
use super::{IntoParams, ReadSnapshot, Result, Rows, Transaction, TransactionBehavior, Value};

const READER_WAIT: Duration = Duration::from_secs(5);

pub(super) trait Runtime: Send + Sync {
    fn execute(&self, statement: MigrationSqlStatement) -> Result<MigrationSqlExecuteResult>;
    fn query(
        &self,
        statement: MigrationSqlStatement,
        priority: OperationPriorityV1,
    ) -> Result<MigrationSqlRows>;
    fn checkpoint_wal_truncate(&self) -> Result<MigrationSqlRows>;
    fn execute_batch(&self, sql: String) -> Result<MigrationSqlBatchResult>;
    fn repair_incremental_auto_vacuum(&self) -> Result<()>;
    #[cfg(any(test, feature = "test-helpers"))]
    fn validate(&self, statement: MigrationSqlStatement) -> Result<()>;
    #[cfg(any(test, feature = "test-helpers"))]
    fn last_insert_rowid(&self) -> i64;
    fn begin_read_snapshot(
        &self,
        priority: OperationPriorityV1,
    ) -> Result<MigrationSqlReadSnapshot>;
    fn begin_health_read_snapshot(&self) -> Result<MigrationSqlReadSnapshot>;
    fn reader_pool_occupancy(&self) -> Option<ReaderPoolSnapshot>;
    #[cfg(any(test, feature = "test-helpers"))]
    fn begin_deferred(&self) -> Result<RuntimeTransaction>;
    fn begin_immediate(&self) -> Result<RuntimeTransaction>;
    fn begin_authorized_long_lease_immediate(&self) -> Result<RuntimeTransaction>;
}

impl Runtime for MigrationSqlHandle {
    fn execute(&self, statement: MigrationSqlStatement) -> Result<MigrationSqlExecuteResult> {
        self.execute(statement).map_err(Into::into)
    }

    fn query(
        &self,
        statement: MigrationSqlStatement,
        priority: OperationPriorityV1,
    ) -> Result<MigrationSqlRows> {
        self.query_with_priority(statement, priority, READER_WAIT)
            .map_err(Into::into)
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

    fn begin_read_snapshot(
        &self,
        priority: OperationPriorityV1,
    ) -> Result<MigrationSqlReadSnapshot> {
        self.begin_read_snapshot_with_priority(priority, READER_WAIT)
            .map_err(Into::into)
    }

    fn begin_health_read_snapshot(&self) -> Result<MigrationSqlReadSnapshot> {
        self.begin_health_read_snapshot(READER_WAIT)
            .map_err(Into::into)
    }

    fn reader_pool_occupancy(&self) -> Option<ReaderPoolSnapshot> {
        self.reader_pool_occupancy()
    }

    #[cfg(any(test, feature = "test-helpers"))]
    fn begin_deferred(&self) -> Result<RuntimeTransaction> {
        self.begin_deferred().map_err(Into::into)
    }

    fn begin_immediate(&self) -> Result<RuntimeTransaction> {
        self.begin_immediate().map_err(Into::into)
    }

    fn begin_authorized_long_lease_immediate(&self) -> Result<RuntimeTransaction> {
        self.begin_authorized_long_lease_immediate()
            .map_err(Into::into)
    }
}

#[derive(Clone)]
pub struct Connection {
    runtime: Arc<dyn Runtime>,
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
    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        self.connection.query(sql, params).await
    }

    pub async fn read_snapshot(&self) -> Result<ReadSnapshot> {
        self.connection.read_snapshot().await
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
    pub fn attach(runtime: MigrationSqlHandle) -> Self {
        Self {
            runtime: Arc::new(runtime),
            read_priority: OperationPriorityV1::Foreground,
        }
    }

    pub fn read_only(&self) -> ReadConnection {
        ReadConnection {
            connection: self.clone(),
        }
    }

    /// The same store and the same writer, with every read this handle issues
    /// admitted as background work.
    ///
    /// Writes are unaffected: the returned handle shares this one's runtime, so
    /// `execute`, `execute_batch`, and every transaction still reach the same
    /// serialized writer. Only the reader-pool admission of non-transactional
    /// `query`/`read_snapshot` calls changes, and those then admit against the
    /// unreserved slice of the general lane.
    ///
    /// Background maintenance that runs on a *write* connection needs this:
    /// [`Self::attach`] defaults to `Foreground`, so a bulk sweep driven from
    /// the writer would otherwise contend for the same reserved lane slice as
    /// interactive queries — and, because reader leases are bounded, be the
    /// first thing to fail once it has saturated that lane itself.
    #[must_use]
    pub fn background_reads(&self) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
            read_priority: OperationPriorityV1::Background,
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
        let priority = self.read_priority;
        let rows = tokio::task::spawn_blocking(move || runtime.query(statement, priority))
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

    /// Live reader-pool occupancy for the store behind this connection.
    ///
    /// Lock-free and lease-free, so it still answers while the pool is
    /// saturated — which is the only moment the numbers matter.
    #[must_use]
    pub fn reader_pool_occupancy(&self) -> Option<ReaderPoolSnapshot> {
        self.runtime.reader_pool_occupancy()
    }

    pub async fn read_snapshot(&self) -> Result<ReadSnapshot> {
        let runtime = Arc::clone(&self.runtime);
        let priority = self.read_priority;
        tokio::task::spawn_blocking(move || runtime.begin_read_snapshot(priority))
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

    /// Begins the authority-bound transaction whose lease renews on progress.
    ///
    /// Reserved for schema installation on a fresh or index-less store and for
    /// full-index bulk replacement — writes that legitimately outlive one fixed
    /// lease while continuously making progress. It steps no store forward from
    /// an older shape. Only its explicit schema-step methods may bypass the
    /// ordinary per-statement deadline; all other operations retain ordinary
    /// bounds, and idleness, shutdown, and authority revocation still cancel.
    pub async fn authorized_long_lease_transaction(&self) -> Result<Transaction> {
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || runtime.begin_authorized_long_lease_immediate())
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
