//! Foreground daemon bootstrap: `run_foreground` entry points, the Unix
//! accept/serve loop, socket preparation, and client-task draining.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic,
//! signatures, or behavior changed. `use super::*` re-exposes every name the
//! parent `daemon` module had in scope so the moved code resolves unchanged.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::task::JoinSet;
#[cfg(unix)]
use tokio::time::Duration;
use tokio::time::timeout;

use crate::errors::{Result, TraceDecayError};

use super::*;

#[cfg(unix)]
pub async fn run_foreground(socket_path: PathBuf) -> Result<()> {
    run_foreground_unix(socket_path).await
}

#[cfg(not(unix))]
pub async fn run_foreground(_socket_path: PathBuf) -> Result<()> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
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
    install_remote_http_application_router(&http_application_registry, &store_administration)
        .await?;
    let http_application_service = http_application::DaemonHttpApplicationService::bind(
        http_application_registry.clone(),
        authority.auth_token(),
    )
    .await?;
    authority.publish_http_application_endpoint(http_application_service.endpoint())?;
    log_daemon_event(
        "daemon_http_application_listening",
        &[("endpoint", http_application_service.endpoint().to_string())],
    );
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
    semantic_artifact_gc.cancel();
    if let Err(error) = semantic_artifact_gc.shutdown().await {
        log_daemon_event(
            "semantic_artifact_gc",
            &[("outcome", "shutdown_failed".to_owned()), ("error", error)],
        );
    }
    maintenance.shutdown().await;
    if let Err(error) = http_application_service.shutdown().await {
        log_daemon_event(
            "daemon_http_application",
            &[
                ("outcome", "shutdown_failed".to_owned()),
                ("error", error.to_string()),
            ],
        );
    }
    shutdown_portable_project_open_tasks(project_open_gates.as_ref()).await;
    let in_flight_drained = timeout(DAEMON_CLIENT_DRAIN_DEADLINE, lifecycle.wait_for_idle())
        .await
        .is_ok();
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    // Client setup and in-flight requests may create schedulers, project
    // servers, or provider executions. Sweep owned background work only after
    // all client work drains, so nothing can admit a provider process after the
    // execution registry is emptied and leave it running past shutdown. The
    // deadline bounds a provider that refuses to stop.
    invocation.shutdown().await;
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    store_administration.shutdown_session_sync().await;
    store_administration.shutdown_host_admission_replay().await;
    if !in_flight_drained {
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
        return endpoint_cleanup;
    }
    shutdown_project_servers(&store_administration).await;
    endpoint_cleanup
}

#[cfg(unix)]
async fn run_foreground_unix(socket_path: PathBuf) -> Result<()> {
    let profile_root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
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
    )
    .await?;
    let http_application_service = http_application::DaemonHttpApplicationService::bind(
        http_application_registry.clone(),
        authority.auth_token(),
    )
    .await?;
    authority.publish_http_application_endpoint(http_application_service.endpoint())?;
    log_daemon_event(
        "daemon_http_application_listening",
        &[("endpoint", http_application_service.endpoint().to_string())],
    );
    let semantic_artifact_gc = spawn_semantic_artifact_gc_maintenance();
    let sync_config = crate::config::SyncConfig::default().with_env_overrides();
    let profile_database = engine
        .store_administration
        .registered_profile_database()
        .await?;
    let maintenance = maintenance::MaintenanceCoordinator::spawn(
        profile_root.clone(),
        Arc::clone(&profile_database),
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
        crate::global_db::global_db_path(),
        engine.store_administration.clone(),
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
    // Await each retained owner directly. An outer timeout would only drop
    // this coordinator and detach whichever owner was active at expiry.
    semantic_artifact_gc.cancel();
    if let Err(error) = semantic_artifact_gc.shutdown().await {
        log_daemon_event(
            "semantic_artifact_gc",
            &[("outcome", "shutdown_failed".to_owned()), ("error", error)],
        );
    }
    if let Err(error) = http_application_service.shutdown().await {
        log_daemon_event(
            "daemon_http_application",
            &[
                ("outcome", "shutdown_failed".to_owned()),
                ("error", error.to_string()),
            ],
        );
    }
    engine.shutdown_project_open_tasks().await;
    // Keep auxiliary process creation blocked until every scheduler and client
    // task is drained or abandoned. A killed app-server call may retry before
    // unwinding, so a shorter guard leaves a shutdown-time respawn race.
    let _codex_shutdown = crate::sessions::codex_app_server::begin_codex_app_server_shutdown();
    // Stop automation before announcing shutdown or waiting for clients.
    // Scheduler tasks may be inside a synchronous auxiliary-agent call, so
    // shutdown also terminates their tracked process trees before joining.
    let (automation_stopped, memory_repair_stopped) = tokio::join!(
        timeout(
            DAEMON_TASK_ABORT_DEADLINE,
            engine.shutdown_automation_schedulers(),
        ),
        timeout(
            DAEMON_TASK_ABORT_DEADLINE,
            engine.shutdown_memory_repair_schedulers(),
        )
    );
    let automation_stopped = automation_stopped.is_ok();
    let memory_repair_stopped = memory_repair_stopped.is_ok();
    if !automation_stopped || !memory_repair_stopped {
        log_daemon_event(
            "daemon_shutdown",
            &[("outcome", "scheduler_lock_timeout".to_string())],
        );
    }
    log_daemon_event(
        "daemon_shutdown",
        &[("socket", socket_path.display().to_string())],
    );
    let in_flight_drained = timeout(
        DAEMON_CLIENT_DRAIN_DEADLINE,
        engine.lifecycle.wait_for_idle(),
    )
    .await
    .is_ok();
    // Once admitted requests are finished (or their bound elapsed), every
    // remaining client task is an idle socket reader or already-cancelled
    // request wrapper. Abort those immediately instead of making shutdown wait
    // for clients to close persistent connections themselves.
    client_tasks.abort_all();
    let clients_drained = drain_client_tasks(&mut client_tasks, DAEMON_TASK_ABORT_DEADLINE).await;
    // Client setup and in-flight requests may create schedulers or project
    // servers. Sweep owned background tasks only after all client work drains.
    engine.shutdown_background_tasks().await;
    if !in_flight_drained || !clients_drained {
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
    // Each MCP server owns its bounded shutdown coordinator. Await the sweep
    // itself so no unvisited server is discarded by an aggregate timeout.
    engine.shutdown_servers().await;
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
pub(super) async fn drain_client_tasks(
    clients: &mut JoinSet<Result<()>>,
    deadline: Duration,
) -> bool {
    let drained = timeout(deadline, async {
        while let Some(completed) = clients.join_next().await {
            log_client_task_result(completed);
        }
    })
    .await
    .is_ok();
    if drained {
        return true;
    }

    clients.abort_all();
    let _ = timeout(DAEMON_TASK_ABORT_DEADLINE, async {
        while let Some(completed) = clients.join_next().await {
            log_client_task_result(completed);
        }
    })
    .await;
    false
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
        Err(_) => std::fs::remove_file(socket_path).map_err(|remove_err| TraceDecayError::Config {
            message: format!(
                "failed to remove stale daemon socket '{}': {remove_err}",
                socket_path.display()
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

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
