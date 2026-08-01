use std::path::Path;

use super::{Connection, IntoParams, ReadConnection, ReadSnapshot, Result, Rows, Transaction};

#[allow(async_fn_in_trait)]
pub trait QueryExecutor {
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
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        Connection::query(self, sql, params).await
    }
}

impl QueryExecutor for ReadConnection {
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        ReadConnection::query(self, sql, params).await
    }
}

impl QueryExecutor for Transaction {
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        Transaction::query(self, sql, params).await
    }
}

impl QueryExecutor for ReadSnapshot {
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
    async fn execute_batch(&self, sql: &str) -> Result<()>;
}

impl Executor for Connection {
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        Connection::execute(self, sql, params).await
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

    async fn execute_batch(&self, sql: &str) -> Result<()> {
        Transaction::execute_batch(self, sql).await
    }
}
