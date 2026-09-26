//! Route resolution and per-route serialization for project opens.
//!
//! Owns the typed refusals a route open can raise (capacity, cancellation,
//! warming), the handshake-to-route mapping, the open and maintenance gates,
//! and the portable owner reconciler.

use super::*;
use tracedecay_daemon_identity::{authority, profile_identity};

pub(super) fn project_server_capacity_error() -> TraceDecayError {
    TraceDecayError::project_route(
        PROJECT_SERVER_CAPACITY_REASON_CODE,
        true,
        format!(
            "daemon project server capacity reached (capacity={MAX_CACHED_PROJECT_SERVERS}); retry after active clients finish"
        ),
    )
}

pub(super) fn project_open_task_capacity_error() -> TraceDecayError {
    TraceDecayError::project_route(
        PROJECT_OPEN_TASK_CAPACITY_REASON_CODE,
        true,
        format!(
            "daemon project open task capacity reached (capacity={MAX_TRACKED_PROJECT_OPEN_TASKS}); retry shortly"
        ),
    )
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
    TraceDecayError::project_route(
        PROJECT_WARMING_REASON_CODE,
        true,
        format!(
            "TraceDecay project '{}' {PROJECT_WARMING_RETRY_HINT}",
            project_path.display(),
        ),
    )
}

/// After the foreground publication bound, prefer a terminal open failure
/// over a warming hint. Warming means the route is still opening.
pub(super) fn prefer_recorded_open_failure<T>(
    result: Result<T>,
    state: &tokio::sync::watch::Receiver<ProjectOpenTaskState>,
) -> Result<T> {
    let error = match result {
        Err(error) => error,
        other => return other,
    };
    if !error_is_project_warming(&error) {
        return Err(error);
    }
    match state.borrow().clone() {
        ProjectOpenTaskState::Failed(failure) => Err(failure.to_error()),
        ProjectOpenTaskState::Opening | ProjectOpenTaskState::Ready => Err(error),
    }
}

pub(super) fn project_route_for_handshake(
    handshake: &DaemonHandshake,
    owner_home: Option<&Path>,
) -> Result<(PathBuf, ProjectRouteKey)> {
    let Some(project_path) = handshake.project_path.as_ref() else {
        return Err(TraceDecayError::project_route(
            PROJECT_REQUIRED_REASON_CODE,
            false,
            "this operation needs a TraceDecay project, and the request named none; \
             run it inside an initialized project or pass --project <path>",
        ));
    };
    let canonical_project_path =
        tracedecay_runtime_core::path_safety::canonical_root_identity(project_path);
    if tracedecay_runtime_core::config::is_ambient_project_root(owner_home, &canonical_project_path)
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "'{}' is an ambient user/filesystem root, not an active TraceDecay code project",
                canonical_project_path.display()
            ),
        });
    }
    let route = ProjectRouteKey::from_handshake(&canonical_project_path, handshake)?;
    Ok((canonical_project_path, route))
}

#[hotpath::measure(label = "daemon.project.bind.identity", future = true)]
pub(super) async fn bind_authenticated_profile_identity(
    handshake: &mut DaemonHandshake,
    store_administration: &StoreAdministration,
) -> Result<StoreAdministration> {
    let profile_root = authority::canonical_identity_path(&handshake.client_identity.profile_root)?;
    let daemon_profile_root = authority::canonical_identity_path(
        store_administration.profile_identity()?.profile_root(),
    )?;
    if profile_root != daemon_profile_root {
        store_administration
            .retain_authenticated_profile_database_scope(&profile_root)
            .await?;
    }
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
) -> Result<Arc<ProjectOpenGate>> {
    let mut gate_route = route.clone();
    match tracedecay_runtime_core::worktree::git_common_dir_outcome(&route.project_path) {
        Ok(Some(git_common_dir)) => gate_route.project_path = git_common_dir,
        Ok(None) => {}
        Err(tracedecay_runtime_core::git_repository::GitRepositoryError::DiscoveryBlocked {
            ..
        }) => {
            return Err(super::core_proxy::repository_discovery_deferred(
                &route.project_path,
                tracedecay_runtime_core::git_discovery::GitDiscoveryUnknown::DeadlineExceeded,
            ));
        }
        Err(_) => {}
    }
    let mut gates = gates.lock().await;
    if let Some(gate) = gates
        .gates
        .get(&gate_route)
        .and_then(std::sync::Weak::upgrade)
    {
        return Ok(gate);
    }
    let gate = Arc::new(ProjectOpenGate::new(()));
    gates.gates.insert(gate_route, Arc::downgrade(&gate));
    Ok(gate)
}

pub(super) async fn project_open_capacity_gate(
    gates: &tokio::sync::Mutex<ProjectOpenGates>,
) -> Arc<ProjectOpenGate> {
    Arc::clone(&gates.lock().await.capacity_gate)
}

pub(super) async fn project_open_tasks(
    gates: &tokio::sync::Mutex<ProjectOpenGates>,
) -> ProjectOpenTasks {
    gates.lock().await.tasks.clone()
}

/// Run one blocking repository/filesystem probe off the async workers.
///
/// Live defect this exists for: a daemon connection resolved its route by
/// running `gix` discovery, enrollment-marker reads, and a HEAD read inline on
/// the tokio worker that was serving it. With sixteen clients arriving at once
/// against a checkout on a slow volume, every worker sat inside that
/// filesystem work, the accept loop was never polled, and the listening socket
/// refused new connections while the process stayed alive and its other
/// servers answered.
///
/// The probe itself cannot be interrupted once it starts, but the caller is
/// bounded: a probe that outlives [`REPOSITORY_DISCOVERY_DEADLINE`] yields the
/// same retryable deferred-discovery refusal a timed-out `git` helper does,
/// and the abandoned probe finishes on the blocking pool without a worker.
pub(super) async fn bounded_repository_probe<Probe, Value>(
    project_path: &Path,
    probe: Probe,
) -> Result<Value>
where
    Probe: FnOnce() -> Value + Send + 'static,
    Value: Send + 'static,
{
    let probe = tokio::task::spawn_blocking(probe);
    let budget = repository_probe_budget(project_path);
    tokio::pin!(probe);
    tokio::pin!(budget);
    match tokio::select! {
        biased;
        joined = &mut probe => Ok(joined),
        () = &mut budget => Err(()),
    } {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(_)) => Err(super::core_proxy::repository_discovery_deferred(
            project_path,
            tracedecay_runtime_core::git_discovery::GitDiscoveryUnknown::ProbeFailed,
        )),
        Err(()) => Err(super::core_proxy::repository_discovery_deferred(
            project_path,
            tracedecay_runtime_core::git_discovery::GitDiscoveryUnknown::DeadlineExceeded,
        )),
    }
}

/// Wall-clock discovery budget, or the moment a test parks the walk.
///
/// The parked walk is already past any useful wait: returning here marks the
/// project discovery-blocked without sleeping out the production deadline.
async fn repository_probe_budget(project_path: &Path) {
    if tracedecay_runtime_core::git_repository::wait_until_repository_discovery_blocks(project_path)
        .await
    {
        return;
    }
    tokio::time::sleep(REPOSITORY_DISCOVERY_DEADLINE).await;
}

/// Finish or refuse repository discovery before any cross-project admission lock.
///
/// Live defect this exists for: one project's `open()` of a git ref ran while
/// the process-wide project-open capacity gate was held, so every other
/// project's open queued behind that hang and the profile runtime never
/// reached Ready.
pub(super) async fn ensure_checkout_topology_before_admission(project_path: &Path) -> Result<()> {
    match super::core_proxy::bounded_repository_identity(
        project_path,
        super::core_proxy::repository_discovery_parent_deadline(),
    )
    .await
    {
        tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome::Resolved(_)
        | tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome::NotRepository => {
            Ok(())
        }
        tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome::Unknown(reason) => {
            Err(super::core_proxy::repository_discovery_deferred(
                project_path,
                reason,
            ))
        }
    }
}

#[hotpath::measure(label = "daemon.project.route.resolve", future = true)]
pub(super) async fn resolved_project_server_key(
    store_administration: &StoreAdministration,
    canonical_project_path: &Path,
    handshake: &DaemonHandshake,
) -> Result<Option<ProjectServerKey>> {
    if !durable_enrollment_resolves_existing_store(store_administration, canonical_project_path)
        .await?
    {
        return Ok(None);
    }
    let registry_database = store_administration.registered_profile_database().await?;
    let Ok(layout) =
        tracedecay_project::project::TraceDecay::resolve_registered_configuration_layout(
            canonical_project_path,
            &crate::daemon::handshake_open_options(handshake),
            registry_database.as_ref(),
        )
        .await
    else {
        // The canonical open remains responsible for typed identity errors and
        // any permitted repair; this is only a mounted-runtime reuse path.
        return Ok(None);
    };
    let probe_path = canonical_project_path.to_path_buf();
    let data_root = layout.data_root.clone();
    let (graph_db_path, fallback_warning) =
        bounded_repository_probe(canonical_project_path, move || {
            let graph_scope =
                tracedecay_runtime_core::branch::current_branch(&probe_path).or_else(|| {
                    tracedecay_runtime_core::worktree::detached_worktree_graph_scope(&probe_path)
                });
            let (graph_db_path, _, fallback_warning) =
                tracedecay_project::project::TraceDecay::resolve_db_for_branch(
                    &probe_path,
                    &data_root,
                    graph_scope.as_deref(),
                );
            (graph_db_path, fallback_warning)
        })
        .await?;
    if fallback_warning.is_some() {
        return Ok(None);
    }
    Ok(Some(ProjectServerKey {
        owner: store_owner_key_from_paths(
            &handshake.client_identity.profile_root,
            &handshake.client_identity.global_db_path,
            layout.identity.project_id,
            &layout.data_root,
            &graph_db_path,
        )?,
        project_root: authority::canonical_identity_path(&layout.project_root)?,
        scope_prefix: handshake.scope_prefix.clone(),
    }))
}

pub(super) async fn cached_or_bind_ready_project_server(
    store_administration: &StoreAdministration,
    route: &ProjectRouteKey,
    resolved_key: Option<&ProjectServerKey>,
    requirement: ProjectServerRequirement,
) -> Option<(ProjectServerKey, Arc<crate::mcp::McpServer>)> {
    let mut servers = store_administration.project_servers().lock().await;
    if let Some((key, server)) = servers.get_route_and_touch_for(route, requirement) {
        return Some((key.clone(), Arc::clone(server)));
    }
    let key = resolved_key?;
    let server = servers.bind_ready_route(route.clone(), key.clone(), requirement)?;
    Some((key.clone(), Arc::clone(server)))
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
    route_cancellation: CancellationToken,
    handshake: DaemonHandshake,
) -> crate::mcp::DatabaseOwnerReconciler {
    Arc::new(move |fresh| {
        let store_administration = store_administration.clone();
        let current_key = Arc::clone(&current_key);
        let route_registered = Arc::clone(&route_registered);
        let route_cancellation = route_cancellation.clone();
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
                        // Terminal revocation: this route can never serve
                        // again, so drop its fence and end everything that
                        // waits on its lifetime.
                        route_registered.store(false, Ordering::Release);
                        route_cancellation.cancel();
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
