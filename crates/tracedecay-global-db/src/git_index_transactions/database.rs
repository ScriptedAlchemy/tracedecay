use crate::{RegisteredGlobalDb, registered::RegisteredGlobalDbWriteTransaction};
#[cfg(any(test, feature = "test-helpers"))]
use tracedecay_runtime_core::db::engine::{Connection, Transaction, TransactionBehavior};
use tracedecay_runtime_core::db::engine::{
    Executor, IntoParams, QueryExecutor, ReadSnapshot, Rows,
};

#[derive(Clone, Copy)]
pub(crate) enum GitMutationDatabase<'db> {
    Registered(&'db RegisteredGlobalDb),
    #[cfg(any(test, feature = "test-helpers"))]
    Engine(&'db Connection),
}

pub(crate) enum GitMutationWriteTransaction<'db> {
    Registered(RegisteredGlobalDbWriteTransaction<'db>),
    #[cfg(any(test, feature = "test-helpers"))]
    Engine(Transaction),
}

impl QueryExecutor for GitMutationWriteTransaction<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        match self {
            Self::Registered(transaction) => transaction.query(sql, params).await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(transaction) => transaction.query(sql, params).await,
        }
    }
}

impl Executor for GitMutationWriteTransaction<'_> {
    async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        match self {
            Self::Registered(transaction) => transaction.execute(sql, params).await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(transaction) => transaction.execute(sql, params).await,
        }
    }

    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.execute_batch(sql).await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(transaction) => transaction.execute_batch(sql).await,
        }
    }
}

impl GitMutationDatabase<'_> {
    pub(crate) async fn begin_write(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<GitMutationWriteTransaction<'_>> {
        match self {
            Self::Registered(db) => db
                .begin_write_transaction()
                .await
                .map(GitMutationWriteTransaction::Registered),
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(db) => db
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .await
                .map(GitMutationWriteTransaction::Engine)
                .map_err(Into::into),
        }
    }

    pub(crate) async fn read_snapshot(
        &self,
    ) -> tracedecay_runtime_core::db::engine::Result<ReadSnapshot> {
        match self {
            Self::Registered(db) => db.read_snapshot().await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(db) => db.read_snapshot().await,
        }
    }
}

impl GitMutationWriteTransaction<'_> {
    pub(crate) async fn commit(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.commit().await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(transaction) => transaction.commit().await,
        }
    }

    pub(crate) async fn rollback(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.rollback().await,
            #[cfg(any(test, feature = "test-helpers"))]
            Self::Engine(transaction) => transaction.rollback().await,
        }
    }
}
