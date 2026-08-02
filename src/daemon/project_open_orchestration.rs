//! Orchestration of a project open, from route registration through warm-up.
//!
//! Covers the lifecycle-guarded open used by the Unix broker and the portable
//! cached-open, warm-up and request paths, so a single route never opens twice
//! and a draining daemon never starts a new one.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

/// Bounds how long a foreground request waits for a route's background open.
/// The open task itself is deliberately left running after the deadline.
pub(super) async fn wait_for_project_open_publication<Publication, Output>(
    project_path: &Path,
    publication: Publication,
) -> Result<Output>
where
    Publication: std::future::Future<Output = Result<Output>>,
{
    timeout(PROJECT_OPEN_REQUEST_DEADLINE, publication)
        .await
        .map_err(|_| project_warming_error(project_path))?
}

pub(super) async fn start_lifecycle_project_open<OpenOperation, OpenFuture>(
    tasks: &ProjectOpenTasks,
    lifecycle: DaemonLifecycle,
    route: ProjectRouteKey,
    project_path: PathBuf,
    initialize_request: Option<JsonRpcRequest>,
    open_project_server: OpenOperation,
) -> ProjectOpenTaskClaim
where
    OpenOperation: FnOnce(CancellationToken) -> OpenFuture + Send + 'static,
    OpenFuture: std::future::Future<Output = Result<Arc<crate::mcp::McpServer>>> + Send + 'static,
{
    if !lifecycle.accepting() {
        return ProjectOpenTaskClaim::Failed(ProjectOpenFailure {
            message: "daemon is draining before project warm-up".to_string(),
            retry_at: None,
        });
    }
    tasks
        .start_cancellable(route, move |cancellation| async move {
            let Some(activity) = lifecycle.try_enter() else {
                return Err(TraceDecayError::Config {
                    message: "daemon is draining before project warm-up".to_string(),
                });
            };
            let _activity = activity;
            // Once admitted, warm-up may be inside a schema migration. The
            // cancellation token is observed only at explicit boundaries around
            // those transactionally safe units; dropping this future on drain
            // would untrack the database owner and can interrupt SQLite
            // mid-statement. The lifecycle activity remains held until the task
            // reports its terminal outcome and shutdown explicitly joins it.
            let result = Box::pin(open_project_server(cancellation.clone())).await;
            match result {
                Ok(server) => {
                    project_open_cancellation_checkpoint(&cancellation)?;
                    if let Some(initialize_request) = initialize_request {
                        // Preserve the regular initialize side effect that records
                        // the negotiated MCP client name on the real server.
                        let initialize: std::pin::Pin<
                            Box<
                                dyn std::future::Future<Output = Option<JsonRpcResponse>>
                                    + Send
                                    + '_,
                            >,
                        > = Box::pin(server.handle_request(&initialize_request));
                        let _ = initialize.await;
                    }
                    Ok(())
                }
                Err(error) => {
                    if cancellation.is_cancelled() {
                        return Err(error);
                    }
                    log_daemon_event(
                        "project_server_warmup",
                        &[
                            ("outcome", "error".to_string()),
                            ("project", project_path.display().to_string()),
                            ("error", error.to_string()),
                        ],
                    );
                    Err(error)
                }
            }
        })
        .await
}

#[cfg_attr(not(unix), allow(dead_code))] // used by unix-only daemon serving paths
pub(super) fn spawn_lifecycle_automation_scheduler_activation<ActivationFuture>(
    lifecycle: DaemonLifecycle,
    activation: ActivationFuture,
) where
    ActivationFuture: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(activity) = lifecycle.try_enter() else {
        return;
    };
    tokio::spawn(async move {
        let _activity = activity;
        tokio::select! {
            biased;
            () = lifecycle.wait_for_draining() => {}
            () = activation => {}
        }
    });
}

pub(super) async fn ensure_registered_project_route(
    store_administration: &StoreAdministration,
    project_path: &Path,
    allow_init: bool,
) -> Result<()> {
    let registry = store_administration.registered_profile_database().await?;
    let context = match registry
        .project_registry_context_by_alias(project_path)
        .await?
    {
        Some(context) => Some(context),
        None => {
            let git_root = crate::worktree::git_worktree_root(project_path)
                .unwrap_or_else(|| project_path.to_path_buf());
            let git_common_dir = crate::worktree::git_common_dir(&git_root);
            registry
                .project_registry_context_by_identity(&git_root, git_common_dir.as_deref())
                .await?
        }
    };
    if context.is_none() {
        let project_path = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.to_path_buf());
        if durable_enrollment_resolves_existing_store(store_administration, &project_path) {
            return Ok(());
        }
        let is_project_root = crate::worktree::git_worktree_root(&project_path)
            .is_none_or(|git_root| git_root == project_path);
        let owns_repository_identity =
            crate::worktree::repository_identity_root(&project_path).is_none();
        if allow_init && is_project_root && owns_repository_identity {
            return Ok(());
        }
        return Err(unenrolled_project_route_error(&project_path));
    }
    Ok(())
}

/// Whether this route's durable on-disk enrollment already resolves a real
/// profile store, so admitting it mounts recovered data rather than
/// manufacturing a new identity.
///
/// The profile registry is a *derived* index: the authoritative identity chain
/// in [`crate::tracedecay::TraceDecay::resolve_registered_configuration_layout`]
/// consults the project's own enrollment marker (and the repository-identity
/// marker) BEFORE it ever asks the registry, and a successful open republishes
/// the registry rows via `register_project_store_in_global_registry`. A guard
/// that admits strictly less than the resolver behind it therefore refuses
/// projects whose data is entirely intact.
///
/// That is exactly what strands a profile parked at a forward-only migration
/// boundary: forward recovery can bring the daemon up on a fresh registry while
/// every project keeps its in-repo enrollment marker and its profile store, and
/// the first daemon-brokered call — including the post-update startup-health
/// probe, which cannot pass `allow_init` — was rejected as "not enrolled". The
/// existing store is required to be present on disk, so an ambient directory
/// (a bare `$HOME`, a checkout whose store really is gone) is still rejected
/// and no path-derived authority is minted here.
fn durable_enrollment_resolves_existing_store(
    store_administration: &StoreAdministration,
    project_path: &Path,
) -> bool {
    let Ok(identity) = store_administration.profile_identity() else {
        return false;
    };
    let Ok(Some(layout)) =
        crate::storage::resolve_persisted_layout(project_path, identity.profile_root())
    else {
        return false;
    };
    layout.graph_db_path.is_file()
        || layout.sessions_db_path.is_file()
        || layout
            .manifest_path
            .as_deref()
            .is_some_and(std::path::Path::is_file)
}

fn unenrolled_project_route_error(project_path: &Path) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "no TraceDecay index found at '{}': project is not enrolled in the authenticated \
             profile; run 'tracedecay init' first",
            project_path.display()
        ),
    }
}

#[cfg(any(not(unix), test))]
pub(super) async fn portable_cached_project_server(
    store_administration: &StoreAdministration,
    canonical_project_path: &Path,
    handshake: &DaemonHandshake,
    requirement: ProjectServerRequirement,
) -> Result<Option<Arc<crate::mcp::McpServer>>> {
    let route = ProjectRouteKey::from_handshake(canonical_project_path, handshake)?;
    let server = {
        let mut servers = store_administration.project_servers().lock().await;
        servers
            .get_route_and_touch_for(&route, requirement)
            .map(|(_, server)| Arc::clone(server))
    };
    let Some(server) = server else {
        return Ok(None);
    };
    ensure_registered_project_route(
        store_administration,
        canonical_project_path,
        handshake.allow_init,
    )
    .await?;
    Ok(Some(server))
}

#[cfg(any(not(unix), test))]
// Cohesive route-open context; a params struct would only move the same ownership bundle.
#[allow(clippy::too_many_arguments)]
async fn begin_portable_project_open(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    handshake: DaemonHandshake,
    canonical_project_path: PathBuf,
    route: ProjectRouteKey,
    initialize_request: Option<JsonRpcRequest>,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> ProjectOpenTaskClaim {
    let tasks = project_open_tasks(project_open_gates.as_ref()).await;
    let open_project_path = canonical_project_path.clone();
    let open_gates = Arc::clone(&project_open_gates);
    Box::pin(start_lifecycle_project_open(
        &tasks,
        lifecycle,
        route,
        canonical_project_path,
        initialize_request,
        move |cancellation| async move {
            // A request is waiting on this open. The writer wait is therefore
            // bounded: an enrolled project takes only its own store's owner
            // lane, and a wait that still outlives the deadline answers with a
            // typed retryable busy error instead of parking without bound.
            let scope = branch_admin::project_open_writer_scope(
                &open_project_path,
                &handshake.client_identity.profile_root,
            );
            match store_administration
                .with_writer_admission(
                    scope,
                    &cancellation,
                    Some(branch_admin::REQUEST_WRITER_ADMISSION_DEADLINE),
                    || async {
                        production_project_server(
                            &store_administration,
                            open_gates.as_ref(),
                            &invocation,
                            &http_application_registry,
                            &open_project_path,
                            &handshake,
                            ProductionProjectCompositionRuntime::Portable {
                                semantic_auto_download: true,
                                startup_catch_up: true,
                            },
                            &cancellation,
                            #[cfg(test)]
                            project_open_attempts.as_ref(),
                        )
                        .await
                        .map(|composition| composition.server)
                    },
                )
                .await
            {
                branch_admin::WriterAdmission::Completed(result) => result,
                branch_admin::WriterAdmission::Cancelled => {
                    return Err(project_open_cancellation_error());
                }
                branch_admin::WriterAdmission::Busy => {
                    return Err(project_open_writer_busy_error(&open_project_path));
                }
            }
        },
    ))
    .await
}

#[cfg(any(not(unix), test))]
pub(super) async fn schedule_portable_project_server_warmup(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    handshake: DaemonHandshake,
    initialize_request: JsonRpcRequest,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<()> {
    let (canonical_project_path, route) = project_route_for_handshake(&handshake)?;
    if portable_cached_project_server(
        &store_administration,
        &canonical_project_path,
        &handshake,
        ProjectServerRequirement::Core,
    )
    .await?
    .is_some()
    {
        return Ok(());
    }
    match Box::pin(begin_portable_project_open(
        lifecycle,
        store_administration,
        project_open_gates,
        invocation,
        http_application_registry,
        handshake,
        canonical_project_path,
        route,
        Some(initialize_request),
        #[cfg(test)]
        project_open_attempts,
    ))
    .await
    {
        ProjectOpenTaskClaim::InFlight(_) => Ok(()),
        ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
        ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
    }
}

#[cfg(any(not(unix), test))]
pub(super) async fn portable_project_server_for_request(
    lifecycle: DaemonLifecycle,
    store_administration: StoreAdministration,
    project_open_gates: Arc<tokio::sync::Mutex<ProjectOpenGates>>,
    invocation: DaemonInvocationState,
    http_application_registry: http_application::DaemonHttpApplicationRegistry,
    handshake: &DaemonHandshake,
    requirement: ProjectServerRequirement,
    #[cfg(test)] project_open_attempts: Option<Arc<AtomicUsize>>,
) -> Result<Arc<crate::mcp::McpServer>> {
    let (canonical_project_path, route) = project_route_for_handshake(handshake)?;
    if let Some(server) = portable_cached_project_server(
        &store_administration,
        &canonical_project_path,
        handshake,
        requirement,
    )
    .await?
    {
        return Ok(server);
    }
    // Foreground requests must never pin a connection while a cold project
    // warm-up runs. The open task remains tracked and continues in the
    // background after this bounded wait expires.
    let claim = Box::pin(begin_portable_project_open(
        lifecycle,
        store_administration.clone(),
        project_open_gates,
        invocation,
        http_application_registry,
        handshake.clone(),
        canonical_project_path.clone(),
        route,
        None,
        #[cfg(test)]
        project_open_attempts,
    ))
    .await;
    match claim {
        ProjectOpenTaskClaim::InFlight(mut state) => {
            let publication = async {
                loop {
                    if let Some(server) = portable_cached_project_server(
                        &store_administration,
                        &canonical_project_path,
                        handshake,
                        requirement,
                    )
                    .await?
                    {
                        return Ok(server);
                    }
                    let current = state.borrow().clone();
                    match current {
                        ProjectOpenTaskState::Opening => {
                            tokio::select! {
                                changed = state.changed() => {
                                    changed.map_err(|_| TraceDecayError::Config {
                                        message: "project open task ended before reporting an outcome"
                                            .to_string(),
                                    })?;
                                }
                                () = tokio::time::sleep(Duration::from_millis(25)) => {}
                            }
                        }
                        ProjectOpenTaskState::Ready => {
                            return Err(TraceDecayError::Config {
                                message: "project open completed without publishing a server"
                                    .to_string(),
                            });
                        }
                        ProjectOpenTaskState::Failed(failure) => {
                            return Err(failure.to_error());
                        }
                    }
                }
            };
            // Riding out an open is a park, not work: the admission slot is
            // released for the wait's duration so a tool that needs no project
            // owner is never shed by a queue of warming clients. The wait stays
            // bounded by PROJECT_OPEN_REQUEST_DEADLINE inside the helper.
            park_admission(wait_for_project_open_publication(
                &canonical_project_path,
                publication,
            ))
            .await
        }
        ProjectOpenTaskClaim::Failed(failure) => Err(failure.to_error()),
        ProjectOpenTaskClaim::Saturated => Err(project_open_task_capacity_error()),
    }
}

#[cfg(any(not(unix), test))]
pub(super) async fn portable_cached_project_open_failure(
    project_open_gates: &tokio::sync::Mutex<ProjectOpenGates>,
    handshake: &DaemonHandshake,
) -> Result<Option<ProjectOpenFailure>> {
    let (_, route) = project_route_for_handshake(handshake)?;
    let tasks = project_open_tasks(project_open_gates).await;
    Ok(tasks.cached_failure(&route).await)
}

#[cfg(not(unix))]
pub(super) async fn shutdown_portable_project_open_tasks(
    project_open_gates: &tokio::sync::Mutex<ProjectOpenGates>,
) {
    project_open_tasks(project_open_gates)
        .await
        .shutdown()
        .await;
}
