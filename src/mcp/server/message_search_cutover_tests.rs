use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectId, ProjectionGenerationId, ProviderId,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use super::{
    MESSAGE_SEARCH_ROOT_SESSION_ID, McpServer, RetainedProjectGraphFuture,
    RetainedProjectGraphResolver,
};
use crate::application::host_admission::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, SessionTemporalFixtureCountV1,
};
use crate::config::PinnedUserDataDir;
use crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;
use crate::mcp::transport::JsonRpcRequest;
use crate::sessions::{SessionMessageRecord, SessionRecord};
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

pub(super) const MESSAGE_SEARCH_PROJECT_ID: &str = "project.message-search-cutover";

fn git(root: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new(crate::git::git_program())
        .current_dir(root)
        .args(args)
        .status()
        .expect("git command should run");
    assert!(status.success(), "git {args:?} failed");
}

async fn indexed_project() -> (
    TraceDecay,
    HostAdmissionTestRuntimeV1,
    TempDir,
    PinnedUserDataDir,
) {
    let pin = PinnedUserDataDir::new();
    let (cg, runtime, dir) = indexed_project_with_id(MESSAGE_SEARCH_PROJECT_ID).await;
    (cg, runtime, dir, pin)
}

async fn indexed_project_with_id(
    project_id: &str,
) -> (TraceDecay, HostAdmissionTestRuntimeV1, TempDir) {
    let dir = TempDir::new().expect("temp project");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join(".gitignore"), ".tracedecay/\n").expect("gitignore");
    std::fs::create_dir_all(dir.path().join("src")).expect("source directory");
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn value() -> u8 { 1 }\n",
    )
    .expect("source");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);
    let runtime = HostAdmissionTestRuntimeV1::project(
        crate::config::user_data_dir().expect("isolated profile root"),
        dir.path(),
        ProjectId::new(project_id).expect("typed project identity"),
    )
    .await
    .expect("registered message-search runtime");
    let cg = runtime
        .initialize_project_graph_for_test(dir.path(), TraceDecayOpenOptions::default())
        .await
        .expect("daemon-owned project init");
    (cg, runtime, dir)
}

pub(super) async fn server_with_authorities() -> (Arc<McpServer>, TempDir, PinnedUserDataDir) {
    server_with_project_refresh_wake(None).await
}

pub(super) async fn server_with_project_refresh_wake(
    project_refresh_wake: Option<SessionTemporalRefreshWake>,
) -> (Arc<McpServer>, TempDir, PinnedUserDataDir) {
    let (cg, runtime, dir, pin) = indexed_project().await;
    let mut context = runtime
        .into_mcp_server_context_for_test(cg, None)
        .expect("registered MCP server context");
    context.project_session_refresh_wake = project_refresh_wake;
    (McpServer::new_with_context(context).await, dir, pin)
}

async fn registered_runtime(project_root: &std::path::Path) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::project(
        crate::config::user_data_dir().expect("isolated profile root"),
        project_root,
        ProjectId::new(MESSAGE_SEARCH_PROJECT_ID).expect("typed project identity"),
    )
    .await
    .expect("registered message-search runtime")
}

async fn message_search(server: &McpServer, arguments: Value) -> Value {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "tools/call".to_string(),
        params: Some(json!({
            "name": "tracedecay_message_search",
            "arguments": arguments,
        })),
    };
    let response = server.handle_request(&request).await;
    let response = response.expect("request should produce a response");
    let result = response.result.expect("successful JSON-RPC tool response");
    result["content"]
        .as_array()
        .expect("message-search content")
        .iter()
        .filter_map(|item| item["text"].as_str())
        .find_map(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| panic!("message-search JSON content: {result}"))
}

fn fixture_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).expect("receipt id"),
            tracedecay_domain::ComponentVersion::new("sanitizer.message-search-test.v1")
                .expect("sanitizer version"),
        )
        .expect("receipt reference"),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(payload).expect("payload reference")),
    )
    .expect("receipt")
}

fn fixture_observation(
    scope: ObservationScopeV1,
    ordinal: u64,
    session_id: &str,
    provider: &str,
    message_id: &str,
    content: &str,
) -> DurableObservationV1 {
    let session_id = SessionId::new(session_id).expect("session id");
    let provider = ProviderId::new(provider).expect("provider id");
    let source = ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone())
        .expect("source");
    let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).expect("range");
    let message_id = ObservationId::new(message_id).expect("message id");
    let record_id = message_id.clone();
    let relations = CanonicalObservationRelationsV1::new(session_id).with_message_id(message_id);
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: json!({"text": content}),
            model: None,
            timestamp: Some(ordinal as i64),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .expect("envelope");
    let payload = serde_json::to_value(envelope).expect("observation payload");
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        scope,
        ObservationSourceGenerationV1::new(1).expect("source generation"),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .expect("observation identity");
    DurableObservationV1::new(
        identity,
        fixture_receipt(&format!("receipt-{ordinal}"), &payload),
        RetentionClass::new("retention.message-search-test").expect("retention"),
        payload,
    )
    .expect("durable observation")
}

async fn persist_fixture_observation(
    runtime: &HostAdmissionTestRuntimeV1,
    scope: HostAdmissionScope,
    observation: DurableObservationV1,
) -> tracedecay_domain::RetrievalAnchorRecord {
    let identity = observation.identity();
    let observation_id = observation.observation_id().clone();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .expect("next cursor");
    let write = ObservationWrite::new(observation, None, next_cursor).expect("observation write");
    let projection =
        ProjectionGenerationId::new("projection.message-search-test.v1").expect("projection");
    let authorization = build_observation_resolution_authorization_v1(
        write.observation(),
        "observation-capture.v1",
    )
    .expect("authorization");
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection.clone(),
        UtcMicros(1),
        authorization,
    )
    .expect("anchor");
    let store = runtime
        .observation_store(scope)
        .expect("registered observation store");
    store
        .persist_observation(
            AnchoredObservationWrite::new(write, anchor.clone(), projection)
                .expect("anchored write"),
        )
        .await
        .expect("persist observation");
    store
        .project_observation(&observation_id)
        .await
        .expect("project observation");
    anchor
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn seed_temporal_message(
    runtime: &HostAdmissionTestRuntimeV1,
    authority_scope: HostAdmissionScope,
    project_key: &str,
    scope: ObservationScopeV1,
    ordinal: u64,
    session_id: &str,
    provider: &str,
    message_id: &str,
    content: &str,
) {
    let observation =
        fixture_observation(scope, ordinal, session_id, provider, message_id, content);
    Box::pin(persist_fixture_observation(
        runtime,
        authority_scope,
        observation,
    ))
    .await;
    let legacy_projection_content = format!("legacy projection poison {ordinal}");
    let session = SessionRecord {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        project_key: project_key.to_string(),
        project_path: "/fixture".to_string(),
        title: None,
        started_at: Some(ordinal as i64),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    let legacy_message = SessionMessageRecord {
        provider: provider.to_string(),
        message_id: format!("legacy-only-{message_id}"),
        session_id: session_id.to_string(),
        role: "assistant".to_string(),
        timestamp: Some(ordinal as i64),
        ordinal: ordinal as i64,
        text: legacy_projection_content.clone(),
        kind: Some("message".to_string()),
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: None,
    };
    assert!(
        !runtime
            .upsert_transcript_batch_for_test(
                authority_scope,
                &session,
                std::slice::from_ref(&legacy_message),
                &format!(
                    "message-search-cutover-test:{}:{}",
                    legacy_message.provider, legacy_message.message_id
                ),
                crate::global_db::ParseOffset::default(),
            )
            .await
            .expect("registered transcript seed")
            .is_empty()
    );
    runtime
        .session_temporal_store_for_test(authority_scope)
        .expect("registered temporal store")
        .materialize_pending_session_refresh_for_test(
            &SessionId::new(session_id).expect("session id"),
        )
        .await
        .expect("materialize canonical temporal projection");
}

#[tokio::test]
async fn retained_project_and_profile_handles_construct_retrieval_services() {
    let (server, _dir, _pin) = server_with_authorities().await;
    assert!(server.project_session_retrieval_service.is_some());
    assert!(server.user_session_retrieval_service.is_some());
    server.shutdown().await;
}

#[tokio::test]
async fn partial_history_search_serves_active_data_without_waiting_for_refresh() {
    let (server, _dir, _pin) =
        server_with_project_refresh_wake(Some(SessionTemporalRefreshWake::unavailable())).await;
    let runtime = server
        .host_admission_test_runtime_for_test()
        .expect("retained host-admission test runtime");
    Box::pin(seed_temporal_message(
        runtime,
        HostAdmissionScope::Project,
        MESSAGE_SEARCH_PROJECT_ID,
        ObservationScopeV1::Project {
            project_id: ProjectId::new(MESSAGE_SEARCH_PROJECT_ID).expect("project id"),
        },
        1,
        MESSAGE_SEARCH_ROOT_SESSION_ID,
        "cursor",
        "message.partial-history",
        "stored partial history evidence",
    ))
    .await;
    let project_before = runtime
        .session_domain_sha256_for_test(HostAdmissionScope::Project)
        .await
        .expect("project session-domain digest before stored retrieval");

    let stored = tokio::time::timeout(
        Duration::from_secs(1),
        message_search(
            &server,
            json!({
                "query": "stored partial history evidence",
                "provider": "cursor",
                "catch_up": false,
                "format": "json",
            }),
        ),
    )
    .await
    .expect("stored retrieval must not join unavailable historical refresh");

    assert_eq!(stored["outcome"], "partial", "{stored}");
    assert_eq!(stored["count"], 1, "{stored}");
    assert!(
        stored["results"][0]["message"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("stored partial history evidence")),
        "{stored}"
    );

    let fresh = message_search(
        &server,
        json!({
            "query": "stored partial history evidence",
            "provider": "cursor",
            "catch_up": true,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(fresh["status"], "unavailable", "{fresh}");
    assert_eq!(fresh["error"]["reason"], "refresh_worker_missing");
    assert_eq!(fresh["service_status"]["backlog"], 0);
    assert_eq!(fresh["service_status"]["blocker"], "worker_missing");
    assert_eq!(
        runtime
            .session_domain_sha256_for_test(HostAdmissionScope::Project)
            .await
            .expect("project session-domain digest after stored retrieval"),
        project_before,
        "stored retrieval and typed refresh rejection must remain read-only"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn fresh_direct_root_reuses_configuration_session_storage() {
    let (cg, runtime, _dir, _pin) = indexed_project().await;
    let sessions_db_path = cg.store_layout().sessions_db_path.clone();
    assert!(
        sessions_db_path.exists(),
        "init must open configuration authority sessions.db"
    );
    let context = runtime
        .into_mcp_server_context_for_test(cg, None)
        .expect("registered MCP server context");
    let server = McpServer::new_with_context(context).await;

    // The daemon-owned server reuses the registered configuration-session
    // authority instead of reopening the path.
    assert!(server.session_db.is_some());
    assert!(server.project_session_retrieval_service.is_some());
    server.shutdown().await;
}

#[tokio::test]
async fn registered_project_and_linked_worktree_select_their_exact_session_authority() {
    const ACTIVE_PROJECT_ID: &str = "project.message-search.active";
    const SELECTED_PROJECT_ID: &str = "project.message-search.selected";

    let _pin = PinnedUserDataDir::new();
    let (active_graph, active_runtime, active_dir) =
        indexed_project_with_id(ACTIVE_PROJECT_ID).await;
    let (selected_graph, selected_runtime, selected_dir) =
        indexed_project_with_id(SELECTED_PROJECT_ID).await;
    let linked_owner = TempDir::new().expect("linked worktree owner");
    let active_linked_root = linked_owner.path().join("active-linked");
    let active_linked_root_arg = active_linked_root.to_string_lossy();
    git(
        active_dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            active_linked_root_arg.as_ref(),
            "-b",
            "feature/active-linked",
        ],
    );
    active_runtime
        .upsert_project_alias(&active_linked_root, ACTIVE_PROJECT_ID)
        .await
        .expect("registered active linked-worktree alias");
    let active_linked_graph = active_runtime
        .initialize_project_graph_for_test(&active_linked_root, TraceDecayOpenOptions::default())
        .await
        .expect("initialize active linked-worktree graph");
    let linked_root = linked_owner.path().join("selected-linked");
    let linked_root_arg = linked_root.to_string_lossy();
    git(
        selected_dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            linked_root_arg.as_ref(),
            "-b",
            "feature/selected-linked",
        ],
    );
    selected_runtime
        .upsert_project_alias(&linked_root, SELECTED_PROJECT_ID)
        .await
        .expect("registered linked-worktree alias");
    let selected_linked_graph = selected_runtime
        .initialize_project_graph_for_test(&linked_root, TraceDecayOpenOptions::default())
        .await
        .expect("initialize linked-worktree graph");

    for (runtime, project_id, message_id, text) in [
        (
            &active_runtime,
            ACTIVE_PROJECT_ID,
            "message-active-route",
            "route identity evidence from active",
        ),
        (
            &selected_runtime,
            SELECTED_PROJECT_ID,
            "message-selected-route",
            "route identity evidence from selected",
        ),
    ] {
        Box::pin(seed_temporal_message(
            runtime,
            HostAdmissionScope::Project,
            project_id,
            ObservationScopeV1::Project {
                project_id: ProjectId::new(project_id).expect("fixture project id"),
            },
            1,
            MESSAGE_SEARCH_ROOT_SESSION_ID,
            "cursor",
            message_id,
            text,
        ))
        .await;
        runtime
            .checkpoint_session_database_for_test(HostAdmissionScope::Project)
            .await
            .expect("checkpoint exact project session authority");
    }

    let selected_graph = Arc::new(selected_graph);
    let selected_linked_graph = Arc::new(selected_linked_graph);
    let active_linked_graph = Arc::new(active_linked_graph);
    let selected_root = selected_dir.path().to_path_buf();
    let resolver_linked_root = linked_root.clone();
    let resolver_active_linked_root = active_linked_root.clone();
    let requested_roots = Arc::new(Mutex::new(Vec::new()));
    let resolver_requested_roots = Arc::clone(&requested_roots);
    let resolver: RetainedProjectGraphResolver = Arc::new(move |request| {
        resolver_requested_roots
            .lock()
            .expect("record retained graph request")
            .push(request.requested_worktree_root.clone());
        let graph = match request
            .owner
            .as_ref()
            .map(|owner| owner.project.project_id.as_str())
        {
            Some(SELECTED_PROJECT_ID) if request.requested_worktree_root == selected_root => {
                Some(Arc::clone(&selected_graph))
            }
            Some(SELECTED_PROJECT_ID)
                if request.requested_worktree_root == resolver_linked_root =>
            {
                Some(Arc::clone(&selected_linked_graph))
            }
            Some(ACTIVE_PROJECT_ID)
                if request.requested_worktree_root == resolver_active_linked_root =>
            {
                Some(Arc::clone(&active_linked_graph))
            }
            _ => None,
        };
        Box::pin(async move { Ok(graph) }) as RetainedProjectGraphFuture
    });
    let mut context = active_runtime
        .into_mcp_server_context_for_test(active_graph, None)
        .expect("active registered MCP context");
    context.retained_project_graph_resolver = Some(resolver);
    let server = McpServer::new_with_context(context).await;

    for (selector, expected_root, expected_text) in [
        (
            json!({"project_id": SELECTED_PROJECT_ID}),
            selected_dir.path(),
            "from selected",
        ),
        (
            json!({"project_path": linked_root}),
            linked_root.as_path(),
            "from selected",
        ),
        (
            json!({"project_path": active_linked_root}),
            active_linked_root.as_path(),
            "from active",
        ),
    ] {
        let mut arguments = selector;
        let arguments = arguments
            .as_object_mut()
            .expect("selector object for message search");
        arguments.insert("query".to_owned(), json!("route identity evidence"));
        arguments.insert("limit".to_owned(), json!(10));
        arguments.insert("format".to_owned(), json!("json"));
        let payload = message_search(&server, Value::Object(arguments.clone())).await;
        assert_eq!(payload["count"], 1, "{payload}");
        assert!(
            payload["results"][0]["message"]["text"]
                .as_str()
                .is_some_and(|text| text.contains(expected_text)),
            "project selection must read only its exact registered session authority: {payload}"
        );
        assert_eq!(
            payload["selected_project_root"],
            Value::String(expected_root.display().to_string()),
            "retrieval response must identify the exact worktree that answered: {payload}"
        );
    }
    assert_eq!(
        *requested_roots
            .lock()
            .expect("recorded retained graph requests"),
        vec![
            selected_dir.path().to_path_buf(),
            linked_root.clone(),
            active_linked_root.clone(),
        ],
        "project-path selection must preserve its linked-worktree identity"
    );

    let inconsistent = message_search(
        &server,
        json!({
            "query": "route identity evidence",
            "project_id": ACTIVE_PROJECT_ID,
            "project_path": selected_dir.path(),
            "format": "json",
        }),
    )
    .await;
    assert_eq!(inconsistent["status"], "unavailable", "{inconsistent}");
    assert_eq!(
        inconsistent["error"]["code"], "project_selector_mismatch",
        "{inconsistent}"
    );

    let all_registered = message_search(
        &server,
        json!({
            "query": "route identity evidence",
            "project_scope": "all_registered",
            "limit": 10,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(
        all_registered["searched_project_count"], 2,
        "{all_registered}"
    );
    assert_eq!(all_registered["count"], 2, "{all_registered}");
    let project_ids = all_registered["results"]
        .as_array()
        .expect("multi-root results")
        .iter()
        .filter_map(|result| result["project_id"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        project_ids,
        std::collections::BTreeSet::from([ACTIVE_PROJECT_ID, SELECTED_PROJECT_ID])
    );

    let wrong = message_search(
        &server,
        json!({
            "query": "route identity evidence",
            "project_id": "project.message-search.unknown",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(wrong["status"], "wrong_scope", "{wrong}");
    assert_eq!(wrong["outcome"], "wrong_scope", "{wrong}");
    assert_eq!(
        wrong["error"]["code"], "session_retrieval_wrong_scope",
        "{wrong}"
    );
    server.shutdown().await;
    drop((active_dir, selected_dir, linked_owner));
}

#[tokio::test]
async fn all_registered_reports_missing_registry_authority_as_typed_unavailable() {
    let (cg, runtime, _dir, _pin) = indexed_project().await;
    let mut context = runtime
        .into_mcp_server_context_for_test(cg, None)
        .expect("registered MCP context");
    context.registry_db = None;
    let server = McpServer::new_with_context(context).await;

    let payload = message_search(
        &server,
        json!({
            "query": "route identity evidence",
            "project_scope": "all_registered",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(payload["status"], "unavailable", "{payload}");
    assert_eq!(payload["outcome"], "unavailable", "{payload}");
    assert_eq!(
        payload["error"]["code"], "project_registry_unavailable",
        "{payload}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn all_registered_all_root_failures_remain_typed_unavailable() {
    let (server, _dir, _pin) =
        server_with_project_refresh_wake(Some(SessionTemporalRefreshWake::unavailable())).await;
    let payload = message_search(
        &server,
        json!({
            "query": "route identity evidence",
            "project_scope": "all_registered",
            "catch_up": true,
            "format": "json",
        }),
    )
    .await;

    assert_eq!(payload["status"], "unavailable", "{payload}");
    assert_eq!(
        payload["error"]["code"], "all_registered_search_unavailable",
        "{payload}"
    );
    assert_eq!(
        payload["projects"][0]["error"]["reason"],
        "refresh_worker_missing"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn transport_selects_one_service_and_all_registered_stays_project_scoped() {
    let (server, _dir, _pin) = server_with_authorities().await;

    let all_registered = message_search(
        &server,
        json!({
            "query": "database backup",
            "project_scope": "all_registered",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(all_registered["status"], "deferred", "{all_registered}");
    assert_eq!(
        all_registered["error"]["code"], "session_retrieval_multi_root_deferred",
        "{all_registered}"
    );
    assert_eq!(all_registered["project_scope"], "all_registered");

    let project = message_search(
        &server,
        json!({"query": "database backup", "format": "json"}),
    )
    .await;
    // A fresh root with no active generations is empty (zero hits), not
    // unavailable: refresh is a separate explicit durable operation.
    assert_eq!(project["outcome"], "complete_zero");
    assert_eq!(project["store_scope"], "project", "{project}");

    let profile = message_search(
        &server,
        json!({
            "query": "database backup",
            "storage_scope": "user",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(profile["outcome"], "complete_zero");
    assert_eq!(profile["store_scope"], "profile", "{profile}");

    let denied = message_search(
        &server,
        json!({
            "query": "database backup",
            "project_id": "project.not-owned",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(denied["outcome"], "wrong_scope");
    assert_eq!(denied["store_scope"], "project", "{denied}");
    server.shutdown().await;
}

#[tokio::test]
async fn transport_executes_nonempty_project_and_profile_queries_read_only_across_restart() {
    let (server, dir, _pin) = server_with_authorities().await;
    assert!(
        server
            .wait_for_startup_catch_up(std::time::Duration::from_secs(5))
            .await
    );
    let runtime = server
        .host_admission_test_runtime_for_test()
        .expect("retained host-admission test runtime");
    let project_key = MESSAGE_SEARCH_PROJECT_ID.to_owned();
    let project_scope = ObservationScopeV1::Project {
        project_id: ProjectId::new(project_key.clone()).expect("project id"),
    };
    for (authority_scope, suffix, project_key, scope) in [
        (
            HostAdmissionScope::Project,
            "project",
            project_key.as_str(),
            project_scope,
        ),
        (
            HostAdmissionScope::Profile,
            "profile",
            "user",
            ObservationScopeV1::Profile,
        ),
    ] {
        Box::pin(seed_temporal_message(
            runtime,
            authority_scope,
            project_key,
            scope.clone(),
            1,
            MESSAGE_SEARCH_ROOT_SESSION_ID,
            "cursor",
            &format!("message-{suffix}-one"),
            &format!("orchard evidence {suffix} one"),
        ))
        .await;
        Box::pin(seed_temporal_message(
            runtime,
            authority_scope,
            project_key,
            scope,
            2,
            &format!("session.{suffix}.two"),
            "cursor",
            &format!("message-{suffix}-two"),
            &format!("orchard evidence {suffix} two"),
        ))
        .await;
        runtime
            .checkpoint_session_database_for_test(authority_scope)
            .await
            .expect("checkpoint seeded session authority");
    }
    for authority_scope in [HostAdmissionScope::Project, HostAdmissionScope::Profile] {
        assert!(
            runtime
                .session_temporal_fixture_count_for_test(
                    authority_scope,
                    SessionTemporalFixtureCountV1::TemporalGenerations,
                )
                .await
                .expect("count active temporal generations")
                >= 2,
            "{authority_scope:?} fixture must publish both active generations"
        );
    }
    let project_before = runtime
        .session_domain_sha256_for_test(HostAdmissionScope::Project)
        .await
        .expect("project session-domain digest");
    let profile_before = runtime
        .session_domain_sha256_for_test(HostAdmissionScope::Profile)
        .await
        .expect("profile session-domain digest");

    let first = message_search(
        &server,
        json!({
            "query": "orchard evidence",
            "provider": "cursor",
            "limit": 1,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(first["outcome"], "partial", "{first}");
    assert_eq!(first["count"], 1);
    assert!(
        first["results"][0]["message"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("orchard evidence")),
        "search must emit canonical hydrated text: {first}"
    );
    assert!(
        !first["results"][0]["message"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("legacy projection poison")),
        "legacy compatibility text must never override hydration: {first}"
    );
    assert_eq!(first["temporal"]["freshness"]["state"], "fresh");
    assert_eq!(first["refresh_required"], false);
    assert_eq!(
        first["temporal"]["anchors"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(first["temporal"]["coverage"]["visible"], 0, "{first}");
    assert!(
        first["temporal"]["coverage"]["unknown"]
            .as_u64()
            .is_some_and(|unknown| unknown > 0),
        "fixtures without valid-time evidence must retain unknown coverage: {first}"
    );
    assert!(
        first["temporal"]["explanations"]
            .as_array()
            .is_some_and(|explanations| !explanations.is_empty())
    );
    let cursor = first["temporal"]["cursor"]
        .as_str()
        .expect("continuation cursor")
        .to_string();
    let denied = message_search(
        &server,
        json!({
            "query": "orchard evidence",
            "provider": "cursor",
            "limit": 1,
            "cursor": format!("{cursor}tampered"),
            "format": "json",
        }),
    )
    .await;
    assert_eq!(denied["outcome"], "denied", "{denied}");

    let fresh = message_search(
        &server,
        json!({
            "query": "orchard evidence",
            "provider": "cursor",
            "limit": 2,
            "catch_up": true,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(fresh["outcome"], "partial", "{fresh}");
    assert_eq!(fresh["temporal"]["freshness"]["state"], "fresh");
    assert_eq!(fresh["refresh_required"], false);

    let legacy_only = message_search(
        &server,
        json!({
            "query": "legacy projection poison",
            "provider": "cursor",
            "limit": 10,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(legacy_only["outcome"], "complete_zero", "{legacy_only}");
    assert_eq!(legacy_only["count"], 0);
    assert_eq!(legacy_only["results"], json!([]));

    let profile = message_search(
        &server,
        json!({
            "query": "orchard evidence",
            "provider": "cursor",
            "storage_scope": "user",
            "limit": 2,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(profile["outcome"], "partial", "{profile}");
    assert_eq!(profile["count"], 2);

    server.shutdown().await;
    assert_eq!(
        runtime
            .session_domain_sha256_for_test(HostAdmissionScope::Project)
            .await
            .expect("project session-domain digest after reads"),
        project_before
    );
    assert_eq!(
        runtime
            .session_domain_sha256_for_test(HostAdmissionScope::Profile)
            .await
            .expect("profile session-domain digest after reads"),
        profile_before
    );
    drop(server);

    let runtime = registered_runtime(dir.path()).await;
    let cg = runtime
        .open_project_graph_for_test(dir.path(), TraceDecayOpenOptions::default())
        .await
        .expect("reopen project through daemon authority");
    let mut context = runtime
        .into_mcp_server_context_for_test(cg, None)
        .expect("restarted registered MCP context");
    context.project_session_refresh_wake = Some(SessionTemporalRefreshWake::unavailable());
    context.user_session_refresh_wake = Some(SessionTemporalRefreshWake::unavailable());
    context.startup_catch_up_enabled = false;
    let restarted = McpServer::new_with_context(context).await;
    let runtime = restarted
        .host_admission_test_runtime_for_test()
        .expect("restarted retained host-admission runtime");
    let restarted_project_before = runtime
        .session_domain_sha256_for_test(HostAdmissionScope::Project)
        .await
        .expect("restarted project session-domain digest");
    let restarted_profile_before = runtime
        .session_domain_sha256_for_test(HostAdmissionScope::Profile)
        .await
        .expect("restarted profile session-domain digest");
    let resumed = tokio::time::timeout(
        Duration::from_secs(1),
        message_search(
            &restarted,
            json!({
                "query": "orchard evidence",
                "provider": "cursor",
                "limit": 1,
                "cursor": cursor,
                "catch_up": false,
                "format": "json",
            }),
        ),
    )
    .await
    .expect("restart retrieval must not wait for historical catch-up");
    assert_eq!(resumed["outcome"], "partial", "{resumed}");
    assert_eq!(resumed["count"], 1);
    restarted.shutdown().await;
    assert_eq!(
        runtime
            .session_domain_sha256_for_test(HostAdmissionScope::Project)
            .await
            .expect("restarted project session-domain digest after reads"),
        restarted_project_before
    );
    assert_eq!(
        runtime
            .session_domain_sha256_for_test(HostAdmissionScope::Profile)
            .await
            .expect("restarted profile session-domain digest after reads"),
        restarted_profile_before
    );
}
