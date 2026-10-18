//! Foreground daemon bootstrap: `run_foreground` entry points, the Unix
//! accept/serve loop, socket preparation, and client-task draining.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::TryFutureExt;

#[cfg(unix)]
use tracedecay_code_index_runtime::{GitWatchMaintenanceWakeV1, git_watch};
use tracedecay_daemon_control::RemoteBrainTlsConfig;
use tracedecay_daemon_identity::authority;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::DAEMON_SHUTDOWN_DEADLINE;
use tracedecay_runtime_core::native_runtime::{self, NativeTaskOutcome};

use super::*;
#[cfg(unix)]
use tracedecay_daemon_service::bounded_stderr_log::BoundedStderrLog;
use tracedecay_daemon_service::shutdown::DAEMON_CLIENT_DRAIN_DEADLINE;
#[cfg(not(unix))]
use tracedecay_daemon_service::shutdown::{DaemonLifecycle, ShutdownStatus};
use tracedecay_runtime_core::logging::log_daemon_event;

/// The shutdown budget handed to the coordinator: the daemon deadline less the
/// slice reserved for writing the terminal shutdown receipts to the daemon log
/// after the coordinator returns.
const DAEMON_SHUTDOWN_COORDINATION_DEADLINE: std::time::Duration =
    DAEMON_SHUTDOWN_DEADLINE.saturating_sub(std::time::Duration::from_millis(100));

/// Exit status (`EX_NOINPUT`) of a daemon whose profile was deleted or
/// replaced under it. It is non-zero so a supervisor that restarts only after
/// a failed exit (launchd with `SuccessfulExit=false`) starts it again on
/// whatever profile is at the path.
pub const PROFILE_LOST_EXIT_CODE: i32 = 66;

/// How a foreground daemon run ended after a clean shutdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaemonRunEnd {
    Stopped,
    /// The profile was deleted or replaced; the process should exit with
    /// [`PROFILE_LOST_EXIT_CODE`].
    ProfileLost,
}

/// How a client connection task ended. The error is rendered because a
/// native task's outcome must be `Clone`.
pub(super) type ClientOutcome = NativeTaskOutcome<std::result::Result<(), String>>;

/// The accepted client connections. Dropping the set aborts every connection
/// still running.
pub(super) type ClientTasks = native_runtime::NativeTaskSet<std::result::Result<(), String>>;

#[tracing::instrument(name = "daemon.bootstrap.catalog_prewarm", level = "trace", skip_all)]
fn prewarm_static_daemon_bootstrap_catalog() {
    if let Err(error) = prewarm_daemon_bootstrap_catalog() {
        tracing::warn!(
            %error,
            "static MCP bootstrap catalog prewarm failed; tools/list will return a typed error"
        );
    }
}

fn log_catalog_prewarm_task(result: std::result::Result<(), String>) {
    if let Err(error) = result {
        tracing::warn!(
            %error,
            "static MCP bootstrap catalog prewarm task failed; tools/list will retry lazily"
        );
    }
}

#[cfg(unix)]
pub fn run_foreground(
    profile: tracedecay_runtime_core::config::ProfileRoot,
    socket_path: PathBuf,
    remote_tls: Option<RemoteBrainTlsConfig>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<DaemonRunEnd>> + Send>> {
    Box::pin(tracing::Instrument::instrument(
        run_foreground_unix(profile, socket_path, remote_tls),
        tracing::trace_span!("daemon.bootstrap.run_foreground"),
    ))
}

#[cfg(not(unix))]
pub fn run_foreground(
    profile: tracedecay_runtime_core::config::ProfileRoot,
    socket_path: PathBuf,
    remote_tls: Option<RemoteBrainTlsConfig>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<DaemonRunEnd>> + Send>> {
    Box::pin(tracing::Instrument::instrument(
        run_foreground_loopback(profile, socket_path, remote_tls)
            .map_ok(|()| DaemonRunEnd::Stopped),
        tracing::trace_span!("daemon.bootstrap.run_foreground"),
    ))
}

#[cfg(not(unix))]
async fn run_foreground_loopback(
    profile: tracedecay_runtime_core::config::ProfileRoot,
    _socket_path: PathBuf,
    remote_tls: Option<RemoteBrainTlsConfig>,
) -> Result<()> {
    let bootstrap_started = Instant::now();
    let profile_root = profile.data_dir().to_path_buf();
    let catalog_prewarm = native_runtime::spawn_blocking(prewarm_static_daemon_bootstrap_catalog);
    let requested = default_loopback_endpoint();
    let _lifecycle_lease = {
        let _span = tracing::trace_span!("daemon.bootstrap.lifecycle_lease").entered();
        {
            tracedecay_runtime_core::lifecycle_lease::acquire_shared_for_profile(
                &profile_root,
                "managed daemon database ownership",
            )
        }
    }?;
    let mut authority =
        authority::DaemonAuthority::acquire(&profile_root, &requested, binary_version()?)?;
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        authority.record().epoch,
        &authority.record().process_run_id,
    )?;
    let store_administration = StoreAdministration::default()
        .with_profile_identity(authority.profile_identity().clone())
        .with_owner_profile(profile.clone());
    let project_open_gates = Arc::new(tokio::sync::Mutex::new(ProjectOpenGates::default()));
    let invocation = DaemonInvocationState::with_progress_producer_incarnation(
        authority.record().epoch,
        profile.home(),
    );
    invocation.configure_github_read_only_credentials(authority.profile_identity());
    store_administration.install_remote_recovery_project_lifecycle()?;
    let deletion_owners = remote_deletion::RemoteDeletionRuntimeOwners {
        administration: store_administration.clone(),
        invocation: invocation.clone(),
        project_open_gates: Arc::clone(&project_open_gates),
    };
    if let remote_deletion::RemoteDeletionBootMode::DeletionOnly(receipt) =
        remote_deletion::resume_remote_account_deletion_for_boot(&deletion_owners).await?
    {
        log_catalog_prewarm_task(catalog_prewarm.await);
        log_daemon_event(
            "remote_account_deletion_resume",
            &[("outcome", format!("{:?}", receipt.status))],
        );
        return Ok(());
    }
    let (worker_plan, catalog_prewarm) = tokio::join!(
        install_profile_worker_plan(&store_administration, &invocation),
        catalog_prewarm
    );
    worker_plan?;
    log_catalog_prewarm_task(catalog_prewarm);
    let (listener, endpoint) = tracing::Instrument::instrument(
        BrokerListener::bind(authority.endpoint()),
        tracing::trace_span!("daemon.bootstrap.listener_bind"),
    )
    .await?;
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
    let http_application_service = tracing::Instrument::instrument(
        http_application::DaemonHttpApplicationService::bind_with_remote_tls(
            http_application_registry.clone(),
            authority.auth_token(),
            remote_tls.as_ref(),
        ),
        tracing::trace_span!("daemon.bootstrap.http_bind"),
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
    let lifecycle = DaemonLifecycle::default();
    let sync_config = tracedecay_configuration::SyncConfig::default();
    let maintenance = maintenance::MaintenanceCoordinator::spawn(
        profile_root.clone(),
        store_administration.clone(),
        invocation.code_index_schedulers.clone(),
        sync_config.retention.clone(),
        maintenance::BranchStoreGcCadenceV1 {
            branch_gc_days: sync_config.branch_gc_days,
        },
    )
    .await;
    let lateness_canary = spawn_lateness_canary();
    let spooled_hook_opener = hook_v2_replay_consumer::spawn_spooled_hook_opener(
        hook_v2_replay_consumer::PortableSpoolOpenerOwners {
            lifecycle: lifecycle.clone(),
            store_administration: store_administration.clone(),
            project_open_gates: Arc::clone(&project_open_gates),
            invocation: invocation.clone(),
            http_application_registry: http_application_registry.clone(),
        },
        profile.clone(),
    );
    let admission = DaemonClientAdmission::new(MAX_CONCURRENT_DAEMON_CLIENTS);
    let per_client_admission = DaemonPerClientAdmission::default();
    let mut clients = ClientTasks::default();
    let mut shutdown_signals = ShutdownSignals::listen()?;
    log_daemon_event(
        "daemon_ready",
        &[(
            "bootstrap_elapsed_ms",
            bootstrap_started.elapsed().as_millis().to_string(),
        )],
    );
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(stream) => stream,
                Err(error) => {
                    log_accept_error_and_backoff(&error).await;
                    continue;
                }
            },
            completed = clients.join_next(), if !clients.is_empty() => {
                if let Some(completed) = completed {
                    log_client_task_result(completed);
                }
                continue;
            },
            () = lifecycle.wait_for_draining() => break,
            () = shutdown_signals.recv() => break,
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
        let spawned = clients.spawn(
            tracing::Instrument::instrument(
                with_connection_admission(permit, async move {
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
                }),
                tracing::trace_span!("daemon.bootstrap.broker_connection"),
            )
            .map_err(|error| error.to_string()),
        );
        if let Err(error) = spawned {
            log_client_spawn_refusal(error);
        }
    }
    lifecycle.begin_draining();
    tracedecay_daemon_service::shutdown::arm_shutdown_exit_bound();
    drop(listener);
    if let Some(opener) = spooled_hook_opener {
        opener.abort();
    }
    if let Some(canary) = lateness_canary {
        canary.abort();
    }
    cancel_retained_session_history(&store_administration).await;
    let shutdown_deadline = Instant::now() + DAEMON_SHUTDOWN_COORDINATION_DEADLINE;
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
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
            tracedecay_daemon_service::shutdown::ShutdownOwner::new(
                "maintenance",
                {
                    let maintenance_cancel = maintenance.clone();
                    move || maintenance_cancel.cancel()
                },
                async move {
                    maintenance_join.shutdown().await;
                },
            ),
            tracedecay_daemon_service::shutdown::ShutdownOwner::with_deadline_result(
                "http_application",
                || {},
                move |_| async move { http_application_service.shutdown().await },
            ),
            hosted_dashboard_shutdown_owner(),
            tracedecay_daemon_service::shutdown::ShutdownOwner::with_deadline_status(
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
            tracedecay_daemon_service::shutdown::ShutdownOwner::new(
                "session_temporal_refresh",
                {
                    let session_refresh = Arc::clone(&session_refresh);
                    move || session_refresh.cancel_supervisors()
                },
                async move {
                    session_refresh.shutdown().await;
                },
            ),
            tracedecay_daemon_service::shutdown::ShutdownOwner::new(
                "host_admission_replay",
                {
                    let replay_cancel = store_administration.clone();
                    move || replay_cancel.cancel_host_admission_replay()
                },
                async move {
                    replay_join.shutdown_host_admission_replay().await;
                },
            ),
        ],
        // Client setup and in-flight requests may create schedulers, project
        // servers, or provider executions. Sweep the invocation registry only
        // after the producer owners settle, so nothing can admit a provider
        // process after the execution registry is emptied and leave it
        // running past shutdown.
        vec![
            tracedecay_daemon_service::shutdown::ShutdownOwner::with_deadline_status(
                "invocation",
                {
                    let invocation_cancel = invocation.clone();
                    move || invocation_cancel.cancel_admissions()
                },
                move |deadline| async move { invocation_join.shutdown(Some(deadline)).await },
            ),
        ],
        vec![tracedecay_daemon_service::shutdown::ShutdownOwner::new(
            "session_sync",
            || {},
            async move {
                session_sync_join.shutdown_session_sync().await;
            },
        )],
        vec![
            tracedecay_daemon_service::shutdown::ShutdownOwner::with_deadline_result(
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
            tracedecay_daemon_service::shutdown::ShutdownOwner::with_deadline_result(
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
    let memory_graph_reconciliation =
        tracedecay_daemon_service::shutdown::ShutdownOwner::with_deadline_result(
            "memory_graph_reconciliation",
            || {},
            move |deadline| async move {
                memory_graph_reconciliation_join
                    .close_stores_for_shutdown(deadline)
                    .await
            },
        );
    let server_store_administration = store_administration.clone();
    let shutdown = tracedecay_daemon_service::shutdown::coordinate_daemon_shutdown(
        &lifecycle,
        shutdown_deadline,
        async move {
            tracedecay_daemon_service::shutdown::DaemonShutdownPlan::new(
                owner_phases,
                move |project_server_deadline| async move {
                    shutdown_project_servers(
                        tokio::time::Instant::from_std(project_server_deadline),
                        &server_store_administration,
                        &http_application_registry,
                    )
                    .await
                },
            )
            .with_clients(client_shutdown_owner(clients))
            .with_terminal_owner_phases(vec![vec![memory_graph_reconciliation]])
        },
    )
    .await;
    log_client_drain_shutdown_receipt(&shutdown);
    log_background_shutdown_receipt(&shutdown.background);
    log_project_server_shutdown_receipt(&shutdown.project_servers);
    log_runtime_hazards();
    endpoint_cleanup
}

fn log_client_drain_shutdown_receipt(
    receipt: &tracedecay_daemon_service::shutdown::DaemonShutdownReceipt,
) {
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

fn log_background_shutdown_receipt(receipt: &tracedecay_daemon_service::shutdown::ShutdownReceipt) {
    for lost in &receipt.lost_work {
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "background_task_lost_work".to_string()),
                ("owner", lost.owner.to_string()),
                ("detail", lost.work.clone()),
            ],
        );
    }
    for owner in receipt.unfinished() {
        // The receipt keeps each owner's typed status; the log must carry it
        // too, or every unfinished owner reads as the same anonymous hang.
        let detail = receipt
            .owners
            .iter()
            .find(|entry| entry.name == *owner)
            .map_or_else(
                || "unreported".to_owned(),
                |entry| match &entry.status {
                    tracedecay_daemon_service::shutdown::ShutdownStatus::Clean => {
                        "clean".to_owned()
                    }
                    tracedecay_daemon_service::shutdown::ShutdownStatus::Failed(error) => {
                        error.clone()
                    }
                    tracedecay_daemon_service::shutdown::ShutdownStatus::TimedOut => {
                        "timed_out".to_owned()
                    }
                },
            );
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "background_task_unfinished".to_string()),
                ("owner", (*owner).to_string()),
                ("detail", detail),
            ],
        );
    }
}

/// The daemon's lifetime count of Tokio-only paths skipped for want of a Tokio
/// runtime, and of writer and reader waits that parked a native thread. Zero
/// is the only healthy value for each. Long native polls are listed by spawn
/// site, and the lateness canary reports timer wakes that ran late.
fn log_runtime_hazards() {
    let long_native_polls = native_runtime::long_native_polls()
        .into_iter()
        .map(|(site, polls)| format!("{site}={polls}"))
        .collect::<Vec<_>>()
        .join(",");
    let lateness = native_runtime::scheduling_lateness();
    log_daemon_event(
        "daemon_runtime_hazards",
        &[
            (
                "tokio_runtime_missing",
                native_runtime::tokio_runtime_missing_total().to_string(),
            ),
            (
                "native_thread_writer_waits",
                native_runtime::native_thread_writer_waits().to_string(),
            ),
            (
                "native_thread_reader_waits",
                tracedecay_rusqlite_runtime::exact_sql::native_thread_reader_waits().to_string(),
            ),
            ("long_native_polls", long_native_polls),
            ("late_timer_wakes", lateness.late_ticks.to_string()),
            (
                "worst_timer_lateness_ms",
                lateness.worst.as_millis().to_string(),
            ),
        ],
    );
}

/// Measures, for the daemon's lifetime, how late native timer wakes run.
fn spawn_lateness_canary() -> Option<native_runtime::NativeTask<()>> {
    native_runtime::spawn_lateness_canary()
        .inspect_err(|error| {
            log_daemon_event(
                "daemon_lateness_canary_unavailable",
                &[("error", error.clone())],
            );
        })
        .ok()
}

fn log_project_server_shutdown_receipt(receipt: &tracedecay_store_runtime::ShutdownTaskReceipt) {
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
            tracedecay_store_runtime::ShutdownStatus::Clean => continue,
            tracedecay_store_runtime::ShutdownStatus::Failed(_) => "failed",
            tracedecay_store_runtime::ShutdownStatus::TimedOut => "timed_out",
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

/// Aborts the accepted client connections and joins them. An aborted client is
/// clean; a client that failed or panicked is reported.
pub(super) fn client_shutdown_owner(
    mut clients: ClientTasks,
) -> tracedecay_daemon_service::shutdown::ShutdownOwner {
    tracedecay_daemon_service::shutdown::ShutdownOwner::with_deadline_result(
        "clients",
        || {},
        move |_| async move {
            clients.abort_all();
            let mut failures = Vec::new();
            while let Some(completed) = clients.join_next().await {
                if let Some(failure) = client_failure(completed) {
                    failures.push(failure);
                }
            }
            if failures.is_empty() {
                Ok(())
            } else {
                Err(failures.join("; "))
            }
        },
    )
}

fn hosted_dashboard_shutdown_owner() -> tracedecay_daemon_service::shutdown::ShutdownOwner {
    tracedecay_daemon_service::shutdown::ShutdownOwner::with_deadline_result(
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
#[expect(
    clippy::too_many_lines,
    reason = "Foreground Unix bootstrap is one ordered authority-acquire, engine-wire, and serve sequence."
)]
async fn run_foreground_unix(
    profile: tracedecay_runtime_core::config::ProfileRoot,
    socket_path: PathBuf,
    remote_tls: Option<RemoteBrainTlsConfig>,
) -> Result<DaemonRunEnd> {
    let bootstrap_started = Instant::now();
    let profile_root = profile.data_dir().to_path_buf();
    let catalog_prewarm = native_runtime::spawn_blocking(prewarm_static_daemon_bootstrap_catalog);
    let endpoint = DaemonEndpoint::Unix(socket_path);
    let _lifecycle = {
        let _span = tracing::trace_span!("daemon.bootstrap.lifecycle_lease").entered();
        {
            tracedecay_runtime_core::lifecycle_lease::acquire_shared_for_profile(
                &profile_root,
                "managed daemon database ownership",
            )
        }
    }?;
    let mut authority =
        authority::DaemonAuthority::acquire(&profile_root, &endpoint, binary_version()?)?;
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        authority.record().epoch,
        &authority.record().process_run_id,
    )?;
    let http_application_registry = http_application::DaemonHttpApplicationRegistry::default();
    let engine = DaemonEngine::default()
        .with_progress_producer_incarnation(authority.record().epoch, profile.home())
        .with_profile_identity(authority.profile_identity().clone())
        .with_owner_profile(profile.clone())
        .with_http_application_registry(http_application_registry.clone());
    engine
        .store_administration
        .install_remote_recovery_project_lifecycle()?;
    let deletion_owners = remote_deletion::RemoteDeletionRuntimeOwners {
        administration: engine.store_administration.clone(),
        invocation: engine.invocation.clone(),
        project_open_gates: Arc::clone(&engine.project_open_gates),
    };
    if let remote_deletion::RemoteDeletionBootMode::DeletionOnly(receipt) =
        remote_deletion::resume_remote_account_deletion_for_boot(&deletion_owners).await?
    {
        log_catalog_prewarm_task(catalog_prewarm.await);
        log_daemon_event(
            "remote_account_deletion_resume",
            &[("outcome", format!("{:?}", receipt.status))],
        );
        return Ok(DaemonRunEnd::Stopped);
    }
    let (worker_plan, catalog_prewarm) = tokio::join!(
        install_profile_worker_plan(&engine.store_administration, &engine.invocation),
        catalog_prewarm
    );
    worker_plan?;
    log_catalog_prewarm_task(catalog_prewarm);
    let socket_path = match authority.endpoint() {
        DaemonEndpoint::Unix(path) => path.clone(),
        DaemonEndpoint::Loopback(_) => {
            return Err(TraceDecayError::Config {
                message: "Unix daemon requires a Unix socket endpoint".to_string(),
            });
        }
    };
    if let Some(parent) = socket_path.parent() {
        match tracedecay_private_fs::create_private_directory(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                tracedecay_private_fs::validate_private_directory(parent).map_err(|error| {
                    TraceDecayError::Config {
                        message: format!(
                            "refusing daemon socket directory '{}': {error}",
                            parent.display()
                        ),
                    }
                })?;
            }
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "failed to create private socket directory '{}': {error}",
                        parent.display()
                    ),
                });
            }
        }
    }
    prepare_socket_path(&authority).await?;

    let (listener, bound_endpoint) = tracing::Instrument::instrument(
        BrokerListener::bind(authority.endpoint()),
        tracing::trace_span!("daemon.bootstrap.listener_bind"),
    )
    .await?;
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
    let http_application_service = tracing::Instrument::instrument(
        http_application::DaemonHttpApplicationService::bind_with_remote_tls(
            http_application_registry.clone(),
            authority.auth_token(),
            remote_tls.as_ref(),
        ),
        tracing::trace_span!("daemon.bootstrap.http_bind"),
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
    let sync_config = tracedecay_configuration::SyncConfig::default();
    let maintenance = maintenance::MaintenanceCoordinator::spawn(
        profile_root.clone(),
        engine.store_administration.clone(),
        engine.invocation.code_index_schedulers.clone(),
        sync_config.retention.clone(),
        maintenance::BranchStoreGcCadenceV1 {
            branch_gc_days: sync_config.branch_gc_days,
        },
    )
    .await;
    // Install the daemon-wide git-metadata owner. Individual projects provide
    // every watcher setting from the pinned configuration already held by
    // their retained server; bootstrap never supplies activation authority.
    let git_watcher = git_watch::GitWatcher::new_with_canonical_scheduler(
        profile.home().map(Path::to_path_buf),
        GitWatchMaintenanceWakeV1::new({
            let maintenance = maintenance.clone();
            move || maintenance.wake()
        }),
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
    )
    .inspect_err(|error| {
        log_daemon_event(
            "pr_autotrack",
            &[
                ("action", "start".to_string()),
                ("outcome", "runtime_unavailable".to_string()),
                ("reason", error.clone()),
            ],
        );
    })
    .ok();
    let engine = engine
        .with_git_watcher(git_watcher)
        .with_maintenance_coordinator(maintenance)
        .with_pr_autotrack_task(pr_autotrack_task)
        .await;
    let mut shutdown_signals = ShutdownSignals::listen()?;
    let mut run_end = DaemonRunEnd::Stopped;
    let lateness_canary = spawn_lateness_canary();
    let spooled_hook_opener =
        hook_v2_replay_consumer::spawn_spooled_hook_opener(engine.clone(), profile.clone());
    let admission = DaemonClientAdmission::new(MAX_CONCURRENT_DAEMON_CLIENTS);
    let mut client_tasks = ClientTasks::default();
    // launchd appends this daemon's stderr to a plain file with no rotation
    // of its own; the journal (systemd) or a terminal is not a regular file
    // and is left alone. Detection is truthful about what fd 2 is, so a
    // failure to inspect it is reported, not treated as "not a file".
    let stderr_log_rotation = match BoundedStderrLog::detect() {
        Ok(Some(log)) => log.spawn_rotation(),
        Ok(None) => None,
        Err(error) => {
            log_daemon_event(
                "daemon_stderr_log_unbounded",
                &[("error", error.to_string())],
            );
            None
        }
    };
    let profile_watch = super::profile_watch::ProfileIdentityWatch::pin(&profile_root)
        .inspect_err(|error| {
            log_daemon_event("profile_watch_unavailable", &[("error", error.to_string())]);
        })
        .ok();
    let mut profile_lost = std::pin::pin!(async {
        match &profile_watch {
            Some(watch) => watch.profile_lost().await,
            None => std::future::pending::<()>().await,
        }
    });
    log_daemon_event(
        "daemon_ready",
        &[(
            "bootstrap_elapsed_ms",
            bootstrap_started.elapsed().as_millis().to_string(),
        )],
    );

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(stream) => stream,
                Err(error) => {
                    log_accept_error_and_backoff(&error).await;
                    continue;
                }
            },
            completed = client_tasks.join_next(), if !client_tasks.is_empty() => {
                if let Some(completed) = completed {
                    log_client_task_result(completed);
                }
                continue;
            },
            () = shutdown_signals.recv() => break,
            () = &mut profile_lost => {
                log_daemon_event(
                    "daemon_shutdown",
                    &[
                        ("outcome", "profile_lost".to_owned()),
                        ("profile", profile_root.display().to_string()),
                    ],
                );
                run_end = DaemonRunEnd::ProfileLost;
                break;
            },
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
        if let Err(error) = client_tasks.spawn(
            tracing::Instrument::instrument(
                with_connection_admission(permit, client),
                tracing::trace_span!("daemon.bootstrap.socket_connection"),
            )
            .map_err(|error| error.to_string()),
        ) {
            log_client_spawn_refusal(error);
        }
    }
    engine.lifecycle.begin_draining();
    tracedecay_daemon_service::shutdown::arm_shutdown_exit_bound();
    if let Some(opener) = spooled_hook_opener {
        opener.abort();
    }
    if let Some(rotation) = stderr_log_rotation {
        rotation.abort();
    }
    if let Some(canary) = lateness_canary {
        canary.abort();
    }
    // Stop accepting and unlink the socket before draining so clients that
    // connect during shutdown get NotFound/ConnectionRefused (which they retry
    // via `connect_with_restart_grace`) instead of a queued connection that
    // will never be served.
    drop(listener);
    let endpoint_cleanup = authority.cleanup_owned_endpoint();
    cancel_retained_session_history(&engine.store_administration).await;
    let shutdown_deadline = Instant::now() + DAEMON_SHUTDOWN_COORDINATION_DEADLINE;
    // Keep auxiliary process creation blocked until every scheduler and client
    // task is drained or abandoned. A killed app-server call may retry before
    // unwinding, so a shorter guard leaves a shutdown-time respawn race. The
    // coordinator owns every spawned shutdown task and applies one deadline to
    // each of them; awaiting its receipt keeps this fence active until those
    // owners have either joined or reported a typed timeout.
    let _codex_shutdown =
        tracedecay_sessions::runtime::hosts::codex_app_server::begin_codex_app_server_shutdown();
    log_daemon_event(
        "daemon_shutdown",
        &[("socket", socket_path.display().to_string())],
    );
    let shutdown_lifecycle = engine.lifecycle.clone();
    let shutdown_engine = engine.clone();
    let shutdown = tracedecay_daemon_service::shutdown::coordinate_daemon_shutdown(
        &shutdown_lifecycle,
        shutdown_deadline,
        async move {
            let mut owner_phases = shutdown_engine.shutdown_owner_phases().await;
            let memory_graph_reconciliation =
                shutdown_engine.memory_graph_reconciliation_shutdown_owner();
            let http_application_owner =
                tracedecay_daemon_service::shutdown::ShutdownOwner::with_deadline_result(
                    "http_application",
                    || {},
                    move |_| async move { http_application_service.shutdown().await },
                );
            let hosted_dashboard_owner = hosted_dashboard_shutdown_owner();
            match owner_phases.first_mut() {
                Some(producers) => {
                    producers.push(http_application_owner);
                    producers.push(hosted_dashboard_owner);
                }
                None => owner_phases.push(vec![http_application_owner, hosted_dashboard_owner]),
            }
            let server_engine = shutdown_engine.clone();
            tracedecay_daemon_service::shutdown::DaemonShutdownPlan::new(
                owner_phases,
                move |project_server_deadline| async move {
                    server_engine
                        .shutdown_servers(project_server_deadline)
                        .await
                },
            )
            .with_clients(client_shutdown_owner(client_tasks))
            .with_terminal_owner_phases(vec![vec![memory_graph_reconciliation]])
        },
    )
    .await;
    log_client_drain_shutdown_receipt(&shutdown);
    log_background_shutdown_receipt(&shutdown.background);
    log_project_server_shutdown_receipt(&shutdown.project_servers);
    log_runtime_hazards();
    endpoint_cleanup.map(|()| run_end)
}

/// Install the daemon-wide worker authority from the profile's exact
/// `ProfileSessions` configuration before publishing a transport endpoint,
/// then mount the process resources session preparation meters against: the
/// daemon's resident-memory authority and the background CPU authority the
/// worker plan just installed. Account-deletion-only boots return before this
/// point and never start projectless capture work.
#[tracing::instrument(name = "daemon.bootstrap.worker_plan", level = "trace", skip_all)]
async fn install_profile_worker_plan(
    store_administration: &StoreAdministration,
    invocation: &DaemonInvocationState,
) -> Result<()> {
    let profile_id = store_administration
        .profile_identity()?
        .profile_id()
        .clone();
    let database = match store_administration
        .registered_profile_session_database()
        .await
    {
        Ok(database) => database,
        // A reset-required profile store is served as that typed state, not a
        // dead daemon. Its persisted worker selection is unreadable, so the
        // plan runs on the selection a reset profile initializes to until the
        // operator resets it.
        Err(error) if error.is_store_reset_required() => {
            log_daemon_event(
                "profile_worker_plan_reset_required",
                &[
                    ("selection", "automatic".to_owned()),
                    ("error", error.to_string()),
                ],
            );
            invocation.install_worker_selection(
                store_administration,
                tracedecay_domain::configuration::CodeIndexWorkerSelectionV1::default(),
            )?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    invocation
        .install_profile_worker_plan(store_administration, database, &profile_id)
        .await?;
    Ok(())
}

/// How long the accept loop pauses after a non-connection accept failure so a
/// persistently failing listener degrades loudly instead of spinning a core.
const DAEMON_ACCEPT_ERROR_BACKOFF: std::time::Duration = std::time::Duration::from_millis(250);

/// One failed accept must never end the daemon. `accept(2)` legitimately
/// fails for per-connection reasons, a client that resets before accept
/// surfaces `ECONNABORTED` on macOS/BSD, and reachability probes connect and
/// drop immediately, and for transient resource pressure (`EMFILE`).
/// Returning the error exited the whole daemon, which a service supervisor
/// then restarts: one aborted connection became a daemon flap.
async fn log_accept_error_and_backoff(error: &TraceDecayError) {
    log_daemon_event(
        "daemon_accept",
        &[
            ("outcome", "error".to_string()),
            ("error", error.to_string()),
        ],
    );
    let connection_scoped = matches!(
        error,
        TraceDecayError::Io(io_error) if matches!(
            io_error.kind(),
            std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::Interrupted
        )
    );
    if !connection_scoped
        && let Err(error) =
            native_runtime::sleep_until(Instant::now() + DAEMON_ACCEPT_ERROR_BACKOFF).await
    {
        tracing::warn!(%error, "daemon accept backoff timer unavailable");
    }
}

/// The process signals that stop the daemon: interrupt, terminate, quit and,
/// on Unix, hangup. The streams are registered once, before the accept loop,
/// and each counts its deliveries, so a signal that lands while the loop is
/// between polls is returned by the next `recv` instead of being lost.
///
/// The first stream starts Asupersync's process-wide dispatcher, which
/// handles every signal kind it knows. A handled SIGHUP or SIGQUIT no longer
/// ends the process by default, so both stop the daemon here.
struct ShutdownSignals(Vec<tracedecay_runtime_core::signal::Signal>);

impl ShutdownSignals {
    fn listen() -> std::io::Result<Self> {
        use tracedecay_runtime_core::signal::SignalKind;
        [
            SignalKind::interrupt(),
            SignalKind::terminate(),
            SignalKind::quit(),
            #[cfg(unix)]
            SignalKind::hangup(),
        ]
        .into_iter()
        .map(tracedecay_runtime_core::signal::signal)
        .collect::<std::io::Result<_>>()
        .map(Self)
    }

    /// Resolves once any shutdown signal has been delivered since the last
    /// `recv`. Cancel-safe: dropping the future loses no delivery.
    async fn recv(&mut self) {
        futures_util::future::select_all(self.0.iter_mut().map(|signal| Box::pin(signal.recv())))
            .await;
    }
}

/// A client that failed or panicked; an aborted client is clean.
fn client_failure(completed: ClientOutcome) -> Option<String> {
    match completed {
        NativeTaskOutcome::Completed(Ok(())) | NativeTaskOutcome::Cancelled => None,
        NativeTaskOutcome::Completed(Err(error)) => Some(error),
        NativeTaskOutcome::Panicked(panic) => Some(panic.to_string()),
    }
}

fn log_client_task_result(completed: ClientOutcome) {
    if let Some(error) = client_failure(completed) {
        log_daemon_event(
            "daemon_client",
            &[("outcome", "error".to_string()), ("error", error)],
        );
    }
}

/// The native runtime refused the connection's task, so the accepted stream
/// is closed unserved.
fn log_client_spawn_refusal(error: String) {
    log_daemon_event(
        "daemon_client",
        &[
            ("outcome", "runtime_unavailable".to_string()),
            ("error", error),
        ],
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
#[tracing::instrument(name = "daemon.bootstrap.prepare_socket", level = "trace", skip_all)]
async fn prepare_socket_path(authority: &authority::DaemonAuthority) -> Result<()> {
    authority.ensure_current()?;
    let socket_path = match authority.endpoint() {
        DaemonEndpoint::Unix(path) => path,
        DaemonEndpoint::Loopback(_) => {
            return Err(TraceDecayError::Config {
                message: "Unix daemon requires a Unix socket endpoint".to_string(),
            });
        }
    };
    ensure_private_socket_parent(socket_path)?;
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
    fn foreground_bootstrap_future_is_heap_bounded() {
        let future = run_foreground(
            tracedecay_runtime_core::config::ProfileRoot::new("/tmp/unused-tracedecay-profile"),
            PathBuf::from("/tmp/unused-tracedecay.sock"),
            None,
        );

        assert_eq!(
            std::mem::size_of_val(&future),
            std::mem::size_of::<
                std::pin::Pin<Box<dyn std::future::Future<Output = Result<DaemonRunEnd>> + Send>>,
            >(),
        );
    }

    /// Runs on the native runtime with no Tokio runtime, so the streams need no
    /// Tokio signal driver. The signal lands before anything awaits `recv`,
    /// as it does when the accept loop is between `select!` polls.
    #[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
    #[test]
    fn a_shutdown_signal_delivered_between_polls_is_not_lost() {
        let mut signals = ShutdownSignals::listen().expect("shutdown signal streams");
        // SAFETY: `raise` only sends SIGHUP to this thread, and the streams
        // above installed its handler, so the process keeps running.
        assert_eq!(unsafe { libc::raise(libc::SIGHUP) }, 0, "raise SIGHUP");

        let received = native_runtime::block_on(native_runtime::timeout(
            std::time::Duration::from_secs(10),
            signals.recv(),
        ))
        .expect("native runtime");

        assert_eq!(
            received,
            Ok(()),
            "a signal delivered while nothing awaited recv must still stop the daemon"
        );
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
        // Bound but never listening: a dropped listener stays connectable while
        // a sibling test's forked child still holds the inherited descriptor.
        socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)
            .expect("stale socket")
            .bind(&socket2::SockAddr::unix(&socket).expect("stale socket address"))
            .expect("bind stale socket");

        let endpoint = DaemonEndpoint::Unix(socket.clone());
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
