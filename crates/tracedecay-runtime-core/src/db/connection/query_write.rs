use super::{
    Connection, Database, DatabaseEngineConnection, DatabaseEngineReadSnapshot,
    DatabaseMemoryTransaction, DatabaseWriteTransaction, DatabaseWriterConnection, Result,
    TraceDecayError, TransactionBehavior, database_query_error, integrity,
};

impl Database {
    /// Runs a bounded scalar inspection on the retained runtime, projecting the
    /// first column of the first row.
    async fn query_scalar<T, P>(&self, operation: &str, sql: &str, params: P) -> Result<T>
    where
        T: crate::db::engine::FromValue,
        P: crate::db::engine::IntoParams,
    {
        let mut rows = self
            .inner
            .conn
            .query(sql, params)
            .await
            .map_err(|error| database_query_error(operation, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| database_query_error(operation, error))?
            .ok_or_else(|| TraceDecayError::Database {
                message: "scalar query returned no rows".to_owned(),
                operation: operation.to_owned(),
            })?;
        row.get(0)
            .map_err(|error| database_query_error(operation, error))
    }

    /// Runs a bounded scalar integer inspection on the retained runtime.
    #[doc(hidden)]
    pub async fn query_scalar_i64(&self, operation: &str, sql: &str) -> Result<i64> {
        self.query_scalar(operation, sql, ()).await
    }

    /// Runs a bounded scalar blob inspection on the retained runtime.
    #[doc(hidden)]
    pub async fn query_scalar_blob(&self, operation: &str, sql: &str) -> Result<Vec<u8>> {
        self.query_scalar(operation, sql, ()).await
    }

    /// Runs a bounded scalar text inspection on the retained runtime.
    #[doc(hidden)]
    pub async fn query_scalar_text(&self, operation: &str, sql: &str) -> Result<String> {
        self.query_scalar(operation, sql, ()).await
    }

    /// Runs a bounded scalar integer inspection with one text identity bound.
    #[doc(hidden)]
    pub async fn query_scalar_i64_with_text(
        &self,
        operation: &str,
        sql: &str,
        value: &str,
    ) -> Result<i64> {
        self.query_scalar(operation, sql, (value,)).await
    }

    pub fn engine_conn(&self) -> DatabaseEngineConnection {
        DatabaseEngineConnection {
            conn: self.inner.conn.clone(),
            _client_guard: self.client_guard(),
        }
    }

    /// Executes one autocommit-style mutation through the canonical writer
    /// broker. Primarily useful for fixtures and maintenance adapters that do
    /// not need to retain a raw writable connection.
    // Visible outside the crate: integration suites in `tests/` are external
    // crates and exercise this fixture path directly.
    #[doc(hidden)]
    pub async fn execute_write<P>(&self, operation: &str, sql: &str, params: P) -> Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.execute_write_engine(operation, sql, params).await
    }

    pub async fn execute_write_engine<P>(
        &self,
        operation: &str,
        sql: &str,
        params: P,
    ) -> Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        let transaction = self.begin_write_transaction(operation).await?;
        let changed = transaction
            .execute_engine(sql, params)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to execute brokered write: {error}"),
                operation: operation.to_owned(),
            })?;
        transaction.commit().await?;
        Ok(changed)
    }

    /// Executes a SQL batch atomically through the canonical writer broker.
    #[doc(hidden)]
    pub async fn execute_write_batch(&self, operation: &str, sql: &str) -> Result<()> {
        let transaction = self.begin_write_transaction(operation).await?;
        transaction
            .execute_batch(sql)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to execute brokered write batch: {error}"),
                operation: operation.to_string(),
            })?;
        transaction.commit().await
    }

    /// Acquires the process-local writer lane for this canonical database.
    ///
    /// Writable handles opened for the same database share one `DatabaseInner`,
    /// so this guard coordinates MCP, dashboard, and automation mutations.
    pub async fn writer(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.writer.lock().await
    }

    pub(super) fn require_active_write_scope(&self, operation: &str) -> Result<()> {
        if !self.inner.writable {
            return Err(integrity::read_only_upgrade_error(
                self.canonical_database_path(),
                operation,
            ));
        }
        self.inner
            ._authority
            .as_ref()
            .ok_or_else(|| {
                integrity::read_only_upgrade_error(self.canonical_database_path(), operation)
            })?
            .require_active_write_scope(operation)
    }

    pub(super) async fn open_writer_connection_unguarded(
        &self,
        operation: &str,
    ) -> Result<Connection> {
        self.require_active_write_scope(operation)?;
        self.inner.write_conn.clone().ok_or_else(|| {
            integrity::read_only_upgrade_error(self.canonical_database_path(), operation)
        })
    }

    /// Opens an isolated writer while holding the process-local writer lane.
    /// The handle cannot escape the guard, preventing raw DML from bypassing
    /// serialization or joining a transaction on the retained reader.
    pub async fn writer_connection(&self, operation: &str) -> Result<DatabaseWriterConnection<'_>> {
        let guard = self.writer().await;
        let conn = self.open_writer_connection_unguarded(operation).await?;
        Ok(DatabaseWriterConnection {
            _guard: guard,
            conn,
            _client_guard: self.client_guard(),
        })
    }

    /// Starts a query-only snapshot on a separate connection that cannot join
    /// a transaction running on the retained writable connection.
    pub(crate) async fn begin_isolated_read_snapshot(
        &self,
        operation: &str,
    ) -> Result<DatabaseEngineReadSnapshot> {
        let snapshot =
            self.inner
                .conn
                .read_snapshot()
                .await
                .map_err(|error| TraceDecayError::Database {
                    message: format!("failed to begin isolated read snapshot: {error}"),
                    operation: operation.to_string(),
                })?;
        Ok(DatabaseEngineReadSnapshot {
            snapshot,
            _client_guard: self.client_guard(),
        })
    }

    pub async fn begin_engine_read_snapshot(
        &self,
        operation: &str,
    ) -> Result<DatabaseEngineReadSnapshot> {
        self.begin_isolated_read_snapshot(operation).await
    }

    pub async fn begin_memory_read_transaction(
        &self,
        operation: &str,
    ) -> Result<DatabaseMemoryTransaction<'_>> {
        self.begin_engine_read_snapshot(operation)
            .await
            .map(DatabaseMemoryTransaction::read)
    }

    /// Starts an immediate transaction that owns the canonical writer lane.
    /// Dropping the returned capability rolls back before releasing the lane.
    pub async fn begin_write_transaction(
        &self,
        operation: &str,
    ) -> Result<DatabaseWriteTransaction<'_>> {
        let guard = self.writer().await;
        let conn = self.open_writer_connection_unguarded(operation).await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to begin isolated writer transaction: {error}"),
                operation: operation.to_string(),
            })?;
        Ok(DatabaseWriteTransaction {
            transaction,
            guard,
            _client_guard: self.client_guard(),
        })
    }

    /// Starts an atomic bulk-replacement transaction on the canonical writer.
    ///
    /// A full index can contain more than a million rows. Unlike an ordinary
    /// mutation, it can legitimately remain active beyond the fixed
    /// transaction lease while continuously making progress. The runtime's
    /// long-lease policy renews that lease only after successful commands;
    /// idle transactions, revoked authority, and shutdown still cancel it.
    pub async fn begin_bulk_write_transaction(
        &self,
        operation: &str,
    ) -> Result<DatabaseWriteTransaction<'_>> {
        let guard = self.writer().await;
        let conn = self.open_writer_connection_unguarded(operation).await?;
        let transaction = conn
            .authorized_long_lease_transaction()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to begin bulk writer transaction: {error}"),
                operation: operation.to_string(),
            })?;
        Ok(DatabaseWriteTransaction {
            transaction,
            guard,
            _client_guard: self.client_guard(),
        })
    }

    pub async fn begin_memory_write_transaction(
        &self,
        operation: &str,
    ) -> Result<DatabaseMemoryTransaction<'_>> {
        self.begin_write_transaction(operation)
            .await
            .map(DatabaseMemoryTransaction::write)
    }
}
