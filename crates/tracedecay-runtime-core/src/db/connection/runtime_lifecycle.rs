use super::memory_graph_reconciliation::{
    ProjectMemoryReconciliationPassLeaseV1, ProjectMemoryReconciliationTelemetryObserverV1,
    ProjectMemoryReconciliationTelemetryV1,
};
use super::{
    Arc, Connection, Database, DatabaseAccessMode, DatabaseAuthority, DatabaseInner,
    MemoryGraphReconciliationRuntimeErrorV1, Path, Result, StoreRuntimeClientLease,
    TraceDecayError, database_slot, integrity, registered_attachment_required,
};

impl Database {
    pub(crate) fn retained_runtime(&self) -> &StoreRuntimeClientLease {
        &self.inner._runtime
    }

    pub fn is_writable(&self) -> bool {
        self.inner.writable
    }

    pub(crate) fn downgrade(&self) -> super::WeakDatabase {
        super::WeakDatabase {
            inner: Arc::downgrade(&self.inner),
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
        let runtime = self.memory_graph_runtime()?;
        // The retained task owner may outlive the database facade while a
        // coordinated capacity retirement fences the exact graph/store pair.
        // It must therefore not keep the verified graph port (and its graph
        // or store lease) alive. Commit upgrades these references explicitly
        // and reports a typed unavailable outcome if an external close won.
        let cancel_runtime = Arc::downgrade(&runtime);
        let close_runtime = Arc::downgrade(&runtime);
        Some(self.inner.memory_graph_reconciliation.task_owner(
            Arc::new(move || {
                let runtime = cancel_runtime
                    .upgrade()
                    .ok_or(MemoryGraphReconciliationRuntimeErrorV1::RuntimeUnavailable)?;
                runtime.cancel_reconciliation();
                Ok(())
            }),
            Arc::new(move || {
                let runtime = close_runtime
                    .upgrade()
                    .ok_or(MemoryGraphReconciliationRuntimeErrorV1::RuntimeUnavailable)?;
                runtime.close_reconciliation().map_err(|error| {
                    MemoryGraphReconciliationRuntimeErrorV1::CloseFailed(error.to_string())
                })
            }),
        ))
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
    ) -> Result<Self> {
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
        let slot = authority
            .as_ref()
            .map(|authority| database_slot(authority.database_identity_key()));
        if let Some(slot) = &slot {
            let mut open = slot.lock().await;
            if let Some(inner) = open.upgrade() {
                return Ok(Self { inner });
            }
            let inner = Arc::new(DatabaseInner::publish(
                runtime,
                true,
                authority,
                Some(Arc::clone(slot)),
            )?);
            *open = Arc::downgrade(&inner);
            return Ok(Self { inner });
        }
        DatabaseInner::publish(runtime, false, None, None)
            .map(Arc::new)
            .map(|inner| Self { inner })
    }

    /// Legacy compatibility lookup.
    ///
    /// Physical creation and schema bootstrap are owned by the registered
    /// runtime. This method can reuse an attachment already published for the
    /// exact authority, but it never opens a path or invents store identity.
    pub async fn initialize(db_path: &Path, authority: &DatabaseAuthority) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "initialize")?;
        authority.require_active_write_scope("initialize")?;
        let slot = database_slot(authority.database_identity_key());
        let open = slot.lock().await;
        if let Some(inner) = open.upgrade() {
            if !inner.writable {
                return Err(integrity::read_only_upgrade_error(db_path, "initialize"));
            }
            return Ok((Self { inner }, false));
        }
        Err(registered_attachment_required("initialize", db_path))
    }

    /// Reuses an already-published writable attachment for `db_path`.
    pub async fn open(db_path: &Path, authority: &DatabaseAuthority) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "open")?;
        authority.require_active_write_scope("open")?;
        let slot = database_slot(authority.database_identity_key());
        let open = slot.lock().await;
        if let Some(inner) = open.upgrade() {
            if !inner.writable {
                return Err(integrity::read_only_upgrade_error(db_path, "open"));
            }
            return Ok((Self { inner }, false));
        }
        Err(registered_attachment_required("open", db_path))
    }

    /// Reuses an already-published attachment for a read-only caller.
    pub async fn open_read_only(
        db_path: &Path,
        authority: &DatabaseAuthority,
    ) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "open_read_only")?;
        let slot = database_slot(authority.database_identity_key());
        if let Some(inner) = slot.lock().await.upgrade() {
            let lease =
                inner
                    ._runtime
                    .issue_client_lease()
                    .map_err(|error| TraceDecayError::Database {
                        operation: "publish read-only database runtime".to_owned(),
                        message: format!("{error:?}"),
                    })?;
            let read_only = DatabaseInner::publish(lease, false, None, None)?;
            return Ok((
                Self {
                    inner: Arc::new(read_only),
                },
                false,
            ));
        }
        Err(registered_attachment_required("open_read_only", db_path))
    }

    /// Returns the canonical runtime facade.
    ///
    /// Mutations must use [`Self::writer_connection`] or an isolated
    /// transaction while holding [`Self::writer`].
    pub fn conn(&self) -> &Connection {
        &self.inner.conn
    }
}
