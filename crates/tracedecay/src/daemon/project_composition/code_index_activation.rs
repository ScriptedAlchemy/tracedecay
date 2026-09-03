//! Code-index activation wiring for one production project route.
//!
//! These builders outlive `production_project_server`: the mount closure runs
//! when the activation owner first admits indexing, and the three sinks stay
//! reachable from the published MCP servers.

use super::*;
use tracedecay_code_index_runtime::code_index_scheduler::query_runtime::QueryRuntimeMountErrorV1;
use tracedecay_semantic_contracts::SemanticResourceCeilings;

/// Inputs the deferred mount closure re-clones on every activation attempt.
/// Bundled so the builder keeps one argument list instead of thirteen
/// positional parameters.
pub(super) struct CodeIndexActivationMountInputs {
    pub(super) invocation: DaemonInvocationState,
    pub(super) project_id: tracedecay_domain::ProjectId,
    pub(super) project_root: PathBuf,
    pub(super) store_root: PathBuf,
    pub(super) semantic_runtime: tracedecay_semantic::DaemonSemanticRuntimeHandleV1,
    pub(super) semantic_lifecycle: Option<Arc<tracedecay_semantic::SemanticModelLifecycleOwnerV1>>,
    pub(super) semantic_resources: SemanticResourceCeilings,
    pub(super) native_graph_activation: bool,
    pub(super) scope: tracedecay_application::ResolvedScope,
    pub(super) route_registered: Arc<AtomicBool>,
    pub(super) cancellation: CancellationToken,
    pub(super) graph_runtime: Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>,
    pub(super) graph_publication_database: Arc<tracedecay_runtime_core::db::Database>,
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
        native_graph_activation,
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
        let native_graph_activation = native_graph_activation;
        let scope = scope.clone();
        let route_registered = Arc::clone(&route_registered);
        let cancellation = cancellation.clone();
        let graph_runtime = Arc::clone(&graph_runtime);
        let graph_publication_database = Arc::clone(&graph_publication_database);
        let profile_id = profile_id.clone();
        Box::pin(hotpath::future!(
            async move {
                if cancellation.is_cancelled() || !route_registered.load(Ordering::Acquire) {
                    return Err("project route was revoked before code-index mount".to_owned());
                }
                let query_project_id = project_id.clone();
                let query_graph_runtime = Arc::clone(&graph_runtime);
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
                    native_graph_activation,
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
                    project_id: query_project_id,
                    graph_runtime: query_graph_runtime,
                    profile_id: profile_id.clone(),
                    scope: scope.clone(),
                    route_registered: Arc::clone(&route_registered),
                    cancellation: cancellation.clone(),
                });
                Ok(())
            },
            label = "daemon.project.activate.mount"
        ))
    });
    mount
}

/// Route-fenced inputs for the post-mount query-authority wait.
struct QueryAuthorityWaitInputs {
    invocation: DaemonInvocationState,
    publications:
        tokio::sync::broadcast::Receiver<code_index_scheduler::CodeIndexGenerationPublishedV1>,
    project_root: PathBuf,
    project_id: tracedecay_domain::ProjectId,
    graph_runtime: Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>,
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
        project_id: authority_project_id,
        graph_runtime: authority_graph_runtime,
        profile_id: authority_profile_id,
        scope: authority_scope,
        route_registered: authority_route_registered,
        cancellation: authority_cancellation,
    } = inputs;
    tokio::spawn(hotpath::future!(
        async move {
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
            let mut awaiting_generation_logged = false;
            loop {
                let configured = tokio::select! {
                    biased;
                    () = authority_cancellation.cancelled() => return,
                    outcome = authority_invocation.mount_query_authority_for_project(
                        &authority_project,
                        &authority_profile_id,
                        &authority_scope,
                    ) => outcome,
                };
                let outcome = match configured {
                    Err(
                        error @ (QueryRuntimeMountErrorV1::Provider(_)
                        | QueryRuntimeMountErrorV1::AuthorityMissing
                        | QueryRuntimeMountErrorV1::Authority(
                            tracedecay_query::retrieval::QueryAuthorityErrorV1::AuthorityUnavailable,
                        )),
                    ) => {
                        // A fresh profile has no evaluated optional authority. The
                        // post-seat owner must therefore install the checked-in core
                        // policy from this project's durable cursor-key authority;
                        // otherwise a ready generation remains unqueryable forever.
                        match authority_graph_runtime
                            .mounted_project_sessions(&authority_project_id)
                            .await
                        {
                            Some(session_db) => {
                                match session_db.load_session_cursor_key_provider_result().await {
                                    Ok(cursor_keys) => {
                                        authority_invocation
                                            .mount_core_query_authority_for_project(
                                                &authority_project,
                                                &authority_scope,
                                                &cursor_keys,
                                            )
                                            .await
                                    }
                                    Err(_) => Err(QueryRuntimeMountErrorV1::KeyUnavailable),
                                }
                            }
                            None => Err(error),
                        }
                    }
                    outcome => outcome,
                };
                if authority_cancellation.is_cancelled()
                    || !authority_route_registered.load(Ordering::Acquire)
                {
                    return;
                }
                match outcome {
                    Err(error @ QueryRuntimeMountErrorV1::GenerationUnavailable) => {
                        if !awaiting_generation_logged {
                            log_query_authority_activation_outcome(&authority_project, Err(error));
                            awaiting_generation_logged = true;
                        }
                        tokio::select! {
                            () = authority_cancellation.cancelled() => return,
                            _ = route_poll.tick() => {},
                            publication = publications.recv() => match publication {
                                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                            }
                        }
                    }
                    outcome => {
                        log_query_authority_activation_outcome(&authority_project, outcome);
                        return;
                    }
                }
            }
        },
        label = "daemon.project.activate.query_authority"
    ));
}

/// Classify and emit the post-wait query-authority mount result.
///
/// `GenerationUnavailable` is the expected pre-seat gap: the waiter already
/// saw a generation id, but the mount still needs a current complete
/// generation. That is typed status, not a WARN. A real mount refusal stays
/// WARN so a broken profile or key cannot hide as warmup.
fn log_query_authority_activation_outcome(
    project: &Path,
    outcome: std::result::Result<(), QueryRuntimeMountErrorV1>,
) {
    match outcome {
        Ok(()) => {
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", project.display().to_string()),
                    ("phase", "code_index_query_authority".to_owned()),
                    ("outcome", "mounted".to_owned()),
                ],
            );
        }
        Err(QueryRuntimeMountErrorV1::GenerationUnavailable) => {
            tracing::info!(
                event = "project_open_phase",
                project = %project.display(),
                phase = "code_index_query_authority",
                outcome = "awaiting_generation",
                "query authority is unseated until a current generation exists"
            );
        }
        Err(error) => {
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", project.display().to_string()),
                    ("phase", "code_index_query_authority".to_owned()),
                    ("outcome", "degraded".to_owned()),
                    ("error", error.to_string()),
                ],
            );
        }
    }
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
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    activation: Arc<code_index_scheduler::CodeIndexActivationV1>,
) -> crate::mcp::server::CodeIndexReconcileSink {
    let sink: crate::mcp::server::CodeIndexReconcileSink = Arc::new(move |root: PathBuf| {
        let schedulers = schedulers.clone();
        let activation = Arc::clone(&activation);
        Box::pin(async move {
            if schedulers.notify_hook_overflow(&root).await {
                true
            } else {
                activation.notify_hook_overflow(&root).await
            }
        })
    });
    sink
}

/// MCP-facing ordinary-read freshness probe. Unlike the explicit reconcile
/// sink, this runs only the scheduler's bounded Git/stat ladder and creates an
/// overflow wake solely when that evidence proves a reconcile is required.
pub(super) fn code_index_freshness_probe_sink(
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
) -> crate::mcp::server::CodeIndexFreshnessProbeSink {
    Arc::new(move |root: PathBuf| {
        let schedulers = schedulers.clone();
        Box::pin(async move { schedulers.probe_freshness(&root).await })
    })
}

pub(super) fn diagnostics_change_generation_resolver(
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
) -> crate::mcp::server::DiagnosticsChangeGenerationResolver {
    Arc::new(move |root: PathBuf| {
        let schedulers = schedulers.clone();
        Box::pin(async move { schedulers.diagnostics_change_generation(&root).await })
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use tempfile::TempDir;

    fn git(root: &Path, arguments: &[&str]) {
        let status = Command::new(
            tracedecay_runtime_core::git::try_git_program()
                .expect("absolute git executable should resolve"),
        )
        .current_dir(root)
        .args(arguments)
        .status()
        .expect("run git");
        assert!(status.success(), "git {arguments:?}");
    }

    fn repository() -> TempDir {
        let root = TempDir::new().expect("repository root");
        git(root.path(), &["init", "-q"]);
        std::fs::write(root.path().join("lib.rs"), "pub fn seed() {}\n").expect("seed source");
        root
    }

    /// `tracedecay init` reports "code-index reconciliation requested" through
    /// this sink before any scheduler is mounted. The pre-mount request must be
    /// accepted and must start the demand-driven mount — otherwise init's
    /// message is a no-op and the first index never runs.
    #[tokio::test]
    async fn reconcile_request_before_mount_activates_indexing() {
        let repository = repository();
        let root = repository
            .path()
            .canonicalize()
            .expect("canonical repository root");
        let mount_attempts = Arc::new(AtomicUsize::new(0));
        let mount: code_index_scheduler::CodeIndexActivationMountV1 = {
            let mount_attempts = Arc::clone(&mount_attempts);
            Arc::new(move || {
                let mount_attempts = Arc::clone(&mount_attempts);
                Box::pin(async move {
                    mount_attempts.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            })
        };
        let overflow_batches = Arc::new(Mutex::new(Vec::new()));
        let hint_sink: code_index_scheduler::CodeIndexActivationHintSinkV1 = {
            let overflow_batches = Arc::clone(&overflow_batches);
            Arc::new(move |batch| {
                let overflow_batches = Arc::clone(&overflow_batches);
                Box::pin(async move {
                    overflow_batches.lock().expect("record batch").push(batch);
                    true
                })
            })
        };
        let activation = Arc::new(code_index_scheduler::CodeIndexActivationV1::new(
            &root,
            Arc::new(AtomicBool::new(true)),
            CancellationToken::new(),
            mount,
            hint_sink,
        ));
        let registry = code_index_scheduler::CodeIndexSchedulerRegistryV1::new(1);
        let sink = code_index_reconcile_sink(registry, Arc::clone(&activation));

        assert!(
            sink(root.clone()).await,
            "a pre-mount reconcile request must be accepted, not dropped"
        );

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let overflow_delivered = overflow_batches
                    .lock()
                    .expect("read batches")
                    .iter()
                    .any(|batch| batch.overflow);
                if mount_attempts.load(Ordering::SeqCst) == 1 && overflow_delivered {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pre-mount reconcile must mount the scheduler and flush the overflow request");
    }
}
