use super::*;
use crate::runtime::snapshot_observation::SnapshotAdmissionRecord;

fn write_checked_in_native_task(tasks: &Path, project: &Path, api_filename: &str) -> PathBuf {
    let task = tasks.join("checked-in-native");
    std::fs::create_dir_all(&task).unwrap();
    let mut metadata: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/transcript_golden/cline_like/input/task_metadata.json"
    ))
    .unwrap();
    metadata["workspacePath"] = Value::String(project.to_string_lossy().into_owned());
    std::fs::write(
        task.join("task_metadata.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let fixture = match api_filename {
        "api_messages.json" => include_str!(
            "../../../../../../tests/fixtures/transcript_golden/cline_like/input/api_messages.json"
        ),
        "api_conversation_history.json" => include_str!(
            "../../../../../../tests/fixtures/transcript_golden/cline_like/input/api_conversation_history.json"
        ),
        other => panic!("unsupported checked-in Cline-family fixture {other}"),
    };
    let api = task.join(api_filename);
    std::fs::write(&api, fixture).unwrap();
    std::fs::write(
        task.join("ui_messages.json"),
        include_str!(
            "../../../../../../tests/fixtures/transcript_golden/cline_like/input/ui_messages.json"
        ),
    )
    .unwrap();
    api
}

#[tokio::test]
async fn checked_in_cline_family_snapshots_preserve_receipts_through_failures_and_replay() {
    use crate::admission::test_support::MemoryHostAdmission;

    for (provider, api_filename) in [
        ("cline", "api_conversation_history.json"),
        ("roo-code", "api_messages.json"),
        ("kilo", "api_conversation_history.json"),
    ] {
        let temp = tempfile::TempDir::new().expect("temp Cline-family storage");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let tasks = temp.path().join("tasks");
        let api = write_checked_in_native_task(&tasks, &project, api_filename);
        let source = ClineLikeSource {
            provider,
            storage_roots: vec![tasks],
            user_registered_roots: None,
            project_matchers: ProjectRootMatcherCache::default(),
            task_metadata: TaskMetadataCache::default(),
        };
        let admission = MemoryHostAdmission::default();

        let first = capture_cline_like_snapshot_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap_or_else(|error| panic!("{provider}: checked-in capture failed: {error}"));
        assert_eq!(first.stats.messages_upserted, 3, "{provider}");

        let observations = admission.observations();
        assert_eq!(observations.len(), 3, "{provider}");
        let receipts = observations
            .iter()
            .map(|item| {
                let envelope: CanonicalObservationEnvelopeV1 =
                    serde_json::from_value(item.observation().payload().clone())
                        .unwrap_or_else(|error| panic!("{provider}: canonical envelope: {error}"));
                assert_eq!(envelope.provider().as_str(), provider);
                item.commit_receipt().clone()
            })
            .collect::<Vec<_>>();

        for _ in 0..16 {
            capture_cline_like_snapshot_observations(
                &admission,
                &source,
                &project,
                ObservationScopeV1::Profile,
                None,
                &ObservationCancellation::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("{provider}: duplicate storm failed: {error}"));
        }
        assert_eq!(
            admission.observations().len(),
            3,
            "{provider}: duplicate storm must coalesce"
        );

        let original = std::fs::read(&api).unwrap();
        std::fs::write(&api, b"[{\"role\":}]").unwrap();
        let poison = capture_cline_like_snapshot_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .expect_err("malformed replacement must remain non-durable");
        assert!(
            matches!(
                poison,
                TranscriptIngestError::NonDurableRecord {
                    reason: "malformed snapshot JSON",
                    ..
                }
            ),
            "{provider}: {poison:?}"
        );
        std::fs::write(&api, original).unwrap();

        let cancelled = ObservationCancellation::default();
        cancelled.cancel();
        let cancelled_outcome = capture_cline_like_snapshot_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &cancelled,
        )
        .await
        .expect_err("cancelled capture must stop before persistence");
        assert!(
            matches!(
                cancelled_outcome,
                TranscriptIngestError::Cancelled {
                    provider: cancelled_provider
                } if cancelled_provider == provider
            ),
            "{provider}: {cancelled_outcome:?}"
        );

        let reopened = admission.clone();
        assert_eq!(
            reopened
                .observations()
                .iter()
                .map(|item| item.commit_receipt().clone())
                .collect::<Vec<_>>(),
            receipts,
            "{provider}: committed receipts changed across admission handoff"
        );
        let replay = capture_cline_like_snapshot_observations(
            &reopened,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap_or_else(|error| panic!("{provider}: replay failed: {error}"));
        assert_eq!(
            replay.stats.messages_upserted, 0,
            "{provider}: acknowledgement-boundary replay must be exact"
        );
    }
}

#[test]
fn snapshot_budget_counts_all_task_input_files_once() {
    let temp = tempfile::TempDir::new().expect("temp Cline task");
    let task_dir = temp.path().join("task-1");
    std::fs::create_dir_all(&task_dir).unwrap();
    let transcript = task_dir.join("api_conversation_history.json");
    std::fs::write(&transcript, b"12345").unwrap();
    std::fs::write(task_dir.join("ui_messages.json"), b"1234").unwrap();
    std::fs::write(task_dir.join("task_metadata.json"), b"123").unwrap();
    std::fs::write(task_dir.join("history_item.json"), b"12").unwrap();
    std::fs::write(task_dir.join("history.json"), b"1").unwrap();

    assert_eq!(snapshot_input_bytes("cline", &transcript).unwrap(), 15);
}

#[test]
fn snapshot_discovery_filters_scope_before_spending_byte_budget() {
    let temp = tempfile::TempDir::new().expect("temp Cline storage");
    let tasks = temp.path().join("tasks");
    let project = temp.path().join("project");
    let other = temp.path().join("other");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    for (task, cwd) in [("relevant", &project), ("unrelated", &other)] {
        let task_dir = tasks.join(task);
        std::fs::create_dir_all(&task_dir).unwrap();
        std::fs::write(task_dir.join("api_messages.json"), b"[]").unwrap();
        std::fs::write(
            task_dir.join("task_metadata.json"),
            serde_json::json!({"cwd": cwd}).to_string(),
        )
        .unwrap();
    }
    let source = ClineLikeSource {
        provider: "cline",
        storage_roots: vec![tasks],
        user_registered_roots: None,
        project_matchers: ProjectRootMatcherCache::default(),
        task_metadata: TaskMetadataCache::default(),
    };

    let paths = source.transcript_paths(&project);
    assert_eq!(paths.len(), 1);
    assert!(paths[0].ends_with("relevant/api_messages.json"));
}

#[tokio::test]
async fn byte_budget_charges_once_and_defers_second_before_parse() {
    use crate::admission::test_support::MemoryHostAdmission;

    let temp = tempfile::TempDir::new().expect("temp Cline storage");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let first_tasks = temp.path().join("first-tasks");
    let first_task = first_tasks.join("first");
    std::fs::create_dir_all(&first_task).unwrap();
    std::fs::write(
        first_task.join("api_messages.json"),
        serde_json::json!([{"role": "assistant", "content": "first"}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        first_task.join("task_metadata.json"),
        serde_json::json!({"cwd": project}).to_string(),
    )
    .unwrap();

    let second_tasks = temp.path().join("second-tasks");
    let second_task = second_tasks.join("hostile");
    std::fs::create_dir_all(&second_task).unwrap();
    let hostile = format!("[{}", "x".repeat(256));
    std::fs::write(second_task.join("api_messages.json"), &hostile).unwrap();
    std::fs::write(
        second_task.join("task_metadata.json"),
        serde_json::json!({"cwd": project}).to_string(),
    )
    .unwrap();
    let source = ClineLikeSource {
        provider: "cline",
        storage_roots: vec![first_tasks, second_tasks.clone()],
        user_registered_roots: None,
        project_matchers: ProjectRootMatcherCache::default(),
        task_metadata: TaskMetadataCache::default(),
    };
    let paths = source.transcript_paths(&project);
    assert_eq!(paths.len(), 2);
    let first_bytes = snapshot_input_bytes("cline", &paths[0]).unwrap();
    let second_bytes = snapshot_input_bytes("cline", &paths[1]).unwrap();

    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    let deferred = capture_cline_like_snapshot_observations(
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

    let second_only = ClineLikeSource {
        provider: "cline",
        storage_roots: vec![second_tasks],
        user_registered_roots: None,
        project_matchers: ProjectRootMatcherCache::default(),
        task_metadata: TaskMetadataCache::default(),
    };
    let err = capture_cline_like_snapshot_observations(
        &admission,
        &second_only,
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
async fn pre_cancelled_snapshot_capture_does_not_advance_cline_source() {
    use crate::admission::test_support::PanicHostAdmission;

    let temp = tempfile::TempDir::new().expect("temp Cline storage");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let task_dir = temp.path().join("tasks").join("cancelled");
    std::fs::create_dir_all(&task_dir).unwrap();
    std::fs::write(
        task_dir.join("api_messages.json"),
        serde_json::json!([{"role": "assistant", "content": "retry me"}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        task_dir.join("task_metadata.json"),
        serde_json::json!({"cwd": project}).to_string(),
    )
    .unwrap();
    let source = ClineLikeSource {
        provider: "cline",
        storage_roots: vec![temp.path().join("tasks")],
        user_registered_roots: None,
        project_matchers: ProjectRootMatcherCache::default(),
        task_metadata: TaskMetadataCache::default(),
    };
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let error = capture_cline_like_snapshot_observations(
        &PanicHostAdmission,
        &source,
        &project,
        ObservationScopeV1::Profile,
        None,
        &cancellation,
    )
    .await
    .expect_err("pre-cancelled Cline capture must stop before persistence");
    assert!(matches!(
        error,
        TranscriptIngestError::Cancelled { provider: "cline" }
    ));
}

fn message(provider: &str, ordinal: i64) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: provider.to_string(),
        message_id: "task-1:native-message-1".to_string(),
        session_id: "task-1".to_string(),
        role: "assistant".to_string(),
        timestamp: Some(1_800_000_000),
        ordinal,
        text: "Redacted response".to_string(),
        kind: Some("message".to_string()),
        model: Some("redacted-model".to_string()),
        tool_names: Some("read_file".to_string()),
        source_path: None,
        source_offset: Some(ordinal),
        metadata_json: Some(serde_json::json!({"task": "redacted"}).to_string()),
    }
}

#[test]
fn provider_identity_and_snapshot_order_feed_canonical_requests() {
    for provider in ["cline", "roo-code", "kilo"] {
        let first =
            normalize_cline_like_snapshot_observations(provider, &[message(provider, 0)]).unwrap();
        let prior =
            normalize_cline_like_snapshot_observations(provider, &[message(provider, 2)]).unwrap();
        let moved =
            normalize_cline_like_snapshot_observations(provider, &[message(provider, 3)]).unwrap();
        assert_eq!(first[0].provider(), provider);
        assert_eq!(first[0].native_record_id(), moved[0].native_record_id());
        assert_eq!(first[0].order(), 0);
        assert_eq!(moved[0].order(), 3);

        let scope = ObservationScopeV1::Profile;
        let generation = ObservationSourceGenerationV1::new(11).unwrap();
        first[0]
            .capture_request(
                scope.clone(),
                generation,
                None,
                ObservationCancellation::default(),
            )
            .expect("first Cline-like SnapshotOrder request");

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
            .expect("continued Cline-like SnapshotOrder request");
    }
}

#[test]
fn usage_snapshot_emits_only_the_usage_fact() {
    let mut usage = message("cline", 1);
    usage.kind = Some("usage".to_string());
    usage.text = serde_json::json!({"input_tokens": 777}).to_string();
    usage.model = None;
    usage.tool_names = None;
    usage.metadata_json = Some(serde_json::json!({"usage": {"input_tokens": 777}}).to_string());

    let records = normalize_cline_like_snapshot_observations("cline", &[usage]).unwrap();
    let range = ObservationSourceRangeV1::new(1, 2).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &records[0].payload,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            canonical_snapshot_envelope(
                &native,
                "cline",
                "task-1",
                records[0].native_record_id(),
                range,
            )
        },
    )
    .expect("usage snapshot envelope");

    let facts = parsed.value()["facts"].as_array().unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["kind"], "uncorrelated_usage");
    assert_eq!(facts[0]["input_tokens"], 777);
    assert_eq!(facts[0]["native_kind"], "usage");
    assert_eq!(facts[0]["native_field"], "usage");
    assert_eq!(
        facts[0]["missing_dimensions"],
        serde_json::json!(["model", "scope", "counter_semantics", "correlation"])
    );
}

#[test]
fn host_admission_failures_preserve_provider_with_bounded_reason_codes() {
    for provider in ["cline", "roo-code", "kilo"] {
        let error = host_admission_error(
            provider,
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
                provider: error_provider,
                reason: "authority_unavailable",
                retryable: true,
                ..
            } if error_provider == provider
        ));
    }
}

#[test]
fn snapshot_normalization_preserves_roo_code_without_generic_metadata() {
    let native = serde_json::json!({
        "provider": "cline",
        "session_id": "forged-task",
        "message_id": "forged-message",
        "role": "assistant",
        "timestamp": 1_800_000_000_i64,
        "ordinal": 7,
        "kind": "message",
        "model": "redacted-model",
        "text": "Redacted response",
        "tool_names": "read_file",
        "usage": {"input_tokens": 12, "output_tokens": 3},
        // Untyped bags / content-without-visibility must not invent facts.
        "reasoning": "Redacted reasoning",
        "git": {"commit": "redacted"},
        "workflow": {"task": "redacted"},
        "source_path": "/must-not-survive",
        "cwd": "/must-not-survive",
        "metadata": {"must-not-survive": true},
    });
    let range = ObservationSourceRangeV1::new(7, 8).unwrap();
    let parsed = parse_normalized_observation_record_v1(
        &serde_json::to_vec(&native).unwrap(),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            canonical_snapshot_envelope(
                &native,
                "roo-code",
                "redacted-task",
                "redacted-task:message",
                range,
            )
        },
    )
    .expect("redacted Roo Code canonical envelope");
    let canonical = parsed.value();
    assert_eq!(canonical["provider"], "roo-code");
    assert_eq!(canonical["stable_record_id"], "redacted-task:message");
    assert_eq!(canonical["relations"]["session_id"], "redacted-task");
    assert_eq!(
        canonical["relations"]["message_id"],
        "redacted-task:message"
    );
    assert!(canonical["relations"].get("thread_id").is_none());
    assert_eq!(canonical["evidence"]["ordering_domain"], "snapshot_order");
    assert_eq!(canonical["evidence"]["range"]["start"], 7);
    // message + tool_names fallback + usage; no invented reasoning/git/workflow
    assert_eq!(canonical["facts"].as_array().unwrap().len(), 3);
    let encoded = canonical.to_string();
    assert!(!encoded.contains("must-not-survive"));
    assert!(!encoded.contains("source_path"));
    assert!(!encoded.contains("metadata"));
    assert!(!encoded.contains("Redacted reasoning"));
}

const GOLDEN_API_HISTORY: &str = include_str!(
    "../../../../../../tests/fixtures/transcript_golden/cline_like/input/api_conversation_history.json"
);
const GOLDEN_API_MESSAGES: &str = include_str!(
    "../../../../../../tests/fixtures/transcript_golden/cline_like/input/api_messages.json"
);
const GOLDEN_EXPECTED_ASSISTANT: &str = include_str!(
    "../../../../../../tests/fixtures/transcript_golden/cline_like/expected/assistant_tool_use.canonical.json"
);
const GOLDEN_PARSER_PROVENANCE: &str = include_str!(
    "../../../../../../tests/fixtures/transcript_golden/cline_like/expected/parser_provenance.json"
);

#[test]
fn fixture_backed_tool_use_name_reaches_canonical_facts() {
    // Checked-in golden input (same shape as write_task). Roo's api_messages.json
    // twin must stay byte-equivalent to the shared Cline/Kilo history fixture.
    let history: Value = serde_json::from_str(GOLDEN_API_HISTORY).expect("golden api history JSON");
    let roo_twin: Value =
        serde_json::from_str(GOLDEN_API_MESSAGES).expect("golden Roo api_messages JSON");
    assert_eq!(
        history, roo_twin,
        "Roo api_messages.json must mirror the shared Cline-family history shape"
    );
    let expected: Value =
        serde_json::from_str(GOLDEN_EXPECTED_ASSISTANT).expect("golden expected envelope");
    let provenance: Value =
        serde_json::from_str(GOLDEN_PARSER_PROVENANCE).expect("golden parser provenance");
    assert_eq!(
        provenance["ordering_domain"], "snapshot_order",
        "parser provenance must declare SnapshotOrder"
    );
    assert_eq!(
        provenance["unknown_version"]["emitted"], false,
        "Cline-family protocol is unversioned — do not invent UnknownVersion"
    );

    let entries = history.as_array().expect("history array");
    let entry = &entries[1];
    assert_eq!(
        entry["content"][1]["type"], "tool_use",
        "golden must evidence content[].type=tool_use parser path"
    );

    for provider in ["cline", "roo-code", "kilo"] {
        let api_name = expected["per_provider"][provider]["api_history_filename"]
            .as_str()
            .expect("per-provider api filename");
        let message = message_from_entry(
            provider,
            entry,
            "task-1",
            Path::new(api_name),
            1,
            Path::new("/tmp/project"),
            &mut BTreeMap::new(),
        )
        .expect("fixture-backed assistant message");
        assert_eq!(
            message.provider, provider,
            "{provider}: parser must tag provider"
        );
        assert_eq!(message.tool_names.as_deref(), Some("read_file"));

        let records = normalize_cline_like_snapshot_observations(provider, &[message]).unwrap();
        let native: Value = serde_json::from_slice(&records[0].payload).unwrap();
        assert_eq!(native["provider"], provider);
        assert_eq!(
            native["tool_names"],
            expected["parser_derived_native_payload"]["tool_names"]
        );
        for absent in expected["parser_derived_native_payload"]["absent"]
            .as_array()
            .expect("absent native keys")
        {
            let key = absent.as_str().expect("absent key");
            assert!(
                native.get(key).is_none(),
                "{provider}: parser must not invent {key}"
            );
        }

        let range = ObservationSourceRangeV1::new(1, 2).unwrap();
        let parsed = parse_normalized_observation_record_v1(
            &records[0].payload,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            |native| {
                canonical_snapshot_envelope(
                    &native,
                    provider,
                    "task-1",
                    records[0].native_record_id(),
                    range,
                )
            },
        )
        .expect("fixture-backed tool-name envelope");
        let canonical = parsed.value();
        assert_eq!(canonical["provider"], provider);
        assert_eq!(canonical["version"], 1);
        assert_eq!(
            canonical["evidence"]["ordering_domain"],
            expected["assistant_envelope"]["evidence"]["ordering_domain"]
        );
        assert_eq!(
            canonical["evidence"]["native_timestamp"],
            expected["assistant_envelope"]["evidence"]["native_timestamp"]
        );
        for absent in expected["assistant_envelope"]["relations"]["absent"]
            .as_array()
            .expect("absent relations")
        {
            let key = absent.as_str().expect("relation key");
            assert!(
                canonical["relations"].get(key).is_none(),
                "{provider}: {key} must stay absent"
            );
        }
        let encoded = canonical.to_string();
        for needle in expected["assistant_envelope"]["encoded_must_contain"]
            .as_array()
            .expect("must_contain")
        {
            let needle = needle.as_str().expect("needle");
            assert!(
                encoded.contains(needle),
                "{provider}: envelope missing parser evidence {needle}"
            );
        }
        for needle in expected["assistant_envelope"]["encoded_must_not_contain"]
            .as_array()
            .expect("must_not_contain")
        {
            let needle = needle.as_str().expect("needle");
            assert!(
                !encoded.contains(needle),
                "{provider}: envelope must not contain {needle}"
            );
        }
        assert!(
            !encoded.contains("\"kind\":\"workflow_lifecycle\""),
            "{provider}: checked-in tool_use fixture must not emit WorkflowLifecycle"
        );
    }
}

#[test]
fn hostile_lookalike_fields_remain_absent_for_all_variants() {
    let entry = serde_json::json!({
        "role": "assistant",
        "content": "protocol echo",
        "requestId": "req-1",
        "threadId": "hostile-thread",
        "turnId": "hostile-turn",
        "agentId": "hostile-agent",
        "parentAgentId": "hostile-parent-agent",
        "parentMessageId": "hostile-parent-message",
        "reasoning": "hostile reasoning",
        "reasoning_visibility": "visible",
        "git": {"evidence_kind": "commit", "reference": "hostile-commit"},
        "workflow": {"evidence_kind": "task", "reference": "hostile-task"},
        "tool_calls": [{
            "id": "hostile-call",
            "name": "hostile_tool",
            "arguments": {"secret": "hostile-arguments"}
        }],
        "tool_result": {
            "invocation_id": "hostile-call",
            "content": "hostile-result",
            "success": true
        }
    });
    for provider in ["cline", "roo-code", "kilo"] {
        let metadata = message_metadata(provider, &entry, Path::new("/tmp/p"));
        let message = SessionMessageRecord {
            provider: provider.to_string(),
            message_id: format!("{provider}-message"),
            session_id: "task-9".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(1_800_000_000),
            ordinal: 0,
            text: "protocol echo".to_string(),
            kind: Some("message".to_string()),
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: Some(0),
            metadata_json: Some(metadata.to_string()),
        };
        let records = normalize_cline_like_snapshot_observations(provider, &[message]).unwrap();
        let native: Value = serde_json::from_slice(&records[0].payload).unwrap();
        for key in [
            "thread_id",
            "turn_id",
            "agent_id",
            "parent_agent_id",
            "parent_message_id",
            "reasoning_visibility",
            "reasoning",
            "git",
            "workflow",
            "tool_calls",
            "tool_result",
        ] {
            assert!(
                native.get(key).is_none(),
                "{provider}: {key} must be absent"
            );
        }

        let range = ObservationSourceRangeV1::new(0, 1).unwrap();
        let parsed = parse_normalized_observation_record_v1(
            &records[0].payload,
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            |native| {
                canonical_snapshot_envelope(
                    &native,
                    provider,
                    "task-9",
                    records[0].native_record_id(),
                    range,
                )
            },
        )
        .expect("hostile lookalikes must normalize without invented facts");
        let canonical = parsed.value();
        let relations = canonical["relations"].as_object().unwrap();
        for key in [
            "thread_id",
            "turn_id",
            "agent_id",
            "parent_agent_id",
            "parent_message_id",
        ] {
            assert!(
                relations.get(key).is_none(),
                "{provider}: {key} must be absent"
            );
        }
        let encoded = canonical.to_string();
        for rejected in [
            "hostile-thread",
            "hostile-turn",
            "hostile-agent",
            "hostile reasoning",
            "hostile-commit",
            "hostile-task",
            "hostile-call",
            "hostile_tool",
            "hostile-arguments",
            "hostile-result",
        ] {
            assert!(
                !encoded.contains(rejected),
                "{provider}: {rejected} must not survive"
            );
        }
        assert!(
            !encoded.contains("\"kind\":\"workflow_lifecycle\""),
            "{provider}: workflow lookalike must not emit WorkflowLifecycle"
        );
    }
}

#[test]
fn native_message_ids_distinguish_delimiter_ambiguous_structural_tuples() {
    assert_eq!(format!("{}:{}", "a:b", "c"), format!("{}:{}", "a", "b:c"));
    let left = stable_message_id(
        "a:b",
        "api-message",
        Some("c"),
        Some(1),
        "assistant",
        0,
        "ignored",
    );
    let right = stable_message_id(
        "a",
        "api-message",
        Some("b:c"),
        Some(1),
        "assistant",
        0,
        "ignored",
    );
    assert_ne!(left, right);
    assert!(
        left.starts_with("cline-like.message-id.v2.")
            && right.starts_with("cline-like.message-id.v2.")
    );
    assert_eq!(
        left,
        stable_message_id(
            "a:b",
            "api-message",
            Some("c"),
            Some(1),
            "assistant",
            0,
            "ignored",
        ),
        "framed IDs must be deterministic for replay"
    );
}

#[test]
fn native_message_ids_remain_unhashed_provider_identity() {
    let id = stable_message_id(
        "task-1",
        "api-message",
        Some("native-xyz"),
        Some(1_800_000_000),
        "assistant",
        0,
        "ignored-for-native",
    );
    assert_eq!(id, "task-1:native-xyz");
}

#[test]
fn derived_message_ids_encode_timestamp_and_semantic_occurrence() {
    let first = stable_message_id(
        "task-1",
        "api-message",
        None,
        Some(1_800_000_000),
        "assistant",
        0,
        "stable body",
    );
    let reordered = stable_message_id(
        "task-1",
        "api-message",
        None,
        Some(1_800_000_000),
        "assistant",
        0,
        "stable body",
    );
    assert_eq!(first, reordered);
    assert!(first.starts_with("cline-like.derived-message.v3."));
    assert_ne!(
        first,
        stable_message_id(
            "task-1",
            "api-message",
            None,
            Some(1_800_000_000),
            "assistant",
            1,
            "stable body",
        )
    );
}
