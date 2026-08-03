use std::sync::Arc;
use std::sync::atomic::Ordering;
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

use super::{MESSAGE_SEARCH_ROOT_SESSION_ID, McpServer};
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
        ProjectId::new(MESSAGE_SEARCH_PROJECT_ID).expect("typed project identity"),
    )
    .await
    .expect("registered message-search runtime");
    let cg = runtime
        .initialize_project_graph_for_test(dir.path(), TraceDecayOpenOptions::default())
        .await
        .expect("daemon-owned project init");
    (cg, runtime, dir, pin)
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
async fn unavailable_project_worker_rejects_before_expensive_reads() {
    let (server, dir, _pin) =
        server_with_project_refresh_wake(Some(SessionTemporalRefreshWake::unavailable())).await;

    let payload = tokio::time::timeout(
        Duration::from_millis(100),
        message_search(
            &server,
            json!({
                "query": "database backup",
                "project_path": dir.path(),
                "format": "json",
            }),
        ),
    )
    .await
    .expect("unavailable retrieval should reject within the fast-path budget");

    assert_eq!(payload["status"], "unavailable");
    assert_eq!(payload["error"]["reason"], "refresh_worker_missing");
    assert_eq!(payload["service_status"]["backlog"], 0);
    assert_eq!(payload["service_status"]["blocker"], "worker_missing");
    assert_eq!(
        server
            .project_session_retrieval_calls
            .load(Ordering::Relaxed),
        0,
        "unavailable status must reject before temporal retrieval starts"
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
    assert_eq!(all_registered["project_scope"], "all_registered");
    // The fan-out is a project-scoped read: it never crosses into the profile
    // retrieval service, whatever the registry answers.
    assert_eq!(
        server.user_session_retrieval_calls.load(Ordering::Relaxed),
        0
    );
    let after_all_registered = server
        .project_session_retrieval_calls
        .load(Ordering::Relaxed);

    let project = message_search(
        &server,
        json!({"query": "database backup", "format": "json"}),
    )
    .await;
    // A fresh root with no active generations is empty (zero hits), not
    // unavailable: refresh is a separate explicit durable operation.
    assert_eq!(project["outcome"], "complete_zero");
    assert_eq!(
        server
            .project_session_retrieval_calls
            .load(Ordering::Relaxed),
        after_all_registered + 1
    );
    assert_eq!(
        server.user_session_retrieval_calls.load(Ordering::Relaxed),
        0
    );

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
    assert_eq!(
        server
            .project_session_retrieval_calls
            .load(Ordering::Relaxed),
        after_all_registered + 1
    );
    assert_eq!(
        server.user_session_retrieval_calls.load(Ordering::Relaxed),
        1
    );

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
    assert_eq!(
        server
            .project_session_retrieval_calls
            .load(Ordering::Relaxed),
        after_all_registered + 2
    );
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
    let context = runtime
        .into_mcp_server_context_for_test(cg, None)
        .expect("restarted registered MCP context");
    let restarted = McpServer::new_with_context(context).await;
    assert!(
        restarted
            .wait_for_startup_catch_up(std::time::Duration::from_secs(5))
            .await
    );
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
    let resumed = message_search(
        &restarted,
        json!({
            "query": "orchard evidence",
            "provider": "cursor",
            "limit": 1,
            "cursor": cursor,
            "format": "json",
        }),
    )
    .await;
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
