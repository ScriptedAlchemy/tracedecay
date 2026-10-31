//! Host hook events: one stateless daemon request over the broker connection.
//!
//! The wire metadata and event constructors are pure data and live in
//! [`tracedecay_hooks::core_events`]. Broker admission and delivery use the
//! daemon's canonical store, connection, handshake, and preamble authorities.

use std::path::{Path, PathBuf};

use asupersync::io::{AsyncWriteExt, BufReader};
use rmcp::model::{ProtocolVersion, RequestMetaObject};
use tracedecay_framing::BoundedLineReader;
use tracedecay_hooks::core_events::{DaemonHookEvent, HOOK_EVENT_METHOD, HookEventNotifyOutcomeV1};

use crate::mcp::project_route::HookProjectRouteCache;
use tracedecay_daemon_identity::{
    ResolvedDaemonConnection, client_connection, current_daemon_connection,
};
use tracedecay_daemon_protocol::{DaemonClientStream, SOCKET_ENV};
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::hook_events::{self, HookEventKind, HookEventPlan};
use tracedecay_mcp::server::attach_stateless_request_context;
use tracedecay_runtime_core::logging::log_daemon_event;

use crate::daemon::core_client::write_daemon_preamble;
use tracedecay_mcp::JsonRpcRequest;

/// An unopened worktree needs only its registered session store to retain a
/// file edit. Graph admission and full owner publication happen after the
/// durable receipt; the existing broker replay resolves the exact route.
#[tracing::instrument(
    name = "daemon.engine.hooks.admit_unmounted_edit",
    level = "trace",
    skip_all
)]
pub(super) async fn admit_unmounted_file_edit(
    stores: &crate::daemon::branch_admin::StoreAdministration,
    handshake: &tracedecay_daemon_protocol::DaemonHandshake,
    request: Option<&JsonRpcRequest>,
) -> Result<Option<tracedecay_daemon_protocol::DaemonHandshake>> {
    let Some(request) = request.filter(|request| {
        request.method == HOOK_EVENT_METHOD && tracedecay_mcp::server::opens_rmcp_session(request)
    }) else {
        return Ok(None);
    };
    // Leave foreign or malformed inline contexts to the RMCP authority. This
    // fast admission path must not acknowledge a request RMCP would refuse.
    let Some(meta) = request
        .params
        .as_ref()
        .and_then(|params| params.get("_meta"))
        .and_then(|meta| serde_json::from_value::<RequestMetaObject>(meta.clone()).ok())
    else {
        return Ok(None);
    };
    if !meta
        .missing_required_keys(&ProtocolVersion::V_2026_07_28)
        .is_empty()
        || meta
            .protocol_version()
            .is_none_or(|version| !ProtocolVersion::KNOWN_VERSIONS.contains(&version))
    {
        return Ok(None);
    }
    let Some(event) = hook_events::parse_hook_event(request.params.as_ref()) else {
        return Ok(None);
    };
    if event.kind != HookEventKind::FileEdit || event.rel_paths.is_empty() {
        return Ok(None);
    }
    let root = HookProjectRouteCache::route_cwd(&event).ok_or_else(|| {
        TraceDecayError::project_route(
            "project_route_unavailable",
            false,
            "file edit did not name its worktree",
        )
    })?;
    let mut routed = handshake.clone();
    routed.project_path = Some(root.to_path_buf());
    let (root, route_key) =
        crate::daemon::project_routing::project_route_for_handshake(&routed, stores.owner_home()?)?;
    if stores
        .project_servers()
        .lock()
        .await
        .get_route_and_touch_for(
            &route_key,
            crate::daemon::project_open_admission::ProjectServerRequirement::RegisteredHostIngest,
        )
        .is_some()
    {
        return Ok(None);
    }
    admit_registered_file_edit(stores, routed, root, &event)
        .await
        .map(Some)
}

async fn admit_registered_file_edit(
    stores: &crate::daemon::branch_admin::StoreAdministration,
    mut routed: tracedecay_daemon_protocol::DaemonHandshake,
    root: PathBuf,
    event: &hook_events::HookEvent,
) -> Result<tracedecay_daemon_protocol::DaemonHandshake> {
    crate::daemon::project_open_orchestration::ensure_registered_project_route(
        stores, &root, false,
    )
    .await?;
    let registry = stores.registered_profile_database().await?;
    let layout = tracedecay_project::project::TraceDecay::resolve_registered_configuration_layout(
        &root,
        &crate::daemon::core_handshake::handshake_open_options(&routed),
        registry.as_ref(),
    )
    .await?;
    let project_id = layout.identity.project_id.as_deref().ok_or_else(|| {
        TraceDecayError::project_route(
            "project_route_unavailable",
            false,
            "file edit has no registered project identity",
        )
    })?;
    let owner = registry
        .project_registry_context_by_id(project_id)
        .await?
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "project_route_unavailable",
                false,
                "file edit project is no longer registered",
            )
        })?;
    let (root, scope) = tracedecay_mcp::scope::resolve_query_scope(&owner, &root)
        .map_err(|error| error.into_route_failure().into_error())?;
    let database = stores
        .registered_project_session_database(&root, &layout)
        .await?;
    let broker = stores.host_admission_broker(&database).await?;
    let payload = hook_events::encode_durable_hook_event_plan(&HookEventPlan::SyncFilesAt {
        root: root.clone(),
        scope,
        agent: event.agent,
        event_kind: event.kind,
        had_command: event.had_command,
        route: event.route.clone(),
        rel_paths: event.rel_paths.clone(),
    })
    .map_err(|_| {
        TraceDecayError::project_route(
            "invalid_host_event_plan",
            false,
            "file edit could not be encoded for durable admission",
        )
    })?;
    let admitted = broker
        .admit(&event.admission_source(), &payload)
        .await
        .map_err(|outcome| {
            TraceDecayError::project_route(
                outcome.reason_code.unwrap_or("host_admission_unavailable"),
                outcome.retryable,
                "file edit was not durably admitted",
            )
        })?;
    // The host may have exhausted its acknowledgement budget by now. Retain
    // operator evidence of the exact durable receipt independently of the
    // response write and the later project/binding publication. Match the path
    // set by digest without putting edited filenames in persistent logs.
    log_daemon_event(
        "hook_file_edit_admitted",
        &[
            ("project", root.display().to_string()),
            (
                "paths_digest",
                sha256_hex(format!("{:?}", event.rel_paths).as_bytes()),
            ),
            ("seq", admitted.seq.to_string()),
        ],
    );
    routed.project_path = Some(root);
    Ok(routed)
}

/// Delivers `event` and resolves after the daemon commits durable admission.
/// Index effects may still be owed; source currency includes that canonical
/// pending work until scheduler acceptance. A host bounds this call itself.
#[tracing::instrument(name = "daemon.engine.hooks.notify", level = "trace", skip_all)]
pub async fn notify_hook_event(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    project_path: &Path,
    event: DaemonHookEvent,
) -> HookEventNotifyOutcomeV1 {
    let connection = std::env::var_os(SOCKET_ENV)
        .filter(|path| !path.is_empty())
        .map_or_else(
            || current_daemon_connection(profile.data_dir()),
            |path| client_connection(profile.data_dir(), Path::new(&path)),
        );
    let Ok(connection) = connection else {
        return HookEventNotifyOutcomeV1::Unavailable;
    };
    notify_hook_event_to_connection(profile, project_path, event, connection).await
}

#[tracing::instrument(name = "daemon.engine.hooks.deliver", level = "trace", skip_all)]
async fn notify_hook_event_to_connection(
    profile: &tracedecay_runtime_core::config::ProfileRoot,
    project_path: &Path,
    event: DaemonHookEvent,
    connection: ResolvedDaemonConnection,
) -> HookEventNotifyOutcomeV1 {
    let Ok(handshake) = crate::daemon::core_handshake::handshake_for_current_client(
        profile,
        Some(project_path.to_path_buf()),
        None,
        false,
        false,
    ) else {
        return HookEventNotifyOutcomeV1::Malformed;
    };
    let Ok(params) = serde_json::to_value(event) else {
        return HookEventNotifyOutcomeV1::Malformed;
    };
    // A stateless request, not a notification: its own daemon connection has
    // no `initialize` session for a notification to ride. The daemon answers
    // after the broker commits admission. `Delivered` does not wait for
    // scheduler acceptance; subsequent source-currency reads account for the
    // exact routed edit while the broker still owes that handoff.
    let mut request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::Value::from(1)),
        method: HOOK_EVENT_METHOD.to_string(),
        params: Some(params),
    };
    attach_stateless_request_context(&mut request);
    let Ok(line) = serde_json::to_string(&request) else {
        return HookEventNotifyOutcomeV1::Malformed;
    };
    let Ok(stream) = DaemonClientStream::connect(connection.endpoint()).await else {
        return HookEventNotifyOutcomeV1::Unavailable;
    };
    let (reader, mut writer) = stream.into_split();
    if write_daemon_preamble(&mut writer, &connection, &handshake)
        .await
        .is_err()
    {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    if writer.write_all(line.as_bytes()).await.is_err() {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    if writer.write_all(b"\n").await.is_err() {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    if writer.flush().await.is_err() {
        return HookEventNotifyOutcomeV1::Unavailable;
    }
    let mut reader = BoundedLineReader::new(BufReader::new(reader));
    loop {
        let Ok(Some(response)) = reader.read_mcp_line().await else {
            return HookEventNotifyOutcomeV1::Unavailable;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&response) else {
            return HookEventNotifyOutcomeV1::Malformed;
        };
        if value.get("id") != request.id.as_ref() {
            continue;
        }
        return if value.get("result").is_some() {
            HookEventNotifyOutcomeV1::Delivered
        } else {
            HookEventNotifyOutcomeV1::Malformed
        };
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    #[tokio::test]
    async fn missing_hook_socket_returns_typed_unavailable_without_retry_delay() {
        // Delivery builds the client handshake first, and that handshake reads
        // the registered product runtime for its truthful binary version. Every
        // real hook-notifying process registers one at its entry point; a
        // per-test process that does not would classify the socket outcome as
        // Malformed (no advertisable version) before it ever reaches the
        // connect this test covers.
        tracedecay_project::product_runtime::register_fixture_product_runtime();
        let socket_dir = tempfile::tempdir().unwrap();
        let missing_socket = socket_dir.path().join("missing.sock");
        let _authority = super::super::tests::seed_socket_authority(&missing_socket);
        let profile = tracedecay_runtime_core::config::ProfileRoot::new(socket_dir.path());
        let connection = client_connection(profile.data_dir(), &missing_socket)
            .expect("seeded daemon authority");
        let started = Instant::now();

        let outcome = notify_hook_event_to_connection(
            &profile,
            socket_dir.path(),
            DaemonHookEvent::cursor_after_shell_execution(socket_dir.path().to_path_buf()),
            connection,
        )
        .await;

        assert_eq!(outcome, HookEventNotifyOutcomeV1::Unavailable);
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "a missing socket must not consume the outer hook timeout"
        );
    }

    #[tokio::test]
    async fn delivery_reports_an_acknowledgement_that_outlasts_the_host_hook_bound() {
        tracedecay_project::product_runtime::register_fixture_product_runtime();
        let profile_dir = tempfile::tempdir().unwrap();
        let profile = tracedecay_runtime_core::config::ProfileRoot::new(profile_dir.path());
        let socket = profile.data_dir().join("daemon.sock");
        let _authority = super::super::tests::seed_socket_authority(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).expect("fake daemon socket");
        let admission = Duration::from_secs(2);
        let daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept hook client");
            let (reader, mut writer) = stream.into_split();
            let mut lines = BufReader::new(reader).lines();
            while let Some(line) = lines.next_line().await.expect("read hook client") {
                let request = serde_json::from_str::<serde_json::Value>(&line).ok();
                if request.is_some_and(|request| request["method"] == HOOK_EVENT_METHOD) {
                    break;
                }
            }
            tokio::time::sleep(admission).await;
            writer
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
                .await
                .expect("acknowledge hook event");
        });

        let outcome = notify_hook_event(
            &profile,
            profile_dir.path(),
            DaemonHookEvent::cursor_after_shell_execution(profile_dir.path().to_path_buf()),
        )
        .await;

        assert_eq!(
            outcome,
            HookEventNotifyOutcomeV1::Delivered,
            "a slow admission is still a delivered event"
        );
        daemon.await.expect("fake daemon");
    }
}
