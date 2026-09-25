//! `RegisteredGlobalDb` implements the session-temporal registered-store handle.
//!
//! The temporal store crate owns projection and retrieval behavior. This
//! module is the only composition edge: connection, path, and relation-graph
//! identity stay here.

use std::future::Future;
use std::path::Path;

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_runtime_core::db::engine::{Error as EngineError, IntoParams, Rows};
use tracedecay_runtime_core::shard_runtime::VerifiedGraphRuntimeWeakProxyV1;
use tracedecay_session_temporal_store::relations::{
    SessionRelationGraphStore, SessionRelationScope,
};
use tracedecay_session_temporal_store::{
    SessionTemporalExec, SessionTemporalQuery, SessionTemporalRegisteredDb, SessionTemporalWriteTxn,
};

use crate::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};

impl SessionTemporalQuery for RegisteredGlobalDbWriteTransaction<'_> {
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        RegisteredGlobalDbWriteTransaction::query(self, sql, params)
    }
}

impl SessionTemporalExec for RegisteredGlobalDbWriteTransaction<'_> {
    fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<u64, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        RegisteredGlobalDbWriteTransaction::execute(self, sql, params)
    }

    fn execute_batch(&self, sql: &str) -> impl Future<Output = Result<(), EngineError>> + Send {
        RegisteredGlobalDbWriteTransaction::execute_batch(self, sql)
    }
}

impl SessionTemporalWriteTxn for RegisteredGlobalDbWriteTransaction<'_> {
    fn commit(self) -> impl Future<Output = Result<(), EngineError>> + Send {
        RegisteredGlobalDbWriteTransaction::commit(self)
    }
}

impl SessionTemporalRegisteredDb for RegisteredGlobalDb {
    type WriteTxn<'a> = RegisteredGlobalDbWriteTransaction<'a>;

    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<DatabaseEngineReadSnapshot, TraceDecayError>> + Send {
        RegisteredGlobalDb::read_snapshot(self)
    }

    fn health_read_snapshot(
        &self,
    ) -> impl Future<Output = Result<DatabaseEngineReadSnapshot, TraceDecayError>> + Send {
        RegisteredGlobalDb::health_read_snapshot(self)
    }

    fn begin_write_transaction(
        &self,
    ) -> impl Future<Output = Result<Self::WriteTxn<'_>, TraceDecayError>> + Send {
        RegisteredGlobalDb::begin_write_transaction(self)
    }

    fn db_path(&self) -> &Path {
        RegisteredGlobalDb::db_path(self)
    }

    fn session_relation_store(
        &self,
    ) -> Result<(SessionRelationScope, SessionRelationGraphStore), TraceDecayError> {
        let (scope, graph, _, _) = self.session_relation_graph()?;
        Ok((scope.clone(), SessionRelationGraphStore::new(graph.clone())))
    }

    fn project_graph_runtime(&self) -> Option<&VerifiedGraphRuntimeWeakProxyV1> {
        RegisteredGlobalDb::project_graph_runtime(self)
    }
}
