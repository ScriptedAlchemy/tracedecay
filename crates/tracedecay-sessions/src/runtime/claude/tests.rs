use super::*;
use serde_json::json;
use tracedecay_capture::claude as canonical;

#[test]
fn live_session_discovery_excludes_large_unrelated_history() {
    let home = tempfile::tempdir().unwrap();
    let projects = home.path().join(".claude/projects");
    for index in 0..256 {
        let project = projects.join(format!("project-{index}"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(format!("unrelated-{index}.jsonl")), b"{}\n").unwrap();
    }
    let target = projects.join("project-173").join("target-session.jsonl");
    std::fs::write(&target, b"{}\n").unwrap();
    let source = ClaudeSource::with_home(home.path())
        .for_user_scope(Some("target-session".to_string()), Vec::new());

    assert_eq!(source.transcript_paths(home.path()), vec![target]);
}

#[test]
fn bounded_scan_carries_identity_cursor_generation_and_coverage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-42.jsonl");
    let complete = b"{\"type\":\"summary\"}\n";
    let contents = [complete.as_slice(), b"{\"partial\":"].concat();
    std::fs::write(&path, &contents).unwrap();

    let identity = identify_claude_source(&path).unwrap();
    let scan = scan_claude_source_frames(identity, StoredCursor::default(), None).unwrap();

    assert_eq!(scan.identity.provider, "claude");
    assert_eq!(scan.identity.session_id, "session-42");
    assert_eq!(
        scan.identity.source_id,
        cursor::claude_observation_source_id(&path)
    );
    assert!(
        scan.identity
            .source_id
            .starts_with("tracedecay-claude-observation-source-v1-sha256-")
    );
    assert_eq!(scan.identity.source_path, path);
    assert_eq!(scan.previous_cursor.state, StoredCursor::default());
    assert_eq!(scan.previous_cursor.key, scan.next_cursor.key);
    assert_eq!(scan.file_generation, scan.next_cursor.state.file_id);
    assert_eq!(scan.frames.len(), 1);
    assert_eq!(scan.frames[0].offset, 0);
    assert_eq!(scan.frames[0].end_offset, complete.len() as u64);
    assert_eq!(scan.frames[0].scope_value()["type"], "summary");
    assert_eq!(
        scan.coverage,
        ClaudeFrameCoverage::Deferred {
            start_offset: 0,
            covered_through: complete.len() as u64,
            reason: JsonlFrameDeferral::Partial {
                offset: complete.len() as u64,
            },
        }
    );
    assert_eq!(scan.next_cursor.state.position, complete.len() as u64);
    assert_eq!(scan.read_through, contents.len() as u64);
}

#[test]
fn bounded_scan_finishes_one_valid_record_past_nominal_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-42.jsonl");
    let contents = b"{\"type\":\"summary\"}\n";
    std::fs::write(&path, contents).unwrap();

    let identity = identify_claude_source(&path).unwrap();
    let scan = scan_claude_source_frames(identity, StoredCursor::default(), Some(1)).unwrap();

    assert_eq!(scan.frames.len(), 1);
    assert_eq!(scan.next_cursor.state.position, contents.len() as u64);
    assert_eq!(scan.read_through, contents.len() as u64);
    assert_eq!(
        scan.coverage,
        ClaudeFrameCoverage::Complete {
            start_offset: 0,
            end_offset: contents.len() as u64,
        }
    );
}

#[test]
fn bounded_scan_blocks_oversized_frame_and_suffix_at_one_mib() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-42.jsonl");
    let oversized = format!(
        "{{\"payload\":\"{}\"}}\n",
        "x".repeat(tracedecay_runtime_core::privacy::MAX_OBSERVATION_RECORD_BYTES)
    );
    std::fs::write(&path, format!("{oversized}{{\"type\":\"summary\"}}\n")).unwrap();

    let identity = identify_claude_source(&path).unwrap();
    let scan = scan_claude_source_frames(identity, StoredCursor::default(), None).unwrap();

    assert!(scan.frames.is_empty());
    assert!(scan.next_cursor.state.position > 0);
    assert_eq!(scan.skipped_frames.len(), 1);
    assert_eq!(
        scan.skipped_frames[0].reason,
        ClaudeSkippedFrameReason::Oversized
    );
    assert!(matches!(
        scan.coverage,
        ClaudeFrameCoverage::Deferred {
            covered_through,
            reason: JsonlFrameDeferral::Backlog { offset, .. },
            ..
        } if covered_through == offset && offset > 0
    ));
}

#[test]
fn bounded_scan_exposes_whitespace_ranges_without_parsing_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-42.jsonl");
    let record = b"{\"type\":\"summary\"}\n";
    std::fs::write(&path, [b"\n".as_slice(), record, b" \t\n"].concat()).unwrap();

    let identity = identify_claude_source(&path).unwrap();
    let scan = scan_claude_source_frames(identity, StoredCursor::default(), None).unwrap();

    assert_eq!(scan.frames.len(), 1);
    assert_eq!(scan.skipped_frames.len(), 2);
    assert_eq!(
        scan.skipped_frames[0],
        ClaudeSkippedFrame {
            offset: 0,
            end_offset: 1,
            resume_fingerprint: scan.skipped_frames[0].resume_fingerprint,
            reason: ClaudeSkippedFrameReason::Whitespace,
        }
    );
    assert_eq!(
        scan.skipped_frames[1],
        ClaudeSkippedFrame {
            offset: (1 + record.len()) as u64,
            end_offset: (1 + record.len() + 3) as u64,
            resume_fingerprint: scan.skipped_frames[1].resume_fingerprint,
            reason: ClaudeSkippedFrameReason::Whitespace,
        }
    );
}

#[test]
fn canonical_mapper_emits_one_conversational_message() {
    let record = json!({
        "type": "user",
        "uuid": "user-1",
        "message": {"role": "user", "content": "hello"},
    });
    let context = ClaudeRecordContext {
        session_id: "session-1",
        project_key: "project-1",
        project_path: "/project-1",
        file_generation: 42,
        offset: 9,
        session_cwd: Some(Path::new("/project-1")),
        source_path: None,
        raw_message_id: Some("user-1"),
        raw_tool_event_ids: &[],
        raw_hook_tool_use_id: None,
    };

    let ClaudeRecordDisposition::Message { draft, message } =
        map_sanitized_claude_record(&record, &context)
    else {
        panic!("conversational row must map");
    };
    assert_eq!(draft.session_id, "session-1");
    assert_eq!(message.message_id, "user-1");
    assert_eq!(message.kind.as_deref(), Some("message"));
    assert_eq!(message.source_path.as_deref(), Some("claude:session-1"));
    let metadata: Value = serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source_generation"], 42);

    assert!(matches!(
        map_sanitized_claude_record(&json!({"type": "summary"}), &context),
        ClaudeRecordDisposition::NonConversational
    ));
}

#[test]
fn legacy_trait_parse_only_folds_sanitizer_issued_values() {
    let dir = tempfile::tempdir().unwrap();
    let secret = "password = p@ssw0rd!";
    let project_root = dir.path().join(secret);
    std::fs::create_dir_all(&project_root).unwrap();
    let transcript = dir.path().join("session-sanitized.jsonl");
    let record = json!({
        "type": "user",
        "uuid": "user-sanitized",
        "cwd": project_root,
        "message": {"role": "user", "content": secret},
    });
    std::fs::write(
        &transcript,
        format!("{}\n", serde_json::to_string(&record).unwrap()),
    )
    .unwrap();

    let parsed = ClaudeSource::with_home(Path::new("/unused"))
        .parse_new(&transcript, StoredCursor::default(), &project_root, None)
        .expect("legacy trait parse");
    let mut durable = parsed.draft.metadata_json.clone().unwrap_or_default();
    for message in &parsed.messages {
        durable.push_str(&message.text);
        durable.push_str(message.metadata_json.as_deref().unwrap_or_default());
    }

    assert!(!durable.contains("p@ssw0rd!"), "{durable}");
    assert!(durable.contains("[TraceDecay redacted:"), "{durable}");
    assert_eq!(parsed.messages.len(), 1);
}

#[test]
fn subagent_provider_metadata_is_sanitized_before_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let raw_secret = "abcdefghijklmnopqrstuvwxyz0123456789";
    let credential = format!("Bearer {raw_secret}");
    let workflow = format!("wf_ {credential}");
    let transcript = dir
        .path()
        .join("parent-session")
        .join("subagents")
        .join("workflows")
        .join(&workflow)
        .join("agent-child.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "").unwrap();
    std::fs::write(
        transcript.with_file_name("agent-child.meta.json"),
        serde_json::to_vec(&json!({
            "agentType": format!("Explore {credential}"),
            "description": format!("Inspect {credential}"),
            "toolUseId": format!("tool {credential}"),
            "spawnDepth": 2,
        }))
        .unwrap(),
    )
    .unwrap();

    let info = claude_subagent_identity(&transcript).expect("subagent identity");
    let durable = serde_json::to_string(&session_metadata(
        None,
        Some(&info),
        &SessionAccumulator::default(),
        None,
    ))
    .unwrap();

    assert!(!durable.contains(raw_secret), "{durable}");
    assert_eq!(info.parent_tool_use_id, None);
    assert_eq!(info.workflow_run_id, None);
    assert!(durable.contains("[TraceDecay redacted:"));
    assert_eq!(info.spawn_depth, Some(2));

    let other_secret = "0123456789abcdefghijklmnopqrstuvwxyz";
    let other_credential = format!("Bearer {other_secret}");
    let other_workflow = format!("wf_ {other_credential}");
    let other_transcript = dir
        .path()
        .join("parent-session")
        .join("subagents")
        .join("workflows")
        .join(&other_workflow)
        .join("agent-other.jsonl");
    std::fs::create_dir_all(other_transcript.parent().unwrap()).unwrap();
    std::fs::write(&other_transcript, "").unwrap();
    std::fs::write(
        other_transcript.with_file_name("agent-other.meta.json"),
        serde_json::to_vec(&json!({
            "toolUseId": format!("tool {other_credential}"),
        }))
        .unwrap(),
    )
    .unwrap();

    let other = claude_subagent_identity(&other_transcript).expect("other identity");
    assert_eq!(other.parent_tool_use_id, None);
    assert_eq!(other.workflow_run_id, None);
}

#[test]
fn subagent_metadata_exceeding_structural_limits_is_denied_as_a_whole() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir
        .path()
        .join("parent-session")
        .join("subagents")
        .join("agent-deep.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "").unwrap();

    let mut nested = json!(true);
    for _ in 0..=tracedecay_capture::ParseLimits::default_policy().depth {
        nested = json!({"next": nested});
    }
    std::fs::write(
        transcript.with_file_name("agent-deep.meta.json"),
        serde_json::to_vec(&json!({
            "agentType": "must-not-survive-partial-scan",
            "nested": nested,
        }))
        .unwrap(),
    )
    .unwrap();

    let info = claude_subagent_identity(&transcript).expect("subagent identity");

    assert_eq!(info.agent_type, None);
    assert_eq!(info.description, None);
    assert_eq!(info.spawn_depth, None);
}

#[test]
fn cursor_key_round_trips_native_bytes_without_collisions() {
    let native_path: Vec<u8> = r"C:\Users\zack\.claude\projects\session.jsonl"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let other_native_path: Vec<u8> = r"C:\Users\other.jsonl"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let key = encode_claude_cursor_key("windows-utf16le", &native_path);
    let encoded = key
        .strip_prefix("tracedecay-claude-cursor-v1-windows-utf16le-")
        .expect("versioned platform prefix");

    assert_eq!(hex::decode(encoded).unwrap(), native_path);
    assert_ne!(
        key,
        encode_claude_cursor_key("windows-utf16le", &other_native_path)
    );
    assert_ne!(
        key,
        encode_claude_cursor_key("unix-bytes", &native_path),
        "platform tag is part of the durable identity"
    );

    let source_id = encode_claude_source_id("windows-utf16le", &native_path);
    let encoded_source = source_id
        .strip_prefix("tracedecay-claude-source-v1-windows-utf16le-")
        .expect("versioned source prefix");
    assert_eq!(hex::decode(encoded_source).unwrap(), native_path);
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_that_render_identically_have_distinct_cursor_keys() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let first = PathBuf::from(OsString::from_vec(b"session-\xff.jsonl".to_vec()));
    let second = PathBuf::from(OsString::from_vec(b"session-\xfe.jsonl".to_vec()));
    assert_eq!(first.to_string_lossy(), second.to_string_lossy());

    let source = ClaudeSource::with_home(Path::new("/unused"));
    assert_ne!(
        source.cursor_key(&first).durable_text(),
        source.cursor_key(&second).durable_text()
    );
    let first_identity = identify_claude_source(&first).unwrap();
    let second_identity = identify_claude_source(&second).unwrap();
    assert_ne!(first_identity.session_id, second_identity.session_id);
    assert_ne!(first_identity.source_id, second_identity.source_id);
    assert!(!first_identity.source_id.contains('/'));
}

#[cfg(unix)]
#[test]
fn observation_source_ids_are_private_and_follow_native_transcript_identity() {
    use std::os::unix::ffi::OsStrExt;

    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("account-one/session.jsonl");
    let second = root.path().join("account-two/session.jsonl");
    let other = root.path().join("account-two/other-session.jsonl");
    std::fs::create_dir_all(first.parent().unwrap()).unwrap();
    std::fs::create_dir_all(second.parent().unwrap()).unwrap();
    std::fs::write(&first, "").unwrap();
    std::fs::write(&second, "").unwrap();
    std::fs::write(&other, "").unwrap();

    let first_identity = identify_claude_source(&first).unwrap();
    let second_identity = identify_claude_source(&second).unwrap();
    let other_identity = identify_claude_source(&other).unwrap();
    assert_eq!(first_identity.session_id, second_identity.session_id);
    assert_eq!(first_identity.source_id, second_identity.source_id);
    assert_ne!(first_identity.source_id, other_identity.source_id);
    assert!(!first_identity.source_id.contains("session"));
    assert!(!other_identity.source_id.contains("other-session"));
    for (identity, path) in [
        (&first_identity, &first),
        (&second_identity, &second),
        (&other_identity, &other),
    ] {
        let canonical = std::fs::canonicalize(path).unwrap();
        let raw_hex = hex::encode(canonical.as_os_str().as_bytes());
        assert!(!identity.source_id.contains(&raw_hex));
        assert!(
            !identity
                .source_id
                .contains(canonical.to_string_lossy().as_ref())
        );
        assert_eq!(
            identity.source_id.len(),
            "tracedecay-claude-observation-source-v1-sha256-".len() + 64
        );
    }
}

#[test]
fn unicode_paths_keep_the_legacy_cursor_key() {
    let path = Path::new("/tmp/claude-session.jsonl");
    let source = ClaudeSource::with_home(Path::new("/unused"));

    assert_eq!(
        source.cursor_key(path).durable_text(),
        path.to_string_lossy()
    );
}

#[test]
fn structured_git_operation_becomes_host_commit_evidence() {
    let mut metadata = Map::new();
    append_git_operation_metadata(
        &mut metadata,
        &json!({
            "gitBranch": "feature/attribution",
            "toolUseResult": {
                "gitOperation": {
                    "commit": {"sha": "ABCDEF12", "kind": "commit"}
                }
            }
        }),
    );
    assert_eq!(metadata["produced_commit_candidates"], json!(["abcdef12"]));
    assert_eq!(metadata["produced_commit_evidence"], "host_event");
    assert_eq!(metadata["git_branch"], "feature/attribution");
}

#[test]
fn unstructured_user_content_cannot_spoof_commit_evidence() {
    let mut metadata = Map::new();
    append_git_operation_metadata(
        &mut metadata,
        &json!({"message": {"content": "gitOperation commit abcdef12"}}),
    );
    assert!(metadata.is_empty());
}

fn assistant_record(content: &Value) -> Value {
    json!({
        "type": "assistant",
        "sessionId": "sess",
        "uuid": "u-assistant",
        "timestamp": "2026-01-01T00:00:05.000Z",
        "message": {
            "id": "msg_1",
            "role": "assistant",
            "model": "claude-opus-4-8",
            "content": content.clone(),
        }
    })
}

fn record_context(raw_message_id: Option<&str>, offset: u64) -> ClaudeRecordContext<'_> {
    ClaudeRecordContext {
        session_id: "sess",
        project_key: "project",
        project_path: "/project",
        file_generation: 7,
        offset,
        session_cwd: None,
        source_path: None,
        raw_message_id,
        raw_tool_event_ids: &[],
        raw_hook_tool_use_id: None,
    }
}

#[test]
fn thinking_blocks_are_split_from_the_visible_message_row() {
    let record = assistant_record(&json!([
        {"type": "thinking", "thinking": "First I inspect the parser."},
        {"type": "thinking", "thinking": "Then I add the row."},
        {"type": "tool_use", "name": "Read", "input": {"file_path": "src/lib.rs"}},
        {"type": "text", "text": "Done."}
    ]));
    let path = Path::new("/tmp/sess.jsonl");

    let mut accumulator = SessionAccumulator::default();
    let message = message_from_line(&record, "sess", path, 10, None, &mut accumulator, None)
        .expect("assistant message row");
    assert_eq!(message.message_id, "msg_1");
    assert_eq!(message.kind.as_deref(), Some("message"));
    assert!(!message.text.contains("First I inspect the parser"));
    assert!(!message.text.contains("Then I add the row"));
    assert!(message.text.contains("src/lib.rs"));
    assert!(message.text.contains("Done."));
    assert_eq!(message.tool_names.as_deref(), Some("Read"));

    let context = record_context(Some("msg_1"), 10);
    let reasoning = reasoning_from_line(&record, path, &context, Some(message.message_id.as_str()))
        .expect("reasoning row for thinking");
    assert_eq!(reasoning.message_id, "msg_1:thinking");
    assert_eq!(reasoning.kind.as_deref(), Some("reasoning"));
    assert_eq!(reasoning.role, "assistant");
    assert_eq!(reasoning.model.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(reasoning.ordinal, 10);
    assert_eq!(reasoning.timestamp, Some(1_767_225_605));
    assert_eq!(
        reasoning.text,
        "First I inspect the parser.\n\nThen I add the row."
    );
    let metadata: Value = serde_json::from_str(reasoning.metadata_json.as_deref().unwrap())
        .expect("reasoning metadata json");
    assert_eq!(metadata["source"], "claude_thinking");
    assert_eq!(metadata["parent_message_id"], "msg_1");
    assert_eq!(metadata["thinking_blocks"], 2);
    assert!(metadata.get("redacted_thinking_blocks").is_none());
}

#[test]
fn redacted_only_thinking_records_no_reasoning_row() {
    // Matches Codex's encrypted-reasoning convention: no plaintext, no row.
    let record = assistant_record(&json!([
        {"type": "redacted_thinking", "data": "ENCRYPTED_SHOULD_NOT_INDEX"},
        {"type": "text", "text": "Answer."}
    ]));
    assert!(
        reasoning_from_line(
            &record,
            Path::new("/tmp/sess.jsonl"),
            &record_context(Some("msg_1"), 3),
            None,
        )
        .is_none()
    );
}

#[test]
fn mixed_thinking_and_redacted_records_the_redacted_count_but_no_plaintext() {
    let record = assistant_record(&json!([
        {"type": "thinking", "thinking": "Visible reasoning."},
        {"type": "redacted_thinking", "data": "ENCRYPTED_SHOULD_NOT_INDEX"}
    ]));
    let reasoning = reasoning_from_line(
        &record,
        Path::new("/tmp/sess.jsonl"),
        &record_context(Some("msg_1"), 4),
        Some("msg_1"),
    )
    .expect("reasoning row for the plaintext block");
    assert_eq!(reasoning.text, "Visible reasoning.");
    assert!(!reasoning.text.contains("ENCRYPTED"));
    let metadata: Value =
        serde_json::from_str(reasoning.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["thinking_blocks"], 1);
    assert_eq!(metadata["redacted_thinking_blocks"], 1);
}

#[test]
fn assistant_message_without_thinking_records_no_reasoning_row() {
    let record = assistant_record(&json!([{"type": "text", "text": "Just an answer."}]));
    assert!(
        reasoning_from_line(
            &record,
            Path::new("/tmp/sess.jsonl"),
            &record_context(Some("msg_1"), 7),
            None,
        )
        .is_none()
    );
}

#[test]
fn reasoning_row_id_falls_back_to_record_uuid_when_message_id_is_absent() {
    let record = json!({
        "type": "assistant",
        "sessionId": "sess",
        "uuid": "u-fallback",
        "timestamp": "2026-01-01T00:00:05.000Z",
        "message": {
            "role": "assistant",
            "content": [{"type": "thinking", "thinking": "Reasoning without a message id."}]
        }
    });
    let reasoning = reasoning_from_line(
        &record,
        Path::new("/tmp/sess.jsonl"),
        &record_context(Some("u-fallback"), 9),
        None,
    )
    .expect("reasoning row");
    assert_eq!(reasoning.message_id, "u-fallback:thinking");
    let metadata: Value =
        serde_json::from_str(reasoning.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["parent_message_id"], "u-fallback");
}

#[test]
fn user_record_never_produces_a_reasoning_row() {
    let record = json!({
        "type": "user",
        "message": {"role": "user", "content": [{"type": "thinking", "thinking": "nope"}]}
    });
    assert!(
        reasoning_from_line(
            &record,
            Path::new("/tmp/sess.jsonl"),
            &record_context(None, 1),
            None,
        )
        .is_none()
    );
}

#[test]
fn redacted_identity_uses_generation_offset_for_message_and_reasoning() {
    let marker = "[TraceDecay redacted:credential]";
    let record = json!({
        "type": "assistant",
        "uuid": marker,
        "message": {
            "id": marker,
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "private chain"},
                {"type": "text", "text": "answer"}
            ]
        }
    });
    let context = record_context(Some("raw-sensitive-id"), 19);
    let ClaudeRecordDisposition::Message { message, .. } =
        map_sanitized_claude_record(&record, &context)
    else {
        panic!("assistant row must map");
    };
    assert_eq!(message.message_id, "sess:7:19");

    let reasoning = reasoning_from_line(
        &record,
        Path::new("/tmp/sess.jsonl"),
        &context,
        Some(message.message_id.as_str()),
    )
    .expect("reasoning row");
    let metadata: Value =
        serde_json::from_str(reasoning.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(reasoning.message_id, "sess:7:19:thinking");
    assert_eq!(metadata["parent_message_id"], "sess:7:19");
}

#[test]
fn redacted_marker_ids_do_not_collide() {
    let record = json!({
        "type": "pr-link",
        "uuid": "[TraceDecay redacted:credential]",
        "prNumber": 5,
    });
    let mut accumulator = SessionAccumulator::default();
    let first = record_metadata::pr_link_row(
        &record,
        "sess",
        7,
        Path::new("/tmp/sess.jsonl"),
        10,
        &mut accumulator,
    )
    .unwrap();
    let second = record_metadata::pr_link_row(
        &record,
        "sess",
        7,
        Path::new("/tmp/sess.jsonl"),
        20,
        &mut accumulator,
    )
    .unwrap();

    assert_eq!(first.message_id, "sess:7:10");
    assert_eq!(second.message_id, "sess:7:20");
    assert_ne!(first.message_id, second.message_id);
}

#[test]
fn claude_checked_in_assistant_fixture_crosses_the_canonical_boundary() {
    let path = format!(
        "{}/../../tests/fixtures/provider_normalization/claude/assistant_tool_use.input.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).unwrap();
    let range = tracedecay_domain::ClaudeByteRangeV1::new(0, bytes.len() as u64).unwrap();
    let parsed = tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1(
        &bytes,
        range,
        tracedecay_domain::ObservationOrderingDomainV1::FileBytes,
        |native| {
            let stable = canonical::stable_record_id(&native, "claude-golden-session", 0)?;
            canonical::normalize(&native, "claude-golden-session", stable, range)
        },
    )
    .unwrap();
    let envelope = serde_json::from_value::<tracedecay_domain::CanonicalObservationEnvelopeV1>(
        parsed.value().clone(),
    )
    .unwrap();
    assert_eq!(envelope.provider().as_str(), "claude");
    assert_eq!(envelope.stable_record_id().as_str(), "msg_claude_1");
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        tracedecay_domain::CanonicalObservationFactV1::Message {
            content: serde_json::Value::String(text),
            ..
        } if text == "The billing pipeline regression is fixed."
    )));
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        tracedecay_domain::CanonicalObservationFactV1::ToolInvocation { name, .. }
            if name == "tracedecay_context"
    )));
    let rendered = serde_json::to_string(parsed.value()).unwrap();
    assert!(
        !rendered.contains("\"type\":\"tool_use\""),
        "tool_use must not leak into Message/searchable JSON; got typed ToolInvocation instead"
    );
    assert!(
        envelope.facts().iter().all(|fact| {
            !matches!(
                fact,
                tracedecay_domain::CanonicalObservationFactV1::WorkflowLifecycle { .. }
            )
        }),
        "Claude checked-in assistant fixture has no native lifecycle evidence"
    );
}

#[test]
fn claude_checked_in_mixed_blocks_keep_authored_message_and_typed_order() {
    let bytes = include_bytes!(
        "../../../../../tests/fixtures/provider_normalization/claude/assistant_thinking_text_tool_use.input.json"
    );
    let range = tracedecay_domain::ClaudeByteRangeV1::new(0, bytes.len() as u64).unwrap();
    let parsed = tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1(
        bytes,
        range,
        tracedecay_domain::ObservationOrderingDomainV1::FileBytes,
        |native| {
            let stable = canonical::stable_record_id(&native, "claude-mixed-session", 0)?;
            canonical::normalize(&native, "claude-mixed-session", stable, range)
        },
    )
    .unwrap();
    let envelope = serde_json::from_value::<tracedecay_domain::CanonicalObservationEnvelopeV1>(
        parsed.value().clone(),
    )
    .unwrap();
    let facts = envelope.facts();
    assert!(facts.iter().any(|fact| matches!(
        fact,
        tracedecay_domain::CanonicalObservationFactV1::Message {
            content: serde_json::Value::String(text),
            ..
        } if text == "The visible provider-authored answer."
    )));
    let reasoning_index = facts
        .iter()
        .position(|fact| {
            matches!(
                fact,
                tracedecay_domain::CanonicalObservationFactV1::Reasoning {
                    visibility:
                        tracedecay_domain::CanonicalReasoningVisibilityV1::Visible,
                    content: Some(serde_json::Value::String(text)),
                } if text == "Inspect the parser before editing."
            )
        })
        .expect("typed reasoning fact");
    let message_index = facts
        .iter()
        .position(|fact| {
            matches!(
                fact,
                tracedecay_domain::CanonicalObservationFactV1::Message {
                    content: serde_json::Value::String(text),
                    ..
                } if text == "The visible provider-authored answer."
            )
        })
        .expect("authored message fact");
    let tool_index = facts
        .iter()
        .position(|fact| {
            matches!(
                fact,
                tracedecay_domain::CanonicalObservationFactV1::ToolInvocation {
                    name,
                    arguments,
                    ..
                } if name == "Read"
                    && arguments.get("file_path").and_then(serde_json::Value::as_str)
                        == Some("src/lib.rs")
            )
        })
        .expect("typed tool invocation");
    assert!(
        reasoning_index < message_index && message_index < tool_index,
        "typed and authored facts must retain provider block order"
    );
    let rendered = serde_json::to_string(parsed.value()).unwrap();
    assert!(!rendered.contains("signature-redacted"));
    assert!(!rendered.contains("\"type\":\"thinking\""));
    assert!(!rendered.contains("\"type\":\"tool_use\""));
}

#[test]
fn claude_workflow_lookalike_emits_no_workflow_lifecycle() {
    let path = format!(
        "{}/../../tests/fixtures/provider_normalization/claude/workflow_lookalike.input.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).unwrap();
    let range = tracedecay_domain::ClaudeByteRangeV1::new(0, bytes.len() as u64).unwrap();
    let parsed = tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1(
        &bytes,
        range,
        tracedecay_domain::ObservationOrderingDomainV1::FileBytes,
        |native| {
            let stable = canonical::stable_record_id(&native, "claude-workflow-lookalike", 0)?;
            canonical::normalize(&native, "claude-workflow-lookalike", stable, range)
        },
    )
    .expect("Claude workflow lookalike must still normalize as an assistant message");
    let envelope = serde_json::from_value::<tracedecay_domain::CanonicalObservationEnvelopeV1>(
        parsed.value().clone(),
    )
    .unwrap();
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        tracedecay_domain::CanonicalObservationFactV1::Message { .. }
    )));
    assert!(
        envelope.facts().iter().all(|fact| {
            !matches!(
                fact,
                tracedecay_domain::CanonicalObservationFactV1::WorkflowLifecycle { .. }
            )
        }),
        "Claude workflow/todos/thread_goal lookalikes must not become WorkflowLifecycle"
    );
    let encoded = serde_json::to_string(parsed.value()).unwrap();
    for rejected in [
        "claude-hostile-task",
        "todo-hostile-1",
        "invented todo",
        "invented goal",
    ] {
        assert!(
            !encoded.contains(rejected),
            "{rejected} must not survive Claude canonicalization"
        );
    }
}

#[test]
fn claude_task_create_and_update_emit_workflow_lifecycle_facts() {
    let record = json!({
        "type": "assistant",
        "cwd": "/redacted/project",
        "sessionId": "claude-task-session",
        "uuid": "claude-task-1",
        "timestamp": "2026-01-01T00:00:05.000Z",
        "message": {
            "id": "msg_claude_task_create",
            "role": "assistant",
            "model": "claude-opus-4-8",
            "content": [
                {
                    "type": "tool_use",
                    "id": "call_task_create_1",
                    "name": "TaskCreate",
                    "input": {
                        "subject": "Gather simplify review scope",
                        "description": "Collect the branch and working-tree diffs.",
                        "activeForm": "Gathering simplify review scope"
                    }
                },
                {
                    "type": "tool_use",
                    "id": "call_task_update_1",
                    "name": "TaskUpdate",
                    "input": {
                        "taskId": "1",
                        "status": "in_progress"
                    }
                },
                {
                    "type": "tool_use",
                    "id": "call_read_1",
                    "name": "Read",
                    "input": {"file_path": "src/lib.rs"}
                }
            ]
        }
    });
    let bytes = serde_json::to_vec(&record).unwrap();
    let range = tracedecay_domain::ClaudeByteRangeV1::new(0, bytes.len() as u64).unwrap();
    let parsed = tracedecay_runtime_core::privacy::parse_normalized_observation_record_v1(
        &bytes,
        range,
        tracedecay_domain::ObservationOrderingDomainV1::FileBytes,
        |native| {
            let stable = canonical::stable_record_id(&native, "claude-task-session", 0)?;
            canonical::normalize(&native, "claude-task-session", stable, range)
        },
    )
    .unwrap();
    let envelope = serde_json::from_value::<tracedecay_domain::CanonicalObservationEnvelopeV1>(
        parsed.value().clone(),
    )
    .unwrap();
    let lifecycle = envelope
        .facts()
        .iter()
        .filter_map(|fact| match fact {
            tracedecay_domain::CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind,
                provider_reference,
                item_id,
                state,
                status,
                content,
                ..
            } => Some((
                *semantic_kind,
                provider_reference.as_deref(),
                item_id.as_deref(),
                state.as_deref(),
                status.as_deref(),
                content.clone(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(lifecycle.len(), 2, "only the two task tool calls lifecycle");

    let (kind, provider_reference, item_id, state, status, content) = &lifecycle[0];
    assert_eq!(
        *kind,
        tracedecay_domain::CanonicalWorkflowSemanticKindV1::Task
    );
    assert_eq!(*state, Some("TaskCreate"));
    assert_eq!(*provider_reference, None);
    assert_eq!(*item_id, None);
    assert_eq!(*status, None);
    assert_eq!(
        content
            .as_ref()
            .and_then(|content| content.get("subject"))
            .and_then(serde_json::Value::as_str),
        Some("Gather simplify review scope")
    );

    let (kind, provider_reference, item_id, state, status, content) = &lifecycle[1];
    assert_eq!(
        *kind,
        tracedecay_domain::CanonicalWorkflowSemanticKindV1::Task
    );
    assert_eq!(*state, Some("TaskUpdate"));
    assert_eq!(*provider_reference, Some("1"));
    assert_eq!(*item_id, Some("1"));
    assert_eq!(*status, Some("in_progress"));
    assert_eq!(
        content
            .as_ref()
            .and_then(|content| content.get("taskId"))
            .and_then(serde_json::Value::as_str),
        Some("1")
    );

    // The ordinary tool call stays a plain ToolInvocation with no lifecycle.
    assert!(envelope.facts().iter().any(|fact| matches!(
        fact,
        tracedecay_domain::CanonicalObservationFactV1::ToolInvocation { name, .. }
            if name == "TaskCreate"
    )));
    let rendered = serde_json::to_string(parsed.value()).unwrap();
    assert!(!rendered.contains("\"type\":\"tool_use\""));
}

static UNKNOWN_PATH_ATTEMPTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn retrying_identity(
    path: &Path,
) -> tracedecay_runtime_core::worktree::GitRepoIdentityOutcome {
    use std::sync::atomic::Ordering;
    let root = path
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "repo"))
        .unwrap_or(path);
    if UNKNOWN_PATH_ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 1 {
        return tracedecay_runtime_core::worktree::GitRepoIdentityOutcome::Unknown;
    }
    tracedecay_runtime_core::worktree::GitRepoIdentityOutcome::Resolved(
        tracedecay_runtime_core::worktree::GitRepoIdentity {
            worktree_root: root.to_path_buf(),
            common_dir: root.join(".git"),
        },
    )
}

#[test]
fn claude_message_metadata_reuses_worktree_for_repeated_cwd() {
    let temp = tempfile::TempDir::new().expect("temp dir");
    let project_root = temp.path().join("repo");
    let nested_cwd = project_root.join("packages/app");
    std::fs::create_dir_all(&nested_cwd).expect("nested cwd");
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&project_root)
        .status()
        .expect("git init");
    assert!(status.success());

    let cache = crate::runtime::shared::ProjectRootMatcherCache::default();
    let record = json!({
        "type": "user",
        "sessionId": "sess",
        "cwd": nested_cwd,
        "message": {"role": "user", "content": "hello"}
    });
    let path = Path::new("/tmp/sess.jsonl");

    let mut accumulator = SessionAccumulator::default();
    let first = message_from_line(
        &record,
        "sess",
        path,
        10,
        Some(&nested_cwd),
        &mut accumulator,
        Some(&cache),
    )
    .expect("first message");
    let first_metadata: serde_json::Value =
        serde_json::from_str(first.metadata_json.as_deref().unwrap()).unwrap();
    let first_worktree = first_metadata["claude_message_worktree"].clone();
    assert!(first_worktree.is_string());

    std::fs::rename(project_root.join(".git"), project_root.join(".git.hidden"))
        .expect("hide git metadata after first lookup");

    let second = message_from_line(
        &record,
        "sess",
        path,
        20,
        Some(&nested_cwd),
        &mut accumulator,
        Some(&cache),
    )
    .expect("second message");
    let second_metadata: serde_json::Value =
        serde_json::from_str(second.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(second_metadata["claude_message_worktree"], first_worktree);
}

#[test]
fn claude_unknown_membership_retries_without_advancing_cursor() {
    use std::sync::atomic::Ordering;
    UNKNOWN_PATH_ATTEMPTS.store(0, Ordering::SeqCst);
    let temp = tempfile::TempDir::new().expect("temp dir");
    let project_root = temp.path().join("repo");
    let nested_cwd = project_root.join("packages/app");
    std::fs::create_dir_all(&nested_cwd).expect("nested cwd");
    let transcript = temp.path().join("retry.jsonl");
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "sessionId": "retry",
                "cwd": nested_cwd,
                "message": {"role": "user", "content": "retry me"}
            })
        ),
    )
    .expect("write transcript");
    let mut source = ClaudeSource::with_home(temp.path());
    source.project_matchers =
        crate::runtime::shared::ProjectRootMatcherCache::with_identity_resolver(retrying_identity);

    let previous = StoredCursor::default();
    assert!(
        source
            .parse_new(&transcript, previous, &project_root, None)
            .is_none(),
        "unknown membership must abort before a new cursor can be persisted"
    );

    let retried = source
        .parse_new(&transcript, previous, &project_root, None)
        .expect("unknown membership must be resolved again on retry");
    assert_eq!(retried.messages.len(), 1);
    assert!(retried.new_cursor.position > previous.position);
    assert_eq!(UNKNOWN_PATH_ATTEMPTS.load(Ordering::SeqCst), 3);
}
