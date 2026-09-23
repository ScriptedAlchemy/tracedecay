use super::*;
use crate::runtime::shared::StoredCursor;
use serde_json::json;
use tracedecay_capture::claude as canonical;
use tracedecay_runtime_core::git_discovery::{
    GitDiscoveryUnknown, GitRepositoryIdentity, GitRepositoryIdentityOutcome,
};

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
fn bounded_scan_blocks_oversized_frame_and_suffix_at_one_mib() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session-42.jsonl");
    let oversized = format!(
        "{{\"payload\":\"{}\"}}\n",
        "x".repeat(tracedecay_privacy::MAX_OBSERVATION_RECORD_BYTES)
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

    assert_ne!(
        cursor::claude_cursor_key(&first).durable_text(),
        cursor::claude_cursor_key(&second).durable_text()
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
fn claude_checked_in_assistant_fixture_crosses_the_canonical_boundary() {
    let path = format!(
        "{}/../../tests/fixtures/provider_normalization/claude/assistant_tool_use.input.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).unwrap();
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, bytes.len() as u64).unwrap();
    let parsed = tracedecay_privacy::parse_normalized_observation_record_v1(
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
    assert_eq!(envelope.stable_record_id().as_str(), "u2");
    assert_eq!(
        envelope
            .relations()
            .message_id()
            .map(tracedecay_domain::ObservationId::as_str),
        Some("msg_claude_1")
    );
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
        "../../../../../../tests/fixtures/provider_normalization/claude/assistant_thinking_text_tool_use.input.json"
    );
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, bytes.len() as u64).unwrap();
    let parsed = tracedecay_privacy::parse_normalized_observation_record_v1(
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
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, bytes.len() as u64).unwrap();
    let parsed = tracedecay_privacy::parse_normalized_observation_record_v1(
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
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, bytes.len() as u64).unwrap();
    let parsed = tracedecay_privacy::parse_normalized_observation_record_v1(
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

static UNKNOWN_PATH_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn retrying_identity(path: &Path) -> GitRepositoryIdentityOutcome {
    use std::sync::atomic::Ordering;
    let root = path
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "repo"))
        .unwrap_or(path);
    if UNKNOWN_PATH_ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 1 {
        return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded);
    }
    GitRepositoryIdentityOutcome::Resolved(GitRepositoryIdentity {
        worktree_root: root.to_path_buf(),
        git_dir: root.join(".git"),
        common_dir: root.join(".git"),
    })
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

    let scan = || {
        try_scan_claude_source_frames_with_resume(
            identify_claude_source(&transcript).unwrap(),
            StoredCursor::default(),
            None,
            None,
        )
        .unwrap()
        .unwrap()
    };

    let mut first = scan();
    assert!(
        source
            .retain_scoped_frames(&mut first, &project_root)
            .is_none(),
        "unknown membership must defer the scan before a cursor or skip range can be persisted"
    );

    let mut retried = scan();
    let excluded = source
        .retain_scoped_frames(&mut retried, &project_root)
        .expect("unknown membership must be resolved again on retry");
    assert!(excluded.is_empty());
    assert_eq!(retried.frames.len(), 1);
    assert_eq!(UNKNOWN_PATH_ATTEMPTS.load(Ordering::SeqCst), 3);
}
