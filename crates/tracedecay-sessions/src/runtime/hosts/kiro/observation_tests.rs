use super::*;
use crate::runtime::snapshot_observation::SnapshotAdmissionRecord;

#[test]
fn workspace_folder_file_uri_round_trips_native_paths() {
    let temp = tempfile::TempDir::new().expect("temporary Kiro workspace");
    let path = temp.path().join("workspace with spaces");
    let uri = url::Url::from_file_path(&path).expect("native path has a file URI");

    assert_eq!(folder_field_to_path(uri.as_str()), Some(path));
}

#[cfg(windows)]
#[test]
fn workspace_folder_file_uri_removes_windows_drive_separator() {
    assert_eq!(
        folder_field_to_path("file:///D:/Kiro%20Workspace"),
        Some(PathBuf::from(r"D:\Kiro Workspace"))
    );
    assert_eq!(
        folder_field_to_path(r"file://D:\Kiro%20Workspace"),
        Some(PathBuf::from(r"D:\Kiro Workspace"))
    );
}

#[tokio::test]
async fn byte_budget_charges_once_and_defers_second_before_parse() {
    use crate::admission::test_support::MemoryHostAdmission;

    let temp = tempfile::TempDir::new().expect("temp Kiro storage");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let hash = "0123456789abcdef0123456789abcdef";
    let agent_dir = temp.path().join("agent");
    let workspace_dir = agent_dir.join(hash);
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace_storage_dir = temp.path().join("workspaces");
    let workspace_metadata = workspace_metadata_path(&workspace_storage_dir, hash);
    std::fs::create_dir_all(workspace_metadata.parent().unwrap()).unwrap();
    std::fs::write(
        &workspace_metadata,
        serde_json::json!({"folder": format!("file://{}", project.display())}).to_string(),
    )
    .unwrap();
    let first_path = workspace_dir.join("a-first.chat");
    std::fs::write(
        &first_path,
        serde_json::json!({
            "executionId": "first",
            "chat": [{"role": "human", "content": "first"}]
        })
        .to_string(),
    )
    .unwrap();
    let hostile = format!("{{\"executionId\":\"hostile\",\"chat\":{}", "x".repeat(256));
    let second_path = workspace_dir.join("z-hostile.chat");
    std::fs::write(&second_path, &hostile).unwrap();
    let source = KiroSource {
        agent_dir,
        workspace_storage_dir,
        user_registered_roots: None,
        project_matchers: ProjectRootMatcherCache::default(),
    };
    let paths = source.transcript_paths(&project);
    assert_eq!(paths, vec![first_path.clone(), second_path.clone()]);
    let first_bytes = source.snapshot_input_bytes(&first_path).unwrap();
    let second_bytes = source.snapshot_input_bytes(&second_path).unwrap();

    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    let deferred = capture_kiro_snapshot_observations(
        &admission,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(first_bytes),
        &cancellation,
    )
    .await
    .expect("second unit must defer without parsing malformed JSON");
    assert!(deferred.deferred_by_byte_cap);
    assert_eq!(deferred.stats.messages_upserted, 1);
    assert_eq!(deferred.bytes_consumed, first_bytes);

    std::fs::remove_file(first_path).unwrap();
    let err = capture_kiro_snapshot_observations(
        &admission,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(second_bytes),
        &cancellation,
    )
    .await
    .expect_err("deferred malformed snapshot must remain retryable");
    assert!(matches!(
        err,
        TranscriptIngestError::NonDurableRecord {
            reason: "malformed snapshot JSON",
            ..
        }
    ));
}

#[tokio::test]
async fn pre_cancelled_snapshot_capture_does_not_advance_kiro_source() {
    use crate::admission::test_support::MemoryHostAdmission;

    let temp = tempfile::TempDir::new().expect("temp Kiro storage");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let hash = "0123456789abcdef0123456789abcdef";
    let agent_dir = temp.path().join("agent");
    let workspace_dir = agent_dir.join(hash);
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace_storage_dir = temp.path().join("workspaces");
    let workspace_metadata = workspace_metadata_path(&workspace_storage_dir, hash);
    std::fs::create_dir_all(workspace_metadata.parent().unwrap()).unwrap();
    std::fs::write(
        workspace_metadata,
        serde_json::json!({"folder": format!("file://{}", project.display())}).to_string(),
    )
    .unwrap();
    std::fs::write(
        workspace_dir.join("cancelled.chat"),
        serde_json::json!({
            "executionId": "cancelled",
            "chat": [{"role": "human", "content": "retry me"}]
        })
        .to_string(),
    )
    .unwrap();
    let source = KiroSource {
        agent_dir,
        workspace_storage_dir,
        user_registered_roots: None,
        project_matchers: ProjectRootMatcherCache::default(),
    };
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let error = capture_kiro_snapshot_observations(
        &admission,
        &source,
        &project,
        ObservationScopeV1::Profile,
        None,
        &cancellation,
    )
    .await
    .expect_err("pre-cancelled Kiro capture must stop before persistence");
    assert!(matches!(
        error,
        TranscriptIngestError::Cancelled { provider: "kiro" }
    ));
    assert!(admission.observations().is_empty());

    let replay = capture_kiro_snapshot_observations(
        &admission,
        &source,
        &project,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .expect("uncancelled Kiro retry must admit the untouched source");
    assert_eq!(replay.stats.messages_upserted, 1);
}

#[tokio::test]
async fn aggregate_budget_replay_charges_committed_prefix_and_retries_suffix() {
    use crate::admission::test_support::MemoryHostAdmission;

    let temp = tempfile::TempDir::new().expect("temp Kiro storage");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let hash = "0123456789abcdef0123456789abcdef";
    let agent_dir = temp.path().join("agent");
    let workspace_dir = agent_dir.join(hash);
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace_storage_dir = temp.path().join("workspaces");
    let workspace_metadata = workspace_metadata_path(&workspace_storage_dir, hash);
    std::fs::create_dir_all(workspace_metadata.parent().unwrap()).unwrap();
    std::fs::write(
        &workspace_metadata,
        serde_json::json!({"folder": format!("file://{}", project.display())}).to_string(),
    )
    .unwrap();
    for id in ["a", "b"] {
        std::fs::write(
            workspace_dir.join(format!("{id}.chat")),
            serde_json::json!({
                "executionId": id,
                "chat": [{"role": "human", "content": format!("message-{id}")}]
            })
            .to_string(),
        )
        .unwrap();
    }
    let source = KiroSource {
        agent_dir,
        workspace_storage_dir,
        user_registered_roots: None,
        project_matchers: ProjectRootMatcherCache::default(),
    };
    let paths = source.transcript_paths(&project);
    assert_eq!(paths.len(), 2);
    let first_bytes = source.snapshot_input_bytes(&paths[0]).unwrap();
    let second_bytes = source.snapshot_input_bytes(&paths[1]).unwrap();
    let full_cap = first_bytes.saturating_add(second_bytes);
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();

    let first = capture_kiro_snapshot_observations(
        &admission,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(first_bytes),
        &cancellation,
    )
    .await
    .expect("first bounded sweep");
    assert_eq!(first.stats.messages_upserted, 1);
    assert_eq!(first.bytes_consumed, first_bytes);
    assert!(first.deferred_by_byte_cap);

    let second = capture_kiro_snapshot_observations(
        &admission,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(first_bytes),
        &cancellation,
    )
    .await
    .expect("committed prefix replay");
    assert_eq!(second.stats.messages_upserted, 0);
    assert_eq!(second.bytes_consumed, first_bytes);
    assert!(second.deferred_by_byte_cap);

    let resumed = capture_kiro_snapshot_observations(
        &admission,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(full_cap),
        &cancellation,
    )
    .await
    .expect("deferred suffix replay");
    assert_eq!(resumed.stats.messages_upserted, 1);
    assert_eq!(resumed.bytes_consumed, full_cap);
    assert!(!resumed.deferred_by_byte_cap);

    let complete = capture_kiro_snapshot_observations(
        &admission,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(full_cap),
        &cancellation,
    )
    .await
    .expect("complete replay");
    assert_eq!(complete.stats.messages_upserted, 0);
    assert_eq!(complete.bytes_consumed, full_cap);
    assert!(!complete.deferred_by_byte_cap);
}

#[test]
fn snapshot_budget_counts_transcript_and_workspace_metadata() {
    let temp = tempfile::TempDir::new().expect("temp Kiro storage");
    let hash = "0123456789abcdef0123456789abcdef";
    let transcript = temp.path().join("agent").join(hash).join("session.json");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, b"1234").unwrap();

    let workspace_storage_dir = temp.path().join("workspaces");
    let workspace_metadata = workspace_metadata_path(&workspace_storage_dir, hash);
    std::fs::create_dir_all(workspace_metadata.parent().unwrap()).unwrap();
    std::fs::write(&workspace_metadata, b"123").unwrap();
    let source = KiroSource {
        agent_dir: temp.path().join("agent"),
        workspace_storage_dir,
        user_registered_roots: None,
        project_matchers: ProjectRootMatcherCache::default(),
    };

    assert_eq!(source.snapshot_input_bytes(&transcript).unwrap(), 7);
}

fn message(ordinal: i64) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: "native-message-1".to_string(),
        session_id: "kiro-session-1".to_string(),
        role: "assistant".to_string(),
        timestamp: Some(1_800_000_000),
        ordinal,
        text: "Redacted response".to_string(),
        kind: Some("message".to_string()),
        model: Some("redacted-model".to_string()),
        tool_names: Some("read_file".to_string()),
        source_path: None,
        source_offset: Some(ordinal),
        metadata_json: Some(serde_json::json!({"projectId": "project-1"}).to_string()),
    }
}

#[test]
fn snapshot_records_build_canonical_capture_requests() {
    let first = normalize_kiro_snapshot_observations(&[message(0)]).unwrap();
    let prior = normalize_kiro_snapshot_observations(&[message(3)]).unwrap();
    let moved = normalize_kiro_snapshot_observations(&[message(4)]).unwrap();
    assert_eq!(first[0].native_record_id(), moved[0].native_record_id());
    assert_eq!(first[0].order(), 0);
    assert_eq!(moved[0].order(), 4);

    let scope = ObservationScopeV1::Profile;
    let generation = ObservationSourceGenerationV1::new(7).unwrap();
    first[0]
        .capture_request(
            scope.clone(),
            generation,
            None,
            ObservationCancellation::default(),
        )
        .expect("first Kiro SnapshotOrder request");

    let expected_cursor = prior[0]
        .cursor_after(scope.clone(), generation)
        .expect("typed post-record cursor");
    moved[0]
        .capture_request(
            scope,
            generation,
            Some(expected_cursor),
            ObservationCancellation::default(),
        )
        .expect("continued Kiro SnapshotOrder request");
}

#[test]
fn host_admission_failures_use_bounded_ingest_reason_codes() {
    let error = host_admission_error(
        PROVIDER,
        HostAdmissionOutcome {
            status: HostAdmissionStatus::Unavailable,
            retryable: true,
            reason_code: Some("authority_unavailable"),
            recovery: None,
            storage_cause: None,
        },
    );
    assert!(matches!(
        error,
        TranscriptIngestError::HostAdmission {
            provider: PROVIDER,
            reason: "authority_unavailable",
            retryable: true,
            ..
        }
    ));
}

#[test]
fn snapshot_normalization_emits_only_redacted_canonical_evidence() {
    let native = serde_json::json!({
        "provider": "kiro",
        "session_id": "redacted-session",
        "message_id": "redacted-message",
        "role": "assistant",
        "timestamp": 1_800_000_000_i64,
        "ordinal": 4,
        "kind": "message",
        "model": "redacted-model",
        "text": "Redacted response",
        // Untyped bags / content-without-visibility must not invent facts.
        "reasoning": "Redacted reasoning",
        "git": {"commit": "redacted"},
        "workflow": {"task": "redacted"},
        "source_path": "/must-not-survive",
        "cwd": "/must-not-survive",
        "metadata": {"must-not-survive": true},
    });
    let range = ObservationSourceRangeV1::new(4, 5).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &serde_json::to_vec(&native).unwrap(),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            canonical_snapshot_envelope(
                &native,
                "kiro",
                "redacted-session",
                "redacted-message",
                range,
            )
        },
    )
    .expect("redacted Kiro canonical envelope");
    let canonical = parsed.value();
    assert_eq!(canonical["provider"], "kiro");
    assert_eq!(canonical["stable_record_id"], "redacted-message");
    assert_eq!(canonical["relations"]["session_id"], "redacted-session");
    assert_eq!(canonical["relations"]["message_id"], "redacted-message");
    assert!(canonical["relations"].get("thread_id").is_none());
    assert!(canonical["relations"].get("turn_id").is_none());
    assert_eq!(canonical["evidence"]["ordering_domain"], "snapshot_order");
    assert_eq!(canonical["evidence"]["range"]["start"], 4);
    assert_eq!(canonical["facts"].as_array().unwrap().len(), 1);
    let encoded = canonical.to_string();
    assert!(!encoded.contains("must-not-survive"));
    assert!(!encoded.contains("source_path"));
    assert!(!encoded.contains("metadata"));
    assert!(!encoded.contains("Redacted reasoning"));
}

#[test]
fn hostile_lookalike_fields_remain_absent() {
    // The Kiro fixtures in tests/transcript_ingest_suite/kiro.rs contain
    // role/content plus transcript-level identity/model/time only. These
    // lookalike keys have no fixture-backed Kiro semantics.
    let entry = serde_json::json!({
        "role": "assistant",
        "content": "echoed protocol noise",
        "threadId": "thread-native-1",
        "turnId": "turn-native-1",
        "agentId": "agent-native-1",
        "parentAgentId": "parent-agent-1",
        "parentMessageId": "parent-msg-1",
        "tool_calls": [{
            "id": "call-read-1",
            "function": {
                "name": "read_file",
                "arguments": "{\"path\":\"src/billing.rs\"}"
            }
        }],
        "reasoning": "check the invoice join",
        "reasoning_visibility": "visible",
        "git": {
            "evidence_kind": "commit",
            "reference": "abc123"
        },
        "workflow": {
            "evidence_kind": "task",
            "reference": "kiro-workflow-1"
        },
        "tool_result": {
            "invocation_id": "call-read-1",
            "content": "arbitrary result",
            "success": true
        },
        "usage": {"input_tokens": 999_999}
    });
    let metadata = message_metadata(&entry, None);
    // V1 metadata may retain a sibling tool_calls bag, but snapshot shaping
    // must not promote that uncontracted object into canonical evidence.
    let message = SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: "kiro-session:msg-native-1".to_string(),
        session_id: "kiro-session".to_string(),
        role: "assistant".to_string(),
        timestamp: Some(1_800_000_000),
        ordinal: 2,
        text: "echoed protocol noise".to_string(),
        kind: Some("message".to_string()),
        model: Some("redacted-model".to_string()),
        tool_names: None,
        source_path: None,
        source_offset: Some(2),
        metadata_json: Some(metadata.to_string()),
    };
    let records = normalize_kiro_snapshot_observations(&[message]).unwrap();
    let native: Value = serde_json::from_slice(&records[0].payload).unwrap();
    for key in [
        "thread_id",
        "turn_id",
        "agent_id",
        "parent_agent_id",
        "parent_message_id",
        "reasoning_visibility",
        "reasoning",
        "tool_calls",
        "tool_result",
        "usage",
        "git",
        "workflow",
    ] {
        assert!(native.get(key).is_none(), "{key} must remain absent");
    }

    let range = ObservationSourceRangeV1::new(2, 3).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &records[0].payload,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            canonical_snapshot_envelope(
                &native,
                PROVIDER,
                "kiro-session",
                "kiro-session:msg-native-1",
                range,
            )
        },
    )
    .expect("typed Kiro envelope");
    let canonical = parsed.value();
    let relations = canonical["relations"].as_object().unwrap();
    for key in [
        "thread_id",
        "turn_id",
        "agent_id",
        "parent_agent_id",
        "parent_message_id",
    ] {
        assert!(relations.get(key).is_none(), "{key} must remain absent");
    }
    let encoded = canonical.to_string();
    for rejected in [
        "thread-native-1",
        "turn-native-1",
        "agent-native-1",
        "check the invoice join",
        "call-read-1",
        "abc123",
        "kiro-workflow-1",
        "arbitrary result",
        "999999",
    ] {
        assert!(!encoded.contains(rejected), "{rejected} must not survive");
    }
    assert!(
        !encoded.contains("\"kind\":\"workflow_lifecycle\""),
        "Kiro hostile workflow lookalike must not emit WorkflowLifecycle"
    );
}

#[test]
fn fixture_backed_workspace_session_message_reaches_canonical_envelope() {
    // Exact modern workspace-session message shape from
    // tests/transcript_ingest_suite/kiro.rs::write_workspace_session_json.
    // Provider-parser path: modern_messages → normalize_kiro_snapshot_observations
    // → canonical_snapshot_envelope (not a hand-built canonical record).
    let input: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/kiro/workspace_session.input.json"
    ))
    .expect("Kiro golden input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/kiro/workspace_session.expected_envelope.json"
    ))
    .expect("Kiro golden expected envelope");
    let session_id = input["sessionId"].as_str().unwrap();
    let model = input["modelId"].as_str();
    let messages = modern_messages(
        input["messages"].as_array().unwrap(),
        session_id,
        Path::new("workspace-session.json"),
        model,
        Path::new("/tmp/project"),
    );
    let message = messages
        .into_iter()
        .find(|message| message.role == "assistant")
        .expect("Kiro golden assistant");
    let text = message.text.clone();
    let message_id = message.message_id.clone();
    assert!(
        message_id.starts_with("kiro.derived-message.v3."),
        "fallback identity must use the framed v3 domain, got {message_id}"
    );

    let records = normalize_kiro_snapshot_observations(&[message]).unwrap();
    let native: Value = serde_json::from_slice(&records[0].payload).unwrap();
    assert_eq!(native["provider"], PROVIDER);
    assert_eq!(native["role"], "assistant");
    assert_eq!(native["text"], text);
    assert_eq!(native["model"], "claude-sonnet-4.6");
    assert!(native.get("tool_calls").is_none());
    assert!(native.get("reasoning").is_none());
    assert!(native.get("metadata").is_none());

    let range = ObservationSourceRangeV1::new(1, 2).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &records[0].payload,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| canonical_snapshot_envelope(&native, PROVIDER, session_id, &message_id, range),
    )
    .expect("fixture-backed Kiro canonical envelope");
    let canonical = parsed.value();
    assert_eq!(canonical["version"], expected["version"]);
    assert_eq!(canonical["provider"], expected["provider"]);
    assert_eq!(
        canonical["native_record_kind"],
        expected["native_record_kind"]
    );
    assert_eq!(canonical["stable_record_id"], message_id);
    assert_eq!(
        canonical["relations"]["session_id"],
        expected["relations"]["session_id"]
    );
    assert_eq!(canonical["relations"]["message_id"], message_id);
    for absent in expected["relations"]["absent"].as_array().unwrap() {
        assert!(
            canonical["relations"]
                .get(absent.as_str().unwrap())
                .is_none()
        );
    }
    assert_eq!(canonical["evidence"], expected["evidence"]);
    assert_eq!(canonical["facts"], expected["facts"]);
    assert_eq!(text, expected["facts"][0]["content"].as_str().unwrap());
    assert!(
        canonical["facts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|fact| fact["kind"] != "workflow_lifecycle"),
        "Kiro workspace fixture has no native lifecycle evidence for WorkflowLifecycle"
    );
}

#[test]
fn native_message_ids_distinguish_delimiter_ambiguous_structural_tuples() {
    assert_eq!(format!("{}:{}", "a:b", "c"), format!("{}:{}", "a", "b:c"));
    let left = stable_message_id(
        "a:b",
        &serde_json::json!({"messageId": "c"}),
        "assistant",
        None,
        0,
        "ignored",
    );
    let right = stable_message_id(
        "a",
        &serde_json::json!({"messageId": "b:c"}),
        "assistant",
        None,
        0,
        "ignored",
    );
    assert_ne!(left, right);
    assert!(left.starts_with("kiro.message-id.v2.") && right.starts_with("kiro.message-id.v2."));
    assert_eq!(
        left,
        stable_message_id(
            "a:b",
            &serde_json::json!({"messageId": "c"}),
            "assistant",
            None,
            0,
            "ignored",
        ),
        "framed IDs must be deterministic for replay"
    );
}

#[test]
fn native_message_ids_remain_unhashed_provider_identity() {
    let id = stable_message_id(
        "sess-1",
        &serde_json::json!({"messageId": "native-xyz", "content": "ignored"}),
        "assistant",
        None,
        0,
        "ignored-for-native",
    );
    assert_eq!(id, "sess-1:native-xyz");
}

#[test]
fn derived_message_ids_encode_role_timestamp_and_semantic_occurrence() {
    let entry = serde_json::json!({"role": "assistant", "content": "stable body"});
    let first = stable_message_id(
        "sess-1",
        &entry,
        "assistant",
        Some(1_800_000_000),
        0,
        "stable body",
    );
    let reordered = stable_message_id(
        "sess-1",
        &entry,
        "assistant",
        Some(1_800_000_000),
        0,
        "stable body",
    );
    assert_eq!(first, reordered);
    assert!(first.starts_with("kiro.derived-message.v3."));
    assert_ne!(
        first,
        stable_message_id(
            "sess-1",
            &entry,
            "user",
            Some(1_800_000_000),
            0,
            "stable body",
        )
    );
    assert_ne!(
        first,
        stable_message_id(
            "sess-1",
            &entry,
            "assistant",
            Some(1_800_000_000),
            1,
            "stable body",
        )
    );
}
