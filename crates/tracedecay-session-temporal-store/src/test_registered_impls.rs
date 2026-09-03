//! Unit-test impls of this crate's handle traits for `tracedecay-global-db` types.
//!
//! `cargo test` compiles this crate with `cfg(test)` as a distinct crate from
//! the lib `tracedecay-global-db` depends on. The production impls therefore
//! do not apply here; these copies bind the test crate's traits to the same
//! public global-db methods.

use std::future::Future;
use std::path::Path;

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_global_db::{
    RegisteredGlobalDb, RegisteredGlobalDbLeaseV1, RegisteredGlobalDbWriteTransaction,
    RegisteredGlobalDbWriterConnection,
};
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_runtime_core::db::engine::{
    Connection, Error as EngineError, Executor, IntoParams, QueryExecutor, Rows, TestConnection,
};
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimeWeakProxyV1;
use tracedecay_store::StoreShardScopeV1;

use crate::handle::{
    SessionTemporalExec, SessionTemporalQuery, SessionTemporalRegisteredDb, SessionTemporalWriteTxn,
};
use crate::relations::{SessionRelationGraphStore, SessionRelationScope};

impl SessionTemporalQuery for Connection {
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        Connection::query(self, sql, params)
    }
}

impl SessionTemporalExec for Connection {
    fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<u64, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        Connection::execute(self, sql, params)
    }

    fn execute_batch(&self, sql: &str) -> impl Future<Output = Result<(), EngineError>> + Send {
        Connection::execute_batch(self, sql)
    }
}

impl SessionTemporalQuery for TestConnection {
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        QueryExecutor::query(self, sql, params)
    }
}

impl SessionTemporalExec for TestConnection {
    fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<u64, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        Executor::execute(self, sql, params)
    }

    fn execute_batch(&self, sql: &str) -> impl Future<Output = Result<(), EngineError>> + Send {
        Executor::execute_batch(self, sql)
    }
}

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

impl SessionTemporalQuery for RegisteredGlobalDbWriterConnection<'_> {
    fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<Rows, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        RegisteredGlobalDbWriterConnection::query(self, sql, params)
    }
}

impl SessionTemporalExec for RegisteredGlobalDbWriterConnection<'_> {
    fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> impl Future<Output = Result<u64, EngineError>> + Send
    where
        P: IntoParams + Send,
    {
        RegisteredGlobalDbWriterConnection::execute(self, sql, params)
    }

    fn execute_batch(&self, sql: &str) -> impl Future<Output = Result<(), EngineError>> + Send {
        RegisteredGlobalDbWriterConnection::execute_batch(self, sql)
    }
}

impl SessionTemporalRegisteredDb for RegisteredGlobalDb {
    type WriteTxn<'a> = RegisteredGlobalDbWriteTransaction<'a>;

    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<DatabaseEngineReadSnapshot, TraceDecayError>> + Send {
        RegisteredGlobalDb::read_snapshot(self)
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
        let lease = self.session_relation_graph_lease()?;
        let binding = self.binding();
        let scope = match &binding.shard_id.scope {
            StoreShardScopeV1::ProjectSessions { project_id } => {
                SessionRelationScope::project_sessions(project_id.clone())
            }
            StoreShardScopeV1::ProfileSessions => {
                SessionRelationScope::profile_sessions(binding.shard_id.profile_id.clone())
            }
            other => {
                return Err(TraceDecayError::Database {
                    operation: "resolve session relation graph".to_owned(),
                    message: format!(
                        "registered shard scope does not own session relations: {other:?}"
                    ),
                });
            }
        };
        Ok((scope, SessionRelationGraphStore::new(lease)))
    }

    fn project_graph_runtime(&self) -> Option<&VerifiedGraphRuntimeWeakProxyV1> {
        RegisteredGlobalDb::project_graph_runtime(self)
    }
}

impl SessionTemporalRegisteredDb for RegisteredGlobalDbLeaseV1 {
    type WriteTxn<'a> = RegisteredGlobalDbWriteTransaction<'a>;

    fn read_snapshot(
        &self,
    ) -> impl Future<Output = Result<DatabaseEngineReadSnapshot, TraceDecayError>> + Send {
        RegisteredGlobalDb::read_snapshot(self)
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
        <RegisteredGlobalDb as SessionTemporalRegisteredDb>::session_relation_store(self)
    }

    fn project_graph_runtime(&self) -> Option<&VerifiedGraphRuntimeWeakProxyV1> {
        RegisteredGlobalDb::project_graph_runtime(self)
    }
}
