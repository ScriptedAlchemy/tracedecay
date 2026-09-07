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

    #[hotpath::measure(label = "daemon.branch_admin.session_runtime_shutdown", future = true)]
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
    #[hotpath::skip]
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

    #[hotpath::skip]
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

    #[hotpath::skip]
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
    #[hotpath::measure(label = "daemon.branch_admin.close_graph_runtimes", future = true)]
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

    #[hotpath::skip]
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
