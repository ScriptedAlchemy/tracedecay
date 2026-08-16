use std::sync::{Arc, atomic::Ordering};

use super::{SessionRuntimeRegistryEntryV1, StoreAdministration, authority};
use crate::errors::{Result, TraceDecayError};

#[derive(Clone)]
pub(in crate::daemon) struct SessionRuntimeMemoryGraphReconciliationShutdownV1 {
    registries:
        Vec<Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>>,
}

impl SessionRuntimeMemoryGraphReconciliationShutdownV1 {
    pub(in crate::daemon) fn cancel(&self) {
        for registry in &self.registries {
            registry.cancel_memory_graph_reconciliation_tasks();
        }
    }

    pub(in crate::daemon) async fn shutdown(&self) -> std::result::Result<(), String> {
        self.cancel();
        let mut failures = Vec::new();
        for registry in &self.registries {
            if let Err(error) = registry.shutdown_retained_runtimes().await {
                failures.push(error.to_string());
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
    pub(in crate::daemon) async fn session_runtime_registry(
        &self,
    ) -> Result<Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>>
    {
        if self
            .session_runtime_registry_admission_closed
            .load(Ordering::Acquire)
        {
            return Err(session_runtime_admission_closed());
        }
        let identity = self.profile_identity()?.clone();
        let profile_root = authority::canonical_identity_path(identity.profile_root())?;
        self.reserve_session_runtime_registry_capacity(&profile_root)
            .await?;
        let (registry, retiring, opening) = {
            let mut registries = self.session_runtime_registries.lock().await;
            if self
                .session_runtime_registry_admission_closed
                .load(Ordering::Acquire)
            {
                return Err(session_runtime_admission_closed());
            }
            let entry = registries
                .entry(profile_root)
                .or_insert_with(|| SessionRuntimeRegistryEntryV1::new(identity.clone()));
            (
                Arc::clone(&entry.registry),
                Arc::clone(&entry.retiring),
                Arc::clone(&entry.opening),
            )
        };
        if retiring.load(Ordering::Acquire) {
            return Err(TraceDecayError::Config {
                message: "session runtime registry is retiring under bounded capacity".to_owned(),
            });
        }
        opening.fetch_add(1, Ordering::AcqRel);
        let retiring_during_open = Arc::clone(&retiring);
        let registry = registry
            .get_or_try_init(|| async move {
                if retiring_during_open.load(Ordering::Acquire) {
                    return Err(TraceDecayError::Config {
                        message: "session runtime registry is retiring under bounded capacity"
                            .to_owned(),
                    });
                }
                crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                    identity,
                )
                .await
                .map(Arc::new)
            })
            .await;
        opening.fetch_sub(1, Ordering::AcqRel);
        let registry = registry.map(Arc::clone)?;
        registry.install_session_sync_service(&self.session_sync_service)?;
        if let Some(lifecycle) = self.remote_recovery_project_lifecycle()? {
            registry.install_remote_recovery_project_lifecycle(&lifecycle)?;
        }
        Ok(registry)
    }

    async fn reserve_session_runtime_registry_capacity(
        &self,
        requested: &std::path::Path,
    ) -> Result<()> {
        loop {
            let candidate = {
                let registries = self.session_runtime_registries.lock().await;
                if registries.contains_key(requested)
                    || registries.len() < self.session_runtime_registry_capacity.max_profiles()
                {
                    return Ok(());
                }
                registries.iter().find_map(|(profile_root, entry)| {
                    (!entry.retiring.load(Ordering::Acquire))
                        .then(|| (profile_root.clone(), entry.clone()))
                })
            };
            let Some((profile_root, entry)) = candidate else {
                return Err(TraceDecayError::Config {
                    message: "session runtime registry capacity is exhausted by retiring profiles"
                        .to_owned(),
                });
            };
            if entry
                .retiring
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            if entry.opening.load(Ordering::Acquire) != 0 {
                entry.retiring.store(false, Ordering::Release);
                return Err(TraceDecayError::Config {
                    message: "session runtime registry capacity is exhausted by an opening profile"
                        .to_owned(),
                });
            }
            let Some(registry) = entry.registry.get().cloned() else {
                let removed = {
                    let mut registries = self.session_runtime_registries.lock().await;
                    let exact_entry = registries.get(&profile_root).is_some_and(|current| {
                        Arc::ptr_eq(&current.registry, &entry.registry)
                            && Arc::ptr_eq(&current.retiring, &entry.retiring)
                            && Arc::ptr_eq(&current.opening, &entry.opening)
                    });
                    exact_entry
                        .then(|| registries.remove(&profile_root))
                        .flatten()
                };
                if removed.is_some() {
                    return Ok(());
                }
                entry.retiring.store(false, Ordering::Release);
                continue;
            };
            if Arc::strong_count(&registry) != 2 {
                entry.retiring.store(false, Ordering::Release);
                return Err(TraceDecayError::Config {
                    message: "session runtime registry capacity is exhausted by an active profile"
                        .to_owned(),
                });
            }
            if let Err(error) = registry.retire_retained_runtimes_for_capacity().await {
                entry.retiring.store(false, Ordering::Release);
                return Err(error);
            }
            let removed = {
                let mut registries = self.session_runtime_registries.lock().await;
                let exact_entry = registries.get(&profile_root).is_some_and(|current| {
                    Arc::ptr_eq(&current.registry, &entry.registry)
                        && Arc::ptr_eq(&current.retiring, &entry.retiring)
                        && Arc::ptr_eq(&current.opening, &entry.opening)
                });
                exact_entry
                    .then(|| registries.remove(&profile_root))
                    .flatten()
            };
            if removed.is_some() {
                return Ok(());
            }
            entry.retiring.store(false, Ordering::Release);
        }
    }

    pub(in crate::daemon) async fn retained_runtime_registry(
        &self,
    ) -> Result<Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>>
    {
        self.ensure_account_active().await?;
        self.session_runtime_registry().await
    }

    pub(in crate::daemon) async fn registered_runtime_registry(
        &self,
    ) -> Result<Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>>
    {
        self.ensure_account_active().await?;
        self.session_runtime_registry().await
    }

    pub(in crate::daemon) async fn close_session_relation_graphs(&self) -> Result<()> {
        let registries = self
            .session_runtime_registries
            .lock()
            .await
            .values()
            .filter_map(|entry| entry.registry.get().cloned())
            .collect::<Vec<_>>();
        let mut first_error = None;
        for registry in registries {
            if let Err(error) = registry.close_mounted_session_relation_graphs().await
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
                    crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                        identity,
                    )
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn profile_identity(
        root: &std::path::Path,
        label: &str,
    ) -> (
        crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
        crate::db::DaemonDatabaseScope,
    ) {
        let profile_root = root.join(label);
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("durable profile identity");
        let scope = crate::db::enter_daemon_database_scope(&profile_root, 1, label)
            .expect("daemon database scope");
        (identity, scope)
    }

    #[tokio::test]
    async fn profile_registry_capacity_retires_idle_profiles_and_refuses_live_ones() {
        let temporary = tempfile::tempdir().expect("profile registry capacity fixture");
        let (first_identity, _first_scope) = profile_identity(temporary.path(), "first").await;
        let (second_identity, _second_scope) = profile_identity(temporary.path(), "second").await;
        let (third_identity, _third_scope) = profile_identity(temporary.path(), "third").await;
        let capacity = SessionRuntimeRegistryCapacityV1::for_test(1)
            .expect("nonzero profile registry capacity");
        let first = StoreAdministration::default()
            .with_session_runtime_registry_capacity_for_test(capacity)
            .with_profile_identity(first_identity);
        let first_registry = first
            .session_runtime_registry()
            .await
            .expect("first profile registry");
        let first_sessions = first_registry
            .profile_sessions()
            .await
            .expect("first profile sessions");
        let first_memory = first_registry
            .profile_memory()
            .await
            .expect("first profile memory");
        drop((first_sessions, first_memory));
        drop(first_registry);

        let second = first.clone().with_profile_identity(second_identity);
        let live_second = second
            .session_runtime_registry()
            .await
            .expect("idle first profile is retired before second opens");
        let live_second_memory = live_second
            .profile_memory()
            .await
            .expect("second profile memory lease");
        assert_eq!(second.session_runtime_registries.lock().await.len(), 1);

        let third = first.clone().with_profile_identity(third_identity);
        let refusal = third
            .session_runtime_registry()
            .await
            .expect_err("a live profile registry must not be evicted under bounded capacity");
        assert!(
            refusal
                .to_string()
                .contains("capacity is exhausted by an active profile"),
            "unexpected profile capacity refusal: {refusal}"
        );
        assert_eq!(second.session_runtime_registries.lock().await.len(), 1);

        drop((live_second_memory, live_second));
        let third_registry = third
            .session_runtime_registry()
            .await
            .expect("profile registry reopens after the active lease drops");
        assert_eq!(
            third_registry.profile_id(),
            third.profile_identity().expect("identity").profile_id()
        );
        assert_eq!(third.session_runtime_registries.lock().await.len(), 1);
    }
}
