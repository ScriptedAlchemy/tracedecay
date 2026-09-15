//! `RegisteredGlobalDb` implements the sessions registered-store handle.
//!
//! The sessions crate owns session-sync, LCM adapter, session-read, and
//! transcript operation bodies. This module is the composition edge:
//! connection, path, and store identity stay here.

use std::future::Future;
use std::path::Path;

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::db::DatabaseEngineReadConnection;
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_runtime_core::db::engine::{
    Error as EngineError, Executor, IntoParams, QueryExecutor, Rows,
};
use tracedecay_runtime_core::store_runtime::VerifiedGraphRuntimeWeakProxyV1;
use tracedecay_sessions::runtime::{
    SessionExec, SessionQuery, SessionRegisteredDb, SessionWriteTxn,
};
use tracedecay_store::{StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use crate::{
    RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction, RegisteredGlobalDbWriterConnection,
};

impl SessionQuery for RegisteredGlobalDbWriteTransaction<'_> {
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

impl SessionExec for RegisteredGlobalDbWriteTransaction<'_> {
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

impl SessionWriteTxn for RegisteredGlobalDbWriteTransaction<'_> {
    fn commit(self) -> impl Future<Output = Result<(), EngineError>> + Send {
        RegisteredGlobalDbWriteTransaction::commit(self)
    }
}

impl SessionQuery for RegisteredGlobalDbWriterConnection<'_> {
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

impl SessionExec for RegisteredGlobalDbWriterConnection<'_> {
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

impl QueryExecutor for RegisteredGlobalDbWriterConnection<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        RegisteredGlobalDbWriterConnection::query(self, sql, params).await
    }
}

impl Executor for RegisteredGlobalDbWriterConnection<'_> {
    async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        RegisteredGlobalDbWriterConnection::execute(self, sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        RegisteredGlobalDbWriterConnection::execute_batch(self, sql).await
    }
}

impl SessionRegisteredDb for RegisteredGlobalDb {
    type WriteTxn<'a> = RegisteredGlobalDbWriteTransaction<'a>;
    type WriterConn<'a> = RegisteredGlobalDbWriterConnection<'a>;

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

    fn writer_connection(&self) -> Result<Self::WriterConn<'_>, TraceDecayError> {
        RegisteredGlobalDb::writer_connection(self)
    }

    fn db_path(&self) -> &Path {
        RegisteredGlobalDb::db_path(self)
    }

    fn registered_binding(&self) -> &StoreRuntimeBindingV1 {
        RegisteredGlobalDb::binding(self)
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        RegisteredGlobalDb::verified_locator(self)
    }

    fn project_graph_runtime(&self) -> Option<&VerifiedGraphRuntimeWeakProxyV1> {
        RegisteredGlobalDb::project_graph_runtime(self)
    }

    fn read_connection(&self) -> DatabaseEngineReadConnection {
        RegisteredGlobalDb::read_connection(self)
    }
}
