use super::*;
use serde_json::json;

#[test]
fn native_record_identity_is_stable_across_json_formatting() {
    let compact: Value =
        serde_json::from_str(r#"{"role":"assistant","message":{"content":"redacted fixture"}}"#)
            .unwrap();
    let spaced: Value = serde_json::from_str(
        r#"{ "message": { "content": "redacted fixture" }, "role": "assistant" }"#,
    )
    .unwrap();
    assert_eq!(
        observation_native_record_id("session-redacted", &compact)
            .unwrap()
            .as_str(),
        observation_native_record_id("session-redacted", &spaced)
            .unwrap()
            .as_str()
    );
}

#[test]
fn no_id_collision_retry_is_bound_to_the_exact_source_occurrence() {
    let native = json!({
        "role": "assistant",
        "message": {"content": "identical no-id record"}
    });
    let first_range = tracedecay_domain::ObservationSourceRangeV1::new(0, 80).unwrap();
    let repeated_range = tracedecay_domain::ObservationSourceRangeV1::new(80, 160).unwrap();

    let (first_primary, first_eligible) =
        cursor_admission_record_id(&native, "session-repeat", first_range, false).unwrap();
    let (repeated_primary, repeated_eligible) =
        cursor_admission_record_id(&native, "session-repeat", repeated_range, false).unwrap();
    let (first_retry, first_retry_eligible) =
        cursor_admission_record_id(&native, "session-repeat", first_range, true).unwrap();
    let (repeated_retry, repeated_retry_eligible) =
        cursor_admission_record_id(&native, "session-repeat", repeated_range, true).unwrap();

    assert_eq!(first_primary, repeated_primary);
    assert!(first_eligible);
    assert!(repeated_eligible);
    assert_ne!(first_retry, repeated_retry);
    assert_ne!(first_retry, first_primary);
    assert!(!first_retry_eligible);
    assert!(!repeated_retry_eligible);
}

#[test]
fn native_id_content_conflict_has_no_positional_retry() {
    let first = json!({
        "id": "cursor-native-conflict",
        "role": "assistant",
        "message": {"content": "first content"}
    });
    let changed = json!({
        "id": null,
        "role": "assistant",
        "message": {
            "id": "cursor-native-conflict",
            "content": "changed content"
        }
    });
    let first_range = tracedecay_domain::ObservationSourceRangeV1::new(0, 80).unwrap();
    let changed_range = tracedecay_domain::ObservationSourceRangeV1::new(80, 160).unwrap();

    let (first_id, first_eligible) =
        cursor_admission_record_id(&first, "session-conflict", first_range, false).unwrap();
    let (changed_id, changed_eligible) =
        cursor_admission_record_id(&changed, "session-conflict", changed_range, false).unwrap();

    assert_eq!(first_id, changed_id);
    assert!(!first_eligible);
    assert!(!changed_eligible);
    assert!(cursor_admission_record_id(&changed, "session-conflict", changed_range, true).is_err());
}

#[test]
fn canonical_record_is_stable_across_hook_sweep_and_mtime_context() {
    let transcript_path = Path::new("/redacted/project/session.fixture.jsonl");
    let hook_context = cursor_observation_context(
        &json!({
            "cwd": "/redacted/project",
            "conversation_id": "route-only-conversation",
            "model": "route-only-model"
        }),
        transcript_path,
        false,
    );
    let sweep_context = cursor_observation_context(
        &cursor_sweep_event("session.fixture", Path::new("/redacted/project"), false),
        transcript_path,
        false,
    );
    let native = json!({
        "role": "assistant",
        "message": {"content": "stable transcript content"}
    });
    let record_id = observation_native_record_id("session.fixture", &native).unwrap();
    let range = tracedecay_domain::ObservationSourceRangeV1::new(10, 90).unwrap();

    let hook = normalize_cursor_observation_with_message_id(
        &cursor_native_with_context(native.clone(), &hook_context, None, None),
        "session.fixture",
        record_id.clone(),
        record_id.clone(),
        range,
        None,
        None,
    )
    .unwrap();
    let sweep = normalize_cursor_observation_with_message_id(
        &cursor_native_with_context(native, &sweep_context, None, None),
        "session.fixture",
        record_id.clone(),
        record_id,
        range,
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        serde_json::to_value(hook).unwrap(),
        serde_json::to_value(sweep).unwrap()
    );
}

#[test]
fn cursor_subagent_lineage_sets_native_agent_relations() {
    let native = json!({
        "role": "assistant",
        "conversation_id": "child-agent",
        "message": {"content": [{"type": "text", "text": "subagent reply"}]}
    });
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 20).unwrap();
    let record_id = observation_native_record_id("child-agent", &native).unwrap();
    let envelope = normalize_cursor_observation(
        &native,
        "child-agent",
        record_id,
        range,
        Some("child-agent"),
        Some("parent-conversation"),
    )
    .unwrap();
    let relations = serde_json::to_value(envelope.relations()).unwrap();
    assert_eq!(relations["thread_id"], "child-agent");
    assert_eq!(relations["agent_id"], "child-agent");
    assert_eq!(relations["parent_agent_id"], "parent-conversation");
    assert!(relations.get("turn_id").is_none());
}

/// Exact assistant+`tool_use` JSONL shape from
/// `tests/transcript_ingest_suite/cursor.rs`
/// (`cursor_tool_use_blocks_populate_tool_event_metadata`). Provider-parser
/// evidence is the native `role`/`message.content[]` Cursor transcript
/// record; the expected output is the canonical envelope projection with
/// explicit Cursor provider provenance, not a generic hand-built record.
#[test]
fn fixture_backed_cursor_jsonl_tool_use_reaches_canonical_envelope() {
    let native: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor/tool_use.input.json"
    ))
    .expect("Cursor golden input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor/tool_use.expected_envelope.json"
    ))
    .expect("Cursor golden expected envelope");
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 64).unwrap();
    let record_id = observation_native_record_id("cursor-tool-fixture", &native).unwrap();
    let envelope = normalize_cursor_observation(
        &native,
        "cursor-tool-fixture",
        record_id.clone(),
        range,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        envelope.provider().as_str(),
        expected["provider"].as_str().unwrap()
    );
    assert_eq!(
        envelope.native_record_kind(),
        expected["native_record_kind"].as_str().unwrap()
    );
    assert_eq!(envelope.stable_record_id().as_str(), record_id.as_str());
    let actual = serde_json::to_value(&envelope).unwrap();
    assert_eq!(actual["version"], expected["version"]);
    assert_eq!(actual["evidence"], expected["evidence"]);
    let relations = actual["relations"].as_object().unwrap();
    assert_eq!(relations["session_id"], expected["relations"]["session_id"]);
    assert_eq!(relations["message_id"], record_id.as_str());
    for absent in expected["relations"]["absent"].as_array().unwrap() {
        assert!(relations.get(absent.as_str().unwrap()).is_none());
    }
    let facts = actual["facts"].as_array().unwrap();
    assert!(facts.iter().any(|fact| fact["kind"] == "session"));
    assert!(facts.iter().any(|fact| {
        fact["kind"] == "message" && fact["content"] == native["message"]["content"]
    }));
    assert!(facts.iter().any(|fact| {
        fact["kind"] == "tool_invocation"
            && fact["arguments"] == native["message"]["content"][1]["input"]
    }));
    assert!(
        envelope
            .facts()
            .iter()
            .all(|fact| { !matches!(fact, CanonicalObservationFactV1::WorkflowLifecycle { .. }) }),
        "Cursor JSONL fixture must not emit WorkflowLifecycle without native lifecycle evidence"
    );
}

#[test]
fn fixture_backed_cursor_workflow_lookalike_emits_no_workflow_lifecycle() {
    let native: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor/workflow_lookalike.input.json"
    ))
    .expect("Cursor workflow lookalike input");
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/cursor/workflow_lookalike.expected_envelope.json"
    ))
    .expect("Cursor workflow lookalike expected");
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 64).unwrap();
    let record_id = observation_native_record_id("cursor-workflow-lookalike", &native).unwrap();
    let envelope = normalize_cursor_observation(
        &native,
        "cursor-workflow-lookalike",
        record_id,
        range,
        None,
        None,
    )
    .unwrap();
    let actual = serde_json::to_value(&envelope).unwrap();
    let facts = actual["facts"].as_array().unwrap();
    assert!(facts.iter().any(|fact| {
        fact["kind"] == "message"
            && fact["content"].as_str() == expected["expected_message"].as_str()
    }));
    for forbidden in expected["forbidden_fact_kinds"].as_array().unwrap() {
        assert!(
            facts.iter().all(|fact| fact["kind"] != *forbidden),
            "forbidden fact kind {forbidden} must remain absent"
        );
    }
    assert!(
        envelope
            .facts()
            .iter()
            .all(|fact| { !matches!(fact, CanonicalObservationFactV1::WorkflowLifecycle { .. }) }),
        "Cursor JSONL workflow lookalikes must not become WorkflowLifecycle"
    );
    let rendered = actual.to_string();
    for rejected in expected["encoded_must_not_contain"].as_array().unwrap() {
        assert!(
            !rendered.contains(rejected.as_str().unwrap()),
            "{rejected} must not survive Cursor JSONL normalization"
        );
    }
}

#[test]
fn every_batch_of_a_rewritten_transcript_keeps_one_replacement_namespace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-redacted.jsonl");
    let event = json!({"session_id": "session-redacted"});
    std::fs::write(
        &path,
        "{\"role\":\"user\",\"content\":\"first generation\"}\n",
    )
    .unwrap();

    let first = parse_cursor_jsonl(
        &event,
        "session-redacted",
        &path,
        StoredCursor::default(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(first.messages.len(), 1);
    assert!(!first.messages[0].message_id.contains(":generation:"));

    // Truncate-and-rewrite, then read the replacement one record per batch
    // so the second batch no longer starts at the file head.
    std::fs::write(
        &path,
        "{\"role\":\"user\",\"content\":\"replacement head\"}\n\
         {\"role\":\"user\",\"content\":\"replacement tail\"}\n",
    )
    .unwrap();

    let head = parse_cursor_jsonl(
        &event,
        "session-redacted",
        &path,
        first.new_cursor,
        Some(1),
        false,
    )
    .unwrap();
    assert_eq!(head.messages.len(), 1);
    let suffix = format!(":generation:{}", head.new_cursor.file_id);
    assert!(head.messages[0].message_id.ends_with(&suffix));

    let tail = parse_cursor_jsonl(
        &event,
        "session-redacted",
        &path,
        head.new_cursor,
        Some(1),
        false,
    )
    .unwrap();
    assert_eq!(tail.messages.len(), 1);
    // Without the stored generation the tail would re-mint the bare
    // `<session>:<offset>` id and overwrite retained pre-rewrite history.
    assert!(tail.messages[0].message_id.ends_with(&suffix));
    assert_ne!(tail.messages[0].message_id, first.messages[0].message_id);
}

#[test]
fn user_scope_selects_one_physical_authority_for_a_mirrored_session() {
    let home = tempfile::tempdir().unwrap();
    let first = home
        .path()
        .join(".cursor/projects/unregistered-a/agent-transcripts/session-mirrored.jsonl");
    let second = home
        .path()
        .join(".cursor/projects/unregistered-b/agent-transcripts/session-mirrored.jsonl");
    std::fs::create_dir_all(first.parent().unwrap()).unwrap();
    std::fs::create_dir_all(second.parent().unwrap()).unwrap();
    std::fs::write(&first, "{\"role\":\"user\",\"content\":\"first mirror\"}\n").unwrap();
    std::fs::write(
        &second,
        "{\"role\":\"user\",\"content\":\"second mirror\"}\n",
    )
    .unwrap();

    let source = CursorSweepSource::with_home(home.path())
        .for_user_scope(&[PathBuf::from("/registered/project")]);
    let paths = source.transcript_paths(Path::new(""));

    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0].file_stem().and_then(|stem| stem.to_str()),
        Some("session-mirrored")
    );
}

/// Fixture for the replayed-ingest journey: one project-scoped Cursor hook
/// event whose transcript already carries two records.
fn cursor_replay_fixture() -> (tempfile::TempDir, String, ProjectId) {
    // Production installs the process-wide capture authorities during daemon
    // bootstrap; capture refuses with a typed `BackgroundResourceUnavailable`
    // without them.
    crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
    let project = tempfile::tempdir().unwrap();
    let transcript = project.path().join("cursor-replayed.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Edit the shared file.\"}]}}\n",
            "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Saved src/lib.rs.\"}]}}\n"
        ),
    )
    .unwrap();
    let event = json!({
        "session_id": "session-replayed",
        "conversation_id": "conversation-replayed",
        "generation_id": "generation-replayed",
        "transcript_path": transcript,
        "workspace_roots": [project.path()],
    })
    .to_string();
    let project_id = ProjectId::new("project.cursor-replayed").unwrap();
    (project, event, project_id)
}

/// A pass that admits observations and then loses the projection queue to a
/// peer drainer still committed those observations.
///
/// This is the production interleaving on a slow runner: the explicit hook
/// ingest admits the transcript, the project catch-up sweep's scheduler tick
/// drains the scope-wide projection queue, and the ingest's own drain then
/// finds nothing left. Reporting only the projections this pass drained itself
/// turns a real commit into a terminal, non-retryable `accepted_for_replay`.
#[tokio::test]
async fn cursor_ingest_reports_its_commit_when_a_peer_drains_the_projection_queue() {
    let (_project, event, project_id) = cursor_replay_fixture();
    let admission = crate::admission::test_support::MemoryHostAdmission::default();
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };

    // Admit exactly as the hook ingest does, then let a peer empty the queue
    // before the ingest's own drain can run.
    let transcript: PathBuf = serde_json::from_str::<Value>(&event).unwrap()["transcript_path"]
        .as_str()
        .map(PathBuf::from)
        .unwrap();
    let source_event: Value = serde_json::from_str(&event).unwrap();
    let context = cursor_observation_context(&source_event, &transcript, false);
    let progress = admit_cursor_jsonl_observations(
        "session-replayed",
        &transcript,
        &context,
        &admission,
        &scope,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    assert!(
        progress.frames_persisted > 0,
        "the admit persists the transcript frames: {progress:?}"
    );
    let peer = projection::drain_cursor_observation_projections(
        &admission,
        &scope,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    assert!(
        peer.messages_upserted > 0,
        "the peer drainer takes the queued rows: {peer:?}"
    );

    // The ingest now re-scans an exhausted source against an empty queue.
    let stats = try_ingest_cursor_transcript_event_capped_with_admission(
        &event, project_id, &admission, None,
    )
    .await
    .unwrap();
    assert_eq!(
        stats.messages_upserted, 0,
        "the peer already projected these rows: {stats:?}"
    );
    assert!(
        stats.observations_committed > 0 || stats.exact_duplicate,
        "an ingest whose observations are durable must not look like a pass that captured nothing: {stats:?}"
    );
}

/// A pass whose observations a peer drainer already projected must not report
/// the same zero-change accounting as a pass that captured nothing.
///
/// The daemon's project catch-up sweep drains the whole Cursor projection
/// queue for a scope, not just the rows it admitted itself, and projection
/// consumes the queue row. So an explicit hook ingest that admitted on a
/// deferred first call can find the queue empty on its next call even though
/// its own observations are durably committed. Reported as an unqualified
/// zero, the admission completes as `accepted_for_replay`: terminal,
/// non-retryable, and proving nothing.
#[tokio::test]
async fn replayed_cursor_ingest_reports_an_exact_duplicate_not_a_bare_replay() {
    let (_project, event, project_id) = cursor_replay_fixture();
    let admission = crate::admission::test_support::MemoryHostAdmission::default();

    let committed = try_ingest_cursor_transcript_event_capped_with_admission(
        &event,
        project_id.clone(),
        &admission,
        None,
    )
    .await
    .unwrap();
    assert!(
        committed.messages_upserted > 0,
        "the first pass admits and projects the transcript: {committed:?}"
    );
    assert_eq!(
        committed.observations_committed, 2,
        "admission is the commit and is accounted for independently of whichever \
         drainer projects it: {committed:?}"
    );
    assert!(
        !committed.exact_duplicate,
        "a pass that committed rows is not a duplicate: {committed:?}"
    );

    // Same event again: the source cursor is at end of file and the projection
    // queue this scope shares with the catch-up sweep is already empty.
    let replayed = try_ingest_cursor_transcript_event_capped_with_admission(
        &event, project_id, &admission, None,
    )
    .await
    .unwrap();
    assert_eq!(
        replayed.messages_upserted, 0,
        "an already-projected replay upserts nothing: {replayed:?}"
    );
    assert!(
        !replayed.source_deferred,
        "nothing is left to defer: {replayed:?}"
    );
    assert!(
        replayed.exact_duplicate,
        "a replay of already-durable observations is an exact duplicate, not a bare accepted-for-replay: {replayed:?}"
    );
}

/// The shared projection queue's residual is not this pass. A deferred drain
/// with leftover rows does not defer the pass, invent a duplicate, or hide
/// the frames admission persisted.
#[test]
fn hook_admission_ignores_a_shared_drain_residual() {
    let committed = account_hook_admission(
        CursorTranscriptIngestStats {
            messages_upserted: 9,
            source_deferred: true,
            exact_duplicate: true,
            ..CursorTranscriptIngestStats::default()
        },
        2,
        false,
        false,
        40,
    );
    assert_eq!(committed.observations_committed, 2);
    assert_eq!(committed.bytes_consumed, 40);
    assert_eq!(committed.messages_upserted, 9);
    assert!(!committed.source_deferred);
    assert!(!committed.exact_duplicate);

    let replayed = account_hook_admission(
        CursorTranscriptIngestStats {
            messages_upserted: 4,
            source_deferred: true,
            exact_duplicate: false,
            ..CursorTranscriptIngestStats::default()
        },
        0,
        true,
        false,
        0,
    );
    assert_eq!(replayed.observations_committed, 0);
    assert!(replayed.exact_duplicate);
    assert!(!replayed.source_deferred);

    let deferred =
        account_hook_admission(CursorTranscriptIngestStats::default(), 0, false, true, 8);
    assert!(deferred.source_deferred);
    assert!(!deferred.exact_duplicate);
    assert_eq!(deferred.observations_committed, 0);
}

/// The duplicate verdict is evidence, not a default: a source this pass has
/// never opened carries no proof that anything was committed before.
#[tokio::test]
async fn first_cursor_ingest_of_an_empty_source_is_never_an_exact_duplicate() {
    crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
    let project = tempfile::tempdir().unwrap();
    let transcript = project.path().join("cursor-empty.jsonl");
    std::fs::write(&transcript, "").unwrap();
    let event = json!({
        "session_id": "session-empty",
        "transcript_path": transcript,
        "workspace_roots": [project.path()],
    })
    .to_string();
    let admission = crate::admission::test_support::MemoryHostAdmission::default();

    let stats = try_ingest_cursor_transcript_event_capped_with_admission(
        &event,
        ProjectId::new("project.cursor-empty").unwrap(),
        &admission,
        None,
    )
    .await
    .unwrap();

    assert_eq!(stats.messages_upserted, 0);
    assert!(
        !stats.exact_duplicate,
        "a first-ever scan proves no prior commit: {stats:?}"
    );
}
