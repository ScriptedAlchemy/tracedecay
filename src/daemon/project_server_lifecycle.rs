//! Project-server teardown: ingest cancellation, detach, request draining,
//! retirement, and per-profile host admission replay.
//!
//! Retirement waits for in-flight requests before aborting, so a rekey or a
//! shutdown never leaves a store mid-write.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

pub(super) async fn cancel_project_server_startup_ingests(
    store_administration: &StoreAdministration,
) {
    let servers = {
        let registry = store_administration.project_servers().lock().await;
        let mut seen = HashSet::new();
        registry
            .values()
            .filter(|server| seen.insert(Arc::as_ptr(server) as usize))
            .cloned()
            .collect::<Vec<_>>()
    };
    for server in servers {
        server.cancel_startup_transcript_ingest();
    }
}

pub(super) async fn shutdown_project_servers(store_administration: &StoreAdministration) {
    store_administration.join_project_server_retirements().await;
    let servers = detach_project_servers(store_administration).await;
    shutdown_detached_project_servers(servers).await;
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

pub(super) async fn shutdown_detached_project_servers(servers: Vec<Arc<crate::mcp::McpServer>>) {
    for server in servers {
        let graph = server.cg().await;
        hook_v2_replay::shutdown_hook_v2_replay_consumer(&graph.hook_store_layout().data_root)
            .await;
        drop(graph);
        server.shutdown().await;
    }
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

pub(super) async fn schedule_project_server_retirement(
    store_administration: &StoreAdministration,
    servers: Vec<Arc<crate::mcp::McpServer>>,
    route_registered: Option<Arc<AtomicBool>>,
) {
    let retirement = tokio::spawn(retire_project_servers(servers, route_registered));
    store_administration
        .track_project_server_retirement(retirement)
        .await;
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
    let state = store_administration
        .host_admission_broker(&user_session_db)
        .await
        .map_err(|error| {
            TraceDecayError::project_route(
                "host_admission_broker_unavailable",
                true,
                error.to_string(),
            )
        })?;
    if let Some(outcome) = state.unavailable_outcome() {
        let reason_code = outcome.reason_code.unwrap_or("spool_unavailable");
        return Err(TraceDecayError::project_route(
            reason_code,
            outcome.retryable,
            "user-profile host admission spool is unavailable",
        ));
    }
    // host_admission_broker already kicks the coalesced worker for user-sessions DBs.
    Ok(())
}

/// Kick cold profile-session/spool setup outside the connection's admission
/// permit. Concurrent requests for one profile share a single bootstrap, while
/// the retained replay worker still coalesces subsequent passes.
pub(super) async fn schedule_user_profile_host_admission_replay_for_identity(
    store_administration: &StoreAdministration,
    client_identity: &DaemonClientIdentity,
) {
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
    }
}

const PROFILE_HOST_ADMISSION_REPLAY_READ_GRACE: Duration = Duration::from_secs(5);

pub(super) async fn await_user_profile_host_admission_replay_for_identity(
    store_administration: &StoreAdministration,
    client_identity: &DaemonClientIdentity,
) -> Result<()> {
    ensure_user_profile_host_admission_replay_for_identity(store_administration, client_identity)
        .await?;
    let broker_path = authority::canonical_identity_path(&crate::sessions::user_sessions_db_path(
        &client_identity.profile_root,
    ))
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
pub(super) async fn replay_user_profile_host_admission_for_identity(
    store_administration: &StoreAdministration,
    client_identity: &DaemonClientIdentity,
) -> Result<()> {
    await_user_profile_host_admission_replay_for_identity(store_administration, client_identity)
        .await
}
