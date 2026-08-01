//! Production project composition: the wiring that builds one project's MCP
//! server from its store runtime, schedulers, and authority ports.
//!
//! `production_project_server` is the single composition root shared by the
//! Unix broker, the portable broker, and the in-process test harness.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

#[derive(Clone)]
pub(super) enum ProductionProjectCompositionRuntime {
    #[cfg(unix)]
    Unix(Box<DaemonEngine>),
    #[cfg(any(not(unix), test, feature = "test-transport"))]
    Portable {
        semantic_auto_download: bool,
        startup_catch_up: bool,
    },
}

impl ProductionProjectCompositionRuntime {
    fn database_owner_reconciler(
        &self,
        _store_administration: &StoreAdministration,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        _current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        route_registered: Arc<AtomicBool>,
        handshake: DaemonHandshake,
    ) -> crate::mcp::DatabaseOwnerReconciler {
        match self {
            #[cfg(unix)]
            Self::Unix(engine) => engine.database_owner_reconciler(
                current_key,
                _current_project_path,
                route_registered,
                handshake,
            ),
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable { .. } => portable_database_owner_reconciler(
                _store_administration.clone(),
                current_key,
                route_registered,
                handshake,
            ),
        }
    }

    fn automation_scheduler_reconciler(
        &self,
        _current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        _current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        _handshake: DaemonHandshake,
    ) -> Option<crate::dashboard::AutomationSchedulerReconciler> {
        match self {
            #[cfg(unix)]
            Self::Unix(engine) => Some(engine.automation_scheduler_reconciler(
                _current_key,
                _current_project_path,
                _handshake,
            )),
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable { .. } => None,
        }
    }

    const fn semantic_auto_download(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(_) => true,
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable {
                semantic_auto_download,
                ..
            } => *semantic_auto_download,
        }
    }

    const fn startup_catch_up(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(_) => true,
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable {
                startup_catch_up, ..
            } => *startup_catch_up,
        }
    }
}

pub(super) struct ProductionProjectComposition {
    pub(super) key: ProjectServerKey,
    pub(super) canonical_project_path: PathBuf,
    pub(super) server: Arc<crate::mcp::McpServer>,
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
    crate::sessions::home_dir()
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
    if let Some(server) = {
        let mut servers = store_administration.project_servers().lock().await;
        servers
            .get_route_and_touch(&route)
            .map(|(key, server)| (key.clone(), Arc::clone(server)))
    } {
        return Ok(ProductionProjectComposition {
            key: server.0,
            canonical_project_path: canonical_project_path.to_path_buf(),
            server: server.1,
            inserted: false,
            #[cfg(any(test, feature = "test-transport"))]
            semantic_auto_download_enabled: None,
        });
    }

    let gate = project_open_gate(project_open_gates, &route).await;
    let _singleflight = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(project_open_cancellation_error()),
        singleflight = gate.lock() => singleflight,
    };
    if let Some(server) = {
        let mut servers = store_administration.project_servers().lock().await;
        servers
            .get_route_and_touch(&route)
            .map(|(key, server)| (key.clone(), Arc::clone(server)))
    } {
        return Ok(ProductionProjectComposition {
            key: server.0,
            canonical_project_path: canonical_project_path.to_path_buf(),
            server: server.1,
            inserted: false,
            #[cfg(any(test, feature = "test-transport"))]
            semantic_auto_download_enabled: None,
        });
    }

    #[cfg(test)]
    if let Some(attempts) = project_open_attempts {
        attempts.fetch_add(1, Ordering::Relaxed);
    }
    let (initial_cg, initial_deferred_post_open_health) =
        Box::pin(open_project_for_handshake_with_health_mode(
            canonical_project_path,
            handshake,
            store_administration,
            true,
        ))
        .await?;
    let initial_key = ProjectServerKey::from_open_project(&initial_cg, handshake)?;
    let synchronous_post_open_health = store_administration
        .project_servers()
        .lock()
        .await
        .requires_synchronous_health(&initial_key.owner);
    let (cg, deferred_post_open_health, key) = if synchronous_post_open_health {
        drop(initial_deferred_post_open_health);
        initial_cg.close();
        let (validated_cg, validated_deferred_post_open_health) =
            Box::pin(open_project_for_handshake_with_health_mode(
                canonical_project_path,
                handshake,
                store_administration,
                false,
            ))
            .await?;
        let validated_key = ProjectServerKey::from_open_project(&validated_cg, handshake)?;
        if validated_key.owner == initial_key.owner {
            store_administration
                .project_servers()
                .lock()
                .await
                .clear_synchronous_health(&validated_key.owner);
        }
        (
            validated_cg,
            validated_deferred_post_open_health,
            validated_key,
        )
    } else {
        (initial_cg, initial_deferred_post_open_health, initial_key)
    };
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
    let semantic_config = &runtime_configuration.config.semantic;
    let semantic_resources = &semantic_config.resources;
    let semantic_runtime = crate::semantic_code::DaemonSemanticRuntimeHandleV1::new(
        semantic_resources.max_concurrent_sessions as usize,
        usize::try_from(semantic_resources.max_resident_bytes / 4096)
            .unwrap_or(usize::MAX)
            .max(semantic_resources.max_batch_size as usize),
        semantic_resources.max_resident_bytes,
    )
    .map_err(|_| TraceDecayError::Config {
        message: "semantic runtime resource ceilings are invalid".to_owned(),
    })?;
    let semantic_auto_download_enabled =
        semantic_config.auto_download && runtime.semantic_auto_download();
    let semantic_startup_selection = semantic_config.selected_model.clone();
    let semantic_database = cg.dashboard_database_guard();
    let project_database_is_read_only = cg.db().filesystem_is_read_only();
    let semantic_lifecycle = crate::semantic_code::shared_lifecycle_owner();
    let existing = {
        let mut servers = store_administration.project_servers().lock().await;
        let existing = servers.get_ready(&key).cloned();
        if existing.is_some() {
            servers.bind_route(route.clone(), key.clone());
        }
        existing
    };
    if let Some(existing) = existing {
        return Ok(ProductionProjectComposition {
            key,
            canonical_project_path: canonical_project_path.to_path_buf(),
            server: existing,
            inserted: false,
            #[cfg(any(test, feature = "test-transport"))]
            semantic_auto_download_enabled: Some(semantic_auto_download_enabled),
        });
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
    let registry_db = Arc::clone(&registered_profile_db);
    let profile_identity = store_administration.profile_identity()?.clone();
    let accounting_db =
        crate::global_db::global_accounting_enabled().then(|| Arc::clone(&registered_profile_db));
    // Route after-edit hooks into the code-index scheduler queue on the
    // portable broker path too (mirrors the Unix `open_project_server`).
    let code_index_schedulers = invocation.code_index_schedulers.clone();
    let code_index_hook_sink: crate::mcp::server::CodeIndexHookSink =
        Arc::new(move |root: PathBuf, rel_paths: Vec<String>| {
            let schedulers = code_index_schedulers.clone();
            Box::pin(async move { schedulers.notify_hook_paths(&root, &rel_paths).await })
        });
    let code_index_publication_identity: crate::mcp::server::CodeIndexPublicationIdentityResolver =
        Arc::new(invocation.code_index_schedulers.clone());
    let code_search_project_id =
        tracedecay_domain::ProjectId::new(authoritative_project_id.clone()).map_err(|error| {
            TraceDecayError::Config {
                message: format!("project search identity is invalid: {error}"),
            }
        })?;
    let code_search_scope =
        project_open_owners::resolved_scope_for_project(cg.project_root(), &code_search_project_id)
            .map_err(|error| TraceDecayError::Config {
                message: format!("project search scope is invalid: {error:?}"),
            })?;
    let code_search_admission = query_mcp_admission::admit_query_mcp_read(
        Some(&profile_identity),
        &code_search_project_id,
        &code_search_scope,
        Arc::clone(&route_registered),
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("project search admission is unavailable: {error}"),
    })?;
    let code_search_authority = code_search_admission.search_authority();
    let read_admission_provider = query_mcp_admission::QueryMcpReadAdmissionProviderV1::new(
        profile_identity.clone(),
        code_search_project_id.clone(),
        Arc::clone(&route_registered),
    );
    // `load_settings` returns defaults as `Ok` when no settings file exists,
    // so an `Err` is an unreadable or unparsable file. Serving silent defaults
    // there would drop the user's `custom_adapters`; record the degradation on
    // the broker instead (same pattern as
    // `application::dashboard_diagnostics::open_diagnostic_broker`).
    let diagnostic_broker =
        match tracedecay_lsp::analyzer::settings::load_settings(&cg.store_layout().dashboard_root)
            .await
        {
            Ok(settings) => Arc::new(tokio::sync::Mutex::new(
                crate::application::dashboard_diagnostics::diagnostic_broker(
                    canonical_project_path.to_path_buf(),
                    settings,
                ),
            )),
            Err(error) => {
                tracing::warn!(
                    dashboard_root = %cg.store_layout().dashboard_root.display(),
                    error = %error,
                    "code diagnostics settings could not be loaded; serving defaults as degraded"
                );
                let mut broker = crate::application::dashboard_diagnostics::diagnostic_broker(
                    canonical_project_path.to_path_buf(),
                    tracedecay_lsp::analyzer::settings::CodeDiagnosticsSettings::default(),
                );
                broker.record_settings_unavailable(error.to_string());
                Arc::new(tokio::sync::Mutex::new(broker))
            }
        };
    let code_index_search_executor = code_index_search_executor(
        invocation.code_index_schedulers.clone(),
        code_search_project_id.clone(),
        read_admission_provider,
    );
    let dashboard_code_index_schedulers = invocation.code_index_schedulers.clone();
    let dashboard_code_index_freshness_reader:
        crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader =
        Arc::new(move |project_root| {
            let schedulers = dashboard_code_index_schedulers.clone();
            Box::pin(async move { schedulers.dashboard_freshness(&project_root).await })
        });
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
    let retained_graph_resolver = retained_project_graph_resolver(store_administration.clone());
    let mut core_context = crate::mcp::server::McpServerConstructionContext::daemon_owned_core(
        Arc::clone(&cg),
        handshake.scope_prefix.clone(),
        crate::mcp::server::McpServerDaemonCoreAuthority {
            profile_identity: profile_identity.clone(),
            transcript_source_home: transcript_source_home.clone(),
            accounting: accounting_db.clone(),
            registry: Arc::clone(&registry_db),
            database_owner_reconciler: Arc::clone(&database_owner_reconciler),
            project_routes: store_administration.project_routes(),
            writers: crate::mcp::server::McpServerWriters::daemon_owned(
                coordinated_dashboard_automation_writer(store_administration.clone()),
                coordinated_hook_branch_writer(store_administration.clone()),
                coordinated_background_refresh_writer(store_administration.clone()),
            ),
        },
    )
    .with_dashboard_code_index_freshness_reader(Arc::clone(&dashboard_code_index_freshness_reader))
    .with_dashboard_feedback_status_reader(Arc::clone(&dashboard_feedback_status_reader))
    .with_diagnostics_lsp(Arc::clone(&diagnostic_broker))
    .with_code_index_hook_sink(Arc::clone(&code_index_hook_sink))
    .with_code_index_publication_identity(Arc::clone(&code_index_publication_identity))
    .with_code_index_search_executor(Arc::clone(&code_index_search_executor))
    .with_code_index_search_authority(code_search_authority.clone())
    .with_project_server_live(Arc::clone(&route_registered))
    .with_application_invocation_executor(Arc::clone(&application_invocation_executor))
    .with_retained_project_graph_resolver(Arc::clone(&retained_graph_resolver));
    if let Some(reconciler) = automation_scheduler_reconciler.as_ref() {
        core_context = core_context.with_automation_scheduler_reconciler(Arc::clone(reconciler));
    }
    project_open_cancellation_checkpoint(cancellation)?;
    let mcp_construction_started = Instant::now();
    let core_candidate = crate::mcp::McpServer::new_with_context(core_context).await;
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
        core_candidate.cancel_startup_transcript_ingest();
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
            resolved.cancel_startup_transcript_ingest();
            return Err(project_open_cancellation_error());
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
        let quarantine_on_upgrade_failure = AtomicBool::new(false);
        let session_capabilities_published = AtomicBool::new(false);
        let mut published_full_candidate = None;
        let full_upgrade: Result<Arc<crate::mcp::McpServer>> = async {
            // The core is reachable from here on, so every step below leaves
            // this block with an error instead of returning behind a published
            // route: the funnel around it owns retiring the owner.
            if let Err(error) = settle_deferred_post_open_health(
                canonical_project_path,
                deferred_post_open_health
                    .as_ref()
                    .map(crate::db::Database::repair_fts_after_open),
            )
            .await
            {
                quarantine_on_upgrade_failure.store(true, Ordering::Release);
                return Err(error);
            }
            if *current_key.lock().await != key {
                return Err(TraceDecayError::Config {
                    message: "project changed branch during core capability admission".to_owned(),
                });
            }
            project_open_cancellation_checkpoint(cancellation)?;
            let project_sessions_started = Instant::now();
            let registered_project_session_db = store_administration
                .registered_project_session_database(cg.project_root(), cg.store_layout())
                .await?;
            crate::global_db::advance_required_session_temporal_state_repair(
                registered_project_session_db.as_ref(),
            )
            .await?;
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "project_sessions_admitted".to_owned()),
                    (
                        "elapsed_ms",
                        project_sessions_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            let profile_sessions_started = Instant::now();
            let registered_user_session_db = store_administration
                .registered_profile_session_database()
                .await?;
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "profile_sessions_admitted".to_owned()),
                    (
                        "elapsed_ms",
                        profile_sessions_started.elapsed().as_millis().to_string(),
                    ),
                ],
            );
            let session_db = Arc::clone(&registered_project_session_db);
            let user_session_db = Arc::clone(&registered_user_session_db);
            let host_admission_broker = store_administration
                .host_admission_broker(&session_db)
                .await?
                .broker()
                .cloned();
            let project_session_refresh_wake = store_administration
                .session_temporal_refresh_schedulers()
                .ensure_project(key.owner.clone(), Arc::clone(&session_db))
                .await;
            let user_session_refresh_wake = store_administration
                .session_temporal_refresh_schedulers()
                .ensure_profile(
                    user_session_db.db_path().to_path_buf(),
                    Arc::clone(&user_session_db),
                )
                .await;
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
                tracedecay_application::doctor::RemoteOperationalReadV1::Unconfigured,
                cg.get_config().sync.retention.clone(),
                invocation.code_index_schedulers.clone(),
                Arc::clone(&diagnostic_broker),
                invocation.feedback_runtime_registrar(),
            );
            let doctor_remediation_dispatcher =
                doctor_kernel::production_doctor_remediation_dispatcher(
                    doctor_kernel::ProductionDoctorRemediationOwnersV1 {
                        project_root: canonical_project_path.to_path_buf(),
                        project_id: code_search_project_id.clone(),
                        layout: cg.store_layout().clone(),
                        registry: Arc::clone(&registry_db),
                        profile_sessions: Arc::clone(&user_session_db),
                        project_sessions: Arc::clone(&session_db),
                        profile_root: profile_identity.profile_root().to_path_buf(),
                        config: cg.get_config().clone(),
                        global_retention: crate::user_config::UserConfig::load()
                            .automation
                            .retention,
                        store_administration: store_administration.clone(),
                        invocation: invocation.clone(),
                        code_index_store_root: code_index_store_root.clone(),
                        semantic_runtime: semantic_runtime.clone(),
                        semantic_database: Arc::clone(&semantic_database),
                        semantic_lifecycle: semantic_lifecycle.clone(),
                        semantic_resources: *semantic_resources,
                        route_registered: Arc::clone(&route_registered),
                    },
                    Arc::clone(&doctor_report_reader),
                );
            let mut full_context = crate::mcp::server::McpServerConstructionContext::daemon_owned(
                Arc::clone(&cg),
                handshake.scope_prefix.clone(),
                crate::mcp::server::McpServerDaemonAuthority {
                    profile_identity: profile_identity.clone(),
                    transcript_source_home,
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
                    database_owner_reconciler,
                    project_routes: store_administration.project_routes(),
                    writers: crate::mcp::server::McpServerWriters::daemon_owned(
                        coordinated_dashboard_automation_writer(store_administration.clone()),
                        coordinated_hook_branch_writer(store_administration.clone()),
                        coordinated_background_refresh_writer(store_administration.clone()),
                    ),
                },
            )
            .with_dashboard_doctor_report_reader(doctor_report_reader)
            .with_dashboard_doctor_remediation_dispatcher(doctor_remediation_dispatcher)
            .with_dashboard_code_index_freshness_reader(dashboard_code_index_freshness_reader)
            .with_dashboard_feedback_status_reader(dashboard_feedback_status_reader)
            .with_diagnostics_lsp(diagnostic_broker)
            .with_code_index_hook_sink(code_index_hook_sink)
            .with_code_index_publication_identity(code_index_publication_identity)
            .with_code_index_search_executor(code_index_search_executor)
            .with_code_index_search_authority(code_search_authority)
            .with_project_server_live(Arc::clone(&route_registered))
            .with_application_invocation_executor(application_invocation_executor)
            .with_startup_catch_up_enabled(runtime.startup_catch_up())
            .with_retained_project_graph_resolver(retained_graph_resolver);
            if let Some(reconciler) = automation_scheduler_reconciler {
                full_context = full_context.with_automation_scheduler_reconciler(reconciler);
            }
            project_open_cancellation_checkpoint(cancellation)?;
            let full_construction_started = Instant::now();
            let full_candidate = crate::mcp::McpServer::new_with_context(full_context).await;
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
                full_candidate.cancel_startup_transcript_ingest();
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
                full_candidate.cancel_startup_transcript_ingest();
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
                        canonical_project_path,
                        &project_id,
                        full_candidate.as_ref(),
                        source_edit_mutation_ready,
                    )
                    .await?;
                    log_full_setup_phase("independent_owners_registered");
                    Some(state)
                };
                let code_index_invocation = invocation.clone();
                let code_index_project_id = code_search_project_id.clone();
                let code_index_scope = code_search_scope.clone();
                let code_index_project = canonical_project_path.to_path_buf();
                let code_index_semantic_runtime = semantic_runtime.clone();
                let code_index_semantic_lifecycle = semantic_lifecycle.clone();
                let code_index_semantic_resources = *semantic_resources;
                let code_index_route_registered = Arc::clone(&route_registered);
                let code_index_cancellation = cancellation.clone();
                tokio::spawn(async move {
                    if code_index_cancellation.is_cancelled()
                        || !code_index_route_registered.load(Ordering::Acquire)
                    {
                        return;
                    }
                    let started = Instant::now();
                    let mut code_index_publications = code_index_invocation
                        .code_index_schedulers
                        .subscribe_generation_publications();
                    let outcome = code_index_invocation
                        .mount_code_index(
                            code_index_project_id,
                            &code_index_project,
                            code_index_store_root,
                            Some(&code_index_semantic_runtime),
                            Some(semantic_database),
                            code_index_semantic_lifecycle,
                            Some(code_index_semantic_resources),
                        )
                        .await;
                    let generation_ready = if outcome.is_ok() {
                        if code_index_invocation
                            .code_index_schedulers
                            .latest_generation_id(&code_index_project)
                            .await
                            .is_some()
                        {
                            true
                        } else {
                            loop {
                                if !code_index_route_registered.load(Ordering::Acquire) {
                                    break false;
                                }
                                tokio::select! {
                                    _ = code_index_cancellation.cancelled() => break false,
                                    publication = code_index_publications.recv() => match publication {
                                        Ok(publication)
                                            if publication.project_root == code_index_project =>
                                        {
                                            break true;
                                        }
                                        Ok(_) => {}
                                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                            if code_index_invocation
                                                .code_index_schedulers
                                                .latest_generation_id(&code_index_project)
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
                        }
                    } else {
                        false
                    };
                    let query_authority_outcome =
                        if generation_ready
                            && !code_index_cancellation.is_cancelled()
                            && code_index_route_registered.load(Ordering::Acquire)
                        {
                            Some(
                                code_index_invocation
                                    .mount_query_authority_for_project(
                                        &code_index_project,
                                        &code_index_scope,
                                    )
                                    .await,
                            )
                        } else {
                            None
                        };
                    let mut fields = vec![
                        ("project", code_index_project.display().to_string()),
                        ("elapsed_ms", started.elapsed().as_millis().to_string()),
                    ];
                    match outcome {
                        Ok(()) => fields.push(("phase", "code_index_mounted".to_owned())),
                        Err(error) => {
                            fields.push(("phase", "code_index_mount_degraded".to_owned()));
                            fields.push(("error", error.to_string()));
                        }
                    }
                    match query_authority_outcome {
                        Some(Ok(())) => {
                            fields.push(("query_authority", "mounted".to_owned()));
                        }
                        Some(Err(error)) => {
                            fields.push(("query_authority", "unavailable".to_owned()));
                            fields.push(("query_authority_error", error.to_string()));
                        }
                        None => {}
                    }
                    log_daemon_event("project_open_phase", &fields);
                });
                log_full_setup_phase("code_index_mount_scheduled");
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
            resolved.revoke_project_server_responses();
            resolved.cancel_startup_transcript_ingest();
            schedule_project_server_retirement(
                store_administration,
                vec![Arc::clone(&resolved)],
                None,
            )
            .await;
            full_candidate.publish_doctor_report();
            log_daemon_event(
                "project_open_phase",
                &[
                    ("project", canonical_project_path.display().to_string()),
                    ("phase", "full_published".to_owned()),
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
                let retain_core = !quarantine_on_upgrade_failure.load(Ordering::Acquire)
                    && !cancellation.is_cancelled()
                    && failed_key == key;
                let (core_retained, failed_full_server) = if retain_core {
                    let mut servers = store_administration.project_servers().lock().await;
                    match published_full_candidate.as_ref() {
                        Some(failed_full_server) => {
                            let displaced =
                                servers.swap_ready_if(&key, Arc::clone(&resolved), |current| {
                                    Arc::ptr_eq(current, failed_full_server)
                                });
                            (displaced.is_some(), displaced)
                        }
                        None => (
                            servers
                                .get_ready(&key)
                                .is_some_and(|current| Arc::ptr_eq(current, &resolved)),
                            None,
                        ),
                    }
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
                        failed_full_server.cancel_startup_transcript_ingest();
                        schedule_project_server_retirement(
                            store_administration,
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
                    let mut removed = {
                        let mut servers = store_administration.project_servers().lock().await;
                        if quarantine_on_upgrade_failure.load(Ordering::Acquire) {
                            servers.quarantine_and_remove_owner(&failed_key.owner)
                        } else {
                            servers.remove_owner(&failed_key.owner)
                        }
                    };
                    if session_capabilities_published.load(Ordering::Acquire)
                        && removed.iter().all(|server| !Arc::ptr_eq(server, &resolved))
                    {
                        removed.push(Arc::clone(&resolved));
                    }
                    for server in &removed {
                        server.revoke_project_server_responses();
                        server.cancel_startup_transcript_ingest();
                    }
                    debug_assert!(
                        !removed.is_empty(),
                        "failed core upgrade must retire its published owner"
                    );
                    // Request execution may itself need the owner writer held by
                    // this open attempt. The tracked retirement starts draining
                    // after this closure returns and releases that writer.
                    schedule_project_server_retirement(
                        store_administration,
                        removed,
                        Some(Arc::clone(&route_registered)),
                    )
                    .await;
                    return Err(error);
                }
            }
        }
    }
    Ok(ProductionProjectComposition {
        key,
        canonical_project_path: canonical_project_path.to_path_buf(),
        server: resolved,
        inserted,
        #[cfg(any(test, feature = "test-transport"))]
        semantic_auto_download_enabled: Some(semantic_auto_download_enabled),
    })
}
