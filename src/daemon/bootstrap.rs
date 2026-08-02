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
    let (listener, endpoint) = BrokerListener::bind(authority.endpoint()).await?;
    authority.publish_endpoint(&endpoint)?;
    log_daemon_event("daemon_listening", &[("endpoint", endpoint.to_string())]);

    let store_administration =
        StoreAdministration::default().with_profile_identity(authority.profile_identity().clone());
    let http_application_registry = http_application::DaemonHttpApplicationRegistry::default();
    install_http_application_cold_resolver(
        &http_application_registry,
        store_administration.clone(),
    )?;
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
    let _semantic_artifact_gc = spawn_semantic_artifact_gc_maintenance();

    let lifecycle = DaemonLifecycle::default();
    let sync_config = crate::config::SyncConfig::default().with_env_overrides();
    let profile_database = store_administration.registered_profile_database().await?;
    let maintenance = maintenance::MaintenanceCoordinator::spawn(
        profile_root.clone(),
        profile_database,
        store_administration.clone(),
        sync_config.retention,
    )
    .await;
    let project_open_gates = Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default()));
    let invocation = DaemonInvocationState::default();
    invocation.configure_github_read_only_credentials(authority.profile_identity());
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
        clients.spawn(async move {
            let _permit = permit;
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
        });
    }
    lifecycle.begin_draining();
    maintenance.shutdown().await;
    cancel_project_server_startup_ingests(&store_administration).await;
    let _ = timeout(
        DAEMON_TASK_ABORT_DEADLINE,
        http_application_service.shutdown(),
    )
    .await;
    shutdown_portable_project_open_tasks(project_open_gates.as_ref()).await;
    cancel_project_server_startup_ingests(&store_administration).await;
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
    let _ = timeout(DAEMON_TASK_ABORT_DEADLINE, invocation.shutdown()).await;
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
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
    let http_application_registry = http_application::DaemonHttpApplicationRegistry::default();
    let engine = DaemonEngine::default()
        .with_profile_identity(authority.profile_identity().clone())
        .with_http_application_registry(http_application_registry.clone());
    install_http_application_cold_resolver(
        &http_application_registry,
        engine.store_administration.clone(),
    )?;
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
    let _semantic_artifact_gc = spawn_semantic_artifact_gc_maintenance();
    let sync_config = crate::config::SyncConfig::default().with_env_overrides();
    let profile_database = engine
        .store_administration
        .registered_profile_database()
        .await?;
    let maintenance = maintenance::MaintenanceCoordinator::spawn(
        profile_root.clone(),
        Arc::clone(&profile_database),
        engine.store_administration.clone(),
        sync_config.retention.clone(),
    )
    .await;
    // Install the git-metadata watcher (design D3/D5). The daemon has no single
    // project root, so it uses the default `[sync]` config plus env overrides.
    // When `auto_watch` is off the watcher is inert. The watcher shares the
    // engine's administration coordinator before it can spawn any writer.
    let git_watcher = git_watch::GitWatcher::new_with_administration(
        sync_config,
        engine.store_administration.clone(),
        maintenance.clone(),
    );
    if git_watcher.is_enabled() {
        git_watcher.spawn(profile_database).await;
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
        client_tasks.spawn(async move {
            let _permit = permit;
            client.await
        });
    }
    engine.lifecycle.begin_draining();
    // Stop accepting and unlink the socket before draining so clients that
    // connect during shutdown get NotFound/ConnectionRefused (which they retry
    // via `connect_with_restart_grace`) instead of a queued connection that
    // will never be served.
    drop(listener);
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    let shutdown_completed = timeout(DAEMON_SHUTDOWN_DEADLINE, async {
        cancel_project_server_startup_ingests(&engine.store_administration).await;
        let _ = timeout(
            DAEMON_TASK_ABORT_DEADLINE,
            http_application_service.shutdown(),
        )
        .await;
        engine.shutdown_project_open_tasks().await;
        cancel_project_server_startup_ingests(&engine.store_administration).await;
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
        let clients_drained =
            drain_client_tasks(&mut client_tasks, DAEMON_TASK_ABORT_DEADLINE).await;
        // Client setup and in-flight requests may create schedulers or project
        // servers. Sweep owned background tasks only after all client work drains.
        let background_drained = timeout(
            DAEMON_TASK_ABORT_DEADLINE,
            engine.shutdown_background_tasks(),
        )
        .await
        .is_ok();
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
        if !background_drained {
            log_daemon_event(
                "daemon_shutdown",
                &[("outcome", "background_task_timeout".to_string())],
            );
        }
        // Graceful shutdown persists tokens-saved counters and checkpoints WALs
        // for every live project server sequentially; with many servers or large
        // WALs that can exceed systemd's stop timeout, which then sends `SIGKILL`
        // to the daemon. On timeout the shutdown future is dropped and we proceed
        // to exit: the remaining persistence is best-effort and the database WAL
        // keeps state crash-safe.
        let completed = timeout(DAEMON_SERVER_SHUTDOWN_DEADLINE, engine.shutdown_servers())
            .await
            .is_ok();
        if !completed {
            log_daemon_event(
                "daemon_shutdown",
                &[
                    ("outcome", "timeout".to_string()),
                    (
                        "deadline_secs",
                        DAEMON_SERVER_SHUTDOWN_DEADLINE.as_secs().to_string(),
                    ),
                ],
            );
        }
    })
    .await
    .is_ok();
    if !shutdown_completed {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "hard_backstop_timeout".to_string()),
                (
                    "deadline_secs",
                    DAEMON_SHUTDOWN_DEADLINE.as_secs().to_string(),
                ),
            ],
        );
    }
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
