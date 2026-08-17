//! Foreground daemon bootstrap: `run_foreground` entry points, the Unix
//! accept/serve loop, socket preparation, and client-task draining.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic,
//! signatures, or behavior changed. `use super::*` re-exposes every name the
//! parent `daemon` module had in scope so the moved code resolves unchanged.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::task::JoinSet;

use crate::errors::{Result, TraceDecayError};

use super::*;

/// Slice of the shutdown budget reserved for writing the terminal shutdown
/// receipts to the daemon log after the coordinator returns.
pub(super) const DAEMON_SHUTDOWN_RECEIPT_LOG_RESERVE: tokio::time::Duration =
    tokio::time::Duration::from_millis(100);

/// Explicit network boundary for serving the canonical enrolled Remote Brain
/// protocol over TLS. Local daemon application traffic keeps its independent
/// loopback-only HTTP listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteBrainTlsConfig {
    listen: std::net::SocketAddr,
    certificate_chain: PathBuf,
    private_key: PathBuf,
}

impl RemoteBrainTlsConfig {
    pub fn from_optional_parts(
        listen: Option<std::net::SocketAddr>,
        certificate_chain: Option<PathBuf>,
        private_key: Option<PathBuf>,
    ) -> Result<Option<Self>> {
        match (listen, certificate_chain, private_key) {
            (None, None, None) => Ok(None),
            (Some(listen), Some(certificate_chain), Some(private_key)) => {
                if listen.ip().is_unspecified() {
                    return Err(TraceDecayError::Config {
                        message: "Remote Brain TLS listener requires an explicit interface address; wildcard addresses are refused".to_owned(),
                    });
                }
                if certificate_chain.as_os_str().is_empty() || private_key.as_os_str().is_empty() {
                    return Err(TraceDecayError::Config {
                        message: "Remote Brain TLS certificate and private-key paths must be non-empty".to_owned(),
                    });
                }
                Ok(Some(Self {
                    listen,
                    certificate_chain,
                    private_key,
                }))
            }
            _ => Err(TraceDecayError::Config {
                message: "Remote Brain TLS listener requires --remote-listen, --remote-tls-cert, and --remote-tls-key together".to_owned(),
            }),
        }
    }

    pub(super) fn listen(&self) -> std::net::SocketAddr {
        self.listen
    }

    pub(super) fn certificate_chain(&self) -> &Path {
        &self.certificate_chain
    }

    pub(super) fn private_key(&self) -> &Path {
        &self.private_key
    }
}

fn prewarm_static_daemon_bootstrap_catalog() {
    if let Err(error) = prewarm_daemon_bootstrap_catalog() {
        tracing::warn!(
            %error,
            "static MCP bootstrap catalog prewarm failed; tools/list will return a typed error"
        );
    }
}

#[cfg(unix)]
pub async fn run_foreground(
    socket_path: PathBuf,
    remote_tls: Option<RemoteBrainTlsConfig>,
) -> Result<()> {
    run_foreground_unix(socket_path, remote_tls).await
}

#[cfg(not(unix))]
pub async fn run_foreground(
    _socket_path: PathBuf,
    remote_tls: Option<RemoteBrainTlsConfig>,
) -> Result<()> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    prewarm_static_daemon_bootstrap_catalog();
    let requested = transport::default_loopback_endpoint();
    let _lifecycle_lease = crate::lifecycle_lease::acquire_shared_for_profile(
        &profile_root,
        "managed daemon database ownership",
    )?;
    let mut authority =
        authority::DaemonAuthority::acquire(&profile_root, &requested, binary_version())?;
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        authority.record().epoch,
        &authority.record().process_run_id,
    )?;
    let store_administration =
        StoreAdministration::default().with_profile_identity(authority.profile_identity().clone());
    let project_open_gates = Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default()));
    let invocation = DaemonInvocationState::default();
    invocation.configure_github_read_only_credentials(authority.profile_identity());
    store_administration.install_remote_recovery_project_lifecycle(
        invocation.clone(),
        Arc::clone(&project_open_gates),
    )?;
    let deletion_owners = remote_deletion::RemoteDeletionRuntimeOwners {
        administration: store_administration.clone(),
        invocation: invocation.clone(),
        project_open_gates: Arc::clone(&project_open_gates),
    };
    if let remote_deletion::RemoteDeletionBootMode::DeletionOnly(receipt) =
        remote_deletion::resume_remote_account_deletion_for_boot(&deletion_owners).await?
    {
        log_daemon_event(
            "remote_account_deletion_resume",
            &[("outcome", format!("{:?}", receipt.status))],
        );
        return Ok(());
    }
    let (listener, endpoint) = BrokerListener::bind(authority.endpoint()).await?;
    authority.publish_endpoint(&endpoint)?;
    log_daemon_event("daemon_listening", &[("endpoint", endpoint.to_string())]);

    let http_application_registry = http_application::DaemonHttpApplicationRegistry::default();
    install_http_application_cold_resolver(
        &http_application_registry,
        store_administration.clone(),
        invocation.clone(),
        Arc::clone(&project_open_gates),
    )?;
    install_remote_http_application_router(
        &http_application_registry,
        &store_administration,
        &invocation,
    )
    .await?;
    let http_application_service =
        http_application::DaemonHttpApplicationService::bind_with_remote_tls(
            http_application_registry.clone(),
            authority.auth_token(),
            remote_tls.as_ref(),
        )
        .await?;
    authority.publish_http_application_endpoint(http_application_service.endpoint())?;
    if let Some(endpoint) = http_application_service.remote_tls_endpoint() {
        authority.publish_remote_brain_tls_endpoint(endpoint)?;
    }
    log_daemon_event(
        "daemon_http_application_listening",
        &[("endpoint", http_application_service.endpoint().to_string())],
    );
    if let Some(endpoint) = http_application_service.remote_tls_endpoint() {
        log_daemon_event(
            "daemon_remote_brain_tls_listening",
            &[("endpoint", format!("https://{endpoint}/remote/"))],
        );
    }
    let semantic_artifact_gc = spawn_semantic_artifact_gc_maintenance();

    let lifecycle = DaemonLifecycle::default();
    let sync_config = crate::config::SyncConfig::default().with_env_overrides();
    let profile_database = store_administration.registered_profile_database().await?;
    let maintenance = maintenance::MaintenanceCoordinator::spawn(
        profile_root.clone(),
        profile_database,
        store_administration.clone(),
        invocation.code_index_schedulers.clone(),
        sync_config.retention.clone(),
        maintenance::BranchStoreGcCadenceV1 {
            branch_gc_days: sync_config.branch_gc_days,
            orphan_db_gc_days: sync_config.orphan_db_gc_days,
        },
    )
    .await;
    let admission = DaemonClientAdmission::new(MAX_CONCURRENT_DAEMON_CLIENTS);
    let per_client_admission = DaemonPerClientAdmission::default();
    let mut clients: JoinSet<Result<()>> = JoinSet::new();
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?,
            completed = clients.join_next(), if !clients.is_empty() => {
                if let Some(Err(error)) = completed {
                    log_daemon_event("daemon_client", &[("outcome", error.to_string())]);
                }
                continue;
            },
            () = lifecycle.wait_for_draining() => break,
            _ = tokio::signal::ctrl_c() => break,
        };
        let permit = match admission.try_admit() {
            DaemonClientAdmissionOutcome::Admitted(permit) => permit,
            DaemonClientAdmissionOutcome::Saturated(response) => {
                reject_saturated_daemon_client(stream, response).await;
                continue;
            }
        };
        let admission_class = permit.class();
        let auth_token = authority.auth_token().to_string();
        let client_lifecycle = lifecycle.clone();
        let store_administration = store_administration.clone();
        let project_open_gates = Arc::clone(&project_open_gates);
        let invocation = invocation.clone();
        let http_application_registry = http_application_registry.clone();
        let per_client_admission = per_client_admission.clone();
        clients.spawn(with_connection_admission(permit, async move {
            Box::pin(serve_windows_broker_client_with_class_and_invocation(
                stream,
                &auth_token,
                &client_lifecycle,
                store_administration,
                project_open_gates,
                invocation,
                http_application_registry,
                per_client_admission,
                admission_class,
                #[cfg(test)]
                None,
            ))
            .await
        }));
    }
    lifecycle.begin_draining();
    drop(listener);
    cancel_retained_session_history(&store_administration).await;
    let shutdown_deadline = tokio::time::Instant::now() + DAEMON_SHUTDOWN_DEADLINE
        - DAEMON_SHUTDOWN_RECEIPT_LOG_RESERVE;
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    let semantic_artifact_gc_cancel = semantic_artifact_gc.clone();
    let semantic_artifact_gc_join = semantic_artifact_gc;
    let maintenance_join = maintenance.clone();
    let project_open = project_open_tasks(project_open_gates.as_ref()).await;
    let session_refresh = Arc::clone(store_administration.session_temporal_refresh_schedulers());
    let replay_join = store_administration.clone();
    let session_sync_join = store_administration.clone();
    let memory_graph_reconciliation_join = store_administration.clone();
    let invocation_join = invocation.clone();
    let git_transactions_join = Arc::clone(store_administration.git_index_transaction_services());
    let native_integration_join = Arc::clone(store_administration.native_integration_services());
    let owner_phases = vec![
        vec![
            shutdown_coordination::ShutdownOwner::with_deadline_result(
                "semantic_artifact_gc",
                move || semantic_artifact_gc_cancel.cancel(),
                move |_| async move { semantic_artifact_gc_join.shutdown().await },
            ),
            shutdown_coordination::ShutdownOwner::new("maintenance", || {}, async move {
                maintenance_join.shutdown().await;
            }),
            shutdown_coordination::ShutdownOwner::with_deadline_result(
                "http_application",
                || {},
                move |_| async move { http_application_service.shutdown().await },
            ),
            hosted_dashboard_shutdown_owner(),
            shutdown_coordination::ShutdownOwner::with_deadline_status(
                "project_open",
                || {},
                move |_| async move {
                    if project_open.shutdown().await {
                        ShutdownStatus::Clean
                    } else {
                        ShutdownStatus::TimedOut
                    }
                },
            ),
            shutdown_coordination::ShutdownOwner::new(
                "session_temporal_refresh",
                || {},
                async move {
                    session_refresh.shutdown().await;
                },
            ),
            shutdown_coordination::ShutdownOwner::new("host_admission_replay", || {}, async move {
                replay_join.shutdown_host_admission_replay().await;
            }),
        ],
        // Client setup and in-flight requests may create schedulers, project
        // servers, or provider executions. Sweep the invocation registry only
        // after the producer owners settle, so nothing can admit a provider
        // process after the execution registry is emptied and leave it
        // running past shutdown.
        vec![shutdown_coordination::ShutdownOwner::new(
            "invocation",
            || {},
            async move {
                invocation_join.shutdown().await;
            },
        )],
        vec![shutdown_coordination::ShutdownOwner::new(
            "session_sync",
            || {},
            async move {
                session_sync_join.shutdown_session_sync().await;
            },
        )],
        vec![
            shutdown_coordination::ShutdownOwner::with_deadline_result(
                "git_index_transactions",
                || {},
                move |_| async move {
                    git_transactions_join
                        .shutdown()
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("{error:?}"))
                },
            ),
            shutdown_coordination::ShutdownOwner::with_deadline_result(
                "native_integration_transactions",
                || {},
                move |_| async move {
                    native_integration_join
                        .shutdown()
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("{error:?}"))
                },
            ),
        ],
    ];
    let memory_graph_reconciliation = shutdown_coordination::ShutdownOwner::with_deadline_result(
        "memory_graph_reconciliation",
        || {},
        move |_| async move {
            let owner = memory_graph_reconciliation_join
                .prepare_memory_graph_reconciliation_shutdown()
                .await
                .map_err(|error| error.to_string())?;
            owner.cancel();
            memory_graph_reconciliation_join
                .close_session_relation_graphs()
                .await
                .map_err(|error| error.to_string())?;
            owner.shutdown().await
        },
    );
    let server_store_administration = store_administration.clone();
    let shutdown = shutdown_orchestration::coordinate_daemon_shutdown(
        &lifecycle,
        shutdown_deadline,
        async move {
            shutdown_orchestration::DaemonShutdownPlan::new(clients, owner_phases, async move {
                shutdown_project_servers(shutdown_deadline, &server_store_administration).await
            })
            .with_terminal_owner_phases(vec![vec![memory_graph_reconciliation]])
        },
    )
    .await;
    log_client_drain_shutdown_receipt(&shutdown);
    log_background_shutdown_receipt(&shutdown.background);
    log_project_server_shutdown_receipt(&shutdown.project_servers);
    endpoint_cleanup
}

fn log_client_drain_shutdown_receipt(receipt: &shutdown_orchestration::DaemonShutdownReceipt) {
    if receipt.in_flight.is_clean() && receipt.clients.is_clean() {
        return;
    }
    log_daemon_event(
        "daemon_shutdown",
        &[
            ("outcome", "client_drain_timeout".to_string()),
            (
                "deadline_secs",
                DAEMON_CLIENT_DRAIN_DEADLINE.as_secs().to_string(),
            ),
            (
                "checkpoint",
                "skipped_active_clients_were_aborted".to_string(),
            ),
        ],
    );
}

fn log_background_shutdown_receipt(receipt: &shutdown_coordination::ShutdownReceipt) {
    for owner in receipt.unfinished() {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "background_task_unfinished".to_string()),
                ("owner", (*owner).to_string()),
            ],
        );
    }
}

fn log_project_server_shutdown_receipt(receipt: &store_shutdown::ShutdownTaskReceipt) {
    if receipt.is_clean() {
        return;
    }
    log_daemon_event(
        "daemon_shutdown",
        &[
            ("outcome", "project_server_shutdown_incomplete".to_string()),
            ("failed", receipt.failed_count().to_string()),
            ("timed_out", receipt.timed_out_count().to_string()),
        ],
    );
    for outcome in &receipt.outcomes {
        let status = match outcome.status {
            store_shutdown::ShutdownTaskStatus::Clean => continue,
            store_shutdown::ShutdownTaskStatus::Failed(_) => "failed",
            store_shutdown::ShutdownTaskStatus::TimedOut => "timed_out",
        };
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "project_server_task_unfinished".to_string()),
                ("owner", outcome.owner.clone()),
                ("status", status.to_string()),
            ],
        );
    }
}

fn hosted_dashboard_shutdown_owner() -> shutdown_coordination::ShutdownOwner {
    shutdown_coordination::ShutdownOwner::with_deadline_result(
        "hosted_dashboard",
        || {},
        move |_| async move {
            crate::mcp::tools::handlers::dashboard::shutdown_dashboard()
                .await
                .map_err(|error| error.to_string())
        },
    )
}

#[cfg(unix)]
async fn run_foreground_unix(
    socket_path: PathBuf,
    remote_tls: Option<RemoteBrainTlsConfig>,
) -> Result<()> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    prewarm_static_daemon_bootstrap_catalog();
    let endpoint = transport::DaemonEndpoint::Unix(socket_path);
    let _lifecycle = crate::lifecycle_lease::acquire_shared_for_profile(
        &profile_root,
        "managed daemon database ownership",
    )?;
    let mut authority =
        authority::DaemonAuthority::acquire(&profile_root, &endpoint, binary_version())?;
    let _database_scope = crate::db::enter_daemon_database_scope(
        &profile_root,
        authority.record().epoch,
        &authority.record().process_run_id,
    )?;
    let http_application_registry = http_application::DaemonHttpApplicationRegistry::default();
    let engine = DaemonEngine::default()
        .with_profile_identity(authority.profile_identity().clone())
        .with_http_application_registry(http_application_registry.clone());
    engine
        .store_administration
        .install_remote_recovery_project_lifecycle(
            engine.invocation.clone(),
            Arc::clone(&engine.project_open_gates),
        )?;
    let deletion_owners = remote_deletion::RemoteDeletionRuntimeOwners {
        administration: engine.store_administration.clone(),
        invocation: engine.invocation.clone(),
        project_open_gates: Arc::clone(&engine.project_open_gates),
    };
    if let remote_deletion::RemoteDeletionBootMode::DeletionOnly(receipt) =
        remote_deletion::resume_remote_account_deletion_for_boot(&deletion_owners).await?
    {
        log_daemon_event(
            "remote_account_deletion_resume",
            &[("outcome", format!("{:?}", receipt.status))],
        );
        return Ok(());
    }
    let socket_path = match authority.endpoint() {
        transport::DaemonEndpoint::Unix(path) => path.clone(),
        transport::DaemonEndpoint::Loopback(_) => {
            return Err(TraceDecayError::Config {
                message: "Unix daemon requires a Unix socket endpoint".to_string(),
            });
        }
    };
    if let Some(parent) = socket_path.parent() {
        let parent_existed = parent.exists();
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to create socket directory '{}': {e}",
                parent.display()
            ),
        })?;
        if !parent_existed {
            set_owner_only_permissions(parent, 0o700)?;
        }
    }
    prepare_socket_path(&authority).await?;

    let (listener, bound_endpoint) = BrokerListener::bind(authority.endpoint()).await?;
    authority.publish_endpoint(&bound_endpoint)?;
    set_owner_only_permissions(&socket_path, 0o600)?;
    log_daemon_event(
        "daemon_listening",
        &[("endpoint", bound_endpoint.to_string())],
    );
    install_http_application_cold_resolver(
        &http_application_registry,
        engine.store_administration.clone(),
        engine.invocation.clone(),
        Arc::clone(&engine.project_open_gates),
    )?;
    install_remote_http_application_router(
        &http_application_registry,
        &engine.store_administration,
        &engine.invocation,
    )
    .await?;
    let http_application_service =
        http_application::DaemonHttpApplicationService::bind_with_remote_tls(
            http_application_registry.clone(),
            authority.auth_token(),
            remote_tls.as_ref(),
        )
        .await?;
    authority.publish_http_application_endpoint(http_application_service.endpoint())?;
    if let Some(endpoint) = http_application_service.remote_tls_endpoint() {
        authority.publish_remote_brain_tls_endpoint(endpoint)?;
    }
    log_daemon_event(
        "daemon_http_application_listening",
        &[("endpoint", http_application_service.endpoint().to_string())],
    );
    if let Some(endpoint) = http_application_service.remote_tls_endpoint() {
        log_daemon_event(
            "daemon_remote_brain_tls_listening",
            &[("endpoint", format!("https://{endpoint}/remote/"))],
        );
    }
    let semantic_artifact_gc = spawn_semantic_artifact_gc_maintenance();
    let sync_config = crate::config::SyncConfig::default().with_env_overrides();
    let profile_database = engine
        .store_administration
        .registered_profile_database()
        .await?;
    let maintenance = maintenance::MaintenanceCoordinator::spawn(
        profile_root.clone(),
        profile_database.clone(),
        engine.store_administration.clone(),
        engine.invocation.code_index_schedulers.clone(),
        sync_config.retention.clone(),
        maintenance::BranchStoreGcCadenceV1 {
            branch_gc_days: sync_config.branch_gc_days,
            orphan_db_gc_days: sync_config.orphan_db_gc_days,
        },
    )
    .await;
    // Install the daemon-wide git-metadata owner. Individual projects provide
    // every watcher setting from the pinned configuration already held by
    // their retained server; bootstrap never supplies activation authority.
    let git_watcher = git_watch::GitWatcher::new_with_canonical_scheduler(
        maintenance.clone(),
        engine.invocation.code_index_schedulers.clone(),
    );
    if matches!(
        git_watcher.spawn().await,
        git_watch::GitWatcherStart::ShuttingDown
    ) {
        log_daemon_event(
            "git_watch_start_rejected",
            &[("reason", "shutting_down".to_string())],
        );
    }
    // PR-branch auto-tracking runs independently of the metadata watcher: it is
    // gated per-project on `sync.auto_track_pr_branches` (default off), so this
    // loop is inert unless a project opts in.
    let pr_autotrack_task = pr_autotrack::spawn_with_administration(
        engine.store_administration.clone(),
        engine.invocation.code_index_schedulers.clone(),
    );
    let engine = engine
        .with_git_watcher(git_watcher)
        .with_maintenance_coordinator(maintenance)
        .with_pr_autotrack_task(pr_autotrack_task)
        .await;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let admission = DaemonClientAdmission::new(MAX_CONCURRENT_DAEMON_CLIENTS);
    let mut client_tasks: JoinSet<Result<()>> = JoinSet::new();

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?,
            completed = client_tasks.join_next(), if !client_tasks.is_empty() => {
                if let Some(completed) = completed {
                    log_client_task_result(completed);
                }
                continue;
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        };
        let permit = match admission.try_admit() {
            DaemonClientAdmissionOutcome::Admitted(permit) => permit,
            DaemonClientAdmissionOutcome::Saturated(response) => {
                reject_saturated_daemon_client(stream, response).await;
                continue;
            }
        };
        let admission_class = permit.class();
        let engine = engine.clone();
        let auth_token = authority.auth_token().to_string();
        let client: std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<()>> + Send + 'static>,
        > = Box::pin(serve_authenticated_socket_client_with_class(
            stream,
            engine,
            auth_token,
            admission_class,
        ));
        client_tasks.spawn(with_connection_admission(permit, client));
    }
    engine.lifecycle.begin_draining();
    // Stop accepting and unlink the socket before draining so clients that
    // connect during shutdown get NotFound/ConnectionRefused (which they retry
    // via `connect_with_restart_grace`) instead of a queued connection that
    // will never be served.
    drop(listener);
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    cancel_retained_session_history(&engine.store_administration).await;
    let shutdown_deadline = tokio::time::Instant::now() + DAEMON_SHUTDOWN_DEADLINE
        - DAEMON_SHUTDOWN_RECEIPT_LOG_RESERVE;
    // Keep auxiliary process creation blocked until every scheduler and client
    // task is drained or abandoned. A killed app-server call may retry before
    // unwinding, so a shorter guard leaves a shutdown-time respawn race. The
    // coordinator owns every spawned shutdown task and applies one deadline to
    // each of them; awaiting its receipt keeps this fence active until those
    // owners have either joined or reported a typed timeout.
    let _codex_shutdown =
        tracedecay_sessions::runtime::codex_app_server::begin_codex_app_server_shutdown();
    log_daemon_event(
        "daemon_shutdown",
        &[("socket", socket_path.display().to_string())],
    );
    let shutdown_lifecycle = engine.lifecycle.clone();
    let shutdown_engine = engine.clone();
    let semantic_artifact_gc_cancel = semantic_artifact_gc.clone();
    let semantic_artifact_gc_join = semantic_artifact_gc;
    let shutdown = shutdown_orchestration::coordinate_daemon_shutdown(
        &shutdown_lifecycle,
        shutdown_deadline,
        async move {
            let mut owner_phases = shutdown_engine.shutdown_owner_phases().await;
            let memory_graph_reconciliation =
                shutdown_engine.memory_graph_reconciliation_shutdown_owner();
            let semantic_artifact_gc_owner =
                shutdown_coordination::ShutdownOwner::with_deadline_result(
                    "semantic_artifact_gc",
                    move || semantic_artifact_gc_cancel.cancel(),
                    move |_| async move { semantic_artifact_gc_join.shutdown().await },
                );
            let http_application_owner = shutdown_coordination::ShutdownOwner::with_deadline_result(
                "http_application",
                || {},
                move |_| async move { http_application_service.shutdown().await },
            );
            let hosted_dashboard_owner = hosted_dashboard_shutdown_owner();
            match owner_phases.first_mut() {
                Some(producers) => {
                    producers.push(semantic_artifact_gc_owner);
                    producers.push(http_application_owner);
                    producers.push(hosted_dashboard_owner);
                }
                None => owner_phases.push(vec![
                    semantic_artifact_gc_owner,
                    http_application_owner,
                    hosted_dashboard_owner,
                ]),
            }
            let server_engine = shutdown_engine.clone();
            shutdown_orchestration::DaemonShutdownPlan::new(
                client_tasks,
                owner_phases,
                async move { server_engine.shutdown_servers(shutdown_deadline).await },
            )
            .with_terminal_owner_phases(vec![vec![memory_graph_reconciliation]])
        },
    )
    .await;
    log_client_drain_shutdown_receipt(&shutdown);
    log_background_shutdown_receipt(&shutdown.background);
    log_project_server_shutdown_receipt(&shutdown.project_servers);
    endpoint_cleanup
}

#[cfg(unix)]
fn log_client_task_result(completed: std::result::Result<Result<()>, tokio::task::JoinError>) {
    let error = match completed {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error.to_string(),
        Err(error) if error.is_cancelled() => return,
        Err(error) => error.to_string(),
    };
    log_daemon_event(
        "daemon_client",
        &[("outcome", "error".to_string()), ("error", error)],
    );
}

#[cfg(unix)]
pub(super) fn set_owner_only_permissions(path: &Path, mode: u32) -> Result<()> {
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to restrict permissions on '{}': {e}",
            path.display()
        ),
    })
}

#[cfg(unix)]
fn remove_stale_socket(socket_path: &Path) -> Result<()> {
    match std::fs::remove_file(socket_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TraceDecayError::Config {
            message: format!(
                "failed to remove stale daemon socket '{}': {error}",
                socket_path.display()
            ),
        }),
    }
}

#[cfg(unix)]
async fn prepare_socket_path(authority: &authority::DaemonAuthority) -> Result<()> {
    authority.ensure_current()?;
    let socket_path = match authority.endpoint() {
        transport::DaemonEndpoint::Unix(path) => path,
        transport::DaemonEndpoint::Loopback(_) => {
            return Err(TraceDecayError::Config {
                message: "Unix daemon requires a Unix socket endpoint".to_string(),
            });
        }
    };
    transport::ensure_private_socket_parent(socket_path)?;
    match UnixStream::connect(socket_path).await {
        Ok(_) => Err(TraceDecayError::Config {
            message: format!(
                "daemon socket '{}' is already in use",
                socket_path.display()
            ),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => remove_stale_socket(socket_path),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn an_already_absent_stale_socket_is_prepared() {
        let root = tempfile::tempdir().expect("temporary fixture root");
        let socket = root.path().join("daemon.sock");

        remove_stale_socket(&socket).expect("a concurrently removed stale socket is already safe");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn insecure_socket_parent_rejection_preserves_stale_socket() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("temporary fixture root");
        let profile_root = root.path().join("profile");
        std::fs::create_dir_all(&profile_root).expect("profile root");
        std::fs::set_permissions(&profile_root, std::fs::Permissions::from_mode(0o700))
            .expect("private profile root");
        let socket_parent = root.path().join("public");
        std::fs::create_dir_all(&socket_parent).expect("socket parent");
        std::fs::set_permissions(&socket_parent, std::fs::Permissions::from_mode(0o755))
            .expect("public socket parent");
        let socket = socket_parent.join("daemon.sock");
        drop(std::os::unix::net::UnixListener::bind(&socket).expect("stale socket"));

        let endpoint = transport::DaemonEndpoint::Unix(socket.clone());
        let authority = authority::DaemonAuthority::acquire(&profile_root, &endpoint, "test")
            .expect("daemon authority");
        let error = prepare_socket_path(&authority)
            .await
            .expect_err("public socket parent must be rejected before stale cleanup");

        assert!(matches!(error, TraceDecayError::Config { .. }), "{error}");
        assert!(error.to_string().contains("private directory"), "{error}");
        assert!(socket.exists(), "rejection must preserve the stale socket");
    }
}
