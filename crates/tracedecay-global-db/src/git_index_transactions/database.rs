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
    #[hotpath::skip]
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
    #[hotpath::skip]
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
    #[hotpath::skip]
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

    #[hotpath::skip]
    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.execute_batch(sql).await,
        }
    }
}

impl GitMutationDatabase<'_> {
    #[hotpath::measure(future = true, label = "global_db.git_index.txn.begin")]
    pub(crate) async fn begin_write(
        &self,
    ) -> tracedecay_domain::errors::Result<GitMutationWriteTransaction<'_>> {
        crate::hotpath_observe::record_transaction_rows(1);
        match self {
            Self::Registered(db) => db
                .begin_write_transaction()
                .await
                .map(GitMutationWriteTransaction::Registered),
        }
    }

    #[hotpath::skip]
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
    #[hotpath::skip]
    pub(crate) async fn commit(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.commit().await,
        }
    }

    #[hotpath::skip]
    pub(crate) async fn rollback(self) -> tracedecay_runtime_core::db::engine::Result<()> {
        match self {
            Self::Registered(transaction) => transaction.rollback().await,
        }
    }
}

impl crate::sqlite_persist::PersistWriteTransaction for GitMutationWriteTransaction<'_> {
    fn commit(
        self,
    ) -> impl std::future::Future<Output = tracedecay_runtime_core::db::engine::Result<()>> + Send
    {
        GitMutationWriteTransaction::commit(self)
    }

    fn rollback(
        self,
    ) -> impl std::future::Future<Output = tracedecay_runtime_core::db::engine::Result<()>> + Send
    {
        GitMutationWriteTransaction::rollback(self)
    }
}
