//! Code-index activation wiring for one production project route.
//!
//! These builders outlive `production_project_server`: the mount closure runs
//! when the activation owner first admits indexing, and the three sinks stay
//! reachable from the published MCP servers. Grouping them here keeps the
//! composition root a readable sequence of phases.

use super::*;

/// Inputs the deferred mount closure re-clones on every activation attempt.
/// Bundled so the builder keeps one argument list instead of thirteen
/// positional parameters.
pub(super) struct CodeIndexActivationMountInputs {
    pub(super) invocation: DaemonInvocationState,
    pub(super) project_id: tracedecay_domain::ProjectId,
    pub(super) project_root: PathBuf,
    pub(super) store_root: PathBuf,
    pub(super) semantic_runtime: crate::semantic_code::DaemonSemanticRuntimeHandleV1,
    pub(super) semantic_lifecycle: Option<Arc<crate::semantic_code::SemanticModelLifecycleOwnerV1>>,
    pub(super) semantic_resources: crate::config::SemanticResourceCeilings,
    pub(super) scope: tracedecay_application::ResolvedScope,
    pub(super) route_registered: Arc<AtomicBool>,
    pub(super) cancellation: CancellationToken,
    pub(super) graph_runtime:
        Arc<crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1>,
    pub(super) graph_publication_database: Arc<crate::db::Database>,
    pub(super) profile_id: tracedecay_domain::configuration::UserProfileId,
}

/// Build the deferred code-index mount for this route. The closure is fenced on
/// both the route registration flag and the open cancellation token, and it
/// subscribes to generation publications *before* mounting so the first sealed
/// generation cannot be missed between the mount and the subscription.
pub(super) fn code_index_activation_mount(
    inputs: CodeIndexActivationMountInputs,
) -> code_index_scheduler::CodeIndexActivationMountV1 {
    let CodeIndexActivationMountInputs {
        invocation,
        project_id,
        project_root,
        store_root,
        semantic_runtime,
        semantic_lifecycle,
        semantic_resources,
        scope,
        route_registered,
        cancellation,
        graph_runtime,
        graph_publication_database,
        profile_id,
    } = inputs;
    let mount: code_index_scheduler::CodeIndexActivationMountV1 = Arc::new(move || {
        let invocation = invocation.clone();
        let project_id = project_id.clone();
        let project_root = project_root.clone();
        let store_root = store_root.clone();
        let semantic_runtime = semantic_runtime.clone();
        let semantic_lifecycle = semantic_lifecycle.clone();
        let semantic_resources = semantic_resources;
        let scope = scope.clone();
        let route_registered = Arc::clone(&route_registered);
        let cancellation = cancellation.clone();
        let graph_runtime = Arc::clone(&graph_runtime);
        let graph_publication_database = Arc::clone(&graph_publication_database);
        let profile_id = profile_id.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() || !route_registered.load(Ordering::Acquire) {
                return Err("project route was revoked before code-index mount".to_owned());
            }
            // Order-sensitive: subscribing before the mount is what keeps the
            // first generation publication observable by the waiter below.
            let publications = invocation
                .code_index_schedulers
                .subscribe_generation_publications();
            let mount = invocation.mount_code_index(
                project_id,
                &project_root,
                store_root,
                Some(&semantic_runtime),
                semantic_lifecycle,
                Some(semantic_resources),
                graph_runtime,
                graph_publication_database,
            );
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    return Err("project route was cancelled during code-index mount".to_owned());
                }
                outcome = mount => outcome.map_err(|error| error.to_string())?,
            }
            if cancellation.is_cancelled() || !route_registered.load(Ordering::Acquire) {
                return Err("project route was revoked after code-index mount".to_owned());
            }

            // Query authority depends on the first sealed generation, but
            // hook hints must become deliverable as soon as the scheduler is
            // mounted. Keep that wait in its own route-fenced task.
            spawn_query_authority_when_generation_ready(QueryAuthorityWaitInputs {
                invocation: invocation.clone(),
                publications,
                project_root: project_root.clone(),
                profile_id: profile_id.clone(),
                scope: scope.clone(),
                route_registered: Arc::clone(&route_registered),
                cancellation: cancellation.clone(),
            });
            Ok(())
        })
    });
    mount
}

/// Route-fenced inputs for the post-mount query-authority wait.
struct QueryAuthorityWaitInputs {
    invocation: DaemonInvocationState,
    publications:
        tokio::sync::broadcast::Receiver<code_index_scheduler::CodeIndexGenerationPublishedV1>,
    project_root: PathBuf,
    profile_id: tracedecay_domain::configuration::UserProfileId,
    scope: tracedecay_application::ResolvedScope,
    route_registered: Arc<AtomicBool>,
    cancellation: CancellationToken,
}

/// Wait for this project's first sealed generation, then mount its query
/// authority. Route revocation, cancellation, and a closed publication channel
/// each end the wait without mounting; a lagged channel or the route poll
/// re-reads the serving slot, because a retained `Noop` restore does not
/// republish.
fn spawn_query_authority_when_generation_ready(inputs: QueryAuthorityWaitInputs) {
    let QueryAuthorityWaitInputs {
        invocation: authority_invocation,
        mut publications,
        project_root: authority_project,
        profile_id: authority_profile_id,
        scope: authority_scope,
        route_registered: authority_route_registered,
        cancellation: authority_cancellation,
    } = inputs;
    tokio::spawn(async move {
        let mut route_poll = tokio::time::interval(std::time::Duration::from_secs(1));
        route_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let generation_ready = if authority_invocation
            .code_index_schedulers
            .latest_generation_id(&authority_project)
            .await
            .is_some()
        {
            true
        } else {
            loop {
                if !authority_route_registered.load(Ordering::Acquire) {
                    break false;
                }
                tokio::select! {
                    () = authority_cancellation.cancelled() => break false,
                    _ = route_poll.tick() => {
                        if !authority_route_registered.load(Ordering::Acquire) {
                            break false;
                        }
                        if authority_invocation
                            .code_index_schedulers
                            .latest_generation_id(&authority_project)
                            .await
                            .is_some()
                        {
                            break true;
                        }
                    }
                    publication = publications.recv() => match publication {
                        Ok(publication)
                            if publication.project_root == authority_project =>
                        {
                            break true;
                        }
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            if authority_invocation
                                .code_index_schedulers
                                .latest_generation_id(&authority_project)
                                .await
                                .is_some()
                            {
                                break true;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break false;
                        }
                    }
                }
            }
        };
        if !generation_ready
            || authority_cancellation.is_cancelled()
            || !authority_route_registered.load(Ordering::Acquire)
        {
            return;
        }
        let outcome = tokio::select! {
            biased;
            () = authority_cancellation.cancelled() => return,
            outcome = authority_invocation.mount_query_authority_for_project(
                &authority_project,
                &authority_profile_id,
                &authority_scope,
            ) => outcome,
        };
        if authority_cancellation.is_cancelled()
            || !authority_route_registered.load(Ordering::Acquire)
        {
            return;
        }
        let mut fields = vec![
            ("project", authority_project.display().to_string()),
            ("phase", "code_index_query_authority".to_owned()),
        ];
        match outcome {
            Ok(()) => fields.push(("outcome", "mounted".to_owned())),
            Err(error) => {
                fields.push(("outcome", "degraded".to_owned()));
                fields.push(("error", error.to_string()));
            }
        }
        log_daemon_event("project_open_phase", &fields);
    });
}

/// Hint sink handed to the activation owner: it coalesces after-edit hook paths
/// and overflow notices onto the mounted scheduler.
pub(super) fn code_index_activation_hint_sink(
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
) -> code_index_scheduler::CodeIndexActivationHintSinkV1 {
    let sink: code_index_scheduler::CodeIndexActivationHintSinkV1 = Arc::new(move |batch| {
        let schedulers = schedulers.clone();
        let project_root = project_root.clone();
        Box::pin(async move {
            let paths_accepted = if batch.paths.is_empty() {
                true
            } else {
                schedulers
                    .notify_hook_paths(&project_root, &batch.paths)
                    .await
            };
            let overflow_accepted = if batch.overflow {
                schedulers.notify_hook_overflow(&project_root).await
            } else {
                true
            };
            paths_accepted && overflow_accepted
        })
    });
    sink
}

/// MCP-facing after-edit hook sink. Accepts hints before the mount completes:
/// the activation owner bounds and coalesces them, keeping this hook path
/// independent of indexing.
pub(super) fn code_index_hook_sink(
    activation: Arc<code_index_scheduler::CodeIndexActivationV1>,
) -> crate::mcp::server::CodeIndexHookSink {
    let sink: crate::mcp::server::CodeIndexHookSink =
        Arc::new(move |root: PathBuf, rel_paths: Vec<String>| {
            let activation = Arc::clone(&activation);
            Box::pin(async move { activation.notify_hook_paths(&root, rel_paths).await })
        });
    sink
}

/// MCP-facing reconcile sink: an overflowed hook batch asks the activation
/// owner for a full reconcile instead of enumerating paths.
pub(super) fn code_index_reconcile_sink(
    activation: Arc<code_index_scheduler::CodeIndexActivationV1>,
) -> crate::mcp::server::CodeIndexReconcileSink {
    let sink: crate::mcp::server::CodeIndexReconcileSink = Arc::new(move |root: PathBuf| {
        let activation = Arc::clone(&activation);
        Box::pin(async move { activation.notify_hook_overflow(&root).await })
    });
    sink
}
