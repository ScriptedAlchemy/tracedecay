use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    MessageOccurrenceIdV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectionGenerationId, ProjectionOutputOrdinalV1, ProviderId,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationStore, ObservationWrite,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use super::{MESSAGE_SEARCH_ROOT_SESSION_ID, McpServer, McpServerConstructionContext};
use crate::config::PinnedUserDataDir;
use crate::global_db::GlobalDb;
use crate::mcp::transport::JsonRpcRequest;
use crate::sessions::{SessionMessageRecord, SessionRecord};
use crate::store::GlobalDbObservationStore;
use crate::tracedecay::TraceDecay;

fn git(root: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new(crate::git::git_program())
        .current_dir(root)
        .args(args)
        .status()
        .expect("git command should run");
    assert!(status.success(), "git {args:?} failed");
}

async fn indexed_project() -> (TraceDecay, TempDir, PinnedUserDataDir) {
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
    let cg = TraceDecay::init(dir.path()).await.expect("project init");
    (cg, dir, pin)
}

async fn server_with_authorities() -> (
    Arc<McpServer>,
    TempDir,
    PinnedUserDataDir,
    Arc<GlobalDb>,
    Arc<GlobalDb>,
) {
    let (cg, dir, pin) = indexed_project().await;
    let registry = Arc::new(GlobalDb::open().await.expect("registry"));
    let project = Arc::new(
        GlobalDb::open_at(&cg.store_layout().sessions_db_path)
            .await
            .expect("project sessions"),
    );
    let profile_root = registry
        .db_path()
        .parent()
        .expect("profile root")
        .to_path_buf();
    let profile = Arc::new(
        GlobalDb::open_at(&crate::sessions::user_sessions_db_path(&profile_root))
            .await
            .expect("profile sessions"),
    );
    let mut context = McpServerConstructionContext::direct(cg, None).with_direct_databases(
        None,
        Some(registry),
        Some(Arc::clone(&project)),
        Some(Arc::clone(&profile)),
        false,
    );
    context.profile_root = Some(profile_root);
    (
        McpServer::new_with_context(context).await,
        dir,
        pin,
        project,
        profile,
    )
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

fn fixture_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[test]
fn mcp_session_adapters_have_no_local_lcm_storage_authority() {
    let server = include_str!("../server.rs");
    for forbidden in [
        ".lcm_describe(",
        ".lcm_expand(",
        "get_session_message(",
        "lcm_anchor_state",
        "lcm_temporal_metadata",
        "lcm_occurrence_anchor",
    ] {
        assert!(
            !server.contains(forbidden),
            "MCP server must delegate `{forbidden}` through typed session ports"
        );
    }
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
    let record_id = ObservationId::new(format!("record-{ordinal}")).expect("record id");
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
        ObservationScopeV1::Profile,
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
    db: &GlobalDb,
    observation: DurableObservationV1,
) -> tracedecay_domain::RetrievalAnchorRecord {
    let identity = observation.identity();
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
    GlobalDbObservationStore::new(db)
        .persist_observation(
            AnchoredObservationWrite::new(write, anchor.clone(), projection)
                .expect("anchored write"),
        )
        .await
        .expect("persist observation");
    anchor
}

async fn seed_temporal_message(
    db: &GlobalDb,
    ordinal: u64,
    session_id: &str,
    provider: &str,
    message_id: &str,
    content: &str,
) {
    let observation = fixture_observation(ordinal, session_id, provider, message_id, content);
    let observation_id = observation.observation_id().clone();
    let anchor = persist_fixture_observation(db, observation).await;
    let legacy_projection_content = format!("legacy projection poison {ordinal}");
    let session = SessionRecord {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        project_key: "fixture-project".to_string(),
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
    let message = SessionMessageRecord {
        provider: provider.to_string(),
        message_id: message_id.to_string(),
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
    assert!(db.upsert_session(&session).await);
    assert!(db.upsert_session_message(&message).await);

    let writer = db.writer_connection().await.expect("writer connection");
    if ordinal == 1 {
        writer
            .execute(
                "INSERT INTO session_query_cursor_keys (
                    key_id, key_version, key_material, created_at, retired_at
                 ) VALUES ('message-search-key', 1, ?1, 1, NULL)",
                [vec![0x45_u8; 32]],
            )
            .await
            .expect("cursor key");
    }
    let frozen = json!({
        "active_generation": 1,
        "cursor_key": {"key_id": "message-search-key", "version": 1},
        "projection_frontier": 0,
        "source_frontier": 0,
        "summary_frontier": 0
    })
    .to_string();
    writer
        .execute(
            "INSERT INTO session_temporal_generations (
                session_id, generation, state, frozen_watermarks_json, created_at
             ) VALUES (?1, 1, 'building', ?2, 1)",
            libsql::params![session_id, frozen],
        )
        .await
        .expect("building generation");
    writer
        .execute(
            "UPDATE session_temporal_generations
             SET state = 'ready', ready_at = 1
             WHERE session_id = ?1 AND generation = 1",
            [session_id],
        )
        .await
        .expect("ready generation");
    writer
        .execute(
            "UPDATE session_temporal_generations
             SET state = 'active', activated_at = 1
             WHERE session_id = ?1 AND generation = 1",
            [session_id],
        )
        .await
        .expect("active generation");
    writer
        .execute(
            "INSERT OR IGNORE INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref,
                snippet_text, index_text, legacy_source, legacy_truncated
             ) VALUES (
                ?1, ?2, ?3, 'assistant', ?4, ?4,
                ?5, ?6, 'inline', NULL, ?5, ?5, 0, 0
             )",
            libsql::params![
                provider,
                message_id,
                session_id,
                ordinal as i64,
                legacy_projection_content.clone(),
                fixture_hash(legacy_projection_content.as_bytes())
            ],
        )
        .await
        .expect("raw message");
    let occurrence_id =
        MessageOccurrenceIdV1::derive(&observation_id, ProjectionOutputOrdinalV1::new(0));
    writer
        .execute(
            "INSERT INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, message_id,
                role, knowledge_at, valid_time_json, evidence_json,
                snippet_text, index_text
             ) VALUES (
                ?1, 1, ?2, ?3, 0, ?4, ?5,
                'assistant', ?6, ?7, ?8, ?9, ?9
             )",
            libsql::params![
                session_id,
                occurrence_id.as_str(),
                observation_id.as_str(),
                anchor.anchor_id().as_str(),
                message_id,
                ordinal as i64,
                json!({"kind": "known", "valid_at": ordinal as i64}).to_string(),
                json!({
                    "authority": "provider_native",
                    "evidence_class": "provider_declared",
                    "source_anchor_id": anchor.anchor_id(),
                    "sanitization_receipt": {
                        "receipt_id": format!("receipt-{ordinal}"),
                        "sanitizer_version": "sanitizer.message-search-test.v1"
                    }
                })
                .to_string(),
                content
            ],
        )
        .await
        .expect("occurrence");
    writer
        .execute(
            "INSERT INTO session_current_entities (
                session_id, generation, entity_kind, entity_id,
                current_assertion_id, current_occurrence_id, coverage_json
             ) VALUES (
                ?1, 1, 'occurrence_anchor', ?2, NULL, ?3,
                '{\"occurrence_count\":1}'
             )",
            libsql::params![
                session_id,
                anchor.anchor_id().as_str(),
                occurrence_id.as_str()
            ],
        )
        .await
        .expect("current entity");
}

#[tokio::test]
async fn retained_project_and_profile_handles_construct_retrieval_services() {
    let (server, _dir, _pin, _project, _profile) = server_with_authorities().await;
    assert!(server.project_session_retrieval_service.is_some());
    assert!(server.user_session_retrieval_service.is_some());
    server.shutdown().await;
}

#[tokio::test]
async fn fresh_direct_root_does_not_create_session_storage() {
    let (cg, _dir, _pin) = indexed_project().await;
    let sessions_db_path = cg.store_layout().sessions_db_path.clone();
    let server = McpServer::new(cg, None).await;

    assert!(server.session_db.is_none());
    assert!(server.project_session_retrieval_service.is_none());
    assert!(!sessions_db_path.exists());
    server.shutdown().await;
}

#[tokio::test]
async fn transport_selects_one_service_and_all_registered_never_invokes() {
    let (server, _dir, _pin, _project, _profile) = server_with_authorities().await;

    let deferred = message_search(
        &server,
        json!({
            "query": "database backup",
            "project_scope": "all_registered",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(deferred["outcome"], "deferred");
    assert_eq!(
        server
            .project_session_retrieval_calls
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        server.user_session_retrieval_calls.load(Ordering::Relaxed),
        0
    );

    let project = message_search(
        &server,
        json!({"query": "database backup", "format": "json"}),
    )
    .await;
    assert_eq!(project["outcome"], "unavailable");
    assert_eq!(
        server
            .project_session_retrieval_calls
            .load(Ordering::Relaxed),
        1
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
    assert_eq!(profile["outcome"], "unavailable");
    assert_eq!(
        server
            .project_session_retrieval_calls
            .load(Ordering::Relaxed),
        1
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
        2
    );
    server.shutdown().await;
}

#[tokio::test]
async fn transport_executes_nonempty_project_and_profile_queries_read_only_across_restart() {
    let (server, dir, _pin, project_db, profile_db) = server_with_authorities().await;
    assert!(
        server
            .wait_for_startup_catch_up(std::time::Duration::from_secs(5))
            .await
    );
    for (database, suffix) in [(&project_db, "project"), (&profile_db, "profile")] {
        seed_temporal_message(
            database,
            1,
            MESSAGE_SEARCH_ROOT_SESSION_ID,
            "cursor",
            &format!("message-{suffix}-one"),
            &format!("orchard evidence {suffix} one"),
        )
        .await;
        seed_temporal_message(
            database,
            2,
            &format!("session.{suffix}.two"),
            "cursor",
            &format!("message-{suffix}-two"),
            &format!("orchard evidence {suffix} two"),
        )
        .await;
        database.checkpoint().await;
    }
    let project_before = Sha256::digest(std::fs::read(project_db.db_path()).expect("project db"));
    let profile_before = Sha256::digest(std::fs::read(profile_db.db_path()).expect("profile db"));

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
    assert!(
        first["temporal"]["coverage"]["visible"]
            .as_u64()
            .is_some_and(|visible| visible > 0)
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
    drop(server);
    project_db.checkpoint().await;
    profile_db.checkpoint().await;
    assert_eq!(
        Sha256::digest(std::fs::read(project_db.db_path()).expect("project db")),
        project_before
    );
    assert_eq!(
        Sha256::digest(std::fs::read(profile_db.db_path()).expect("profile db")),
        profile_before
    );

    let cg = TraceDecay::open(dir.path()).await.expect("reopen project");
    let registry = Arc::new(GlobalDb::open().await.expect("reopen registry"));
    let project = Arc::new(
        GlobalDb::open_at(&cg.store_layout().sessions_db_path)
            .await
            .expect("reopen project sessions"),
    );
    let profile_root = registry
        .db_path()
        .parent()
        .expect("profile root")
        .to_path_buf();
    let profile = Arc::new(
        GlobalDb::open_at(&crate::sessions::user_sessions_db_path(&profile_root))
            .await
            .expect("reopen profile sessions"),
    );
    let mut context = McpServerConstructionContext::direct(cg, None).with_direct_databases(
        None,
        Some(registry),
        Some(Arc::clone(&project)),
        Some(Arc::clone(&profile)),
        false,
    );
    context.profile_root = Some(profile_root);
    let restarted = McpServer::new_with_context(context).await;
    project.checkpoint().await;
    profile.checkpoint().await;
    let restarted_project_before =
        Sha256::digest(std::fs::read(project.db_path()).expect("restarted project db"));
    let restarted_profile_before =
        Sha256::digest(std::fs::read(profile.db_path()).expect("restarted profile db"));
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
    project.checkpoint().await;
    profile.checkpoint().await;
    assert_eq!(
        Sha256::digest(std::fs::read(project.db_path()).expect("restarted project db")),
        restarted_project_before
    );
    assert_eq!(
        Sha256::digest(std::fs::read(profile.db_path()).expect("restarted profile db")),
        restarted_profile_before
    );
}
