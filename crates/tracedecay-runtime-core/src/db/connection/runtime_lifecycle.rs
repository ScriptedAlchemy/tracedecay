use super::memory_graph_reconciliation::{
    MemoryGraphReconciliationRuntimeErrorV1, ProjectMemoryReconciliationPassLeaseV1,
    ProjectMemoryReconciliationTelemetryObserverV1, ProjectMemoryReconciliationTelemetryV1,
};
use super::{
    Arc, Connection, Database, DatabaseAccessMode, DatabaseAuthority, DatabaseInner,
    DatabaseOwnerV1, Path, Result, StoreRuntimeClientLease, TraceDecayError, integrity,
};

impl Database {
    pub fn is_writable(&self) -> bool {
        self.inner.writable
    }

    pub(crate) fn downgrade(&self) -> super::WeakDatabase {
        super::WeakDatabase {
            inner: Arc::downgrade(&self.inner),
            client: Arc::downgrade(&self.client),
        }
    }

    pub(crate) fn schedule_memory_graph_reconciliation<Operation, OperationFuture>(
        &self,
        operation: Operation,
    ) -> super::MemoryGraphReconciliationTaskScheduleV1
    where
        Operation: Fn(super::WeakDatabase) -> OperationFuture + Send + 'static,
        OperationFuture: std::future::Future<Output = bool> + Send + 'static,
    {
        self.inner
            .memory_graph_reconciliation
            .schedule(self, operation)
    }

    pub fn project_memory_reconciliation_telemetry_observer(
        &self,
    ) -> ProjectMemoryReconciliationTelemetryObserverV1 {
        ProjectMemoryReconciliationTelemetryObserverV1::new(
            Arc::clone(&self.inner.memory_graph_reconciliation_telemetry),
            Arc::downgrade(&self.inner),
        )
    }

    pub(crate) fn project_memory_reconciliation_telemetry(
        &self,
    ) -> &ProjectMemoryReconciliationTelemetryV1 {
        &self.inner.memory_graph_reconciliation_telemetry
    }

    pub(crate) fn begin_project_memory_reconciliation_pass(
        &self,
    ) -> std::result::Result<ProjectMemoryReconciliationPassLeaseV1, &'static str> {
        Arc::clone(&self.inner.memory_graph_reconciliation_telemetry).begin_reconciliation_pass()
    }

    pub fn memory_graph_reconciliation_task_owner(
        &self,
    ) -> Option<super::MemoryGraphReconciliationTaskOwnerV1> {
        let runtime = self.inner.memory_graph_runtime.get()?;
        // The retained task owner may outlive the database facade while a
        // coordinated capacity retirement fences the exact graph/store pair.
        // It must therefore not keep the verified graph port (and its graph
        // or store lease) alive. Retained cancellation upgrades explicitly
        // and reports a typed unavailable outcome if an external close won.
        let cancel_runtime = Arc::downgrade(runtime);
        Some(
            self.inner
                .memory_graph_reconciliation
                .task_owner(Arc::new(move || {
                    let runtime = cancel_runtime
                        .upgrade()
                        .ok_or(MemoryGraphReconciliationRuntimeErrorV1::RuntimeUnavailable)?;
                    runtime.cancel_reconciliation();
                    Ok(())
                })),
        )
    }

    pub(crate) fn memory_graph_reconciliation_pending(&self) -> bool {
        self.inner.memory_graph_reconciliation.pending()
    }

    /// Canonical path held by this database's verified runtime locator.
    pub fn canonical_database_path(&self) -> &Path {
        &self.inner.canonical_path
    }

    /// Returns the canonical path bound to this already-open database.
    ///
    /// Primarily exposed for read-only inspection and integration fixtures;
    /// callers must not treat the path as a substitute for write authority.
    #[doc(hidden)]
    pub fn database_path(&self) -> &Path {
        self.canonical_database_path()
    }

    /// Physical `SQLite` identity captured when this retained handle was opened.
    pub fn opened_file_identity(&self) -> u64 {
        self.inner.opened_file_identity
    }

    pub fn filesystem_is_read_only(&self) -> bool {
        std::fs::metadata(self.canonical_database_path())
            .is_ok_and(|metadata| metadata.permissions().readonly())
    }

    /// Clones the originating revocable write capability for actor-time checks.
    pub fn write_authority(&self) -> Result<DatabaseAuthority> {
        if !self.inner.writable {
            return Err(integrity::read_only_upgrade_error(
                self.canonical_database_path(),
                "acquire database write authority",
            ));
        }
        self.inner
            ._authority
            .clone()
            .ok_or_else(|| TraceDecayError::Database {
                message: "writable database facade has no originating authority".to_owned(),
                operation: "acquire database write authority".to_owned(),
            })
    }

    /// Publishes one verified registry runtime as the only physical owner of
    /// this database path.
    ///
    /// The runtime already carries its typed binding, verified locator, and
    /// opened file identity. A read-write facade additionally retains the
    /// originating authority; a read-only facade never requests it. Neither
    /// mode derives identity from a path or extracts the physical attachment.
    pub async fn publish_runtime(
        runtime: StoreRuntimeClientLease,
        access: DatabaseAccessMode,
    ) -> Result<DatabaseOwnerV1> {
        let writable = access.is_writable();
        let authority = if writable {
            if !runtime.writer_present() {
                return Err(TraceDecayError::Database {
                    message: "registered runtime has no physical writer".to_owned(),
                    operation: "publish database runtime".to_owned(),
                });
            }
            let authority = runtime
                .database_authority("publish database runtime")
                .map_err(|error| TraceDecayError::Database {
                    message: format!("{error:?}"),
                    operation: "publish database runtime".to_owned(),
                })?;
            authority.require_active_write_scope("publish database runtime")?;
            Some(authority)
        } else {
            None
        };
        let inner = Arc::new(DatabaseInner::publish(runtime, writable, authority)?);
        DatabaseOwnerV1::from_published_inner(inner).map_err(|error| TraceDecayError::Database {
            message: format!("failed to construct database owner: {error:?}"),
            operation: "publish database runtime".to_owned(),
        })
    }

    /// Returns the canonical runtime facade.
    ///
    /// Mutations must use [`Self::writer_connection`] or an isolated
    /// transaction while holding [`Self::writer`].
    pub fn conn(&self) -> &Connection {
        &self.inner.conn
    }
}
