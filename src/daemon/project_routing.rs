//! Route resolution and per-route serialization for project opens.
//!
//! Owns the typed refusals a route open can raise (capacity, cancellation,
//! warming), the handshake-to-route mapping, the open and maintenance gates,
//! and the portable owner reconciler.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

pub(super) fn project_server_capacity_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "daemon project server capacity reached (capacity={MAX_CACHED_PROJECT_SERVERS}); retry after active clients finish"
        ),
    }
}

pub(super) fn project_open_task_capacity_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "daemon project open task capacity reached (capacity={MAX_TRACKED_PROJECT_OPEN_TASKS}); retry shortly"
        ),
    }
}

pub(super) fn project_open_cancellation_error() -> TraceDecayError {
    TraceDecayError::Config {
        message: "daemon is draining during project warm-up".to_string(),
    }
}

pub(super) fn project_open_cancellation_checkpoint(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        return Err(project_open_cancellation_error());
    }
    Ok(())
}

pub(super) fn project_warming_error(project_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "TraceDecay project '{}' {PROJECT_WARMING_RETRY_HINT}",
            project_path.display(),
        ),
    }
}

pub(super) fn project_route_for_handshake(
    handshake: &DaemonHandshake,
) -> Result<(PathBuf, ProjectRouteKey)> {
    let Some(project_path) = handshake.project_path.as_ref() else {
        return Err(TraceDecayError::Config {
            message: "project server requested without project_path".to_string(),
        });
    };
    let canonical_project_path = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.clone());
    let route = ProjectRouteKey::from_handshake(&canonical_project_path, handshake)?;
    Ok((canonical_project_path, route))
}

pub(super) async fn bind_authenticated_profile_identity(
    handshake: &mut DaemonHandshake,
    store_administration: &StoreAdministration,
) -> Result<StoreAdministration> {
    let profile_root = authority::canonical_identity_path(&handshake.client_identity.profile_root)?;
    let profile_identity = profile_identity::load_or_create(&profile_root)?;
    let scoped_administration = store_administration
        .clone()
        .with_profile_identity(profile_identity);
    let profile_database = scoped_administration.registered_profile_database().await?;
    let global_db_path = authority::canonical_identity_path(profile_database.db_path())?;
    let supplied_global_db_path =
        authority::canonical_identity_path(&handshake.client_identity.global_db_path)?;
    if supplied_global_db_path != global_db_path {
        return Err(TraceDecayError::Config {
            message: "daemon client global database does not match its registered profile runtime"
                .to_owned(),
        });
    }
    handshake.client_identity = DaemonClientIdentity {
        profile_root,
        global_db_path,
    };
    Ok(scoped_administration)
}

pub(super) async fn project_open_gate(
    gates: &tokio::sync::Mutex<ProjectOpenGates>,
    route: &ProjectRouteKey,
) -> Arc<ProjectOpenGate> {
    let mut gates = gates.lock().await;
    if let Some(gate) = gates.gates.get(route).and_then(std::sync::Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(ProjectOpenGate::new(()));
    gates.gates.insert(route.clone(), Arc::downgrade(&gate));
    gate
}

pub(super) async fn project_open_tasks(
    gates: &tokio::sync::Mutex<ProjectOpenGates>,
) -> ProjectOpenTasks {
    gates.lock().await.tasks.clone()
}

#[cfg(unix)]
pub(super) async fn maintenance_transition_gate(
    gates: &tokio::sync::Mutex<MaintenanceTransitionGates>,
    key: &ProjectServerKey,
) -> Arc<MaintenanceTransitionGate> {
    let transition_key = MaintenanceTransitionKey {
        profile_root: key.owner.profile_root.clone(),
        project_id: key.owner.project_id.clone(),
        scope_prefix: key.scope_prefix.clone(),
    };
    let mut gates = gates.lock().await;
    if let Some(gate) = gates
        .get(&transition_key)
        .and_then(std::sync::Weak::upgrade)
    {
        return gate;
    }
    let gate = Arc::new(MaintenanceTransitionGate::new(()));
    gates.insert(transition_key, Arc::downgrade(&gate));
    gate
}

#[cfg(any(not(unix), test, feature = "test-transport"))]
pub(super) fn portable_database_owner_reconciler(
    store_administration: StoreAdministration,
    current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
    route_registered: Arc<AtomicBool>,
    handshake: DaemonHandshake,
) -> crate::mcp::DatabaseOwnerReconciler {
    Arc::new(move |fresh| {
        let store_administration = store_administration.clone();
        let current_key = Arc::clone(&current_key);
        let route_registered = Arc::clone(&route_registered);
        let handshake = handshake.clone();
        Box::pin(async move {
            let scope = crate::daemon::branch_admin::graph_writer_scope(
                &fresh,
                crate::daemon::branch_admin::StoreWriterClass::Owner,
            );
            let transition = store_administration
                .with_writer_in(scope, || async {
                    if !route_registered.load(Ordering::Acquire) {
                        return None;
                    }
                    let new_key = match ProjectServerKey::from_open_project(&fresh, &handshake) {
                        Ok(key) => key,
                        Err(error) => {
                            eprintln!(
                                "[tracedecay] failed to rekey daemon database owner: {error}"
                            );
                            return None;
                        }
                    };
                    let mut current = current_key.lock().await;
                    if *current == new_key {
                        return None;
                    }
                    let old_key = current.clone();
                    let rekeyed = store_administration
                        .project_servers()
                        .lock()
                        .await
                        .rekey(&old_key, &new_key);
                    if !rekeyed {
                        route_registered.store(false, Ordering::Release);
                    }
                    *current = new_key.clone();
                    Some((old_key.owner, new_key.owner, rekeyed))
                })
                .await;
            let Some((old_owner, new_owner, rekeyed)) = transition else {
                return;
            };
            if rekeyed
                && new_owner.project_id.is_some()
                && let Ok(database) = store_administration
                    .registered_project_session_database(fresh.project_root(), fresh.store_layout())
                    .await
            {
                store_administration
                    .session_temporal_refresh_schedulers()
                    .rekey_project(&old_owner, new_owner, database)
                    .await;
            } else {
                store_administration
                    .session_temporal_refresh_schedulers()
                    .retire_project(&old_owner)
                    .await;
            }
        })
    })
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct CatalogRefreshClientKey {
    client_identity: DaemonClientIdentity,
    client_instance_id: String,
}

#[cfg(unix)]
impl CatalogRefreshClientKey {
    pub(super) fn from_handshake(handshake: &DaemonHandshake) -> Self {
        Self {
            client_identity: handshake.client_identity.clone(),
            client_instance_id: handshake.client_instance_id.clone(),
        }
    }
}
