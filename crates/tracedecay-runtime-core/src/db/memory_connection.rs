//! Unified engine adapter for memory and diagnostics stores.

use crate::db::engine::{self, IntoParams, Rows, TransactionBehavior};
use tracedecay_domain::errors::{SqliteDriverError, TraceDecayError};

impl From<engine::Error> for SqliteDriverError {
    fn from(error: engine::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<engine::Error> for TraceDecayError {
    fn from(error: engine::Error) -> Self {
        Self::Sqlite(error.into())
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
    Transaction(&'a MemoryTransaction),
}

impl<'a> MemoryConnection<'a> {
    #[hotpath::skip]
    pub const fn runtime(connection: &'a engine::Connection) -> Self {
        Self::Runtime(connection)
    }

    #[hotpath::skip]
    pub const fn transaction(transaction: &'a MemoryTransaction) -> Self {
        Self::Transaction(transaction)
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
            Self::Transaction(transaction) => {
                transaction
                    .execute(sql, engine::params_from_iter(params))
                    .await
            }
        }
    }

    #[hotpath::skip]
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
            Self::Transaction(transaction) => {
                transaction
                    .query(sql, engine::params_from_iter(params))
                    .await
            }
        }
    }

    #[hotpath::skip]
    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        match self {
            Self::Runtime(connection) => connection.execute_batch(sql).await.map_err(Into::into),
            Self::Transaction(transaction) => transaction.execute_batch(sql).await,
        }
    }

    #[hotpath::skip]
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
            Self::Transaction(_) => Err(MemoryConnectionError::NestedTransaction),
        }
    }
}

pub enum MemoryTransaction {
    Runtime(engine::Transaction),
}

impl MemoryTransaction {
    #[hotpath::skip]
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

    #[hotpath::skip]
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

    #[hotpath::skip]
    pub async fn execute_batch(&self, sql: &str) -> Result<()> {
        match self {
            Self::Runtime(transaction) => transaction.execute_batch(sql).await.map_err(Into::into),
        }
    }

    #[hotpath::skip]
    pub async fn commit(self) -> Result<()> {
        match self {
            Self::Runtime(transaction) => transaction.commit().await.map_err(Into::into),
        }
    }

    #[hotpath::skip]
    pub async fn rollback(self) -> Result<()> {
        match self {
            Self::Runtime(transaction) => transaction.rollback().await.map_err(Into::into),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_error_converts_without_exposing_the_private_engine_type() {
        let err: TraceDecayError = engine::Error::Runtime("writer unavailable".to_string()).into();

        assert!(matches!(err, TraceDecayError::Sqlite(_)));
        assert!(err.to_string().contains("writer unavailable"));
    }
}
