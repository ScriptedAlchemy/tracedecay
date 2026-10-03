use std::sync::{Arc, atomic::Ordering};

use super::{SessionRuntimeRegistryEntryV1, StoreAdministration};
use tracedecay_daemon_identity::authority;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_store_runtime::RemoteRecoveryProjectLifecycle;

#[derive(Clone)]
pub(in crate::daemon) struct SessionRuntimeMemoryGraphReconciliationShutdownV1 {
    registries: Vec<Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>>,
}

impl SessionRuntimeMemoryGraphReconciliationShutdownV1 {
    pub(in crate::daemon) fn cancel(&self) {
        for registry in &self.registries {
            registry.cancel_terminal_tasks();
            registry.cancel_memory_graph_reconciliation_tasks();
        }
    }

    #[tracing::instrument(
        name = "daemon.branch_admin.session_runtime_shutdown",
        level = "trace",
        skip_all
    )]
    pub(in crate::daemon) async fn shutdown(&self) -> std::result::Result<(), String> {
        self.cancel();
        let mut failures = Vec::new();
        for registry in &self.registries {
            if let Err(error) = registry.shutdown_terminal_tasks().await {
                failures.push(error);
            }
            if let Err(error) = registry.shutdown_memory_graph_reconciliation_tasks().await {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

impl StoreAdministration {
    #[cfg(test)]
    pub(in crate::daemon) async fn install_long_lived_session_runtime_registry_for_test(
        &self,
    ) -> Result<()> {
        let identity = self.profile_identity()?.clone();
        let profile_root = authority::canonical_identity_path(identity.profile_root())?;
        crate::register_runtime_ports()?;
        let registry = Arc::new(
            tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open_with_session_maintenance(
                identity.clone(),
                true,
            )
            .await?,
        );
        let cell = {
            let mut registries = self.session_runtime_registries.lock().await;
            Arc::clone(
                &registries
                    .entry(profile_root)
                    .or_insert_with(|| SessionRuntimeRegistryEntryV1 {
                        identity,
                        registry: Arc::new(tokio::sync::OnceCell::new()),
                    })
                    .registry,
            )
        };
        cell.set(registry).map_err(|_| TraceDecayError::Config {
            message: "test session runtime registry was already initialized".to_owned(),
        })
    }

    pub(in crate::daemon) async fn session_runtime_registry(
        &self,
    ) -> Result<Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>> {
        if self
            .session_runtime_registry_admission_closed
            .load(Ordering::Acquire)
        {
            return Err(session_runtime_admission_closed());
        }
        let identity = self.profile_identity()?.clone();
        let profile_root = authority::canonical_identity_path(identity.profile_root())?;
        let registry = {
            let mut registries = self.session_runtime_registries.lock().await;
            if self
                .session_runtime_registry_admission_closed
                .load(Ordering::Acquire)
            {
                return Err(session_runtime_admission_closed());
            }
            Arc::clone(
                &registries
                    .entry(profile_root)
                    .or_insert_with(|| SessionRuntimeRegistryEntryV1 {
                        identity: identity.clone(),
                        registry: Arc::new(tokio::sync::OnceCell::new()),
                    })
                    .registry,
            )
        };
        let registry = registry
            .get_or_try_init(|| async move {
                crate::register_runtime_ports()?;
                // Boxed: the registry-open composition is a mega future whose
                // inline layout overflows 2MB runtime stacks.
                Box::pin(tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity))
                    .await
                    .map(Arc::new)
            })
            .await
            .map(Arc::clone)?;
        registry.install_session_sync_service(&self.session_sync_service)?;
        if let Some(lifecycle) = self.remote_recovery_project_lifecycle()? {
            registry.install_remote_recovery_project_lifecycle(
                Arc::clone(&lifecycle) as Arc<dyn RemoteRecoveryProjectLifecycle>
            )?;
        }
        Ok(registry)
    }

    pub(in crate::daemon) async fn registered_runtime_registry(
        &self,
    ) -> Result<Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>> {
        Box::pin(self.ensure_account_active()).await?;
        Box::pin(self.session_runtime_registry()).await
    }

    /// Shutdown-only: drains the retained graph owners out of every session
    /// runtime registry and closes their Grafeo runtimes. Call only after
    /// [`SessionRuntimeMemoryGraphReconciliationShutdownV1::shutdown`] has
    /// joined terminal hook, schema-convergence, and reconciliation workers;
    /// the drain drops the runtimes those workers publish through.
    #[tracing::instrument(
        name = "daemon.branch_admin.close_graph_runtimes",
        level = "trace",
        skip_all
    )]
    pub(in crate::daemon) async fn close_retained_graph_runtimes_for_shutdown(&self) -> Result<()> {
        let registries = self
            .session_runtime_registries
            .lock()
            .await
            .values()
            .filter_map(|entry| entry.registry.get().cloned())
            .collect::<Vec<_>>();
        let mut first_error = None;
        for registry in registries {
            if let Err(error) = registry.close_retained_graph_runtimes_for_shutdown().await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Terminal store close. Ordering is the correctness contract: close
    /// registry admission, cancel reconciliation, join the workers while their
    /// runtimes are still alive, release the telemetry sampler's retained
    /// store handles, and only then drain the retained owners and close every
    /// store no lease still holds, so each writer truncates its WAL. Closing
    /// before the join leaves the standing owner attachments leased.
    #[tracing::instrument(
        name = "daemon.branch_admin.close_stores_for_shutdown",
        level = "trace",
        skip_all
    )]
    pub(in crate::daemon) async fn close_stores_for_shutdown(
        &self,
    ) -> std::result::Result<(), String> {
        let owner = self
            .prepare_memory_graph_reconciliation_shutdown()
            .await
            .map_err(|error| error.to_string())?;
        owner.cancel();
        owner.shutdown().await?;
        self.store_telemetry_sampling()
            .release_retained_handles_for_shutdown();
        self.close_retained_graph_runtimes_for_shutdown()
            .await
            .map_err(|error| error.to_string())
    }

    /// Shutdown-only: stops the store opens and schema installs of every
    /// mounted session runtime registry at their next safe point, so an
    /// admitted project open returns promptly instead of finishing its mount.
    pub(in crate::daemon) async fn cancel_store_opens_for_shutdown(&self) {
        for entry in self.session_runtime_registries.lock().await.values() {
            if let Some(registry) = entry.registry.get() {
                registry.cancel_store_opens_for_shutdown();
            }
        }
    }

    pub(in crate::daemon) async fn prepare_memory_graph_reconciliation_shutdown(
        &self,
    ) -> Result<SessionRuntimeMemoryGraphReconciliationShutdownV1> {
        self.session_runtime_registry_admission_closed
            .store(true, Ordering::Release);
        let registry_entries = self
            .session_runtime_registries
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut registries = Vec::with_capacity(registry_entries.len());
        for entry in registry_entries {
            let identity = entry.identity;
            let initialized = entry
                .registry
                .get_or_try_init(|| async move {
                    crate::register_runtime_ports()?;
                    tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity)
                        .await
                        .map(Arc::new)
                })
                .await?;
            registries.push(Arc::clone(initialized));
        }
        Ok(SessionRuntimeMemoryGraphReconciliationShutdownV1 { registries })
    }
}

fn session_runtime_admission_closed() -> TraceDecayError {
    TraceDecayError::Config {
        message: "session runtime registry admission is closed for daemon shutdown".to_owned(),
    }
}
