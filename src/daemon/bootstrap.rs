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
use tokio::time::Duration;

use crate::errors::{Result, TraceDecayError};

use super::*;

pub(super) const DAEMON_SHUTDOWN_RECEIPT_LOG_RESERVE: Duration = Duration::from_millis(100);

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
    let semantic_artifact_gc = spawn_semantic_artifact_gc_maintenance();

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
    let shutdown_deadline = tokio::time::Instant::now() + DAEMON_SHUTDOWN_DEADLINE
        - DAEMON_SHUTDOWN_RECEIPT_LOG_RESERVE;
    let maintenance_cancel = maintenance.clone();
    let maintenance_join = maintenance.clone();
    let project_open = project_open_tasks(&project_open_gates).await;
    let project_open_cancel = project_open.clone();
    let invocation_cancel = invocation.clone();
    let invocation_join = invocation.clone();
    let replay_cancel = store_administration.clone();
    let replay_join = store_administration.clone();
    let startup_ingest_servers = project_servers_for_shutdown(&store_administration).await;
    let http_application_cancel = http_application_service.shutdown_signal();
    let semantic_artifact_gc_cancel = semantic_artifact_gc.clone();
    let semantic_artifact_gc_join = semantic_artifact_gc;
    let owner_phases = vec![
        vec![
            shutdown_coordination::ShutdownOwner::new(
                "project_server_startup_ingest",
                move || {
                    cancel_project_server_startup_ingests(&startup_ingest_servers);
                },
                async {},
            ),
            shutdown_coordination::ShutdownOwner::with_deadline_status(
                "maintenance",
                move || maintenance_cancel.cancel(),
                move |deadline| async move { maintenance_join.shutdown_until(deadline).await },
            ),
            shutdown_coordination::ShutdownOwner::with_deadline_result(
                "semantic_artifact_gc",
                move || semantic_artifact_gc_cancel.cancel(),
                move |_| async move { semantic_artifact_gc_join.shutdown().await },
            ),
            shutdown_coordination::ShutdownOwner::with_deadline_result(
                "http_application",
                move || http_application_cancel.cancel(),
                move |_| async move { http_application_service.shutdown().await },
            ),
            shutdown_coordination::ShutdownOwner::with_deadline_status(
                "project_open",
                move || project_open_cancel.cancel(),
                move |deadline| async move { project_open.shutdown_until(deadline).await.status() },
            ),
            shutdown_coordination::ShutdownOwner::with_deadline_status(
                "host_admission_replay",
                move || replay_cancel.cancel_host_admission_replay(),
                move |deadline| async move {
                    replay_join
                        .shutdown_host_admission_replay_until(deadline)
                        .await
                },
            ),
        ],
        vec![shutdown_coordination::ShutdownOwner::new(
            "invocation",
            move || invocation_cancel.cancel(),
            async move { invocation_join.shutdown().await },
        )],
    ];
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    let server_store_administration = store_administration.clone();
    // Keep auxiliary process creation blocked until every scheduler and client
    // task is drained or abandoned. Otherwise an app-server call can respawn
    // after the first child tree is terminated but before daemon exit.
    let _codex_shutdown = crate::sessions::codex_app_server::begin_codex_app_server_shutdown();
    let shutdown = shutdown_orchestration::coordinate_daemon_shutdown(
        &lifecycle,
        shutdown_deadline,
        async move {
            shutdown_orchestration::DaemonShutdownPlan::new(clients, owner_phases, async move {
                shutdown_project_servers(shutdown_deadline, &server_store_administration).await
            })
        },
    )
    .await;
    if !shutdown.in_flight.is_clean() || !shutdown.clients.is_clean() {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "client_drain_timeout".to_string()),
                (
                    "deadline_secs",
                    DAEMON_SHUTDOWN_DEADLINE.as_secs().to_string(),
                ),
                (
                    "checkpoint",
                    "skipped_active_clients_were_aborted".to_string(),
                ),
            ],
        );
    }
    log_background_shutdown_receipt(&shutdown.background);
    log_project_server_shutdown_receipt(&shutdown.project_servers);
    endpoint_cleanup
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
        if outcome.status == store_shutdown::ShutdownTaskStatus::Clean {
            continue;
        }
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
        .with_pr_autotrack_task(pr_autotrack_task);
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
    let shutdown_deadline = tokio::time::Instant::now() + DAEMON_SHUTDOWN_DEADLINE
        - DAEMON_SHUTDOWN_RECEIPT_LOG_RESERVE;
    // The coordinator owns every spawned shutdown task and applies this one
    // deadline to each of them. Awaiting its receipt keeps the Codex spawn
    // fence active until those owners have either joined or reported timeout;
    // an outer timeout here would strand that coordinator during process exit.
    let _codex_shutdown = crate::sessions::codex_app_server::begin_codex_app_server_shutdown();
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
            let semantic_artifact_gc_owner =
                shutdown_coordination::ShutdownOwner::with_deadline_result(
                    "semantic_artifact_gc",
                    move || semantic_artifact_gc_cancel.cancel(),
                    move |_| async move { semantic_artifact_gc_join.shutdown().await },
                );
            let http_application_owner = shutdown_coordination::ShutdownOwner::with_deadline_result(
                "http_application",
                {
                    let signal = http_application_service.shutdown_signal();
                    move || signal.cancel()
                },
                move |_| async move { http_application_service.shutdown().await },
            );
            match owner_phases.first_mut() {
                Some(producers) => {
                    producers.push(semantic_artifact_gc_owner);
                    producers.push(http_application_owner);
                }
                None => owner_phases.push(vec![semantic_artifact_gc_owner, http_application_owner]),
            }
            let server_engine = shutdown_engine.clone();
            shutdown_orchestration::DaemonShutdownPlan::new(
                client_tasks,
                owner_phases,
                async move { server_engine.shutdown_servers(shutdown_deadline).await },
            )
        },
    )
    .await;
    if !shutdown.in_flight.is_clean() || !shutdown.clients.is_clean() {
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
