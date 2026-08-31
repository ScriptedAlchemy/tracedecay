use std::path::Path;

use super::{Connection, IntoParams, ReadConnection, ReadSnapshot, Result, Rows, Transaction};

#[allow(async_fn_in_trait)]
pub trait QueryExecutor {
    #[hotpath::skip]
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams;
}

#[allow(async_fn_in_trait)]
pub trait WalCheckpointExecutor: QueryExecutor {
    #[hotpath::skip]
    async fn checkpoint_wal_truncate(&self) -> Result<Rows>;
}

#[allow(async_fn_in_trait)]
pub trait DatabaseAttachmentExecutor {
    #[hotpath::skip]
    async fn attach_database(&self, path: &Path, database_name: &str) -> Result<()>;
}

impl DatabaseAttachmentExecutor for Transaction {
    #[hotpath::skip]
    async fn attach_database(&self, path: &Path, database_name: &str) -> Result<()> {
        Transaction::attach_database(self, path, database_name).await
    }
}

impl WalCheckpointExecutor for Connection {
    #[hotpath::skip]
    async fn checkpoint_wal_truncate(&self) -> Result<Rows> {
        Connection::checkpoint_wal_truncate(self).await
    }
}

impl QueryExecutor for Connection {
    #[hotpath::skip]
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        Connection::query(self, sql, params).await
    }
}

impl QueryExecutor for ReadConnection {
    #[hotpath::skip]
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        ReadConnection::query(self, sql, params).await
    }
}

impl QueryExecutor for Transaction {
    #[hotpath::skip]
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        Transaction::query(self, sql, params).await
    }
}

impl QueryExecutor for ReadSnapshot {
    #[hotpath::skip]
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        ReadSnapshot::query(self, sql, params).await
    }
}

#[allow(async_fn_in_trait)]
pub trait Executor: QueryExecutor {
    #[hotpath::skip]
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams;
    #[hotpath::skip]
    async fn execute_batch(&self, sql: &str) -> Result<()>;
}

impl Executor for Connection {
    #[hotpath::skip]
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        Connection::execute(self, sql, params).await
    }

    #[hotpath::skip]
    async fn execute_batch(&self, sql: &str) -> Result<()> {
        Connection::execute_batch(self, sql).await
    }
}

impl Executor for Transaction {
    #[hotpath::skip]
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        Transaction::execute(self, sql, params).await
    }

    #[hotpath::skip]
    async fn execute_batch(&self, sql: &str) -> Result<()> {
        Transaction::execute_batch(self, sql).await
    }
}
