use crate::{RegisteredGlobalDb, registered::RegisteredGlobalDbWriteTransaction};
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_runtime_core::db::engine::{Executor, IntoParams, QueryExecutor, Rows};

#[derive(Clone, Copy)]
pub(crate) enum GitMutationDatabase<'db> {
    Registered(&'db RegisteredGlobalDb),
}

pub(crate) enum GitMutationWriteTransaction<'db> {
    Registered(RegisteredGlobalDbWriteTransaction<'db>),
}

pub(crate) enum GitMutationReadSnapshot {
    Registered(DatabaseEngineReadSnapshot),
}

impl QueryExecutor for GitMutationReadSnapshot {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        match self {
            Self::Registered(snapshot) => snapshot.query(sql, params).await,
        }
    }
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
        }
    }

    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.execute_batch(sql).await,
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
        }
    }

    pub(crate) async fn read_snapshot(
        &self,
    ) -> tracedecay_runtime_core::db::engine::Result<GitMutationReadSnapshot> {
        match self {
            Self::Registered(db) => db
                .read_snapshot()
                .await
                .map(GitMutationReadSnapshot::Registered)
                .map_err(|error| {
                    tracedecay_runtime_core::db::engine::Error::invalid_operation(error.to_string())
                }),
        }
    }
}

impl GitMutationWriteTransaction<'_> {
    pub(crate) async fn commit(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.commit().await,
        }
    }

    pub(crate) async fn rollback(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.rollback().await,
        }
    }
}
