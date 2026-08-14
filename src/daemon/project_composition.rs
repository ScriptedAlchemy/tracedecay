//! Production project composition: the wiring that builds one project's MCP
//! server from its store runtime, schedulers, and authority ports.
//!
//! `production_project_server` is the single composition root shared by the
//! Unix broker, the portable broker, and the in-process test harness.

use super::*;

mod code_index_activation;
mod runtime;
mod session_database_admission;
use code_index_activation::{
    CodeIndexActivationMountInputs, code_index_activation_hint_sink, code_index_activation_mount,
    code_index_hook_sink, code_index_reconcile_sink,
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

#[cfg(test)]
pub(super) fn daemon_transcript_source_home(profile_root: &Path) -> Option<PathBuf> {
    profile_root.parent().map(Path::to_path_buf)
}

#[cfg(not(test))]
pub(super) fn daemon_transcript_source_home(_profile_root: &Path) -> Option<PathBuf> {
    tracedecay_sessions::runtime::home_dir()
}

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
    let project_open_started = Instant::now();
    project_open_cancellation_checkpoint(cancellation)?;
    ensure_registered_project_route(
        store_administration,
        canonical_project_path,
        handshake.allow_init,
    )
    .await?;
    let route = ProjectRouteKey::from_handshake(canonical_project_path, handshake)?;
    if let Some((cached_key, cached_server)) = cached_route_server(store_administration, &route).await
    {
        return Ok(cached_project_composition(
            canonical_project_path,
            cached_key,
            cached_server,
            None,
        ));
    }

    let gate = project_open_gate(project_open_gates, &route).await;
    let _singleflight = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(project_open_cancellation_error()),
        singleflight = gate.lock() => singleflight,
    };
    // Order-sensitive: the same lookup runs again behind the single-flight gate
    // so a concurrent open that published while this caller waited is reused.
    if let Some((cached_key, cached_server)) = cached_route_server(store_administration, &route).await
    {
        return Ok(cached_project_composition(
            canonical_project_path,
            cached_key,
            cached_server,
            None,
        ));
    }

    #[cfg(test)]
    if let Some(attempts) = project_open_attempts {
        attempts.fetch_add(1, Ordering::Relaxed);
    }
    let cg = Box::pin(open_project_for_handshake(
        canonical_project_path,
        handshake,
        store_administration,
    ))
    .await?;
    let key = ProjectServerKey::from_open_project(&cg, handshake)?;
    let cg = Arc::new(cg);
    log_daemon_event(
        "project_open_phase",
        &[
            ("project", canonical_project_path.display().to_string()),
            ("phase", "graph_admitted".to_owned()),
            (
                "elapsed_ms",
                project_open_started.elapsed().as_millis().to_string(),
            ),
        ],
    );
    project_open_cancellation_checkpoint(cancellation)?;
    // A deletion may arrive while the graph opens. Recheck the durable replay
    // fence before this in-flight open can republish its registry authority.
    ensure_registered_project_route(store_administration, canonical_project_path, false).await?;
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
    let SemanticProjectRuntime {
        handle: semantic_runtime,
        lifecycle: semantic_lifecycle,
        resources: semantic_resources,
        auto_download_enabled: semantic_auto_download_enabled,
        startup_selection: semantic_startup_selection,
    } = semantic_project_runtime(&runtime_configuration, &runtime)?;
    let project_database_is_read_only = !cg.db().is_writable();
    let existing = {
        let mut servers = store_administration.project_servers().lock().await;
        let existing = servers.get_ready(&key).cloned();
        if existing.is_some() {
            servers.bind_route(route.clone(), key.clone());
        }
        existing
    };
    if let Some(existing) = existing {
        return Ok(cached_project_composition(
            canonical_project_path,
            key,
            existing,
            Some(semantic_auto_download_enabled),
        ));
    }

    let current_key = Arc::new(tokio::sync::Mutex::new(key.clone()));
    let current_project_path = Arc::new(tokio::sync::Mutex::new(
        canonical_project_path.to_path_buf(),
    ));
    let route_registered = Arc::new(AtomicBool::new(true));
    let database_owner_reconciler = runtime.database_owner_reconciler(
        store_administration,
        Arc::clone(&current_key),
        Arc::clone(&current_project_path),
        Arc::clone(&route_registered),
        handshake.clone(),
    );
    let automation_scheduler_reconciler = runtime.automation_scheduler_reconciler(
        Arc::clone(&current_key),
        Arc::clone(&current_project_path),
        handshake.clone(),
    );
    let authoritative_project_id =
        key.owner
            .project_id
            .clone()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project session runtime requires an authoritative project identity"
                    .to_owned(),
            })?;
    let registered_profile_db = store_administration.registered_profile_database().await?;
    let graph_runtime = store_administration.registered_runtime_registry().await?;
    let registry_db = Arc::clone(&registered_profile_db);
    let profile_identity = store_administration.profile_identity()?.clone();
    let accounting_db =
        crate::global_db::global_accounting_enabled().then(|| Arc::clone(&registered_profile_db));
    let ProjectCodeIndexAuthorities {
        publication_identity: code_index_publication_identity,
        project_id: code_search_project_id,
        scope: code_search_scope,
        graph_projection_read_port: code_graph_projection_read_port,
        ignored_dependency_admission: code_index_ignored_dependency_admission,
        generation_census_reader,
        graph_read_admission_port: code_graph_read_admission_port,
        search_authority: code_search_authority,
        read_admission_provider,
    } = project_code_index_authorities(
        invocation,
        &cg,
        canonical_project_path,
        &authoritative_project_id,
        &profile_identity,
        &route_registered,
        project_database_is_read_only,
    )?;
    let code_index_mount = code_index_activation_mount(CodeIndexActivationMountInputs {
        invocation: invocation.clone(),
        project_id: code_search_project_id.clone(),
        project_root: canonical_project_path.to_path_buf(),
        store_root: code_index_store_root.clone(),
        semantic_runtime: semantic_runtime.clone(),
        semantic_lifecycle: semantic_lifecycle.clone(),
        semantic_resources,
        scope: code_search_scope.clone(),
        route_registered: Arc::clone(&route_registered),
        cancellation: cancellation.clone(),
        graph_runtime: Arc::clone(&graph_runtime),
        graph_publication_database: Arc::new(cg.db().clone()),
        profile_id: cg.store_runtime_registry().profile_id().clone(),
    });
    let code_index_hint_sink = code_index_activation_hint_sink(
        invocation.code_index_schedulers.clone(),
        canonical_project_path.to_path_buf(),
    );
    let code_index_activation = Arc::new(code_index_scheduler::CodeIndexActivationV1::new(
        canonical_project_path,
        Arc::clone(&route_registered),
        cancellation.clone(),
        code_index_mount,
        code_index_hint_sink,
    ));
    let code_index_hook_sink = code_index_hook_sink(Arc::clone(&code_index_activation));
    let code_index_reconcile_sink = code_index_reconcile_sink(Arc::clone(&code_index_activation));
    // The daemon mounts the same broker the MCP server and the directly
    // served dashboard open: persisted analyzer settings (with a recorded
    // degradation for an unreadable file) plus the home-level OpenCode
    // analyzer-ownership registration adopted on top of the project-level one.
    let diagnostic_broker = tracedecay_usecases::dashboard_diagnostics::open_diagnostic_broker(
        canonical_project_path.to_path_buf(),
        &cg.store_layout().dashboard_root,
    )
    .await;
    let code_index_search_executor = code_index_search_executor(
        invocation.code_index_schedulers.clone(),
        code_search_project_id.clone(),
        read_admission_provider.clone(),
    );
    let code_index_branch_diff_executor = code_index_branch_diff_executor(
        invocation.code_index_schedulers.clone(),
        code_search_project_id.clone(),
        read_admission_provider,
    );
    let dashboard_code_index_freshness_reader =
        project_dashboard_freshness_reader(invocation.code_index_schedulers.clone());
    let dashboard_feedback_status_reader = crate::dashboard::feedback_api::feedback_status_reader(
        invocation.feedback_runtime_registrar(),
    );
    let application_invocation_executor: Arc<dyn crate::daemon_client::DaemonInvocationExecutor> =
        Arc::new(InProcessDaemonInvocationExecutor::new(
            invocation.clone(),
            store_administration.clone(),
            canonical_project_path.to_path_buf(),
            code_search_scope.clone(),
        ));
    let transcript_source_home = daemon_transcript_source_home(profile_identity.profile_root());
    let retained_server_resolver = retained_project_server_resolver(store_administration.clone());
    let mut core_context = crate::mcp::server::McpServerConstructionContext::daemon_owned_core(
        Arc::clone(&cg),
        handshake.scope_prefix.clone(),
        crate::mcp::server::McpServerDaemonCoreAuthority {
            profile_identity: profile_identity.clone(),
            accounting: accounting_db.clone(),
            registry: Arc::clone(&registry_db),
            database_owner_reconciler: Arc::clone(&database_owner_reconciler),
            project_routes: store_administration.project_routes(),
            writers: crate::mcp::server::McpServerWriters::daemon_owned(
                coordinated_dashboard_automation_writer(store_administration.clone()),
                coordinated_background_refresh_writer(store_administration.clone()),
            ),
        },
    )
    .with_dashboard_code_index_freshness_reader(Arc::clone(&dashboard_code_index_freshness_reader))
    .with_dashboard_feedback_status_reader(Arc::clone(&dashboard_feedback_status_reader))
    .with_diagnostics_lsp(Arc::clone(&diagnostic_broker))
    .with_code_index_hook_sink(Arc::clone(&code_index_hook_sink))
    .with_code_index_reconcile_sink(Arc::clone(&code_index_reconcile_sink))
    .with_code_index_publication_identity(Arc::clone(&code_index_publication_identity))
    .with_code_index_search_executor(Arc::clone(&code_index_search_executor))
    .with_code_index_branch_diff_executor(Arc::clone(&code_index_branch_diff_executor))
    .with_code_graph_projection_read_port(Arc::clone(&code_graph_projection_read_port))
    .with_code_index_ignored_dependency_admission(Arc::clone(
        &code_index_ignored_dependency_admission,
    ))
    .with_code_graph_read_admission_port(Arc::clone(&code_graph_read_admission_port))
    .with_code_index_search_authority(code_search_authority.clone())
    .with_project_server_live(Arc::clone(&route_registered))
    .with_application_invocation_executor(Arc::clone(&application_invocation_executor))
    .with_daemon_invocation_service(invocation.service.clone())
    .with_retained_project_server_resolver(Arc::clone(&retained_server_resolver));
    if let Some(reconciler) = automation_scheduler_reconciler.as_ref() {
        core_context = core_context.with_automation_scheduler_reconciler(Arc::clone(reconciler));
    }
    project_open_cancellation_checkpoint(cancellation)?;
    let mcp_construction_started = Instant::now();
    let core_candidate = crate::mcp::McpServer::new_with_context(core_context).await;
    core_candidate
        .install_generation_census_reader(Arc::clone(&generation_census_reader))
        .map_err(|_| TraceDecayError::Config {
            message: "core MCP generation census authority was already installed".to_owned(),
        })?;
    log_daemon_event(
        "project_open_phase",
        &[
            ("project", canonical_project_path.display().to_string()),
            ("phase", "mcp_core_constructed".to_owned()),
            (
                "elapsed_ms",
                mcp_construction_started.elapsed().as_millis().to_string(),
            ),
        ],
    );
    if cancellation.is_cancelled() {
        core_candidate.shutdown().await;
        return Err(project_open_cancellation_error());
    }
    let project_id = key
        .owner
        .project_id
        .clone()
        .ok_or_else(|| TraceDecayError::Config {
            message: "project-open owners require an authoritative project identity".to_owned(),
        })?;
    let resolution = store_administration
        .project_servers()
        .lock()
        .await
        .bind_or_insert_route_bounded(
            route,
            key.clone(),
            core_candidate,
            MAX_CACHED_PROJECT_SERVERS,
            |server| Arc::strong_count(server) > 1,
        );
    let Some((mut resolved, inserted)) = resolution else {
        route_registered.store(false, Ordering::Release);
        return Err(project_server_capacity_error());
    };
    if !inserted {
        route_registered.store(false, Ordering::Release);
    } else {
        if cancellation.is_cancelled() {
            return Err(project_open_cancellation_error());
        }
        if !invocation
            .code_index_schedulers
            .register_activation(&code_search_scope, &code_index_activation)
        {
            route_registered.store(false, Ordering::Release);
            return Err(TraceDecayError::Config {
                message: "code-index activation scope does not match the project route".to_owned(),
            });
        }
        // The core's own lane never opens: only the full server reaches a Git
        // transaction authority. Its gate is kept so a rolled-back publication
        // can report a terminal failure instead of warming forever.
        let core_source_edit_mutation = if project_database_is_read_only {
            None
        } else {
            Some(
                project_open_owners::install_project_open_source_edit_preview_owner(
                    resolved.as_ref(),
                    Arc::clone(&cg),
                    Arc::clone(&code_graph_projection_read_port),
                    canonical_project_path,
                    &project_id,
                )
                .await?,
            )
        };
        // Publish the graph/search/diagnostic core before session admission.
        // Source-edit previews are available, while mutations fail closed as
        // warming until the full server has its transaction authority.
        {
            let mut servers = store_administration.project_servers().lock().await;
            if !servers.mark_ready(&key) {
                return Err(TraceDecayError::Config {
                    message: "project server disappeared before core publication completed"
                        .to_owned(),
                });
            }
        }
        log_daemon_event(
            "project_open_phase",
            &[
                ("project", canonical_project_path.display().to_string()),
                ("phase", "core_published".to_owned()),
                (
                    "elapsed_ms",
                    project_open_started.elapsed().as_millis().to_string(),
                ),
            ],
        );
        let semantic_startup_project = canonical_project_path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let started = Instant::now();
            let _ = crate::semantic_code::apply_config_and_queue_startup(
                semantic_startup_selection.as_deref(),
                semantic_auto_download_enabled,
            );
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", semantic_startup_project.display().to_string()),
                    ("phase", "semantic_startup_configured".to_owned()),
                    ("elapsed_ms", started.elapsed().as_millis().to_string()),
                ],
            );
        });
        let session_capabilities_published = AtomicBool::new(false);
        let mut published_full_candidate = None;
        let full_upgrade: Result<Arc<crate::mcp::McpServer>> = async {
            // The core is reachable from here on, so every step below leaves
            // this block with an error instead of returning behind a published
            // route: the funnel around it owns retiring the owner. Retired
            // relational graph repair is deliberately absent; the bounded
            // code-index activation below owns background indexing.
            if *current_key.lock().await != key {
                return Err(TraceDecayError::Config {
                    message: "project changed branch during core capability admission".to_owned(),
                });
            }
            project_open_cancellation_checkpoint(cancellation)?;
            let project_session_open = async {
                let started = Instant::now();
                let database = store_administration
                    .registered_project_session_database(cg.project_root(), cg.store_layout())
                    .await?;
                Ok((database, started.elapsed()))
            };
            let profile_session_open = async {
                let started = Instant::now();
                let database = store_administration
                    .registered_profile_session_database()
                    .await?;
                Ok((database, started.elapsed()))
            };
            let (
                (registered_project_session_db, project_sessions_elapsed),
                (registered_user_session_db, profile_sessions_elapsed),
            ) = join_independent_session_opens(project_session_open, profile_session_open).await?;
            if !project_database_is_read_only {
                bind_verified_project_graph_runtime(
                    &graph_runtime,
                    code_search_project_id.clone(),
                    Arc::new(cg.db().clone()),
                    registered_project_session_db.as_ref(),
                )
                .await?;
            }
            log_session_database_admission(
                canonical_project_path,
                project_sessions_elapsed,
                profile_sessions_elapsed,
            );
            let session_db = Arc::clone(&registered_project_session_db);
            let user_session_db = Arc::clone(&registered_user_session_db);
            invocation
                .service
                .mount_session_holder_databases([
                    Arc::clone(&registered_profile_db),
                    Arc::clone(&user_session_db),
                ])
                .await;
            let delivery_access = project_open_owners::daemon_owned_project_source_access_at(
                &code_search_scope,
                canonical_project_path,
                &runtime_configuration,
                tracedecay_application::now_micros(),
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("project delivery source access denied: {error}"),
            })?;
            project_delivery_mount::ensure_project_delivery_settlement(
                invocation,
                canonical_project_path,
                Arc::clone(&session_db),
                &code_search_scope,
                &delivery_access,
            )
            .await?;
            let host_admission_broker = Some(
                store_administration
                    .host_admission_broker(&session_db)
                    .await?,
            );
            let project_session_refresh_wake = store_administration
                .session_temporal_refresh_schedulers()
                .ensure_project_with_history(
                    key.owner.clone(),
                    Arc::clone(&session_db),
                    Arc::new(
                        session_temporal_refresh_scheduler::ProjectSessionHistoricalIngestor::new(
                            Arc::clone(&session_db),
                            profile_identity.clone(),
                            canonical_project_path.to_path_buf(),
                            code_search_project_id.clone(),
                            transcript_source_home.clone(),
                        ),
                    ),
                )
                .await;
            let user_session_refresh_wake = store_administration
                .session_temporal_refresh_schedulers()
                .ensure_profile_with_history(
                    user_session_db.db_path().to_path_buf(),
                    Arc::clone(&user_session_db),
                    Arc::new(
                        session_temporal_refresh_scheduler::ProfileSessionHistoricalIngestor::new(
                            Arc::clone(&user_session_db),
                            Arc::clone(&registry_db),
                            profile_identity.clone(),
                            transcript_source_home.clone(),
                        ),
                    ),
                )
                .await;
            let session_sync_owner = store_administration.session_sync_service();
            session_sync_owner
                .register_project(crate::daemon::session_sync::DaemonSessionSyncConfig {
                    brain_id: profile_identity.brain_id().clone(),
                    profile_id: profile_identity.profile_id().clone(),
                    project_id: code_search_project_id.clone(),
                    profile_root: profile_identity.profile_root().to_path_buf(),
                    project_root: canonical_project_path.to_path_buf(),
                    transcript_source_home: transcript_source_home.clone(),
                    project_sessions: Arc::clone(&session_db),
                    user_sessions: Arc::clone(&user_session_db),
                    registry: Arc::clone(&registry_db),
                    startup_import: cg.get_config().sync.session_start_sync,
                    project_refresh: project_session_refresh_wake.clone(),
                    user_refresh: user_session_refresh_wake.clone(),
                })
                .await?;
            let session_sync_port: Arc<
                dyn tracedecay_application::session_sync::SessionSyncServicePort,
            > = session_sync_owner;
            let session_sync_service = Arc::downgrade(&session_sync_port);
            let store_telemetry_sampling = store_administration.store_telemetry_sampling();
            register_route_store_telemetry(
                &store_telemetry_sampling,
                &cg,
                &code_search_scope,
                [
                    registry_db.as_ref(),
                    user_session_db.as_ref(),
                    session_db.as_ref(),
                ],
            );
            let doctor_report_reader = doctor_kernel::production_doctor_report_reader(
                canonical_project_path.to_path_buf(),
                code_search_project_id.clone(),
                cg.store_layout().clone(),
                cg.db().clone(),
                Arc::clone(&registry_db),
                Arc::clone(&user_session_db),
                Arc::clone(&session_db),
                profile_identity.profile_root().to_path_buf(),
                transcript_source_home.clone(),
                tracedecay_application::RemoteOperationalReadV1::Unavailable,
                cg.get_config().sync.retention.clone(),
                invocation.code_index_schedulers.clone(),
                Arc::clone(&diagnostic_broker),
                invocation.feedback_runtime_registrar(),
                store_telemetry_sampling,
                Arc::clone(cg.configuration_runtime()),
            );
            let (delivery_settlement_authority, delivery_settlement_recorder) =
                project_delivery_settlement_ports(invocation, canonical_project_path).await?;
            let mut full_context = crate::mcp::server::McpServerConstructionContext::daemon_owned(
                Arc::clone(&cg),
                handshake.scope_prefix.clone(),
                crate::mcp::server::McpServerDaemonAuthority {
                    profile_identity: profile_identity.clone(),
                    databases: crate::mcp::server::McpServerDaemonDatabases {
                        accounting: accounting_db,
                        registry: registry_db,
                        project_sessions: session_db,
                        user_sessions: user_session_db,
                        registered_project_sessions: Arc::clone(&registered_project_session_db),
                        registered_user_sessions: registered_user_session_db,
                    },
                    host_admission_broker,
                    project_session_refresh_wake,
                    user_session_refresh_wake,
                    session_sync_service,
                    database_owner_reconciler,
                    project_routes: store_administration.project_routes(),
                    writers: crate::mcp::server::McpServerWriters::daemon_owned(
                        coordinated_dashboard_automation_writer(store_administration.clone()),
                        coordinated_background_refresh_writer(store_administration.clone()),
                    ),
                    delivery_settlement_authority,
                    delivery_settlement_recorder,
                },
            )
            .with_dashboard_doctor_report_reader(doctor_report_reader)
            .with_dashboard_code_index_freshness_reader(dashboard_code_index_freshness_reader)
            .with_dashboard_feedback_status_reader(dashboard_feedback_status_reader)
            .with_diagnostics_lsp(diagnostic_broker)
            .with_code_index_hook_sink(code_index_hook_sink)
            .with_code_index_reconcile_sink(code_index_reconcile_sink)
            .with_code_index_publication_identity(code_index_publication_identity)
            .with_code_index_search_executor(code_index_search_executor)
            .with_code_index_branch_diff_executor(code_index_branch_diff_executor)
            .with_code_graph_projection_read_port(Arc::clone(&code_graph_projection_read_port))
            .with_code_index_ignored_dependency_admission(Arc::clone(
                &code_index_ignored_dependency_admission,
            ))
            .with_code_graph_read_admission_port(Arc::clone(&code_graph_read_admission_port))
            .with_code_index_search_authority(code_search_authority)
            .with_project_server_live(Arc::clone(&route_registered))
            .with_application_invocation_executor(application_invocation_executor)
            .with_daemon_invocation_service(invocation.service.clone())
            .with_startup_catch_up_enabled(runtime.startup_catch_up())
            .with_retained_project_server_resolver(retained_server_resolver);
            if let Some(reconciler) = automation_scheduler_reconciler {
                full_context = full_context.with_automation_scheduler_reconciler(reconciler);
            }
            project_open_cancellation_checkpoint(cancellation)?;
            let full_construction_started = Instant::now();
            let full_candidate = crate::mcp::McpServer::new_with_context(full_context).await;
            full_candidate
                .install_generation_census_reader(Arc::clone(&generation_census_reader))
                .map_err(|_| TraceDecayError::Config {
                    message: "full MCP generation census authority was already installed"
                        .to_owned(),
                })?;
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "mcp_full_constructed".to_owned()),
                    (
                        "elapsed_ms",
                        full_construction_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            if *current_key.lock().await != key {
                full_candidate.shutdown().await;
                return Err(TraceDecayError::Config {
                    message: "project changed branch during full capability admission".to_owned(),
                });
            }
            let upgraded = store_administration
                .project_servers()
                .lock()
                .await
                .replace_ready_if(&key, Arc::clone(&full_candidate), |current| {
                    Arc::ptr_eq(current, &resolved)
                });
            if !upgraded {
                full_candidate.shutdown().await;
                return Err(TraceDecayError::Config {
                    message: "project server changed during session capability upgrade".to_owned(),
                });
            }
            published_full_candidate = Some(Arc::clone(&full_candidate));
            session_capabilities_published.store(true, Ordering::Release);
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "session_capabilities_published".to_owned()),
                    (
                        "elapsed_ms",
                        project_open_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            let full_setup: Result<()> = async {
                let full_setup_started = Instant::now();
                let log_full_setup_phase = |phase: &'static str| {
                    log_daemon_event(
                        "project_open_phase",
                        &[
                            ("project", canonical_project_path.display().to_string()),
                            ("phase", phase.to_owned()),
                            (
                                "elapsed_ms",
                                full_setup_started.elapsed().as_millis().to_string(),
                            ),
                        ],
                    );
                };
                project_open_cancellation_checkpoint(cancellation)?;
                let source_edit_mutation_ready = if project_database_is_read_only {
                    None
                } else {
                    Some(
                        project_open_owners::install_project_open_source_edit_preview_owner(
                            full_candidate.as_ref(),
                            Arc::clone(&cg),
                            Arc::clone(&code_graph_projection_read_port),
                            canonical_project_path,
                            &project_id,
                        )
                        .await?,
                    )
                };
                log_full_setup_phase("source_edit_preview_ready");
                ensure_git_index_transactions_for_mutation_owners(
                    store_administration,
                    Arc::clone(&registered_project_session_db),
                    canonical_project_path,
                    key.owner.project_id.as_deref(),
                )
                .await?;
                log_full_setup_phase("git_transactions_ready");
                let dependent_owners = if project_database_is_read_only {
                    None
                } else {
                    let source_edit_mutation_ready =
                        source_edit_mutation_ready.ok_or_else(|| TraceDecayError::Config {
                            message:
                                "writable project did not install source edit preview authority"
                                    .to_owned(),
                        })?;
                    let state = project_open_owners::register_project_open_production_owners(
                        invocation,
                        store_administration.git_index_transaction_services(),
                        store_administration.native_integration_services(),
                        canonical_project_path,
                        &project_id,
                        full_candidate.as_ref(),
                        source_edit_mutation_ready,
                    )
                    .await?;
                    log_full_setup_phase("independent_owners_registered");
                    Some(state)
                };
                project_open_cancellation_checkpoint(cancellation)?;
                match invocation
                    .semantic_runtime_registrar()
                    .register(canonical_project_path.to_path_buf(), semantic_runtime)
                    .await
                {
                    Ok(()) | Err(DaemonSemanticRuntimeRegistrationError::AlreadyRegistered) => {}
                    Err(DaemonSemanticRuntimeRegistrationError::RegistryClosed) => {
                        return Err(TraceDecayError::Config {
                            message: "semantic runtime registration failed: the daemon project runtime registry is closed".to_owned(),
                        });
                    }
                }
                log_full_setup_phase("semantic_runtime_registered");
                if let Some(dependent_owners) = dependent_owners {
                    project_open_owners::register_project_open_dependent_owners(
                        invocation,
                        canonical_project_path,
                        dependent_owners,
                    )
                    .await?;
                    log_full_setup_phase("production_owners_registered");
                    mount_http_application_router(
                        http_application_registry,
                        &project_id,
                        canonical_project_path,
                    )
                    .await?;
                    log_full_setup_phase("http_application_mounted");
                }
                Ok(())
            }
            .await;
            full_setup?;
            if *current_key.lock().await != key {
                return Err(TraceDecayError::Config {
                    message: "project changed branch during full capability admission".to_owned(),
                });
            }
            // The registry cutover prevents new core leases. Existing core
            // requests may finish while dependent owners warm, then the
            // displaced server is drained without closing the shared graph.
            resolved
                .revoke_project_server_responses_after_drain()
                .await;
            schedule_project_server_retirement(
                store_administration,
                key.owner.clone(),
                vec![Arc::clone(&resolved)],
                None,
            )
            .await;
            full_candidate.publish_doctor_report();
            let indexing_requested = code_index_activation.activate();
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "full_published".to_owned()),
                    (
                        "code_index",
                        if indexing_requested {
                            "warming"
                        } else {
                            "unavailable"
                        }
                        .to_owned(),
                    ),
                    (
                        "elapsed_ms",
                        project_open_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            Ok(full_candidate)
        }
        .await;
        match full_upgrade {
            Ok(full_server) => resolved = full_server,
            Err(error) => {
                let failed_key = current_key.lock().await.clone();
                let retain_core = !cancellation.is_cancelled() && failed_key == key;
                let (core_retained, failed_full_server) = if retain_core {
                    reclaim_core_after_failed_upgrade(
                        store_administration,
                        &key,
                        &resolved,
                        published_full_candidate.as_ref(),
                    )
                    .await
                } else {
                    (false, None)
                };
                // The retained core owns preview-only source editing and never
                // receives the full server's Git mutation authority. Once the
                // upgrade fails, its mutation lane must become terminal rather
                // than remaining in a warming state forever.
                if let Some(mutation) = &core_source_edit_mutation {
                    mutation.mark_failed();
                }
                if core_retained {
                    if let Some(failed_full_server) = failed_full_server {
                        failed_full_server.revoke_project_server_responses();
                        schedule_project_server_retirement(
                            store_administration,
                            key.owner.clone(),
                            vec![failed_full_server],
                            None,
                        )
                        .await;
                    }
                    log_daemon_event(
                        "project_open_phase",
                        &[
                            ("project", canonical_project_path.display().to_string()),
                            ("phase", "full_upgrade_degraded".to_owned()),
                            ("error", error.to_string()),
                            (
                                "elapsed_ms",
                                project_open_started.elapsed().as_millis().to_string(),
                            ),
                        ],
                    );
                } else {
                    retire_failed_project_open_owner(
                        store_administration,
                        &failed_key,
                        &resolved,
                        session_capabilities_published.load(Ordering::Acquire),
                        &route_registered,
                    )
                    .await;
                    return Err(error);
                }
            }
        }
    }
    Ok(ProductionProjectComposition {
        #[cfg(unix)]
        key,
        canonical_project_path: canonical_project_path.to_path_buf(),
        server: resolved,
        #[cfg(unix)]
        inserted,
        #[cfg(any(test, feature = "test-transport"))]
        semantic_auto_download_enabled: Some(semantic_auto_download_enabled),
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
    handle: crate::semantic_code::DaemonSemanticRuntimeHandleV1,
    lifecycle: Option<Arc<crate::semantic_code::SemanticModelLifecycleOwnerV1>>,
    resources: crate::config::SemanticResourceCeilings,
    auto_download_enabled: bool,
    startup_selection: Option<String>,
}

/// Derive this route's semantic runtime handle and startup choices. The
/// composition runtime can veto auto-download even when configuration allows
/// it, so both inputs are consulted here rather than at the use site.
fn semantic_project_runtime(
    runtime_configuration: &tracedecay_usecases::config::PinnedRuntimeConfiguration,
    runtime: &ProductionProjectCompositionRuntime,
) -> Result<SemanticProjectRuntime> {
    let semantic_config = &runtime_configuration.config.semantic;
    let semantic_resources = &semantic_config.resources;
    // The configured ceiling still caps concurrency; this only narrows it to
    // what the serving reservation leaves room for and adds one slot so an
    // interactive query keeps a warm session while a rebuild holds the rest.
    let handle = crate::semantic_code::DaemonSemanticRuntimeHandleV1::new(
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
        lifecycle: crate::semantic_code::shared_lifecycle_owner(),
        resources: *semantic_resources,
        auto_download_enabled: semantic_config.auto_download && runtime.semantic_auto_download(),
        startup_selection: semantic_config.selected_model.clone(),
    })
}

/// Every exact-scope code-index port this route publishes to its MCP servers.
struct ProjectCodeIndexAuthorities {
    publication_identity: crate::mcp::server::CodeIndexPublicationIdentityResolver,
    project_id: tracedecay_domain::ProjectId,
    scope: tracedecay_application::ResolvedScope,
    graph_projection_read_port: Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort>,
    ignored_dependency_admission:
        Arc<dyn tracedecay_usecases::code_index::CodeIndexIgnoredDependencyAdmissionPortV1>,
    generation_census_reader: crate::runtime_telemetry::GenerationCensusReader,
    graph_read_admission_port: crate::mcp::server::CodeGraphReadAdmissionPort,
    search_authority: tracedecay_query::code_search::CodeIndexSearchAuthorityV1,
    read_admission_provider: query_mcp_admission::QueryMcpReadAdmissionProviderV1,
}

/// Resolve the project's search identity and bind every code-index read port to
/// that one exact scope. Scope resolution reads the graph's own project root,
/// not the handshake path, so a relocated store still binds its own scope.
fn project_code_index_authorities(
    invocation: &DaemonInvocationState,
    cg: &Arc<crate::tracedecay::TraceDecay>,
    canonical_project_path: &Path,
    authoritative_project_id: &str,
    profile_identity: &crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1,
    route_registered: &Arc<AtomicBool>,
    project_database_is_read_only: bool,
) -> Result<ProjectCodeIndexAuthorities> {
    let publication_identity: crate::mcp::server::CodeIndexPublicationIdentityResolver =
        Arc::new(invocation.code_index_schedulers.clone());
    let project_id = tracedecay_domain::ProjectId::new(authoritative_project_id.to_owned())
        .map_err(|error| TraceDecayError::Config {
            message: format!("project search identity is invalid: {error}"),
        })?;
    let scope = project_open_owners::resolved_scope_for_project(cg.project_root(), &project_id)
        .map_err(|error| TraceDecayError::Config {
            message: format!("project search scope is invalid: {error:?}"),
        })?;
    let graph_projection_read_port = project_open_owners::project_code_graph_projection_read_port(
        invocation.code_index_schedulers.clone(),
        canonical_project_path.to_path_buf(),
        scope.clone(),
    );
    let ignored_dependency_admission =
        project_open_owners::project_code_index_ignored_dependency_admission_port(
            invocation.code_index_schedulers.clone(),
            canonical_project_path.to_path_buf(),
            scope.clone(),
            !project_database_is_read_only,
        );
    let generation_census_reader = project_open_owners::project_code_index_generation_census_reader(
        invocation.code_index_schedulers.clone(),
        canonical_project_path.to_path_buf(),
        scope.clone(),
    );
    let graph_read_admission_port: crate::mcp::server::CodeGraphReadAdmissionPort = Arc::new(
        crate::daemon::callable_code_authorization::DaemonCodeGraphReadAdmission::production(
            canonical_project_path.to_path_buf(),
            scope.clone(),
            Arc::clone(cg.configuration_runtime()),
        ),
    );
    let search_admission = query_mcp_admission::admit_query_mcp_read(
        Some(profile_identity),
        &project_id,
        &scope,
        Arc::clone(route_registered),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("project search admission is unavailable: {error}"),
    })?;
    let search_authority = search_admission.search_authority();
    let read_admission_provider = query_mcp_admission::QueryMcpReadAdmissionProviderV1::new(
        profile_identity.clone(),
        project_id.clone(),
        Arc::clone(route_registered),
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
        read_admission_provider,
    })
}

/// Dashboard-facing freshness reader for this route's code-index schedulers.
fn project_dashboard_freshness_reader(
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
) -> crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader {
    let reader: crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader =
        Arc::new(move |project_root| {
            let schedulers = schedulers.clone();
            Box::pin(async move { schedulers.dashboard_freshness(&project_root).await })
        });
    reader
}

/// Register the project graph and the session databases this route owns with
/// the sampling authority. An unavailable registration is recorded and skipped,
/// never fatal: telemetry must not fail an otherwise healthy project open.
fn register_route_store_telemetry(
    sampling: &crate::daemon::maintenance::StoreTelemetrySamplingRegistry,
    cg: &Arc<crate::tracedecay::TraceDecay>,
    scope: &tracedecay_application::ResolvedScope,
    session_databases: [&crate::global_db::RegisteredGlobalDb; 3],
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
    Arc<tracedecay_usecases::observability::DeliverySettlementAuthorityV1>,
    Arc<tracedecay_usecases::observability::BoundedDeliverySettlementRecorderV1>,
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
    schedule_project_server_retirement(
        store_administration,
        failed_key.owner.clone(),
        removed,
        Some(Arc::clone(route_registered)),
    )
    .await;
}
