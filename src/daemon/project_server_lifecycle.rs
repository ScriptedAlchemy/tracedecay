//! Project-server teardown: detach, request draining, retirement, and
//! per-profile host admission replay.
//!
//! Retirement waits for in-flight requests before aborting, so a rekey or a
//! shutdown never leaves a store mid-write.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::profile_host_admission_replay::ProfileHostAdmissionBootstrapStatus;
use super::shutdown_coordination::ShutdownStatus;
use super::store_shutdown::{ShutdownTaskOutcome, ShutdownTaskReceipt, join_shutdown_tasks_until};
use super::*;
use std::collections::HashSet;

pub(super) async fn cancel_retained_session_history(store_administration: &StoreAdministration) {
    store_administration
        .session_temporal_refresh_schedulers()
        .cancel_historical_ingest()
        .await;
}

/// One bounded, idempotent project-server teardown. Servers whose shutdown
/// timed out are retained on the administration so a retry re-drives exactly
/// those owners; typed failures are replayed into every subsequent receipt.
pub(super) async fn shutdown_project_servers(
    deadline: tokio::time::Instant,
    store_administration: &StoreAdministration,
) -> ShutdownTaskReceipt {
    let (detached, mut receipt) =
        match tokio::time::timeout_at(deadline, detach_project_servers(store_administration)).await
        {
            Ok(servers) => (servers, ShutdownTaskReceipt::default()),
            Err(_) => (
                Vec::new(),
                ShutdownTaskReceipt::timed_out("project_server_detach"),
            ),
        };
    let mut retained = {
        let mut owners = store_administration
            .retained_project_shutdown_owners
            .lock()
            .await;
        let mut retained = std::mem::take(&mut *owners);
        for server in detached {
            if !retained.iter().any(|owner| {
                matches!(
                    owner,
                    super::branch_admin::RetainedProjectShutdownOwner::TimedOut {
                        server: retained_server,
                    } if Arc::ptr_eq(retained_server, &server)
                )
            }) {
                retained
                    .push(super::branch_admin::RetainedProjectShutdownOwner::TimedOut { server });
            }
        }
        retained
    };
    let mut attempted = Vec::new();
    let mut replayed_failures = ShutdownTaskReceipt::default();
    for owner in &retained {
        match owner {
            super::branch_admin::RetainedProjectShutdownOwner::Failed { error } => {
                replayed_failures.outcomes.push(ShutdownTaskOutcome {
                    owner: "retained_project_server".to_owned(),
                    status: ShutdownStatus::Failed(error.clone()),
                })
            }
            super::branch_admin::RetainedProjectShutdownOwner::TimedOut { server } => {
                attempted.push(Arc::clone(server))
            }
        }
    }
    let (retirements, server_attempts) = tokio::join!(
        store_administration.join_project_server_retirements_until(deadline),
        shutdown_detached_project_servers(deadline, attempted),
    );
    apply_project_shutdown_attempts(&mut retained, &server_attempts);
    {
        let mut owners = store_administration
            .retained_project_shutdown_owners
            .lock()
            .await;
        *owners = retained;
    }
    receipt.extend(replayed_failures);
    receipt.extend(retirements);
    receipt.extend(server_attempts);
    receipt
}

fn apply_project_shutdown_attempts(
    retained: &mut Vec<super::branch_admin::RetainedProjectShutdownOwner>,
    server_attempts: &ShutdownTaskReceipt,
) {
    let mut statuses = server_attempts
        .outcomes
        .iter()
        .map(|outcome| outcome.status.clone());
    retained.retain_mut(|owner| match owner {
        super::branch_admin::RetainedProjectShutdownOwner::Failed { .. } => true,
        super::branch_admin::RetainedProjectShutdownOwner::TimedOut { .. } => {
            let status = statuses.next().unwrap_or(ShutdownStatus::TimedOut);
            match status {
                ShutdownStatus::Clean => false,
                ShutdownStatus::TimedOut => true,
                ShutdownStatus::Failed(error) => {
                    *owner = super::branch_admin::RetainedProjectShutdownOwner::Failed { error };
                    true
                }
            }
        }
    });
}

pub(super) async fn detach_project_servers(
    store_administration: &StoreAdministration,
) -> Vec<Arc<crate::mcp::McpServer>> {
    let servers: Vec<Arc<crate::mcp::McpServer>> = store_administration
        .with_writer(|| async {
            let mut registry = store_administration.project_servers().lock().await;
            let mut seen = HashSet::new();
            let servers = registry
                .values()
                .filter(|server| seen.insert(Arc::as_ptr(server) as usize))
                .cloned()
                .collect();
            // Servers retain daemon callbacks that clone StoreAdministration.
            // Remove the registry's side of that cycle before awaiting server
            // shutdown so every physical store runtime can be dropped.
            registry.servers.clear();
            registry.aliases.clear();
            servers
        })
        .await;
    servers
}

pub(super) async fn shutdown_detached_project_servers(
    deadline: tokio::time::Instant,
    servers: Vec<Arc<crate::mcp::McpServer>>,
) -> ShutdownTaskReceipt {
    join_shutdown_tasks_until(
        deadline,
        servers.into_iter().enumerate().map(|(ordinal, server)| {
            (format!("project_server[{ordinal}]"), None, async move {
                let graph = server.cg().await;
                hook_v2_replay::shutdown_hook_v2_replay_consumer(
                    &graph.hook_store_layout().data_root,
                )
                .await;
                drop(graph);
                server.shutdown_until(deadline).await
            })
        }),
    )
    .await
}

const PROJECT_SERVER_REQUEST_DRAIN_DEADLINE: Duration = Duration::from_secs(35);
const PROJECT_SERVER_ABORT_DRAIN_DEADLINE: Duration = Duration::from_secs(2);

async fn wait_for_project_server_request_drains(servers: &[Arc<crate::mcp::McpServer>]) {
    for server in servers {
        server.wait_for_project_server_request_drain().await;
    }
}

async fn retire_project_servers(
    servers: Vec<Arc<crate::mcp::McpServer>>,
    route_registered: Option<Arc<AtomicBool>>,
) {
    if tokio::time::timeout(
        PROJECT_SERVER_REQUEST_DRAIN_DEADLINE,
        wait_for_project_server_request_drains(&servers),
    )
    .await
    .is_err()
    {
        tracing::warn!(
            deadline_secs = PROJECT_SERVER_REQUEST_DRAIN_DEADLINE.as_secs(),
            server_count = servers.len(),
            "retired project requests exceeded their drain deadline; cancelling them"
        );
        for server in &servers {
            server.abort_project_server_requests();
        }
        if tokio::time::timeout(
            PROJECT_SERVER_ABORT_DRAIN_DEADLINE,
            wait_for_project_server_request_drains(&servers),
        )
        .await
        .is_err()
        {
            tracing::warn!(
                deadline_secs = PROJECT_SERVER_ABORT_DRAIN_DEADLINE.as_secs(),
                server_count = servers.len(),
                "cancelled project requests have not yielded; retaining safe shutdown ownership"
            );
            wait_for_project_server_request_drains(&servers).await;
        }
    }
    if let Some(route_registered) = route_registered {
        route_registered.store(false, Ordering::Release);
    }
    for server in servers {
        server.shutdown().await;
    }
}

pub(super) async fn retire_project_servers_now(servers: Vec<Arc<crate::mcp::McpServer>>) {
    retire_project_servers(servers, None).await;
}

pub(super) async fn schedule_project_server_retirement(
    store_administration: &StoreAdministration,
    owner: StoreOwnerKey,
    servers: Vec<Arc<crate::mcp::McpServer>>,
    route_registered: Option<Arc<AtomicBool>>,
) {
    let mut admission = store_administration
        .acquire_project_server_retirement_admission()
        .await;
    admission.spawn_and_track(owner, retire_project_servers(servers, route_registered));
}

/// Kick coalesced per-profile replay without awaiting a pass (handshake-safe).
pub(super) async fn ensure_user_profile_host_admission_replay_for_identity(
    store_administration: &StoreAdministration,
    _client_identity: &DaemonClientIdentity,
) -> Result<()> {
    let user_session_db = store_administration
        .registered_profile_session_database()
        .await
        .map_err(|error| {
            TraceDecayError::project_route(
                "registered_authority_unavailable",
                true,
                error.to_string(),
            )
        })?;
    store_administration
        .host_admission_broker(&user_session_db)
        .await?;
    // host_admission_broker already kicks the coalesced worker for user-sessions DBs.
    Ok(())
}

/// Kick cold profile-session/spool setup outside the connection's admission
/// permit. Concurrent requests for one profile share a single bootstrap, while
/// the retained replay worker still coalesces subsequent passes.
pub(super) async fn schedule_user_profile_host_admission_replay_for_identity(
    store_administration: &StoreAdministration,
    client_identity: &DaemonClientIdentity,
) -> Option<ProfileHostAdmissionBootstrapStatus> {
    if let Err(error) = store_administration
        .ensure_profile_host_admission_bootstrap(&client_identity.profile_root)
        .await
    {
        let reason_code = error
            .project_route_context()
            .map_or("authority_unavailable", |(reason_code, _, _)| reason_code);
        log_daemon_event(
            "profile_host_admission_bootstrap_schedule_failed",
            &[("reason_code", reason_code.to_owned())],
        );
        return None;
    }
    match store_administration
        .profile_host_admission_bootstrap_status(&client_identity.profile_root)
        .await
    {
        Ok(status) => status,
        Err(error) => {
            let reason_code = error
                .project_route_context()
                .map_or("authority_unavailable", |(reason_code, _, _)| reason_code);
            log_daemon_event(
                "profile_host_admission_bootstrap_status_failed",
                &[("reason_code", reason_code.to_owned())],
            );
            None
        }
    }
}

const PROFILE_HOST_ADMISSION_REPLAY_READ_GRACE: Duration = Duration::from_secs(5);

pub(super) async fn await_user_profile_host_admission_replay_for_identity(
    store_administration: &StoreAdministration,
    client_identity: &DaemonClientIdentity,
) -> Result<()> {
    ensure_user_profile_host_admission_replay_for_identity(store_administration, client_identity)
        .await?;
    let broker_path = authority::canonical_identity_path(
        &tracedecay_sessions::runtime::user_sessions_db_path(&client_identity.profile_root),
    )
    .map_err(|error| {
        TraceDecayError::project_route("host_admission_broker_unavailable", true, error.to_string())
    })?;
    if !store_administration
        .wait_user_profile_host_admission_replay_idle(
            &broker_path,
            PROFILE_HOST_ADMISSION_REPLAY_READ_GRACE,
        )
        .await
    {
        return Err(TraceDecayError::project_route(
            "profile_host_admission_replay_warming",
            true,
            "retained profile events are still replaying",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod shutdown_owner_tests {
    use super::*;

    #[tokio::test]
    async fn terminal_shutdown_failure_replays_without_retaining_server_owner() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project root");
        let (graph, _runtime) =
            crate::tracedecay::TraceDecay::init_test_fixture_with_registered_runtime(
                project.path(),
                "project.shutdown-owner",
            )
            .await
            .expect("registered graph");
        let server = crate::mcp::McpServer::new(graph, None).await;
        server.shutdown().await;
        let weak_server = Arc::downgrade(&server);
        let administration = StoreAdministration::default();
        administration
            .retained_project_shutdown_owners
            .lock()
            .await
            .push(
                super::super::branch_admin::RetainedProjectShutdownOwner::TimedOut {
                    server: Arc::clone(&server),
                },
            );
        let mut failed_attempt = ShutdownTaskReceipt::default();
        failed_attempt.outcomes.push(ShutdownTaskOutcome {
            owner: "project_server[0]".to_owned(),
            status: ShutdownStatus::Failed("injected terminal failure".to_owned()),
        });
        {
            let mut owners = administration.retained_project_shutdown_owners.lock().await;
            apply_project_shutdown_attempts(&mut owners, &failed_attempt);
        }
        drop(server);

        for _ in 0..2 {
            let receipt = shutdown_project_servers(
                tokio::time::Instant::now() + Duration::from_secs(5),
                &administration,
            )
            .await;
            assert!(
                receipt.outcomes.iter().any(|outcome| {
                    matches!(
                        &outcome.status,
                        ShutdownStatus::Failed(error) if error == "injected terminal failure"
                    )
                }),
                "terminal failure must remain visible on every shutdown receipt"
            );
        }
        assert!(
            weak_server.upgrade().is_none(),
            "a terminal receipt must not keep the failed project server or its daemon callbacks alive"
        );
    }
}

#[cfg(test)]
pub(super) async fn replay_user_profile_host_admission_for_identity(
    store_administration: &StoreAdministration,
    client_identity: &DaemonClientIdentity,
) -> Result<()> {
    await_user_profile_host_admission_replay_for_identity(store_administration, client_identity)
        .await
}
