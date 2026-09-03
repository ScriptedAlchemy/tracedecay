//! Registered session-store handle the sessions crate composes against.
//!
//! `RegisteredGlobalDb` implements this in `tracedecay-global-db`; this crate
//! must not depend on that crate. Query/execute futures are `Send` here
//! because RPITIT `async fn` on `QueryExecutor`/`Executor` does not imply
//! `Send` for a generic associated write transaction.
//!
//! These traits intentionally do not share
//! `SessionTemporalQuery`/`SessionTemporalExec`/`SessionTemporalWriteTxn` —
//! this crate cannot depend on `tracedecay-session-temporal-store`. Two ports
//! at two layers, one implementor, is the same relationship
//! `registered_lcm` already has with `SessionTemporalAccess`.

use std::future::Future;
use std::path::Path;

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::DatabaseEngineReadConnection;
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_runtime_core::db::engine::{Error as EngineError, Executor, IntoParams, Rows};
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimeWeakProxyV1;
use tracedecay_store::{StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

/// Read-only SQL the session store can issue on a snapshot, read connection, or txn.
pub trait SessionQuery: Send + Sync {
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send;
}

/// Mutating SQL without a commit obligation (write txn or writer connection).
pub trait SessionExec: SessionQuery {
    fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<u64, EngineError>> + Send
    where
        P: IntoParams + Send;

    fn execute_batch(&self, sql: &str) -> impl Future<Output = Result<(), EngineError>> + Send;
}

/// Write transaction the session store can query, mutate, and commit.
pub trait SessionWriteTxn: SessionExec {
    fn commit(self) -> impl Future<Output = Result<(), EngineError>> + Send;
}

impl SessionQuery for DatabaseEngineReadSnapshot {
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

impl SessionQuery for DatabaseEngineReadConnection {
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        DatabaseEngineReadConnection::query(self, sql, params)
    }
}

/// Registered session-store handle the sessions crate composes against.
///
/// `RegisteredGlobalDb` / `RegisteredGlobalDbLeaseV1` implement this in
/// `tracedecay-global-db`. This crate must not depend on that crate.
pub trait SessionRegisteredDb: Sync {
    type WriteTxn<'a>: SessionWriteTxn + Executor
    where
        Self: 'a;
    type WriterConn<'a>: SessionExec
    where
        Self: 'a;

    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<DatabaseEngineReadSnapshot, TraceDecayError>> + Send;

    fn begin_write_transaction(
        &self,
    ) -> impl Future<Output = Result<Self::WriteTxn<'_>, TraceDecayError>> + Send;

    fn writer_connection(&self) -> Result<Self::WriterConn<'_>, TraceDecayError>;

    fn db_path(&self) -> &Path;

    fn registered_binding(&self) -> &StoreRuntimeBindingV1;

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1;

    fn project_graph_runtime(&self) -> Option<&VerifiedGraphRuntimeWeakProxyV1>;

    /// Short-held reader lease. Not in the original port sketch; registered
    /// session/transcript point lookups refuse a pinned snapshot on purpose.
    fn read_connection(&self) -> DatabaseEngineReadConnection;
}

/// Inherent-method host for former `impl RegisteredGlobalDb` session-store operations.
///
/// Deref reaches the handle so existing `self.read_snapshot()` /
/// `self.begin_write_transaction()` call sites stay intact.
#[derive(Clone, Copy)]
pub struct SessionStoreAccess<'a, D: SessionRegisteredDb + ?Sized>(&'a D);

impl<'a, D: SessionRegisteredDb + ?Sized> SessionStoreAccess<'a, D> {
    #[hotpath::skip]
    pub const fn new(db: &'a D) -> Self {
        Self(db)
    }

    #[hotpath::skip]
    pub const fn inner(&self) -> &'a D {
        self.0
    }
}

impl<D: SessionRegisteredDb + ?Sized> std::ops::Deref for SessionStoreAccess<'_, D> {
    type Target = D;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}
