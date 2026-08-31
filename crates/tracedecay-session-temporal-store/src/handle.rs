//! Registered-store handle the session-temporal crate composes against.
//!
//! `RegisteredGlobalDb` lives in `tracedecay-global-db` and implements this
//! trait there. This crate must not depend on that crate — doing so would
//! recreate the wave-7 spine chain.
//!
//! Query/execute futures are `Send` here because RPITIT `async fn` on
//! `QueryExecutor`/`Executor` does not imply `Send` for a generic associated
//! write transaction. Callers that must return `impl Future + Send` (the
//! `tracedecay-store` session ports) go through these bounds.

use std::future::Future;
use std::path::Path;

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_runtime_core::db::engine::{Error as EngineError, IntoParams, Rows};
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimeWeakProxyV1;

use crate::relations::{SessionRelationGraphStore, SessionRelationScope};

/// Read-only SQL the session-temporal store can issue on a snapshot or txn.
pub trait SessionTemporalQuery: Send + Sync {
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send;
}

/// Mutating SQL without a commit obligation (write txn or test connection).
pub trait SessionTemporalExec: SessionTemporalQuery {
    fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<u64, EngineError>> + Send
    where
        P: IntoParams + Send;

    fn execute_batch(&self, sql: &str) -> impl Future<Output = Result<(), EngineError>> + Send;
}

/// Write transaction the session-temporal store can query, mutate, and commit.
pub trait SessionTemporalWriteTxn: SessionTemporalExec {
    fn commit(self) -> impl Future<Output = Result<(), EngineError>> + Send;
}

impl SessionTemporalQuery for DatabaseEngineReadSnapshot {
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        DatabaseEngineReadSnapshot::query(self, sql, params)
    }
}

/// Durable registered session store the temporal projector and retrieval ports
/// compose. Implementors own connection, path, and relation-graph identity;
/// this crate owns generation, projection, and hydration behavior.
pub trait SessionTemporalRegisteredDb: Sync {
    type WriteTxn<'a>: SessionTemporalWriteTxn
    where
        Self: 'a;

    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<DatabaseEngineReadSnapshot, TraceDecayError>> + Send;

    fn begin_write_transaction(
        &self,
    ) -> impl Future<Output = Result<Self::WriteTxn<'_>, TraceDecayError>> + Send;

    fn db_path(&self) -> &Path;

    fn session_relation_store(
        &self,
    ) -> Result<(SessionRelationScope, SessionRelationGraphStore), TraceDecayError>;

    fn project_graph_runtime(&self) -> Option<&VerifiedGraphRuntimeWeakProxyV1>;
}

/// Inherent-method host for former `impl RegisteredGlobalDb` temporal operations.
///
/// Deref reaches the handle so existing `self.read_snapshot()` /
/// `self.begin_write_transaction()` call sites stay intact.
#[derive(Clone, Copy)]
pub struct SessionTemporalAccess<'a, D: SessionTemporalRegisteredDb + ?Sized>(&'a D);

impl<'a, D: SessionTemporalRegisteredDb + ?Sized> SessionTemporalAccess<'a, D> {
    pub const fn new(db: &'a D) -> Self {
        Self(db)
    }

    pub const fn inner(&self) -> &'a D {
        self.0
    }
}

impl<D: SessionTemporalRegisteredDb + ?Sized> std::ops::Deref for SessionTemporalAccess<'_, D> {
    type Target = D;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}
