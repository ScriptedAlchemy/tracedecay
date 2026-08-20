use super::{
    Connection, DatabaseEngineConnection, DatabaseEngineLongLeaseTransaction,
    DatabaseEngineReadSnapshot, DatabaseMemoryTransaction, DatabaseWriteTransaction,
    DatabaseWriterConnection, Path, Result, TraceDecayError,
};

impl DatabaseWriterConnection<'_> {
    pub fn engine_connection(&self) -> DatabaseEngineConnection {
        DatabaseEngineConnection {
            conn: self.conn.clone(),
            _client_guard: self._client_guard.clone(),
        }
    }

    #[cfg(test)]
    pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        self.conn.execute_batch(sql).await
    }

    #[cfg(test)]
    pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.conn.execute(sql, params).await
    }

    #[cfg(test)]
    pub async fn execute_engine<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.conn.execute(sql, params).await
    }

    #[cfg(test)]
    pub async fn query_engine<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.conn.query(sql, params).await
    }
}

impl DatabaseEngineConnection {
    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.conn.query(sql, params).await
    }

    pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.conn.execute(sql, params).await
    }

    pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        self.conn.execute_batch(sql).await
    }

    pub(crate) async fn authorized_long_lease_transaction(
        &self,
    ) -> crate::db::engine::Result<DatabaseEngineLongLeaseTransaction> {
        self.conn
            .authorized_long_lease_transaction()
            .await
            .map(|transaction| DatabaseEngineLongLeaseTransaction {
                transaction,
                _client_guard: self._client_guard.clone(),
            })
    }
}

impl crate::db::engine::QueryExecutor for DatabaseEngineConnection {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        DatabaseEngineConnection::query(self, sql, params).await
    }
}

impl crate::db::engine::Executor for DatabaseEngineConnection {
    async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        DatabaseEngineConnection::execute(self, sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        DatabaseEngineConnection::execute_batch(self, sql).await
    }
}

impl DatabaseEngineReadSnapshot {
    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.snapshot.query(sql, params).await
    }

    pub async fn commit(self) -> crate::db::engine::Result<()> {
        drop(self);
        Ok(())
    }

    pub async fn rollback(self) -> crate::db::engine::Result<()> {
        drop(self);
        Ok(())
    }
}

impl crate::db::engine::QueryExecutor for DatabaseEngineReadSnapshot {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        DatabaseEngineReadSnapshot::query(self, sql, params).await
    }
}

impl DatabaseEngineLongLeaseTransaction {
    pub(crate) async fn commit(self) -> crate::db::engine::Result<()> {
        self.transaction.commit().await
    }

    pub(crate) async fn rollback(self) -> crate::db::engine::Result<()> {
        self.transaction.rollback().await
    }
}

impl crate::db::engine::QueryExecutor for DatabaseEngineLongLeaseTransaction {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.transaction.query(sql, params).await
    }
}

impl crate::db::engine::Executor for DatabaseEngineLongLeaseTransaction {
    async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.transaction.execute(sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        self.transaction.execute_batch(sql).await
    }
}

impl<'a> DatabaseMemoryTransaction<'a> {
    pub fn read(snapshot: DatabaseEngineReadSnapshot) -> Self {
        Self::Read(snapshot)
    }

    pub fn write(transaction: DatabaseWriteTransaction<'a>) -> Self {
        Self::Write(transaction)
    }

    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        match self {
            Self::Read(snapshot) => snapshot.query(sql, params).await,
            Self::Write(transaction) => transaction.query_engine(sql, params).await,
        }
    }

    pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        match self {
            Self::Read(_) => Err(crate::db::engine::Error::Runtime(
                "cannot execute a write in a memory read snapshot".to_owned(),
            )),
            Self::Write(transaction) => transaction.execute_engine(sql, params).await,
        }
    }

    pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        match self {
            Self::Read(_) => Err(crate::db::engine::Error::Runtime(
                "cannot execute a write in a memory read snapshot".to_owned(),
            )),
            Self::Write(transaction) => transaction.execute_batch_engine(sql).await,
        }
    }

    pub async fn commit(self) -> Result<()> {
        match self {
            Self::Read(snapshot) => {
                snapshot
                    .commit()
                    .await
                    .map_err(|error| TraceDecayError::Database {
                        message: format!("failed to commit memory read snapshot: {error}"),
                        operation: "commit memory read snapshot".to_owned(),
                    })
            }
            Self::Write(transaction) => transaction.commit().await,
        }
    }

    pub async fn rollback(self) -> Result<()> {
        match self {
            Self::Read(snapshot) => {
                snapshot
                    .rollback()
                    .await
                    .map_err(|error| TraceDecayError::Database {
                        message: format!("failed to roll back memory read snapshot: {error}"),
                        operation: "rollback memory read snapshot".to_owned(),
                    })
            }
            Self::Write(transaction) => transaction.rollback().await,
        }
    }
}

impl crate::db::engine::QueryExecutor for DatabaseMemoryTransaction<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        DatabaseMemoryTransaction::query(self, sql, params).await
    }
}

impl crate::db::engine::Executor for DatabaseMemoryTransaction<'_> {
    async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        DatabaseMemoryTransaction::execute(self, sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        DatabaseMemoryTransaction::execute_batch(self, sql).await
    }
}

impl crate::db::engine::DatabaseAttachmentExecutor for DatabaseMemoryTransaction<'_> {
    async fn attach_database(
        &self,
        path: &Path,
        database_name: &str,
    ) -> crate::db::engine::Result<()> {
        match self {
            Self::Read(_) => Err(crate::db::engine::Error::invalid_operation(
                "cannot attach a database to a memory read snapshot",
            )),
            Self::Write(transaction) => {
                transaction
                    .transaction
                    .attach_database(path, database_name)
                    .await
            }
        }
    }
}

impl DatabaseWriteTransaction<'_> {
    pub async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.transaction.execute(sql, params).await
    }

    pub async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.transaction.query(sql, params).await
    }

    pub async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        self.transaction.execute_batch(sql).await
    }

    pub async fn execute_batch_engine(&self, sql: &str) -> crate::db::engine::Result<()> {
        self.transaction.execute_batch(sql).await
    }

    pub async fn execute_engine<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.transaction.execute(sql, params).await
    }

    pub async fn query_engine<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.transaction.query(sql, params).await
    }

    pub async fn commit(self) -> Result<()> {
        let Self {
            transaction,
            guard,
            _client_guard,
        } = self;
        let tracks_graph_generation = {
            let mut rows = transaction
                .query(
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'metadata'",
                    (),
                )
                .await
                .map_err(|error| TraceDecayError::Database {
                    message: format!("failed to inspect graph generation authority: {error}"),
                    operation: "commit write transaction".to_owned(),
                })?;
            rows.next()
                .await
                .map_err(|error| TraceDecayError::Database {
                    message: format!("failed to read graph generation authority: {error}"),
                    operation: "commit write transaction".to_owned(),
                })?
                .is_some()
        };
        if tracks_graph_generation {
            let next_generation = {
                let mut rows = transaction
                    .query(
                        "SELECT value FROM metadata
                         WHERE key = 'graph_transaction_generation'",
                        (),
                    )
                    .await
                    .map_err(|error| TraceDecayError::Database {
                        message: format!("failed to query graph transaction generation: {error}"),
                        operation: "commit write transaction".to_owned(),
                    })?;
                match rows
                    .next()
                    .await
                    .map_err(|error| TraceDecayError::Database {
                        message: format!("failed to read graph transaction generation: {error}"),
                        operation: "commit write transaction".to_owned(),
                    })? {
                    Some(row) => {
                        let raw: String =
                            row.get(0).map_err(|error| TraceDecayError::Database {
                                message: format!(
                                    "failed to decode graph transaction generation: {error}"
                                ),
                                operation: "commit write transaction".to_owned(),
                            })?;
                        raw.parse::<u64>()
                            .map_err(|error| TraceDecayError::Database {
                                message: format!(
                                    "invalid graph transaction generation '{raw}': {error}"
                                ),
                                operation: "commit write transaction".to_owned(),
                            })?
                            .checked_add(1)
                            .ok_or_else(|| TraceDecayError::Database {
                                message: "graph transaction generation overflowed".to_owned(),
                                operation: "commit write transaction".to_owned(),
                            })?
                    }
                    None => 1,
                }
            };
            transaction
                .execute(
                    "INSERT OR REPLACE INTO metadata (key, value)
                     VALUES ('graph_transaction_generation', ?1)",
                    (next_generation.to_string(),),
                )
                .await
                .map_err(|error| TraceDecayError::Database {
                    message: format!("failed to advance graph transaction generation: {error}"),
                    operation: "commit write transaction".to_owned(),
                })?;
        }
        let transaction = transaction.commit().await;
        drop(guard);
        drop(_client_guard);
        transaction.map_err(|error| TraceDecayError::Database {
            message: format!("failed to commit isolated writer transaction: {error}"),
            operation: "commit write transaction".to_string(),
        })
    }

    pub async fn rollback(self) -> Result<()> {
        let Self {
            transaction,
            guard,
            _client_guard,
        } = self;
        let transaction = transaction.rollback().await;
        drop(guard);
        drop(_client_guard);
        transaction.map_err(|error| TraceDecayError::Database {
            message: format!("failed to roll back isolated writer transaction: {error}"),
            operation: "rollback write transaction".to_string(),
        })
    }
}

impl crate::db::engine::QueryExecutor for DatabaseWriteTransaction<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> crate::db::engine::Result<crate::db::engine::Rows>
    where
        P: crate::db::engine::IntoParams,
    {
        self.query_engine(sql, params).await
    }
}

impl crate::db::engine::Executor for DatabaseWriteTransaction<'_> {
    async fn execute<P>(&self, sql: &str, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        self.execute_engine(sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> crate::db::engine::Result<()> {
        self.execute_batch_engine(sql).await
    }
}

impl crate::db::engine::DatabaseAttachmentExecutor for DatabaseWriteTransaction<'_> {
    async fn attach_database(
        &self,
        path: &Path,
        database_name: &str,
    ) -> crate::db::engine::Result<()> {
        self.transaction.attach_database(path, database_name).await
    }
}
