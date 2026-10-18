use crate::RegisteredGlobalDb;
use crate::registered::RegisteredGlobalDbWriteTx;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_runtime_core::db::engine::{IntoParams, QueryExecutor, Rows};

#[derive(Clone, Copy)]
pub(crate) enum GitMutationDatabase<'db> {
    Registered(&'db RegisteredGlobalDb),
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

impl GitMutationDatabase<'_> {
    /// Runs `body` as one write job. A refused or failed job becomes
    /// `wrap(cause)`; the body's own error is returned as is.
    #[tracing::instrument(name = "global_db.git_index.txn.job", level = "trace", skip_all)]
    pub(crate) async fn transact<R, E>(
        &self,
        wrap: impl FnOnce(TraceDecayError) -> E,
        body: impl AsyncFn(RegisteredGlobalDbWriteTx<'_, '_>) -> Result<R, E> + Send + Sync + 'static,
    ) -> Result<R, E>
    where
        R: Send + 'static,
        E: Send + 'static,
    {
        match self {
            Self::Registered(db) => db.transact_or(wrap, body).await,
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
