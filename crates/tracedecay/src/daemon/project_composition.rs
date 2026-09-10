//! Production project composition: the wiring that builds one project's MCP
//! server from its store runtime, schedulers, and authority ports.
//!
//! `production_project_server` is the single composition root shared by the
//! Unix broker, the portable broker, and the in-process test harness.

use super::*;
use tracedecay_code_index_runtime::code_index_scheduler;
use tracedecay_daemon_identity::profile_identity;
use tracedecay_daemon_service::{
    DaemonSemanticRuntimeRegistrationError, daemon_owned_project_source_access_at,
};
use tracedecay_runtime_core::logging::log_daemon_event;
use tracedecay_semantic_contracts::SemanticResourceCeilings;
use tracedecay_session_runtime::session_sync::DaemonSessionSyncConfig;
use tracedecay_session_runtime::session_temporal_refresh_scheduler::{
    ProfileSessionHistoricalIngestor, ProjectSessionHistoricalIngestor,
};

mod code_index_activation;
#[cfg(test)]
mod future_size_tests;
mod runtime;
mod session_database_admission;
use code_index_activation::{
    CodeIndexActivationMountInputs, code_index_activation_hint_sink, code_index_activation_mount,
    code_index_freshness_probe_sink, code_index_hook_sink, code_index_reconcile_sink,
};
pub(in crate::daemon) use runtime::ProductionProjectCompositionRuntime;
use runtime::bind_verified_project_graph_runtime;
use session_database_admission::{join_independent_session_opens, log_session_database_admission};

pub(super) struct ProductionProjectComposition {
    #[cfg(unix)]
    pub(super) key: ProjectServerKey,
    pub(super) canonical_project_path: PathBuf,
    pub(super) server: Arc<crate::mcp::McpServer>,
    #[cfg(unix)]
    pub(super) inserted: bool,
    #[cfg(any(test, feature = "test-transport"))]
    pub(super) semantic_auto_download_enabled: Option<bool>,
}

pub(super) fn project_server_response_lifecycle_has_in_flight(
    lifecycle: &crate::mcp::server::ProjectServerResponseLifecycle,
) -> bool {
    Arc::clone(lifecycle.response_gate())
        .try_write_owned()
        .is_err()
}

fn project_server_has_in_flight_response(server: &Arc<crate::mcp::McpServer>) -> bool {
    let lifecycle = server.project_server_response_lifecycle();
    Arc::strong_count(server) > 1 || project_server_response_lifecycle_has_in_flight(&lifecycle)
}

#[hotpath::measure(label = "daemon.project.compose.release_idle", future = true)]
async fn release_one_idle_project_server_before_open(
    store_administration: &StoreAdministration,
    invocation: &DaemonInvocationState,
    capacity_gate: Arc<ProjectOpenGate>,
    capacity_admission: tokio::sync::OwnedMutexGuard<()>,
) -> Result<tokio::sync::OwnedMutexGuard<()>> {
    let runtime_registry = store_administration.session_runtime_registry().await?;
    // The route cache and invocation schedulers have independent bounds. Retire
    // the whole idle owner before either fills: evicting only its MCP server
    // leaves the code-index worker holding its scheduler slot.
    let project_server_cache_saturated = store_administration
        .project_servers()
        .lock()
        .await
        .servers
        .len()
        >= MAX_CACHED_PROJECT_SERVERS;
    let graph_admission_available = runtime_registry.has_project_graph_admission_capacity()?;
    if graph_admission_available && !project_server_cache_saturated {
        return Ok(capacity_admission);
    }
    if let Some(error) = store_administration
        .completed_capacity_retirement_failure()
        .await
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "a prior project server retirement failed before capacity reuse: {error}"
            ),
        });
    }
    let profile_identity = store_administration.profile_identity()?.clone();
    let mut retirement_admission = store_administration
        .acquire_project_server_retirement_admission()
        .await;
    let victim = {
        let mut servers = store_administration.project_servers().lock().await;
        servers.retire_lru_ready_under_graph_pressure(project_server_has_in_flight_response)
    };
    let victim = match victim {
        Ok(victim) => victim,
        // Nothing is idle enough to retire. That is terminal only when graph
        // admission itself is exhausted; a merely saturated project-server
        // cache still has the bounded route-cache eviction (and the
        // already-cached-key fast path) behind it, so leave that decision to
        // the bind below rather than refusing an open this path was not
        // entered to refuse.
        Err(()) if graph_admission_available => return Ok(capacity_admission),
        Err(()) => return Err(project_server_capacity_error()),
    };
    let Some((retired_owner, retired_servers)) = victim else {
        return Ok(capacity_admission);
    };
    let prior_owner_retirements = retirement_admission.prior_completions_for_owner(&retired_owner);
    for (_, server) in &retired_servers {
        server.revoke_project_server_responses();
    }
    let project_roots = retired_servers
        .iter()
        .map(|(key, _)| key.project_root.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut hook_data_roots = std::collections::BTreeSet::new();
    for (_, server) in &retired_servers {
        let graph = server.cg().await;
        hook_data_roots.insert(graph.hook_store_layout().data_root.clone());
    }
    let retired_servers = retired_servers
        .into_iter()
        .map(|(_, server)| server)
        .collect::<Vec<_>>();
    let retired_server_count = retired_servers.len();
    let retirement_administration = store_administration.clone();
    let retirement_invocation = invocation.clone();
    let completion =
        retirement_admission.spawn_and_track_fallible(retired_owner.clone(), async move {
            let _capacity_admission = capacity_admission;
            retirement_administration
                .session_temporal_refresh_schedulers()
                .retire_project(&retired_owner)
                .await;
            super::project_server_lifecycle::retire_project_servers(retired_servers, None).await;
            for data_root in hook_data_roots {
                super::hook_v2_replay_consumer::shutdown_hook_v2_replay_consumer(&data_root).await;
            }
            for prior in prior_owner_retirements {
                prior.wait().await?;
            }
            let project_id =
                retired_owner
                    .project_id
                    .clone()
                    .ok_or_else(|| TraceDecayError::Config {
                        message:
                            "retired project server omitted its authoritative project identity"
                                .to_owned(),
                    })?;
            let project_id = tracedecay_domain::ProjectId::new(project_id).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("retired project server identity is invalid: {error}"),
                }
            })?;
            super::branch_admin::retire_registered_context_scout_owner(
                &project_id,
                &retired_owner.graph_db_path,
            );
            let runtime_quiescence = retirement_invocation
                .quiesce_project_runtime_owners(
                    profile_identity.profile_id(),
                    &project_id,
                    &project_roots,
                )
                .await?;
            let project_sessions_path = retired_owner
                .store_root
                .join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);
            retirement_administration
                .git_index_transaction_services()
                .retire_project_database(&project_id, &project_sessions_path)
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "could not retire project Git transaction actors before capacity reuse: {error}"
                    ),
                })?;
            retirement_administration
                .native_integration_services()
                .retire_project_database(&project_id, &project_sessions_path)
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "could not retire project native integration actors before capacity reuse: {error}"
                    ),
                })?;
            retirement_administration
                .session_sync_service()
                .retire_project(profile_identity.profile_id(), &project_id)
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "could not retire project session sync before capacity reuse: {error}"
                    ),
                })?;
            let telemetry_sampling = retirement_administration.store_telemetry_sampling();
            telemetry_sampling.release_retained_handle(&project_sessions_path);
            telemetry_sampling.release_retained_handle(&retired_owner.graph_db_path);
            runtime_registry
                .retire_project_session_relation_graph(&project_id)
                .await?;
            runtime_registry
                .retire_project_memory_graph(&project_id)
                .await?;
            runtime_registry
                .drop_project_runtime_caches(&project_id)
                .await;
            drop(runtime_quiescence);
            Ok(())
        });
    hotpath::gauge!("project_servers").inc(-(retired_server_count as f64));
    drop(retirement_admission);
    completion.wait().await?;
    let capacity_admission = Arc::clone(&capacity_gate).lock_owned().await;
    if !store_administration
        .session_runtime_registry()
        .await?
        .has_project_graph_admission_capacity()?
    {
        return Err(project_server_capacity_error());
    }
    Ok(capacity_admission)
}

#[cfg(test)]
pub(super) fn daemon_transcript_source_home(profile_root: &Path) -> Option<PathBuf> {
    profile_root.parent().map(Path::to_path_buf)
}

#[cfg(not(test))]
pub(super) fn daemon_transcript_source_home(_profile_root: &Path) -> Option<PathBuf> {
    tracedecay_sessions::runtime::home_dir()
}

/// Borrowed handles every project-open phase reads. A phase future captures
/// one pointer to this record instead of the handles themselves.
struct ProjectOpenInputs<'a> {
    store_administration: &'a StoreAdministration,
    project_open_gates: &'a tokio::sync::Mutex<ProjectOpenGates>,
    invocation: &'a DaemonInvocationState,
    http_application_registry: &'a http_application::DaemonHttpApplicationRegistry,
    canonical_project_path: &'a Path,
    handshake: &'a DaemonHandshake,
    runtime: &'a ProductionProjectCompositionRuntime,
    cancellation: &'a CancellationToken,
    /// Start of this open attempt. `project_open_phase` events report elapsed
    /// time from here unless they name a narrower phase start.
    started: Instant,
    #[cfg(test)]
    project_open_attempts: Option<&'a Arc<AtomicUsize>>,
}

/// Compose one project's MCP server: admit the route, open its graph, publish
/// the graph/search core, then upgrade that core in place to the full
/// session-capable server.
///
/// Each phase is its own future that owns its temporaries, so this state
/// machine carries only the compact phase results across awaits. The phases
/// are boxed at these call sites: under `--features hotpath` every measured
/// async fn embeds its body by value, and boxing here keeps the measured
/// wrapper (and every instrumented caller) a few words wide instead of
/// inlining the whole open.
#[hotpath::measure(label = "daemon.project.compose.server", future = true)]
#[allow(
    clippy::too_many_arguments,
    reason = "This composition entry binds route admission, store lifetime, invocation and HTTP owners before publishing a server."
)]
pub(super) async fn production_project_server(
    store_administration: &StoreAdministration,
    project_open_gates: &tokio::sync::Mutex<ProjectOpenGates>,
    invocation: &DaemonInvocationState,
    http_application_registry: &http_application::DaemonHttpApplicationRegistry,
    canonical_project_path: &Path,
    handshake: &DaemonHandshake,
    runtime: ProductionProjectCompositionRuntime,
    cancellation: &CancellationToken,
    #[cfg(test)] project_open_attempts: Option<&Arc<AtomicUsize>>,
) -> Result<ProductionProjectComposition> {
    let inputs = ProjectOpenInputs {
        store_administration,
        project_open_gates,
        invocation,
        http_application_registry,
        canonical_project_path,
        handshake,
        runtime: &runtime,
        cancellation,
        started: Instant::now(),
        #[cfg(test)]
        project_open_attempts,
    };
    let admitted = match Box::pin(inputs.admit_route()).await? {
        RouteAdmission::Cached(composition) => return Ok(composition),
        RouteAdmission::Admitted(admitted) => admitted,
    };
    let opened = match Box::pin(inputs.open_graph(&admitted.route)).await? {
        GraphOpen::Cached(composition) => return Ok(composition),
        GraphOpen::Opened(opened) => opened,
    };
    let (core_candidate, core) = Box::pin(inputs.compose_core_server(&opened)).await?;
    let CoreRouteBinding {
        mut resolved,
        inserted,
    } = Box::pin(inputs.bind_core_route(
        admitted.route,
        &opened.key,
        core_candidate,
        &core.route_registered,
    ))
    .await?;
    if inserted {
        let activation = Box::pin(inputs.activate_core_route(&opened, &core, &resolved)).await?;
        let upgrade = match Box::pin(inputs.construct_full_server(&opened, &core, &resolved)).await
        {
            Ok(PublishedFullServer { server, session_db }) => {
                match Box::pin(inputs.finish_full_server(
                    &opened,
                    &core,
                    &activation,
                    &resolved,
                    &server,
                    session_db,
                ))
                .await
                {
                    Ok(()) => Ok(server),
                    Err(error) => Err((error, Some(server))),
                }
            }
            Err(error) => Err((error, None)),
        };
        match upgrade {
            Ok(full_server) => resolved = full_server,
            Err((error, published_full_server)) => {
                Box::pin(inputs.settle_failed_full_upgrade(
                    &opened,
                    &core,
                    &activation,
                    &resolved,
                    published_full_server,
                    error,
                ))
                .await?;
            }
        }
    } else {
        drop(admitted.foreground_project_open);
        core.route_registered.store(false, Ordering::Release);
    }
    Ok(ProductionProjectComposition {
        #[cfg(unix)]
        key: opened.key,
        canonical_project_path: canonical_project_path.to_path_buf(),
        server: resolved,
        #[cfg(unix)]
        inserted,
        #[cfg(any(test, feature = "test-transport"))]
        semantic_auto_download_enabled: Some(opened.semantic.auto_download_enabled),
    })
}

/// The primary checkout this route must be served from, when a linked worktree
/// is checked out on the *same* branch as its primary.
///
/// `ProjectServerKey` is root-bound so distinct linked worktrees keep exact
/// root-bound servers over one shared `StoreOwnerKey` — a worktree on another
/// branch (or detached) serves a different generation and must not be answered
/// from the primary's graph. A worktree on the same branch serves the same
/// branch content from the same store owner and the same graph database, so a
/// second physical server for it is a duplicate owner over one authority, not
/// an exact route: the route belongs in the alias map beside the primary's.
///
/// Returns `None` for a primary checkout, a non-worktree path, a detached or
/// unborn checkout, and any worktree whose branch differs from the primary's.
fn shared_primary_checkout_root(project_path: &std::path::Path) -> Option<PathBuf> {
    let primary = tracedecay_runtime_core::worktree::repository_identity_root(project_path)?;
    let branch = tracedecay_runtime_core::branch::current_branch(project_path)?;
    if tracedecay_runtime_core::branch::current_branch(&primary)? != branch {
        return None;
    }
    tracedecay_daemon_identity::authority::canonical_identity_path(&primary).ok()
}

/// One `project_open_phase` event. `detail` precedes the elapsed time so the
/// field order matches every existing consumer of these lines.
fn log_project_open_phase(
    project: &Path,
    phase: &str,
    detail: Option<(&str, String)>,
    since: Instant,
) {
    let mut fields = vec![
        ("project", project.display().to_string()),
        ("phase", phase.to_owned()),
    ];
    if let Some(detail) = detail {
        fields.push(detail);
    }
    fields.push(("elapsed_ms", since.elapsed().as_millis().to_string()));
    log_daemon_event("project_open_phase", &fields);
}

/// Outcome of route admission: a published server already answers this route,
/// or the caller now owns the open behind the route's single-flight gate.
enum RouteAdmission {
    Cached(ProductionProjectComposition),
    Admitted(AdmittedProjectRoute),
}

/// Guards an admitted opener holds until its route publishes or fails.
///
/// Fields drop in declaration order: the graph admission slot is released
/// first, then the foreground-open marker, then the single-flight gate that
/// lets the next opener of this route observe the published server.
struct AdmittedProjectRoute {
    _capacity_admission: tokio::sync::OwnedMutexGuard<()>,
    foreground_project_open: tracedecay_store_runtime::ForegroundProjectOpenAdmission,
    _singleflight: tokio::sync::OwnedMutexGuard<()>,
    route: ProjectRouteKey,
}

/// Outcome of the graph open: an owner for this key was already published (the
/// route is now bound to it), or this caller opened the graph and owns the
/// composition that follows.
enum GraphOpen {
    Cached(ProductionProjectComposition),
    /// Boxed so the root state machine holds a pointer, not the pinned
    /// configuration snapshot, across every later phase.
    Opened(Box<OpenedProjectGraph>),
}

/// The opened graph and the route-wide choices resolved from its configuration.
struct OpenedProjectGraph {
    cg: Arc<crate::tracedecay::TraceDecay>,
    key: ProjectServerKey,
    runtime_configuration: tracedecay_configuration::config::PinnedRuntimeConfiguration,
    semantic: SemanticProjectRuntime,
    project_database_is_read_only: bool,
    code_index_store_root: PathBuf,
}

/// Ports both of this route's MCP servers publish. Built once before the core
/// server; the full server that replaces the core publishes the same ports.
struct ProjectRoutePorts {
    code_index: ProjectCodeIndexAuthorities,
    dashboard_code_index_freshness_reader:
        tracedecay_dashboard_api::code_index_freshness_api::CodeIndexFreshnessReader,
    dashboard_explorer_semantic_reader: tracedecay_dashboard_api::ExplorerSemanticReader,
    dashboard_feedback_status_reader: tracedecay_dashboard_api::feedback_api::FeedbackStatusReader,
    dashboard_pr_autotrack_reader: tracedecay_dashboard_api::PrAutoTrackManagedSummaryReader,
    diagnostic_broker: Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    code_index_hook_sink: crate::mcp::server::CodeIndexHookSink,
    code_index_reconcile_sink: crate::mcp::server::CodeIndexReconcileSink,
    code_index_freshness_probe_sink: crate::mcp::server::CodeIndexFreshnessProbeSink,
    application_invocation_executor: Arc<dyn tracedecay_daemon_protocol::DaemonInvocationExecutor>,
    retained_server_resolver: crate::mcp::server::RetainedProjectServerResolver,
    automation_scheduler_reconciler:
        Option<tracedecay_dashboard_api::AutomationSchedulerReconciler>,
}

/// Route-owned state shared by the core server, the full server that replaces
/// it, and the retirement funnel behind both.
struct ComposedCoreServer {
    /// Authoritative project identity from the route key; every owner
    /// registration below binds to it.
    project_id: String,
    /// The key the route currently serves. A rekey moves it, and every
    /// publication below compares against it first.
    current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
    route_registered: Arc<AtomicBool>,
    route_cancellation: CancellationToken,
    database_owner_reconciler: crate::mcp::DatabaseOwnerReconciler,
    profile_identity: profile_identity::LocalProfileIdentityAuthorityV1,
    registered_profile_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    accounting_db: Option<tracedecay_global_db::RegisteredGlobalDbLeaseV1>,
    graph_runtime: Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>,
    transcript_source_home: Option<PathBuf>,
    code_index_activation: Arc<code_index_scheduler::CodeIndexActivationV1>,
    semantic_runtime_readiness: tokio::sync::watch::Receiver<bool>,
    ports: ProjectRoutePorts,
}

impl ComposedCoreServer {
    /// Publish this route's ports on a server construction context. The core
    /// and the full server publish the same set.
    fn publish_route_ports(
        &self,
        context: crate::mcp::server::McpServerConstructionContext,
        cg: &Arc<crate::tracedecay::TraceDecay>,
        invocation: &DaemonInvocationState,
    ) -> crate::mcp::server::McpServerConstructionContext {
        let ports = &self.ports;
        let code_index = &ports.code_index;
        let mut context = context
            .with_dashboard_code_index_freshness_reader(Arc::clone(
                &ports.dashboard_code_index_freshness_reader,
            ))
            .with_dashboard_explorer_semantic_reader(Arc::clone(
                &ports.dashboard_explorer_semantic_reader,
            ))
            .with_dashboard_feedback_status_reader(Arc::clone(
                &ports.dashboard_feedback_status_reader,
            ))
            .with_dashboard_pr_autotrack_reader(Arc::clone(&ports.dashboard_pr_autotrack_reader))
            .with_diagnostics_lsp(Arc::clone(&ports.diagnostic_broker))
            .with_code_index_hook_sink(Arc::clone(&ports.code_index_hook_sink))
            .with_code_index_reconcile_sink(Arc::clone(&ports.code_index_reconcile_sink))
            .with_code_index_freshness_probe_sink(Arc::clone(
                &ports.code_index_freshness_probe_sink,
            ))
            .with_code_index_publication_identity(Arc::clone(&code_index.publication_identity))
            .with_code_index_search_executor(Arc::clone(&code_index.search_executor))
            .with_code_index_branch_diff_executor(Arc::clone(&code_index.branch_diff_executor))
            .with_code_graph_projection_read_port(Arc::clone(
                &code_index.graph_projection_read_port,
            ))
            .with_code_index_ignored_dependency_admission(Arc::clone(
                &code_index.ignored_dependency_admission,
            ))
            .with_code_graph_read_admission_port(Arc::clone(&code_index.graph_read_admission_port))
            .with_verified_graph_query_port(
                tracedecay_graph_query::admitted_verified_graph_query_port_with_source(
                    Arc::clone(&code_index.graph_read_admission_port),
                    Arc::clone(&code_index.graph_projection_read_port),
                    cg.source_read_context(),
                ),
            )
            .with_code_index_search_authority(code_index.search_authority.clone())
            .with_admitted_project_scope(code_index.scope.clone())
            .with_project_server_live(Arc::clone(&self.route_registered))
            .with_application_invocation_executor(Arc::clone(
                &ports.application_invocation_executor,
            ))
            .with_daemon_invocation_service(invocation.service.clone())
            .with_retained_project_server_resolver(Arc::clone(&ports.retained_server_resolver));
        if let Some(reconciler) = ports.automation_scheduler_reconciler.as_ref() {
            context = context.with_automation_scheduler_reconciler(Arc::clone(reconciler));
        }
        context
    }
}

/// The published core and whether this open inserted it. `inserted == false`
/// means the route bound to an owner published concurrently for the same key.
struct CoreRouteBinding {
    resolved: Arc<crate::mcp::McpServer>,
    inserted: bool,
}

/// Owners the published core carries into the full upgrade and its failure
/// funnel.
struct CoreRouteActivation {
    publication_attempt: tracedecay_daemon_service::ProjectRuntimePublicationAttemptV1,
    /// The core's preview-only source-edit lane; `None` for a read-only database.
    core_source_edit_mutation:
        Option<Arc<tracedecay_daemon_service::project_owner_registration::SourceEditMutationGate>>,
}

/// Both session databases this route serves, admitted together.
struct AdmittedSessionDatabases {
    session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    user_session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
}

/// The full server after it replaced the core in the owner registry, with the
/// project session database its dependent owners still have to mount.
struct PublishedFullServer {
    server: Arc<crate::mcp::McpServer>,
    session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
}

impl ProjectOpenInputs<'_> {
    fn log_phase(&self, phase: &str, detail: Option<(&str, String)>, since: Instant) {
        log_project_open_phase(self.canonical_project_path, phase, detail, since);
    }

    /// Route admission: registry enrollment, the published-server cache, the
    /// route's single-flight gate, the foreground-open marker, and one graph
    /// admission slot (releasing an idle server when the daemon is at capacity).
    #[hotpath::measure(label = "daemon.project.compose.admit_route", future = true)]
    async fn admit_route(&self) -> Result<RouteAdmission> {
        project_open_cancellation_checkpoint(self.cancellation)?;
        self.invocation
            .configuration_runtime_registrar()
            .ensure_worker_plan()?;
        ensure_registered_project_route(
            self.store_administration,
            self.canonical_project_path,
            self.handshake.allow_init,
        )
        .await?;
        let route = ProjectRouteKey::from_handshake(self.canonical_project_path, self.handshake)?;
        if let Some((cached_key, cached_server)) =
            cached_route_server(self.store_administration, &route).await
        {
            return Ok(RouteAdmission::Cached(cached_project_composition(
                self.canonical_project_path,
                cached_key,
                cached_server,
                None,
            )));
        }

        let gate = project_open_gate(self.project_open_gates, &route).await;
        let singleflight = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => return Err(project_open_cancellation_error()),
            singleflight = gate.lock_owned() => singleflight,
        };
        // Order-sensitive: the same lookup runs again behind the single-flight
        // gate so a concurrent open that published while this caller waited is
        // reused.
        if let Some((cached_key, cached_server)) =
            cached_route_server(self.store_administration, &route).await
        {
            return Ok(RouteAdmission::Cached(cached_project_composition(
                self.canonical_project_path,
                cached_key,
                cached_server,
                None,
            )));
        }
        let foreground_project_open = self
            .store_administration
            .session_runtime_registry()
            .await?
            .begin_foreground_project_open()?;
        let capacity_gate = project_open_capacity_gate(self.project_open_gates).await;
        let capacity_admission = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => return Err(project_open_cancellation_error()),
            admission = Arc::clone(&capacity_gate).lock_owned() => admission,
        };
        let capacity_admission = release_one_idle_project_server_before_open(
            self.store_administration,
            self.invocation,
            capacity_gate,
            capacity_admission,
        )
        .await?;
        Ok(RouteAdmission::Admitted(AdmittedProjectRoute {
            _capacity_admission: capacity_admission,
            foreground_project_open,
            _singleflight: singleflight,
            route,
        }))
    }

    /// Open the project graph behind the admitted route, re-check the deletion
    /// fence and the owner registry, and resolve the route-wide semantic and
    /// configuration choices every later phase reads.
    #[hotpath::measure(label = "daemon.project.compose.open_graph", future = true)]
    async fn open_graph(&self, route: &ProjectRouteKey) -> Result<GraphOpen> {
        #[cfg(test)]
        if let Some(attempts) = self.project_open_attempts {
            attempts.fetch_add(1, Ordering::Relaxed);
        }
        let cg = Arc::new(
            open_project_for_handshake(
                self.canonical_project_path,
                self.handshake,
                self.store_administration,
            )
            .await?,
        );
        let mut key = ProjectServerKey::from_open_project(&cg, self.handshake)?;
        if let Some(shared_root) = shared_primary_checkout_root(self.canonical_project_path) {
            key.project_root = shared_root;
        }
        self.log_phase("graph_admitted", None, self.started);
        project_open_cancellation_checkpoint(self.cancellation)?;
        // A deletion may arrive while the graph opens. Recheck the durable
        // replay fence before this in-flight open can republish its registry
        // authority.
        ensure_registered_project_route(
            self.store_administration,
            self.canonical_project_path,
            false,
        )
        .await?;
        ensure_context_scout_owner_before_advertising(&cg)?;
        cg.register_project_store_in_global_registry().await?;
        let code_index_store_root = cg.store_layout().data_root.join("code-index-v1");
        let runtime_configuration = cg
            .configuration_runtime()
            .client()
            .current()
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("authoritative runtime configuration unavailable: {error}"),
            })?;
        let semantic_project_id =
            tracedecay_domain::ProjectId::new(key.owner.project_id.clone().ok_or_else(|| {
                TraceDecayError::Config {
                    message: "semantic selection requires authoritative project identity"
                        .to_owned(),
                }
            })?)
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?;
        let semantic = semantic_project_runtime(
            &runtime_configuration,
            self.runtime,
            self.store_administration
                .session_runtime_registry()
                .await?
                .project_semantic_lifecycle(&semantic_project_id)
                .await?,
        )?;
        let project_database_is_read_only = !cg.db().is_writable();
        let existing = {
            let mut servers = self.store_administration.project_servers().lock().await;
            let existing = servers.get_ready(&key).cloned();
            if existing.is_some() {
                servers.bind_route(route.clone(), key.clone());
            }
            existing
        };
        if let Some(existing) = existing {
            return Ok(GraphOpen::Cached(cached_project_composition(
                self.canonical_project_path,
                key,
                existing,
                Some(semantic.auto_download_enabled),
            )));
        }
        Ok(GraphOpen::Opened(Box::new(OpenedProjectGraph {
            cg,
            key,
            runtime_configuration,
            semantic,
            project_database_is_read_only,
            code_index_store_root,
        })))
    }

    /// Build every route-owned port and construct the core (graph, search,
    /// diagnostics) server candidate. Nothing is published yet.
    #[hotpath::measure(label = "daemon.project.compose.core", future = true)]
    async fn compose_core_server(
        &self,
        opened: &OpenedProjectGraph,
    ) -> Result<(Arc<crate::mcp::McpServer>, ComposedCoreServer)> {
        let OpenedProjectGraph {
            cg,
            key,
            runtime_configuration,
            semantic,
            project_database_is_read_only,
            code_index_store_root,
        } = opened;
        let current_key = Arc::new(tokio::sync::Mutex::new(key.clone()));
        let current_project_path = Arc::new(tokio::sync::Mutex::new(
            self.canonical_project_path.to_path_buf(),
        ));
        let route_registered = Arc::new(AtomicBool::new(true));
        // Route-owned cancellation lifetime. Terminal route revocation (a
        // failed owner rekey) must end this route's activation and query
        // waiters without touching the caller's project-open token, so they
        // hang off a child: cancelling a child never propagates to its parent.
        let route_cancellation = self.cancellation.child_token();
        let database_owner_reconciler = self.runtime.database_owner_reconciler(
            self.store_administration,
            Arc::clone(&current_key),
            Arc::clone(&current_project_path),
            Arc::clone(&route_registered),
            route_cancellation.clone(),
            self.handshake.clone(),
        );
        let automation_scheduler_reconciler = self.runtime.automation_scheduler_reconciler(
            Arc::clone(&current_key),
            Arc::clone(&current_project_path),
            self.handshake.clone(),
        );
        let project_id = key
            .owner
            .project_id
            .clone()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project session runtime requires an authoritative project identity"
                    .to_owned(),
            })?;
        let registered_profile_db = self
            .store_administration
            .registered_profile_database()
            .await?;
        let graph_runtime = self
            .store_administration
            .registered_runtime_registry()
            .await?;
        let profile_identity = self.store_administration.profile_identity()?.clone();
        let accounting_db = tracedecay_global_db::global_accounting_enabled()
            .then(|| registered_profile_db.clone());
        let code_index = project_code_index_authorities(
            self.invocation,
            cg,
            self.canonical_project_path,
            &project_id,
            &profile_identity,
            &route_registered,
            *project_database_is_read_only,
        )?;
        let (semantic_runtime_ready, semantic_runtime_readiness) =
            tokio::sync::watch::channel(false);
        let code_index_mount = code_index_activation_mount(CodeIndexActivationMountInputs {
            invocation: self.invocation.clone(),
            project_id: code_index.project_id.clone(),
            project_root: self.canonical_project_path.to_path_buf(),
            store_root: code_index_store_root.clone(),
            semantic_runtime: semantic.handle.clone(),
            semantic_lifecycle: semantic.lifecycle.clone(),
            semantic_resources: semantic.resources,
            semantic_document_composition: semantic.document_composition,
            native_graph_activation: runtime_configuration.config().native_graph_activation,
            scope: code_index.scope.clone(),
            route_registered: Arc::clone(&route_registered),
            cancellation: route_cancellation.clone(),
            graph_runtime: Arc::clone(&graph_runtime),
            graph_publication_database: Arc::new(cg.db().clone()),
            semantic_runtime_ready,
            profile_id: cg.store_runtime_registry().profile_id().clone(),
        });
        let code_index_hint_sink = code_index_activation_hint_sink(
            self.invocation.code_index_schedulers.clone(),
            self.canonical_project_path.to_path_buf(),
        );
        let code_index_automatic_admission =
            if tracedecay_runtime_core::worktree::is_linked_worktree(self.canonical_project_path)
                && !cg.get_config().sync.watch_linked_worktrees
            {
                code_index_scheduler::CodeIndexAutomaticAdmissionV1::LinkedWorktreeDisabled
            } else {
                code_index_scheduler::CodeIndexAutomaticAdmissionV1::Admitted
            };
        let code_index_activation = Arc::new(
            code_index_scheduler::CodeIndexActivationV1::new_with_admission(
                self.canonical_project_path,
                Arc::clone(&route_registered),
                route_cancellation.clone(),
                code_index_automatic_admission,
                code_index_mount,
                code_index_hint_sink,
            ),
        );
        let code_index_hook_sink = code_index_hook_sink(Arc::clone(&code_index_activation));
        let code_index_reconcile_sink = code_index_reconcile_sink(
            self.invocation.code_index_schedulers.clone(),
            Arc::clone(&code_index_activation),
        );
        let code_index_freshness_probe_sink = code_index_freshness_probe_sink(
            self.invocation.code_index_schedulers.clone(),
            Arc::clone(&code_index_activation),
        );
        // The daemon mounts the same broker the MCP server and the directly
        // served dashboard open: persisted analyzer settings (with a recorded
        // degradation for an unreadable file) plus the home-level OpenCode
        // analyzer-ownership registration adopted on top of the project-level
        // one.
        let diagnostic_broker =
            tracedecay_application::dashboard_diagnostics::open_diagnostic_broker(
                self.canonical_project_path.to_path_buf(),
                &cg.store_layout().dashboard_root,
            )
            .await;
        let application_invocation_executor: Arc<
            dyn tracedecay_daemon_protocol::DaemonInvocationExecutor,
        > = Arc::new(InProcessDaemonInvocationExecutor::new(
            self.invocation.clone(),
            self.store_administration.clone(),
            self.canonical_project_path.to_path_buf(),
            code_index.scope.clone(),
        ));
        let transcript_source_home = daemon_transcript_source_home(profile_identity.profile_root());
        let core = ComposedCoreServer {
            project_id,
            current_key,
            route_registered,
            route_cancellation,
            database_owner_reconciler,
            profile_identity,
            registered_profile_db,
            accounting_db,
            graph_runtime,
            transcript_source_home,
            code_index_activation,
            semantic_runtime_readiness,
            ports: ProjectRoutePorts {
                code_index,
                dashboard_code_index_freshness_reader: project_dashboard_freshness_reader(
                    self.invocation.code_index_schedulers.clone(),
                ),
                dashboard_explorer_semantic_reader: project_dashboard_explorer_semantic_reader(
                    cg.configuration_runtime().client(),
                ),
                dashboard_feedback_status_reader:
                    tracedecay_dashboard_api::feedback_api::feedback_status_reader(
                        self.invocation.feedback_runtime_registrar(),
                    ),
                dashboard_pr_autotrack_reader: project_dashboard_pr_autotrack_reader(),
                diagnostic_broker,
                code_index_hook_sink,
                code_index_reconcile_sink,
                code_index_freshness_probe_sink,
                application_invocation_executor,
                retained_server_resolver: retained_project_server_resolver(
                    self.store_administration.clone(),
                ),
                automation_scheduler_reconciler,
            },
        };
        let core_context = core.publish_route_ports(
            crate::mcp::server::McpServerConstructionContext::daemon_owned_core(
                Arc::clone(cg),
                self.handshake.scope_prefix.clone(),
                crate::mcp::server::McpServerDaemonCoreAuthority {
                    profile_identity: core.profile_identity.clone(),
                    accounting: core.accounting_db.clone(),
                    registry: core.registered_profile_db.clone(),
                    database_owner_reconciler: Arc::clone(&core.database_owner_reconciler),
                    project_routes: self.store_administration.project_routes(),
                    writers: crate::mcp::server::McpServerWriters::daemon_owned(
                        coordinated_dashboard_automation_writer(self.store_administration.clone()),
                        coordinated_background_refresh_writer(self.store_administration.clone()),
                    ),
                },
            ),
            cg,
            self.invocation,
        );
        project_open_cancellation_checkpoint(self.cancellation)?;
        let mcp_construction_started = Instant::now();
        let core_candidate = crate::mcp::McpServer::new_with_context(core_context).await;
        core_candidate
            .install_generation_census_reader(Arc::clone(
                &core.ports.code_index.generation_census_reader,
            ))
            .map_err(|_| TraceDecayError::Config {
                message: "core MCP generation census authority was already installed".to_owned(),
            })?;
        self.log_phase("mcp_core_constructed", None, mcp_construction_started);
        Ok((core_candidate, core))
    }

    /// Bind the route to the core candidate inside the bounded owner registry,
    /// retiring whatever the bounded eviction displaced.
    #[hotpath::measure(label = "daemon.project.compose.bind_route", future = true)]
    async fn bind_core_route(
        &self,
        route: ProjectRouteKey,
        key: &ProjectServerKey,
        core_candidate: Arc<crate::mcp::McpServer>,
        route_registered: &AtomicBool,
    ) -> Result<CoreRouteBinding> {
        if self.cancellation.is_cancelled() {
            core_candidate.shutdown().await;
            return Err(project_open_cancellation_error());
        }
        // Retirement admission precedes the owner registry. Once bounded
        // eviction removes an idle server, its exact Arc crosses only the
        // synchronous `spawn_and_track` handoff below; no caller cancellation
        // can drop it between registry removal and canonical shutdown
        // ownership.
        let mut retirement_admission = self
            .store_administration
            .acquire_project_server_retirement_admission()
            .await;
        let resolution = {
            let mut servers = self.store_administration.project_servers().lock().await;
            servers.bind_or_insert_route_bounded(
                route,
                key.clone(),
                core_candidate,
                MAX_CACHED_PROJECT_SERVERS,
                project_server_has_in_flight_response,
            )
        };
        let Some((resolved, inserted, retired)) = resolution else {
            route_registered.store(false, Ordering::Release);
            return Err(project_server_capacity_error());
        };
        for (retired_key, retired_server) in retired {
            let owner = retired_key.owner;
            self.store_administration
                .session_temporal_refresh_schedulers()
                .retire_project(&owner)
                .await;
            retirement_admission.spawn_and_track(
                owner,
                super::project_server_lifecycle::retire_project_servers(vec![retired_server], None),
            );
            hotpath::gauge!("project_servers").inc(-1.0);
        }
        // The owner registry guard was dropped before the synchronous
        // retirement handoff. Release admission before the remaining
        // project-open awaits.
        drop(retirement_admission);
        if inserted {
            hotpath::gauge!("project_servers").inc(1.0);
        }
        Ok(CoreRouteBinding { resolved, inserted })
    }

    /// Publish the inserted core: register the code-index activation, start
    /// the semantic owner publication, install the preview-only source-edit
    /// lane, mark the route ready, and kick off the background default-model
    /// selection.
    #[hotpath::measure(label = "daemon.project.compose.publish_core", future = true)]
    async fn activate_core_route(
        &self,
        opened: &OpenedProjectGraph,
        core: &ComposedCoreServer,
        resolved: &Arc<crate::mcp::McpServer>,
    ) -> Result<CoreRouteActivation> {
        if self.cancellation.is_cancelled() {
            return Err(project_open_cancellation_error());
        }
        if !self
            .invocation
            .code_index_schedulers
            .register_activation(&core.ports.code_index.scope, &core.code_index_activation)
        {
            core.route_registered.store(false, Ordering::Release);
            return Err(TraceDecayError::Config {
                message: "code-index activation scope does not match the project route".to_owned(),
            });
        }
        let publication_attempt = project_open_owners::spawn_semantic_owner_registration(
            self.invocation.clone(),
            self.canonical_project_path.to_path_buf(),
            Arc::clone(opened.cg.configuration_runtime()),
            core.ports.code_index.scope.clone(),
            core.semantic_runtime_readiness.clone(),
            Arc::clone(&core.route_registered),
            core.route_cancellation.clone(),
        )
        .await?;
        // The core's own lane never opens: only the full server reaches a Git
        // transaction authority. Its gate is kept so a rolled-back publication
        // can report a terminal failure instead of warming forever.
        let core_source_edit_mutation = if opened.project_database_is_read_only {
            None
        } else {
            Some(
                project_open_owners::install_project_open_source_edit_preview_owner(
                    resolved.as_ref(),
                    Arc::clone(&opened.cg),
                    Arc::clone(&core.ports.code_index.graph_projection_read_port),
                    self.canonical_project_path,
                    &core.project_id,
                )
                .await?,
            )
        };
        // Publish the graph/search/diagnostic core before session admission.
        // Source-edit previews are available, while mutations fail closed as
        // warming until the full server has its transaction authority.
        {
            let mut servers = self.store_administration.project_servers().lock().await;
            if !servers.mark_ready(&opened.key) {
                return Err(TraceDecayError::Config {
                    message: "project server disappeared before core publication completed"
                        .to_owned(),
                });
            }
        }
        self.log_phase("core_published", None, self.started);
        if !retain_project_semantic_startup(
            core.graph_runtime.as_ref(),
            self.canonical_project_path.to_path_buf(),
            self.invocation.code_index_schedulers.clone(),
            Arc::clone(opened.cg.configuration_runtime()),
            opened.semantic.lifecycle.clone(),
            self.runtime.semantic_auto_download(),
        ) {
            if let Some(mutation) = &core_source_edit_mutation {
                mutation.mark_failed();
            }
            retire_failed_project_open_owner(
                self.store_administration,
                &opened.key,
                resolved,
                false,
                &core.route_registered,
            )
            .await;
            return Err(TraceDecayError::Config {
                message: "semantic startup task owner is not accepting work".to_owned(),
            });
        }
        Ok(CoreRouteActivation {
            publication_attempt,
            core_source_edit_mutation,
        })
    }

    /// Admit the project and profile session databases together, settle the
    /// project session graph for serving, and bind the graph runtime.
    #[hotpath::measure(label = "daemon.project.compose.admit_sessions", future = true)]
    async fn admit_session_databases(
        &self,
        cg: &Arc<crate::tracedecay::TraceDecay>,
        project_id: &tracedecay_domain::ProjectId,
        project_database_is_read_only: bool,
    ) -> Result<AdmittedSessionDatabases> {
        let project_session_open = async {
            let started = Instant::now();
            let database = self
                .store_administration
                .registered_project_session_database(cg.project_root(), cg.store_layout())
                .await?;
            Ok((database, started.elapsed()))
        };
        let profile_session_open = async {
            let started = Instant::now();
            let database = self
                .store_administration
                .registered_profile_session_database()
                .await?;
            Ok((database, started.elapsed()))
        };
        let ((session_db, project_sessions_elapsed), (user_session_db, profile_sessions_elapsed)) =
            join_independent_session_opens(project_session_open, profile_session_open).await?;
        let session_runtime_registry = self.store_administration.session_runtime_registry().await?;
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => {
                return Err(project_open_cancellation_error());
            }
            settlement = session_runtime_registry
                .settle_project_session_graph_for_serving(project_id) => {
                settlement?;
            }
        }
        if !project_database_is_read_only {
            bind_verified_project_graph_runtime(cg.db(), session_db.as_ref()).await?;
        }
        log_session_database_admission(
            self.canonical_project_path,
            project_sessions_elapsed,
            profile_sessions_elapsed,
        );
        Ok(AdmittedSessionDatabases {
            session_db,
            user_session_db,
        })
    }

    /// Construct the full server over the admitted session databases and
    /// runtime owners, then swap it in for the core.
    ///
    /// The core is reachable from here on, so every step leaves this function
    /// with an error instead of returning behind a published route: the
    /// caller's funnel owns retiring the owner. Retired relational graph repair
    /// is deliberately absent; the bounded code-index activation owns
    /// background indexing.
    #[hotpath::measure(label = "daemon.project.compose.construct_full", future = true)]
    async fn construct_full_server(
        &self,
        opened: &OpenedProjectGraph,
        core: &ComposedCoreServer,
        resolved: &Arc<crate::mcp::McpServer>,
    ) -> Result<PublishedFullServer> {
        let OpenedProjectGraph {
            cg,
            key,
            runtime_configuration,
            project_database_is_read_only,
            ..
        } = opened;
        let code_index = &core.ports.code_index;
        if *core.current_key.lock().await != *key {
            return Err(TraceDecayError::Config {
                message: "project changed branch during core capability admission".to_owned(),
            });
        }
        project_open_cancellation_checkpoint(self.cancellation)?;
        let AdmittedSessionDatabases {
            session_db,
            user_session_db,
        } = self
            .admit_session_databases(
                cg,
                &code_index.scope.project_id,
                *project_database_is_read_only,
            )
            .await?;
        self.invocation
            .service
            .mount_session_holder_databases([
                core.registered_profile_db.clone(),
                user_session_db.clone(),
            ])
            .await;
        let delivery_access = daemon_owned_project_source_access_at(
            &code_index.scope,
            self.canonical_project_path,
            runtime_configuration,
            tracedecay_contracts::now_micros(),
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("project delivery source access denied: {error}"),
        })?;
        project_delivery_mount::ensure_project_delivery_settlement(
            self.invocation,
            self.canonical_project_path,
            session_db.clone(),
            &code_index.scope,
            &delivery_access,
        )
        .await?;
        let host_admission_broker = Some(
            self.store_administration
                .host_admission_broker(&session_db)
                .await?,
        );
        let refresh_schedulers = self
            .store_administration
            .session_temporal_refresh_schedulers();
        // Session ingest prepares captures under the one process background
        // CPU authority the bootstrap worker plan installed and mounted
        // into the refresh schedulers; project open without it is a
        // composition defect, not a degraded mode.
        let background_cpu =
            refresh_schedulers
                .background_cpu()
                .ok_or_else(|| TraceDecayError::Config {
                    message:
                        "process background CPU authority was not mounted during daemon bootstrap"
                            .to_owned(),
                })?;
        let project_session_refresh_wake = refresh_schedulers
            .ensure_project_with_history(
                key.owner.clone(),
                session_db.clone(),
                Arc::new(ProjectSessionHistoricalIngestor::new(
                    session_db.clone(),
                    Arc::new(core.profile_identity.clone()),
                    self.canonical_project_path.to_path_buf(),
                    code_index.project_id.clone(),
                    core.transcript_source_home.clone(),
                    refresh_schedulers.codex_discovery(),
                    Arc::clone(&background_cpu),
                )),
            )
            .await;
        let user_session_refresh_wake = refresh_schedulers
            .ensure_profile_with_history(
                user_session_db.db_path().to_path_buf(),
                user_session_db.clone(),
                Arc::new(ProfileSessionHistoricalIngestor::new(
                    user_session_db.clone(),
                    core.registered_profile_db.clone(),
                    Arc::new(core.profile_identity.clone()),
                    core.transcript_source_home.clone(),
                    refresh_schedulers.codex_discovery(),
                    Arc::clone(&background_cpu),
                )),
            )
            .await;
        let session_sync_owner = self.store_administration.session_sync_service();
        session_sync_owner
            .register_project(DaemonSessionSyncConfig {
                brain_id: core.profile_identity.brain_id().clone(),
                profile_id: core.profile_identity.profile_id().clone(),
                project_id: code_index.project_id.clone(),
                profile_root: core.profile_identity.profile_root().to_path_buf(),
                project_root: self.canonical_project_path.to_path_buf(),
                transcript_source_home: core.transcript_source_home.clone(),
                project_sessions: session_db.clone(),
                user_sessions: user_session_db.clone(),
                registry: core.registered_profile_db.clone(),
                background_cpu: Arc::clone(&background_cpu),
                startup_import: cg.get_config().sync.session_start_sync,
                project_refresh: project_session_refresh_wake.clone(),
                user_refresh: user_session_refresh_wake.clone(),
            })
            .await?;
        let session_sync_port: Arc<dyn tracedecay_contracts::session_sync::SessionSyncServicePort> =
            session_sync_owner;
        let session_sync_service = Arc::downgrade(&session_sync_port);
        let store_telemetry_sampling = self.store_administration.store_telemetry_sampling();
        register_route_store_telemetry(
            &store_telemetry_sampling,
            cg,
            &code_index.scope,
            [
                core.registered_profile_db.as_ref(),
                user_session_db.as_ref(),
                session_db.as_ref(),
            ],
        );
        // Live Remote Brain operational read composed from the mounted remote
        // credential/spool/recovery authorities. Every operator surface
        // (Doctor, MCP, dashboard) re-observes current listener, enrollment,
        // spool, replay, backup, and failover state through this one provider;
        // typed `Unavailable` remains only when the remote plane is genuinely
        // unreadable.
        let remote_operational_status: tracedecay_contracts::RemoteOperationalStatusReaderV1 = {
            let remote_credentials = core.graph_runtime.remote_credential_authority();
            Arc::new(move || remote_credentials.operational_status())
        };
        let remote_operational_read = {
            let remote_operational_status = Arc::clone(&remote_operational_status);
            Arc::new(move || remote_operational_status().doctor_read())
        };
        let doctor_report_reader = doctor_kernel::production_doctor_report_reader(
            self.canonical_project_path.to_path_buf(),
            code_index.project_id.clone(),
            cg.store_layout().clone(),
            cg.db().clone(),
            core.registered_profile_db.clone(),
            user_session_db.clone(),
            session_db.clone(),
            core.profile_identity.profile_root().to_path_buf(),
            core.transcript_source_home.clone(),
            remote_operational_read,
            cg.get_config().sync.retention.clone(),
            self.invocation.code_index_schedulers.clone(),
            Arc::clone(&core.ports.diagnostic_broker),
            self.invocation.feedback_runtime_registrar(),
            self.invocation.semantic_owner_runtime_registrar(),
            store_telemetry_sampling,
            Arc::clone(cg.configuration_runtime()),
        );
        let (delivery_settlement_authority, delivery_settlement_recorder) =
            project_delivery_settlement_ports(self.invocation, self.canonical_project_path).await?;
        let profile_session_refresh = self
            .store_administration
            .profile_session_refresh_service(&user_session_db)
            .await;
        let full_context = core
            .publish_route_ports(
                crate::mcp::server::McpServerConstructionContext::daemon_owned(
                    Arc::clone(cg),
                    self.handshake.scope_prefix.clone(),
                    crate::mcp::server::McpServerDaemonAuthority {
                        profile_identity: core.profile_identity.clone(),
                        databases: crate::mcp::server::McpServerDaemonDatabases {
                            accounting: core.accounting_db.clone(),
                            registry: core.registered_profile_db.clone(),
                            project_sessions: session_db.clone(),
                            profile_sessions: user_session_db,
                        },
                        host_admission_broker,
                        background_cpu,
                        project_session_refresh_wake,
                        user_session_refresh_wake,
                        profile_session_refresh,
                        session_sync_service,
                        database_owner_reconciler: Arc::clone(&core.database_owner_reconciler),
                        project_routes: self.store_administration.project_routes(),
                        writers: crate::mcp::server::McpServerWriters::daemon_owned(
                            coordinated_dashboard_automation_writer(
                                self.store_administration.clone(),
                            ),
                            coordinated_background_refresh_writer(
                                self.store_administration.clone(),
                            ),
                        ),
                        delivery_settlement_authority,
                        delivery_settlement_recorder,
                    },
                ),
                cg,
                self.invocation,
            )
            .with_remote_operational_status(remote_operational_status)
            .with_dashboard_doctor_report_reader(doctor_report_reader)
            .with_startup_catch_up_enabled(self.runtime.startup_catch_up());
        project_open_cancellation_checkpoint(self.cancellation)?;
        let full_construction_started = Instant::now();
        let full_candidate = crate::mcp::McpServer::new_with_context(full_context).await;
        full_candidate
            .install_generation_census_reader(Arc::clone(&code_index.generation_census_reader))
            .map_err(|_| TraceDecayError::Config {
                message: "full MCP generation census authority was already installed".to_owned(),
            })?;
        self.log_phase("mcp_full_constructed", None, full_construction_started);
        if *core.current_key.lock().await != *key {
            full_candidate.shutdown().await;
            return Err(TraceDecayError::Config {
                message: "project changed branch during full capability admission".to_owned(),
            });
        }
        let upgraded = self
            .store_administration
            .project_servers()
            .lock()
            .await
            .replace_ready_if(key, Arc::clone(&full_candidate), |current| {
                Arc::ptr_eq(current, resolved)
            });
        if !upgraded {
            full_candidate.shutdown().await;
            return Err(TraceDecayError::Config {
                message: "project server changed during session capability upgrade".to_owned(),
            });
        }
        Ok(PublishedFullServer {
            server: full_candidate,
            session_db,
        })
    }

    /// Mount the full server's dependent owners: the source-edit lane, Git
    /// index transactions, the production owners, the semantic runtime, the
    /// owners that depend on them, and the HTTP application router.
    ///
    /// The widest project-open phase: the two owner registrations it awaits
    /// are the largest leaves of the open (each ~20 KB, ~80 KB when
    /// instrumented), so its caller boxes this phase rather than doubling it
    /// into its own state.
    #[hotpath::measure(label = "daemon.project.compose.full_owners", future = true)]
    async fn mount_full_server_owners(
        &self,
        opened: &OpenedProjectGraph,
        core: &ComposedCoreServer,
        full_server: &crate::mcp::McpServer,
        session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        core_source_edit_mutation: Option<
            Arc<tracedecay_daemon_service::project_owner_registration::SourceEditMutationGate>,
        >,
    ) -> Result<()> {
        let full_setup_started = Instant::now();
        project_open_cancellation_checkpoint(self.cancellation)?;
        // The shared invocation registry admits one source-edit owner per
        // project root. Core publication already registered it; the full
        // upgrade reuses that owner and marks its mutation gate ready after
        // Git transaction authority exists.
        let source_edit_mutation_ready = if opened.project_database_is_read_only {
            None
        } else {
            Some(
                core_source_edit_mutation.ok_or_else(|| TraceDecayError::Config {
                    message: "writable project did not install source edit preview authority"
                        .to_owned(),
                })?,
            )
        };
        self.log_phase("source_edit_preview_ready", None, full_setup_started);
        ensure_git_index_transactions_for_mutation_owners(
            self.store_administration,
            session_db,
            self.canonical_project_path,
            opened.key.owner.project_id.as_deref(),
        )
        .await?;
        self.log_phase("git_transactions_ready", None, full_setup_started);
        let dependent_owners = if opened.project_database_is_read_only {
            None
        } else {
            let source_edit_mutation_ready =
                source_edit_mutation_ready.ok_or_else(|| TraceDecayError::Config {
                    message: "writable project did not install source edit preview authority"
                        .to_owned(),
                })?;
            let state = project_open_owners::register_project_open_production_owners(
                self.invocation,
                self.store_administration.git_index_transaction_services(),
                self.store_administration.native_integration_services(),
                self.canonical_project_path,
                &core.project_id,
                full_server,
                source_edit_mutation_ready,
            )
            .await?;
            self.log_phase("independent_owners_registered", None, full_setup_started);
            Some(state)
        };
        project_open_cancellation_checkpoint(self.cancellation)?;
        match self
            .invocation
            .semantic_runtime_registrar()
            .register(
                self.canonical_project_path.to_path_buf(),
                opened.semantic.handle.clone(),
            )
            .await
        {
            Ok(()) | Err(DaemonSemanticRuntimeRegistrationError::AlreadyRegistered) => {}
            Err(DaemonSemanticRuntimeRegistrationError::RegistryClosed) => {
                return Err(TraceDecayError::Config {
                    message: "semantic runtime registration failed: the daemon project runtime registry is closed".to_owned(),
                });
            }
            Err(DaemonSemanticRuntimeRegistrationError::ConcurrentBuildFailed { detail }) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "semantic runtime registration failed after a concurrent build: {detail}"
                    ),
                });
            }
        }
        self.log_phase("semantic_runtime_registered", None, full_setup_started);
        if let Some(dependent_owners) = dependent_owners {
            project_open_owners::register_project_open_dependent_owners(
                self.invocation,
                self.canonical_project_path,
                full_server,
                dependent_owners,
            )
            .await?;
            self.log_phase("production_owners_registered", None, full_setup_started);
            mount_http_application_router(
                self.http_application_registry,
                &core.project_id,
                self.canonical_project_path,
            )
            .await?;
            self.log_phase("http_application_mounted", None, full_setup_started);
        }
        Ok(())
    }

    /// The full server is live in the registry: mount its dependent owners,
    /// commit the runtime publication, drain and retire the displaced core,
    /// and start code indexing.
    #[hotpath::measure(label = "daemon.project.compose.publish_full", future = true)]
    async fn finish_full_server(
        &self,
        opened: &OpenedProjectGraph,
        core: &ComposedCoreServer,
        activation: &CoreRouteActivation,
        resolved: &Arc<crate::mcp::McpServer>,
        full_server: &Arc<crate::mcp::McpServer>,
        session_db: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    ) -> Result<()> {
        self.log_phase("session_capabilities_published", None, self.started);
        Box::pin(self.mount_full_server_owners(
            opened,
            core,
            full_server.as_ref(),
            session_db,
            activation.core_source_edit_mutation.clone(),
        ))
        .await?;
        if *core.current_key.lock().await != opened.key {
            return Err(TraceDecayError::Config {
                message: "project changed branch during full capability admission".to_owned(),
            });
        }
        if !self
            .invocation
            .service
            .project_runtimes
            .mark_publication_ready(&activation.publication_attempt)
        {
            return Err(TraceDecayError::Config {
                message: "project runtime publication attempt was superseded".to_owned(),
            });
        }
        // The registry cutover prevents new core leases. Existing core
        // requests may finish while dependent owners warm, then the displaced
        // server is drained without closing the shared graph.
        resolved.revoke_project_server_responses_after_drain().await;
        schedule_project_server_retirement(
            self.store_administration,
            opened.key.owner.clone(),
            vec![Arc::clone(resolved)],
            None,
        )
        .await;
        full_server.publish_doctor_report();
        let code_index_status = match core.code_index_activation.automatic_admission() {
            code_index_scheduler::CodeIndexAutomaticAdmissionV1::Admitted => {
                if core.code_index_activation.activate() {
                    "warming"
                } else {
                    "unavailable"
                }
            }
            code_index_scheduler::CodeIndexAutomaticAdmissionV1::LinkedWorktreeDisabled => {
                log_daemon_event(
                    "code_index_activation_skipped",
                    &[
                        ("project", self.canonical_project_path.display().to_string()),
                        ("reason", "linked_worktree_disabled".to_owned()),
                    ],
                );
                "linked_worktree_disabled"
            }
        };
        self.log_phase(
            "full_published",
            Some(("code_index", code_index_status.to_owned())),
            self.started,
        );
        Ok(())
    }

    /// A failed upgrade either degrades the route to the still-published core
    /// (logging the failure and retiring the full server if it had gone live)
    /// or, when the core cannot be reclaimed, retires every server this
    /// attempt published and fails the open.
    #[hotpath::measure(label = "daemon.project.compose.settle_failed_upgrade", future = true)]
    async fn settle_failed_full_upgrade(
        &self,
        opened: &OpenedProjectGraph,
        core: &ComposedCoreServer,
        activation: &CoreRouteActivation,
        resolved: &Arc<crate::mcp::McpServer>,
        published_full_server: Option<Arc<crate::mcp::McpServer>>,
        error: TraceDecayError,
    ) -> Result<()> {
        let failed_key = core.current_key.lock().await.clone();
        let retain_core = !self.cancellation.is_cancelled() && failed_key == opened.key;
        let (core_retained, failed_full_server) = if retain_core {
            reclaim_core_after_failed_upgrade(
                self.store_administration,
                &opened.key,
                resolved,
                published_full_server.as_ref(),
            )
            .await
        } else {
            (false, None)
        };
        // The retained core owns preview-only source editing and never
        // receives the full server's Git mutation authority. Once the upgrade
        // fails, its mutation lane must become terminal rather than remaining
        // in a warming state forever.
        if let Some(mutation) = &activation.core_source_edit_mutation {
            mutation.mark_failed();
        }
        if core_retained {
            self.invocation
                .service
                .project_runtimes
                .mark_publication_failed(&activation.publication_attempt);
            if let Some(failed_full_server) = failed_full_server {
                failed_full_server.revoke_project_server_responses();
                schedule_project_server_retirement(
                    self.store_administration,
                    opened.key.owner.clone(),
                    vec![failed_full_server],
                    None,
                )
                .await;
            }
            self.log_phase(
                "full_upgrade_degraded",
                Some(("error", error.to_string())),
                self.started,
            );
            return Ok(());
        }
        retire_failed_project_open_owner(
            self.store_administration,
            &failed_key,
            resolved,
            published_full_server.is_some(),
            &core.route_registered,
        )
        .await;
        Err(error)
    }
}

/// The existing retained-task owner drains startup selection before semantic
/// lifecycle shutdown. A blocking selection is always joined, even after its
/// async task receives cancellation.
pub(super) fn retain_project_semantic_startup(
    registry: &tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1,
    semantic_startup_project: PathBuf,
    semantic_startup_schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    configuration: Arc<tracedecay_configuration::ProjectConfigurationRuntime>,
    semantic_lifecycle: Option<Arc<tracedecay_semantic::SemanticModelLifecycleOwnerV1>>,
    semantic_download_allowed: bool,
) -> bool {
    let semantic_configuration_client = configuration.client();
    let task_key = tracedecay_domain::canonical_text::encode_lowercase_hex(
        semantic_startup_project.as_os_str().as_encoded_bytes(),
    );
    registry.retain_hook_task("semantic-config-selection", &task_key, move |cancellation| async move {
            let started = Instant::now();
            let selected: Result<()> = async {
                let owner = semantic_lifecycle.ok_or_else(|| TraceDecayError::Config {
                    message: "project semantic lifecycle owner is unavailable".to_owned(),
                })?;
                // Linked worktrees read and apply the same logical project's
                // configuration under its one selection gate. A delayed open
                // cannot replay a snapshot captured before another open.
                if cancellation.is_cancelled() {
                    return Err(project_open_cancellation_error());
                }
                let selection = owner.configuration_selection_guard().await;
                if cancellation.is_cancelled() {
                    return Err(project_open_cancellation_error());
                }
                let current = semantic_configuration_client
                    .current()
                    .await
                    .map_err(|error| TraceDecayError::Config {
                        message: format!("semantic startup configuration unavailable: {error}"),
                    })?;
                if cancellation.is_cancelled() {
                    return Err(project_open_cancellation_error());
                }
                let owner = Arc::clone(&owner);
                tokio::task::spawn_blocking(move || {
                    let _selection = selection;
                    owner.select_model(
                        current.config().semantic.selected_model.as_deref(),
                        current.config().semantic.auto_download && semantic_download_allowed,
                    )
                })
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: format!("semantic startup worker failed: {error}"),
                })?
                .map_err(|error| TraceDecayError::Config {
                    message: format!("semantic startup selection failed: {error:?}"),
                })?;
                Ok(())
            }
            .await;
            match selected {
                Ok(()) if !cancellation.is_cancelled() => {
                    let _ = semantic_startup_schedulers
                        .reschedule_semantic_generation(&semantic_startup_project)
                        .await;
                }
                Ok(()) => {}
                Err(error) => {
                    tracing::warn!(%error, project = %semantic_startup_project.display(), "semantic startup selection unavailable");
                }
            }
            log_project_open_phase(
                &semantic_startup_project,
                "semantic_config_selection_settled",
                None,
                started,
            );
    })
}

/// Dashboard-facing semantic status reader: whether this project committed
/// semantic pins plus the runtime status resolved from current configuration.
fn project_dashboard_explorer_semantic_reader(
    configuration_client: Arc<tracedecay_configuration::ProductionConfigurationDaemonClient>,
) -> tracedecay_dashboard_api::ExplorerSemanticReader {
    Arc::new(move |project_root: std::path::PathBuf| {
        let configuration_client = Arc::clone(&configuration_client);
        Box::pin(async move {
            let activated =
                tracedecay_application::semantic_runtime::project_committed_semantic_pins(
                    &project_root,
                )
                .is_some();
            let configuration = configuration_client
                .current()
                .await
                .ok()
                .and_then(|pinned| {
                    tracedecay_application::semantic_runtime::SemanticConfigurationPinV1::from_current(
                        &pinned.into_current_state(),
                    )
                    .ok()
                });
            let status = Some(
                tracedecay_application::semantic_runtime::resolve_project_semantic_runtime_status(
                    Some(&project_root),
                    configuration,
                ),
            );
            tracedecay_dashboard_api::ExplorerSemanticReadV1 { activated, status }
        })
    })
}

/// Look this route up in the published project-server cache, refreshing its
/// recency on a hit. Callers reuse the returned server instead of opening.
async fn cached_route_server(
    store_administration: &StoreAdministration,
    route: &ProjectRouteKey,
) -> Option<(ProjectServerKey, Arc<crate::mcp::McpServer>)> {
    let mut servers = store_administration.project_servers().lock().await;
    servers
        .get_route_and_touch(route)
        .map(|(key, server)| (key.clone(), Arc::clone(server)))
}

/// The composition returned by every cache hit. `inserted` is always false:
/// reusing a published server never publishes a route.
fn cached_project_composition(
    canonical_project_path: &Path,
    key: ProjectServerKey,
    server: Arc<crate::mcp::McpServer>,
    semantic_auto_download_enabled: Option<bool>,
) -> ProductionProjectComposition {
    #[cfg(not(unix))]
    let _ = key;
    #[cfg(not(any(test, feature = "test-transport")))]
    let _ = semantic_auto_download_enabled;
    ProductionProjectComposition {
        #[cfg(unix)]
        key,
        canonical_project_path: canonical_project_path.to_path_buf(),
        server,
        #[cfg(unix)]
        inserted: false,
        #[cfg(any(test, feature = "test-transport"))]
        semantic_auto_download_enabled,
    }
}

/// Semantic-code choices this route resolves once from its authoritative
/// runtime configuration.
struct SemanticProjectRuntime {
    handle: tracedecay_semantic::DaemonSemanticRuntimeHandleV1,
    lifecycle: Option<Arc<tracedecay_semantic::SemanticModelLifecycleOwnerV1>>,
    resources: SemanticResourceCeilings,
    document_composition: tracedecay_domain::EmbeddingDocumentCompositionV1,
    auto_download_enabled: bool,
}

/// Derive this route's semantic runtime handle and startup choices. The
/// composition runtime can veto auto-download even when configuration allows
/// it, so both inputs are consulted here rather than at the use site.
fn semantic_project_runtime(
    runtime_configuration: &tracedecay_configuration::config::PinnedRuntimeConfiguration,
    runtime: &ProductionProjectCompositionRuntime,
    lifecycle: Arc<tracedecay_semantic::SemanticModelLifecycleOwnerV1>,
) -> Result<SemanticProjectRuntime> {
    let semantic_config = &runtime_configuration.config().semantic;
    let semantic_resources = &semantic_config.resources;
    // The configured ceiling still caps concurrency; this only narrows it to
    // what the serving reservation leaves room for and adds one slot so an
    // interactive query keeps a warm session while a rebuild holds the rest.
    let handle = tracedecay_semantic::DaemonSemanticRuntimeHandleV1::new(
        tracedecay_semantic::embedding_parallelism::embedding_pool_sessions(
            semantic_resources.max_threads,
            semantic_resources.max_concurrent_sessions,
        ),
        usize::try_from(semantic_resources.max_resident_bytes / 4096)
            .unwrap_or(usize::MAX)
            .max(semantic_resources.max_batch_size as usize),
        semantic_resources.max_resident_bytes,
    )
    .map_err(|_| TraceDecayError::Config {
        message: "semantic runtime resource ceilings are invalid".to_owned(),
    })?;
    Ok(SemanticProjectRuntime {
        handle,
        lifecycle: Some(lifecycle),
        resources: *semantic_resources,
        document_composition: semantic_config.document_composition,
        auto_download_enabled: semantic_config.auto_download && runtime.semantic_auto_download(),
    })
}

/// Every exact-scope code-index port this route publishes to its MCP servers.
struct ProjectCodeIndexAuthorities {
    publication_identity: crate::mcp::server::CodeIndexPublicationIdentityResolver,
    project_id: tracedecay_domain::ProjectId,
    scope: tracedecay_contracts::ResolvedScope,
    graph_projection_read_port: Arc<dyn tracedecay_graph_query::CodeGraphProjectionReadPort>,
    ignored_dependency_admission:
        Arc<dyn tracedecay_application::code_index::CodeIndexIgnoredDependencyAdmissionPortV1>,
    generation_census_reader: tracedecay_session_memory::runtime_telemetry::GenerationCensusReader,
    graph_read_admission_port: crate::mcp::server::CodeGraphReadAdmissionPort,
    search_authority: tracedecay_query::code_search::CodeIndexSearchAuthorityV1,
    search_executor: crate::mcp::server::CodeIndexSearchExecutor,
    branch_diff_executor: crate::mcp::server::CodeIndexBranchDiffExecutor,
}

/// Resolve the project's search identity and bind every code-index read port to
/// that one exact scope. Scope resolution reads the graph's own project root,
/// not the handshake path, so a relocated store still binds its own scope.
fn project_code_index_authorities(
    invocation: &DaemonInvocationState,
    cg: &Arc<crate::tracedecay::TraceDecay>,
    canonical_project_path: &Path,
    authoritative_project_id: &str,
    profile_identity: &profile_identity::LocalProfileIdentityAuthorityV1,
    route_registered: &Arc<AtomicBool>,
    project_database_is_read_only: bool,
) -> Result<ProjectCodeIndexAuthorities> {
    let publication_identity: crate::mcp::server::CodeIndexPublicationIdentityResolver =
        Arc::new(invocation.code_index_schedulers.clone());
    let project_id = tracedecay_domain::ProjectId::new(authoritative_project_id.to_owned())
        .map_err(|error| TraceDecayError::Config {
            message: format!("project search identity is invalid: {error}"),
        })?;
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(cg.project_root(), &project_id)
            .map_err(|error| TraceDecayError::Config {
            message: format!("project search scope is invalid: {error:?}"),
        })?;
    let graph_projection_read_port =
        tracedecay_code_index_runtime::project_reads::project_code_graph_projection_read_port(
            invocation.code_index_schedulers.clone(),
            canonical_project_path.to_path_buf(),
            scope.clone(),
        );
    let ignored_dependency_admission = tracedecay_code_index_runtime::project_reads::
        project_code_index_ignored_dependency_admission_port(
            invocation.code_index_schedulers.clone(),
            canonical_project_path.to_path_buf(),
            scope.clone(),
            !project_database_is_read_only,
        );
    let generation_census_reader =
        tracedecay_code_index_runtime::project_reads::project_code_index_generation_census_reader(
            invocation.code_index_schedulers.clone(),
            canonical_project_path.to_path_buf(),
            scope.clone(),
        );
    let graph_read_admission_port: crate::mcp::server::CodeGraphReadAdmissionPort = Arc::new(
        tracedecay_daemon_service::DaemonCodeGraphReadAdmission::production(
            canonical_project_path.to_path_buf(),
            scope.clone(),
            Arc::clone(cg.configuration_runtime()),
        ),
    );
    let search_admission = tracedecay_daemon_service::admit_query_mcp_read(
        Some(profile_identity),
        &project_id,
        &scope,
        Arc::clone(route_registered),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("project search admission is unavailable: {error}"),
    })?;
    let search_authority = search_admission.search_authority();
    let read_admission_provider = tracedecay_daemon_service::QueryMcpReadAdmissionProviderV1::new(
        profile_identity.clone(),
        project_id.clone(),
        Arc::clone(route_registered),
    );
    let search_executor = code_index_search_executor(
        invocation.code_index_schedulers.clone(),
        project_id.clone(),
        read_admission_provider.clone(),
        tracedecay_code_index_runtime::mcp_admission::RegisteredProjectScopeResolverV1,
    );
    let branch_diff_executor = code_index_branch_diff_executor(
        invocation.code_index_schedulers.clone(),
        project_id.clone(),
        read_admission_provider,
        tracedecay_code_index_runtime::mcp_admission::RegisteredProjectScopeResolverV1,
    );
    Ok(ProjectCodeIndexAuthorities {
        publication_identity,
        project_id,
        scope,
        graph_projection_read_port,
        ignored_dependency_admission,
        generation_census_reader,
        graph_read_admission_port,
        search_authority,
        search_executor,
        branch_diff_executor,
    })
}

/// Dashboard-facing freshness reader for this route's code-index schedulers.
fn project_dashboard_freshness_reader(
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
) -> tracedecay_dashboard_api::code_index_freshness_api::CodeIndexFreshnessReader {
    let reader: tracedecay_dashboard_api::code_index_freshness_api::CodeIndexFreshnessReader =
        Arc::new(move |project_root| {
            let schedulers = schedulers.clone();
            Box::pin(async move { schedulers.dashboard_freshness(&project_root).await })
        });
    reader
}

fn project_dashboard_pr_autotrack_reader()
-> tracedecay_dashboard_api::PrAutoTrackManagedSummaryReader {
    Arc::new(|store_root| {
        tracedecay_application::pr_tracking::managed_summary(&store_root).map(|entries| {
            entries
                .into_iter()
                .map(
                    |entry| tracedecay_dashboard_api::PrAutoTrackManagedSummaryEntryV1 {
                        branch: entry.branch,
                        pr: entry.pr,
                        head_branch: entry.head_branch,
                    },
                )
                .collect()
        })
    })
}

/// Register the project graph and the session databases this route owns with
/// the sampling authority. An unavailable registration is recorded and skipped,
/// never fatal: telemetry must not fail an otherwise healthy project open.
fn register_route_store_telemetry(
    sampling: &tracedecay_maintenance::telemetry::StoreTelemetrySamplingRegistry,
    cg: &Arc<crate::tracedecay::TraceDecay>,
    scope: &tracedecay_contracts::ResolvedScope,
    session_databases: [&tracedecay_global_db::RegisteredGlobalDb; 3],
) {
    let record_telemetry_registration = |path: &Path, registered: bool| {
        if !registered {
            log_daemon_event(
                "store_telemetry_registration",
                &[
                    ("store", path.display().to_string()),
                    ("outcome", "unavailable".to_owned()),
                ],
            );
        }
    };
    record_telemetry_registration(
        cg.db().database_path(),
        sampling.register_port(cg.db().database_path(), scope, || {
            cg.storage_telemetry_handle()
        }),
    );
    for database in session_databases {
        record_telemetry_registration(
            database.db_path(),
            sampling.register_port(database.db_path(), scope, || {
                database.storage_telemetry_handle()
            }),
        );
    }
}

/// Read this project's mounted delivery settlement ports. Both are required:
/// the full server refuses to publish without a settlement authority and a
/// recorder, so an unmounted port fails the upgrade instead of degrading.
async fn project_delivery_settlement_ports(
    invocation: &DaemonInvocationState,
    canonical_project_path: &Path,
) -> Result<(
    Arc<tracedecay_application::observability::DeliverySettlementAuthorityV1>,
    Arc<tracedecay_application::observability::BoundedDeliverySettlementRecorderV1>,
)> {
    let authority = invocation
        .service
        .delivery_settlement_authority(Some(canonical_project_path))
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("delivery settlement authority is invalid: {error}"),
        })?
        .ok_or_else(|| TraceDecayError::Config {
            message: "delivery settlement authority is not mounted".to_owned(),
        })?;
    let recorder = invocation
        .service
        .delivery_settlement_recorder(Some(canonical_project_path))
        .await
        .ok_or_else(|| TraceDecayError::Config {
            message: "delivery settlement recorder is not mounted".to_owned(),
        })?;
    Ok((authority, recorder))
}

/// Put the published core back in front of a failed full upgrade. Returns
/// whether the core is the live server again, plus the displaced full server
/// when one had already been published.
async fn reclaim_core_after_failed_upgrade(
    store_administration: &StoreAdministration,
    key: &ProjectServerKey,
    resolved: &Arc<crate::mcp::McpServer>,
    published_full_candidate: Option<&Arc<crate::mcp::McpServer>>,
) -> (bool, Option<Arc<crate::mcp::McpServer>>) {
    let mut servers = store_administration.project_servers().lock().await;
    match published_full_candidate {
        Some(failed_full_server) => {
            let displaced = servers.swap_ready_if(key, Arc::clone(resolved), |current| {
                Arc::ptr_eq(current, failed_full_server)
            });
            (displaced.is_some(), displaced)
        }
        None => (
            servers
                .get_ready(key)
                .is_some_and(|current| Arc::ptr_eq(current, resolved)),
            None,
        ),
    }
}

/// Retire every server this failed open attempt published, including the core
/// itself when session capabilities had already gone live.
#[hotpath::measure(label = "daemon.project.compose.retire_failed", future = true)]
async fn retire_failed_project_open_owner(
    store_administration: &StoreAdministration,
    failed_key: &ProjectServerKey,
    resolved: &Arc<crate::mcp::McpServer>,
    session_capabilities_published: bool,
    route_registered: &Arc<AtomicBool>,
) {
    let mut removed = store_administration
        .project_servers()
        .lock()
        .await
        .remove_owner(&failed_key.owner);
    if session_capabilities_published && removed.iter().all(|server| !Arc::ptr_eq(server, resolved))
    {
        removed.push(Arc::clone(resolved));
    }
    for server in &removed {
        server.revoke_project_server_responses();
    }
    debug_assert!(
        !removed.is_empty(),
        "failed core upgrade must retire its published owner"
    );
    // Request execution may itself need the owner writer held by this open
    // attempt. The tracked retirement starts draining after the caller returns
    // and releases that writer.
    super::project_server_lifecycle::retire_evicted_project_owner(
        store_administration,
        failed_key.owner.clone(),
        removed,
        Some(Arc::clone(route_registered)),
    )
    .await;
}
