use std::path::Path;

use super::{
    BackendKind, Connection, Error, IntoParams, ReadConnection, ReadSnapshot, Result, Rows,
    Transaction, WriteStatement,
};

#[allow(async_fn_in_trait)]
pub trait QueryExecutor {
    fn backend_kind(&self) -> BackendKind;

    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams;
}

#[allow(async_fn_in_trait)]
pub trait WalCheckpointExecutor: QueryExecutor {
    async fn checkpoint_wal_truncate(&self) -> Result<Rows>;
}

#[allow(async_fn_in_trait)]
pub trait DatabaseAttachmentExecutor {
    async fn attach_database(&self, path: &Path, database_name: &str) -> Result<()>;
}

impl DatabaseAttachmentExecutor for Transaction {
    async fn attach_database(&self, path: &Path, database_name: &str) -> Result<()> {
        Transaction::attach_database(self, path, database_name).await
    }
}

impl WalCheckpointExecutor for Connection {
    async fn checkpoint_wal_truncate(&self) -> Result<Rows> {
        Connection::checkpoint_wal_truncate(self).await
    }
}

impl QueryExecutor for Connection {
    fn backend_kind(&self) -> BackendKind {
        Connection::backend_kind(self)
    }

    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        Connection::query(self, sql, params).await
    }
}

impl QueryExecutor for ReadConnection {
    fn backend_kind(&self) -> BackendKind {
        ReadConnection::backend_kind(self)
    }

    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        ReadConnection::query(self, sql, params).await
    }
}

impl QueryExecutor for Transaction {
    fn backend_kind(&self) -> BackendKind {
        Transaction::backend_kind(self)
    }

    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        Transaction::query(self, sql, params).await
    }
}

impl QueryExecutor for ReadSnapshot {
    fn backend_kind(&self) -> BackendKind {
        ReadSnapshot::backend_kind(self)
    }

    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        ReadSnapshot::query(self, sql, params).await
    }
}

#[allow(async_fn_in_trait)]
pub trait Executor: QueryExecutor {
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams;
    /// Executes owned parameterized writes in input order.
    ///
    /// Concrete engine connections and transactions override this with one
    /// blocking submission. The default preserves compatibility for narrow
    /// executor adapters while retaining exact failed-statement attribution.
    async fn execute_statements(&self, statements: Vec<WriteStatement>) -> Result<Vec<u64>> {
        let mut results = Vec::with_capacity(statements.len());
        for (index, statement) in statements.into_iter().enumerate() {
            let (sql, params) = statement.into_parts();
            results.push(
                self.execute(&sql, params)
                    .await
                    .map_err(|error| Error::statement_batch(index, error))?,
            );
        }
        Ok(results)
    }

    async fn execute_batch(&self, sql: &str) -> Result<()>;
}

impl Executor for Connection {
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        Connection::execute(self, sql, params).await
    }

    async fn execute_statements(&self, statements: Vec<WriteStatement>) -> Result<Vec<u64>> {
        Connection::execute_statements(self, statements).await
    }

    async fn execute_batch(&self, sql: &str) -> Result<()> {
        Connection::execute_batch(self, sql).await
    }
}

impl Executor for Transaction {
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        Transaction::execute(self, sql, params).await
    }

    async fn execute_statements(&self, statements: Vec<WriteStatement>) -> Result<Vec<u64>> {
        Transaction::execute_statements(self, statements).await
    }

    async fn execute_batch(&self, sql: &str) -> Result<()> {
        Transaction::execute_batch(self, sql).await
    }
}
