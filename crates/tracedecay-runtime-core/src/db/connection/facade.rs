use super::{
    Connection, DatabaseEngineConnection, DatabaseEngineReadSnapshot, DatabaseEngineStatement,
    DatabaseEngineStatementTarget, DatabaseMemoryTransaction, DatabaseMemoryWriter,
    DatabaseWriteTransaction, DatabaseWriterConnection, Path, Result, TraceDecayError,
};

impl DatabaseWriterConnection<'_> {
    pub fn engine_connection(&self) -> &Connection {
        &self.conn
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

    pub fn memory_store(&self) -> crate::memory::store::MemoryStore<'_> {
        crate::memory::store::MemoryStore::new_runtime(&self.conn)
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

impl DatabaseEngineStatement<'_> {
    pub async fn execute<P>(&self, params: P) -> crate::db::engine::Result<u64>
    where
        P: crate::db::engine::IntoParams,
    {
        match &self.target {
            DatabaseEngineStatementTarget::Transaction(transaction) => {
                transaction.execute(&self.sql, params).await
            }
        }
    }

    pub fn reset(&self) {}
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

impl DatabaseMemoryWriter<'_> {
    /// Returns a memory store whose writable connection remains protected by
    /// the canonical database writer lane for this capability's lifetime.
    pub fn store(&self) -> crate::memory::store::MemoryStore<'_> {
        self.writer.memory_store()
    }

    /// Returns a retriever bound to the same serialized memory authority.
    pub fn retriever(&self) -> crate::memory::retrieval::FactRetriever<'_> {
        crate::memory::retrieval::FactRetriever::new_runtime(&self.writer.conn)
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

    pub(crate) async fn prepare_engine(
        &self,
        sql: &str,
    ) -> crate::db::engine::Result<DatabaseEngineStatement<'_>> {
        self.transaction.validate(sql).await?;
        Ok(DatabaseEngineStatement {
            target: DatabaseEngineStatementTarget::Transaction(&self.transaction),
            sql: sql.to_owned(),
        })
    }

    pub async fn commit(self) -> Result<()> {
        let Self { transaction, guard } = self;
        let transaction = transaction.commit().await;
        drop(guard);
        transaction.map_err(|error| TraceDecayError::Database {
            message: format!("failed to commit isolated writer transaction: {error}"),
            operation: "commit write transaction".to_string(),
        })
    }

    pub async fn rollback(self) -> Result<()> {
        let Self { transaction, guard } = self;
        let transaction = transaction.rollback().await;
        drop(guard);
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
