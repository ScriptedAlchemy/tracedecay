//! Unified engine adapter for memory and diagnostics stores.

use crate::db::engine::{self, IntoParams, Rows, TransactionBehavior};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SqliteDriverError {
    message: String,
}

impl From<engine::Error> for SqliteDriverError {
    fn from(error: engine::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryConnectionError {
    #[error(transparent)]
    Runtime(#[from] engine::Error),
    #[error("nested memory transactions are unsupported")]
    NestedTransaction,
}

pub type Result<T> = std::result::Result<T, MemoryConnectionError>;

#[derive(Clone, Copy)]
pub enum MemoryConnection<'a> {
    Runtime(&'a engine::Connection),
    RuntimeTransaction(&'a engine::Transaction),
    Transaction(&'a MemoryTransaction),
    DatabaseTransaction(&'a crate::db::DatabaseMemoryTransaction<'a>),
}

impl<'a> MemoryConnection<'a> {
    pub const fn runtime(connection: &'a engine::Connection) -> Self {
        Self::Runtime(connection)
    }

    pub const fn runtime_transaction(transaction: &'a engine::Transaction) -> Self {
        Self::RuntimeTransaction(transaction)
    }

    pub const fn transaction(transaction: &'a MemoryTransaction) -> Self {
        Self::Transaction(transaction)
    }

    pub const fn database_transaction(
        transaction: &'a crate::db::DatabaseMemoryTransaction<'a>,
    ) -> Self {
        Self::DatabaseTransaction(transaction)
    }

    pub async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        let params = params.into_params()?;
        match self {
            Self::Runtime(connection) => connection
                .execute(sql, engine::params_from_iter(params))
                .await
                .map_err(Into::into),
            Self::RuntimeTransaction(transaction) => transaction
                .execute(sql, engine::params_from_iter(params))
                .await
                .map_err(Into::into),
            Self::Transaction(transaction) => {
                transaction
                    .execute(sql, engine::params_from_iter(params))
                    .await
            }
            Self::DatabaseTransaction(transaction) => transaction
                .execute(sql, engine::params_from_iter(params))
                .await
                .map_err(Into::into),
        }
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let params = params.into_params()?;
        match self {
            Self::Runtime(connection) => connection
                .query(sql, engine::params_from_iter(params))
                .await
                .map_err(Into::into),
            Self::RuntimeTransaction(transaction) => transaction
                .query(sql, engine::params_from_iter(params))
                .await
                .map_err(Into::into),
            Self::Transaction(transaction) => {
                transaction
                    .query(sql, engine::params_from_iter(params))
                    .await
            }
            Self::DatabaseTransaction(transaction) => transaction
                .query(sql, engine::params_from_iter(params))
                .await
                .map_err(Into::into),
        }
    }

    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        match self {
            Self::Runtime(connection) => connection.execute_batch(sql).await.map_err(Into::into),
            Self::RuntimeTransaction(transaction) => {
                transaction.execute_batch(sql).await.map_err(Into::into)
            }
            Self::Transaction(transaction) => transaction.execute_batch(sql).await,
            Self::DatabaseTransaction(transaction) => {
                transaction.execute_batch(sql).await.map_err(Into::into)
            }
        }
    }

    pub async fn transaction_with_behavior(
        &self,
        behavior: TransactionBehavior,
    ) -> Result<MemoryTransaction> {
        match self {
            Self::Runtime(connection) => connection
                .transaction_with_behavior(behavior)
                .await
                .map(MemoryTransaction::Runtime)
                .map_err(Into::into),
            Self::RuntimeTransaction(_) | Self::Transaction(_) | Self::DatabaseTransaction(_) => {
                Err(MemoryConnectionError::NestedTransaction)
            }
        }
    }
}

pub enum MemoryTransaction {
    Runtime(engine::Transaction),
}

impl MemoryTransaction {
    pub async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        let params = params.into_params()?;
        match self {
            Self::Runtime(transaction) => transaction
                .execute(sql, engine::params_from_iter(params))
                .await
                .map_err(Into::into),
        }
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let params = params.into_params()?;
        match self {
            Self::Runtime(transaction) => transaction
                .query(sql, engine::params_from_iter(params))
                .await
                .map_err(Into::into),
        }
    }

    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        match self {
            Self::Runtime(transaction) => transaction.execute_batch(sql).await.map_err(Into::into),
        }
    }

    pub async fn commit(self) -> Result<()> {
        match self {
            Self::Runtime(transaction) => transaction.commit().await.map_err(Into::into),
        }
    }

    pub async fn rollback(self) -> Result<()> {
        match self {
            Self::Runtime(transaction) => transaction.rollback().await.map_err(Into::into),
        }
    }
}
