use std::io::Write;

use tempfile::TempDir;
use tracedecay_domain::{
    ObservationScopeV1, ProviderUsageCounterSemanticsV1, ProviderUsageCountersV1,
    ProviderUsageModelV1, ProviderUsageScopeV1,
};
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::storage::PrivateStoreIo;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_sessions::runtime::SessionProvider;
use tracedecay_sessions::runtime::hosts::claude::ClaudeSource;
use tracedecay_sessions::runtime::hosts::claude_observation::ingest_source_with_observations_with_admission;
use tracedecay_sessions::runtime::shared::TranscriptIngestStats;

use crate::restart_atomicity::{
    claude_observation_cursor, durable_table_count, ingest_global_sources_for_provider,
    mark_test_project, open_project_session_db, try_ingest_claude_source,
};
use crate::support::{init_git_repo, init_project_at, setup};

/// Writes a Claude Code transcript (one JSON object per line) for `session` whose
/// recorded `cwd` is `project`.
pub(super) fn write_claude_transcript(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let cwd = project.to_string_lossy();
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "u1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {"role": "user", "content": "Investigate the billing pipeline regression"}
        }),
        serde_json::json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "u2",
            "timestamp": "2026-01-01T00:00:05.000Z",
            "message": {
                "id": "msg_claude_1",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "usage": {
                    "input_tokens": 1200,
                    "output_tokens": 340,
                    "cache_creation_input_tokens": 500,
                    "cache_read_input_tokens": 8000,
                    "service_tier": "standard"
                },
                "content": [
                    {"type": "text", "text": "The billing pipeline regression is fixed."},
                    {"type": "tool_use", "name": "tracedecay_context", "input": {}}
                ]
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

/// Runs one user-scoped Claude source through the production observation
/// pipeline against the registered profile authority.
async fn ingest_claude_profile(
    runtime: &HostAdmissionTestRuntimeV1,
    source: &ClaudeSource,
    profile: &std::path::Path,
) -> TranscriptIngestStats {
    ingest_source_with_observations_with_admission(
        source,
        profile,
        ObservationScopeV1::Profile,
        &runtime.facade(),
        None,
        ObservationCancellation::default(),
    )
    .await
    .unwrap()
    .transcript
}

fn write_claude_rows(home: &std::path::Path, session: &str, rows: &[serde_json::Value]) {
    let dir = home.join(".claude/projects/-user-scope");
    std::fs::create_dir_all(&dir).unwrap();
    let contents = rows
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        dir.join(format!("{session}.jsonl")),
        format!("{contents}\n"),
    )
    .unwrap();
}

// macOS filesystems reject invalid UTF-8 path components with EILSEQ.
#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
async fn claude_non_utf8_cursor_key_survives_atomic_persistence() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".claude/projects/-non-utf8");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(OsString::from_vec(b"session-\xff.jsonl".to_vec()));
    let row = serde_json::json!({
        "type": "user",
        "cwd": project,
        "sessionId": "native-session-id",
        "uuid": "native-path-row",
        "timestamp": "2026-01-01T00:00:00Z",
        "message": {"role": "user", "content": "Native path evidence"}
    });
    std::fs::write(&path, format!("{row}\n")).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 1);

    let offset = claude_observation_cursor(&db, &path)
        .await
        .expect("lossless source cursor persisted");
    assert_eq!(offset, std::fs::metadata(&path).unwrap().len());

    drop(db);
    let reopened = open_project_session_db(&project).await.unwrap();
    let replay = try_ingest_claude_source(&reopened, &source, &project)
        .await
        .unwrap();
    assert_eq!(replay, TranscriptIngestStats::default());
    assert_eq!(
        claude_observation_cursor(&reopened, &path).await,
        Some(offset),
        "source cursor survives restart"
    );
}

#[tokio::test]
async fn claude_user_scope_excludes_registered_project_rows() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let registered = tmp.path().join("registered");
    let general = tmp.path().join("general-chat");
    let profile = tmp.path().join("profile");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&registered).unwrap();
    std::fs::create_dir_all(&general).unwrap();
    PrivateStoreIo::create_dir_all(&profile).unwrap();

    write_claude_rows(
        &home,
        "mixed-session",
        &[
            serde_json::json!({
                "type": "user", "cwd": registered, "sessionId": "mixed-session",
                "uuid": "project-row", "timestamp": "2026-01-01T00:00:00Z",
                "message": {"role": "user", "content": "registered project secret decision"}
            }),
            serde_json::json!({
                "type": "assistant", "cwd": general, "sessionId": "mixed-session",
                "uuid": "general-row", "timestamp": "2026-01-01T00:00:01Z",
                "message": {"role": "assistant", "content": "general preference evidence"}
            }),
            serde_json::json!({
                "type": "user", "sessionId": "mixed-session",
                "uuid": "missing-cwd-row", "timestamp": "2026-01-01T00:00:02Z",
                "message": {"role": "user", "content": "registered session fallback evidence"}
            }),
        ],
    );
    write_claude_rows(
        &home,
        "locationless-session",
        &[serde_json::json!({
            "type": "user", "sessionId": "locationless-session",
            "uuid": "locationless-row", "timestamp": "2026-01-01T00:00:03Z",
            "message": {"role": "user", "content": "locationless general evidence"}
        })],
    );

    let runtime = HostAdmissionTestRuntimeV1::profile(&profile).await.unwrap();
    let source = ClaudeSource::with_home(&home).for_user_scope(None, vec![registered.clone()]);
    let stats = ingest_claude_profile(&runtime, &source, &profile).await;
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 2);
    assert_eq!(
        runtime
            .session_for_test(HostAdmissionScope::Profile, "claude", "mixed-session")
            .await
            .unwrap()
            .unwrap()
            .project_path,
        "user"
    );
    assert!(
        runtime
            .search_session_messages_for_test(
                HostAdmissionScope::Profile,
                "claude",
                None,
                "registered project secret",
                10,
            )
            .await
            .unwrap()
            .is_empty(),
        "registered-project evidence must never enter user-sessions.db"
    );
    assert_eq!(
        runtime
            .search_session_messages_for_test(
                HostAdmissionScope::Profile,
                "claude",
                None,
                "preference",
                10,
            )
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        runtime
            .search_session_messages_for_test(
                HostAdmissionScope::Profile,
                "claude",
                None,
                "locationless",
                10,
            )
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        runtime
            .search_session_messages_for_test(
                HostAdmissionScope::Profile,
                "claude",
                None,
                "registered session fallback",
                10,
            )
            .await
            .unwrap()
            .is_empty(),
        "rows without cwd inherit the registered session cwd"
    );
}

#[tokio::test]
async fn claude_user_scope_live_filter_only_ingests_requested_session() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let general = tmp.path().join("general-chat");
    let profile = tmp.path().join("profile");
    std::fs::create_dir_all(&general).unwrap();
    PrivateStoreIo::create_dir_all(&profile).unwrap();
    for (session, content) in [("wanted", "wanted evidence"), ("other", "other evidence")] {
        write_claude_rows(
            &home,
            session,
            &[serde_json::json!({
                "type": "user", "cwd": general, "sessionId": session,
                "uuid": format!("{session}-row"), "timestamp": "2026-01-01T00:00:00Z",
                "message": {"role": "user", "content": content}
            })],
        );
    }
    let runtime = HostAdmissionTestRuntimeV1::profile(&profile).await.unwrap();
    let source = ClaudeSource::with_home(&home).for_user_scope(Some("wanted".into()), vec![]);
    let stats = ingest_claude_profile(&runtime, &source, &profile).await;
    assert_eq!(stats.sessions_upserted, 1);
    assert_eq!(stats.messages_upserted, 1);
    assert!(
        runtime
            .session_for_test(HostAdmissionScope::Profile, "claude", "wanted")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        runtime
            .session_for_test(HostAdmissionScope::Profile, "claude", "other")
            .await
            .unwrap()
            .is_none()
    );
}

fn write_claude_subagent_transcript(
    home: &std::path::Path,
    parent_session: &str,
    agent_id: &str,
) -> std::path::PathBuf {
    let dir = home
        .join(".claude/projects/-some-slug")
        .join(parent_session)
        .join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("agent-{agent_id}.jsonl"));
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "sessionId": format!("agent-{agent_id}"),
                "uuid": "child-u1",
                "timestamp": "2026-01-01T00:00:10.000Z",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "The child worker verified billing fallback evidence."}
                    ]
                }
            })
        ),
    )
    .unwrap();
    path
}

#[tokio::test]
async fn claude_transcript_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    init_git_repo(&project);
    write_claude_transcript(&home, &project, "claude-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 2);
    assert_eq!(stats.sessions_upserted, 1);

    let results = db
        .search_session_messages("claude", None, "billing pipeline", 10)
        .await;
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .any(|hit| hit.message.tool_names.as_deref() == Some("tracedecay_context"))
    );
    assert!(
        results
            .iter()
            .any(|hit| hit.message.model.as_deref() == Some("claude-opus-4-8"))
    );
    // The structured ISO-8601 timestamps land as epoch seconds (2026-01-01).
    assert!(
        results
            .iter()
            .any(|hit| hit.message.timestamp == Some(1_767_225_600))
    );
    assert!(
        results
            .iter()
            .any(|hit| hit.message.timestamp == Some(1_767_225_605))
    );

    // Anthropic-style `message.usage` counters belong to the immutable
    // provider-usage observation family, never conversational metadata.
    for hit in &results {
        let metadata = hit
            .message
            .metadata_json
            .as_deref()
            .map(|metadata| serde_json::from_str::<serde_json::Value>(metadata).unwrap());
        assert!(
            metadata
                .as_ref()
                .is_none_or(|metadata| metadata.get("usage").is_none()),
            "{metadata:?}"
        );
    }

    // Privacy contract: Message facts carry only authored text. Tool use is a
    // typed ToolInvocation fact / tool_events metadata, never searchable JSON.
    let raw = db
        .lcm_load_raw_message("claude", "u2")
        .await
        .expect("authored Claude content should be in raw LCM storage");
    assert_eq!(raw.content, "The billing pipeline regression is fixed.");
}

/// Anthropic-style `message.usage` counters land in the immutable
/// provider-usage observation family through the canonical observation route,
/// with exact native evidence: the message's own model, per-message delta
/// semantics, cache-write from `cache_creation_input_tokens`, and unmeasured
/// counters typed-absent. Non-counter fields (`service_tier`) never survive.
#[tokio::test]
async fn claude_usage_counters_land_in_provider_usage_observations() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    init_git_repo(&project);
    mark_test_project(&project);
    write_claude_transcript(&home, &project, "claude-usage-observations");

    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Claude)).await;

    let observations = db.provider_usage_observations("claude").await;
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert_eq!(observation.session_id.as_str(), "claude-usage-observations");
    assert_eq!(observation.native_kind, "assistant");
    assert_eq!(observation.native_field, "message.usage");
    assert_eq!(observation.native_scope, ProviderUsageScopeV1::Message);
    assert_eq!(
        observation.counter_semantics,
        ProviderUsageCounterSemanticsV1::Delta
    );
    assert_eq!(
        observation.model,
        ProviderUsageModelV1::Known {
            model: "claude-opus-4-8".to_owned(),
        }
    );
    assert_eq!(
        observation.counters,
        ProviderUsageCountersV1::Known {
            input_tokens: Some(1200),
            output_tokens: Some(340),
            cache_read_tokens: Some(8000),
            cache_write_tokens: Some(500),
            reasoning_tokens: None,
            total_tokens: None,
        }
    );
}

/// Writes a transcript whose assistant turn carries a `thinking` block followed
/// by visible text, plus a `redacted_thinking` block that must never surface as
/// plaintext.
fn write_claude_transcript_with_thinking(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".claude/projects/-thinking-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let cwd = project.to_string_lossy();
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "tu1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {"role": "user", "content": "Trace the ingestion path"}
        }),
        serde_json::json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "tu2",
            "timestamp": "2026-01-01T00:00:05.000Z",
            "message": {
                "id": "msg_thinking_1",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [
                    {"type": "thinking", "thinking": "Reasoning breadcrumb about the parser."},
                    {"type": "redacted_thinking", "data": "ENCRYPTED_SHOULD_NEVER_INDEX"},
                    {"type": "tool_use", "name": "Read", "input": {"file_path": "src/lib.rs"}},
                    {"type": "text", "text": "Traced it."}
                ]
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn claude_thinking_blocks_do_not_project_as_ordinary_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    init_git_repo(&project);
    let transcript = write_claude_transcript_with_thinking(&home, &project, "claude-thinking");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    // Only the two provider-authored visible messages project as rows; thinking
    // stays a typed reasoning fact and never enters indexed message text.
    let stats = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 2);

    assert!(
        db.search_session_messages("claude", None, "reasoning breadcrumb", 10)
            .await
            .is_empty(),
        "thinking text must not project as an ordinary message"
    );

    let visible_results = db
        .search_session_messages("claude", None, "Traced it", 10)
        .await;
    let message = visible_results
        .iter()
        .find(|hit| hit.message.kind.as_deref() == Some("message"))
        .expect("assistant authored message row");
    assert_eq!(message.message.message_id, "tu2");
    assert_eq!(message.message.text, "Traced it.");
    assert_eq!(message.message.tool_names.as_deref(), Some("Read"));
    let redacted_results = db
        .search_session_messages("claude", None, "ENCRYPTED_SHOULD_NEVER_INDEX", 10)
        .await;
    assert!(
        redacted_results.is_empty(),
        "redacted thinking bytes must never enter indexed text"
    );

    // The source transcript remains lossless even though indexed text is filtered.
    let raw = std::fs::read_to_string(transcript).unwrap();
    assert!(raw.contains("Reasoning breadcrumb about the parser."));
    assert!(raw.contains("redacted_thinking"));
    assert!(raw.contains("ENCRYPTED_SHOULD_NEVER_INDEX"));

    // Re-ingesting the unchanged transcript is a durable no-op.
    let second = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    assert_eq!(second.messages_upserted, 0);
}

#[tokio::test]
async fn claude_transcript_for_other_project_is_skipped() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let other = tmp.path().join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    // Transcript records a cwd that is NOT the project we ingest for.
    let path = write_claude_transcript(&home, &other, "claude-other");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    assert_eq!(
        stats.messages_upserted, 0,
        "a transcript whose cwd is a different project must be skipped"
    );

    // The cursor must still advance past the filtered-out content, or every
    // future sweep re-reads and re-filters the whole foreign transcript.
    assert_eq!(
        claude_observation_cursor(&db, &path).await,
        Some(std::fs::metadata(&path).unwrap().len()),
        "source cursor should sit at EOF for a fully filtered transcript"
    );
}

#[tokio::test]
async fn claude_transcript_crossing_worktrees_is_split_by_record_cwd() {
    let tmp = TempDir::new().unwrap();
    let (home, project_a) = setup(&tmp);
    init_git_repo(&project_a);
    let project_b = tmp.path().join("project-b");
    init_project_at(&project_b);
    init_git_repo(&project_b);

    let dir = home.join(".claude/projects/-mixed-worktree");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mixed-worktree-session.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "type": "user",
                "cwd": project_a.to_string_lossy(),
                "sessionId": "mixed-worktree-session",
                "uuid": "mixed-a",
                "timestamp": "2026-01-01T00:00:00.000Z",
                "message": {"role": "user", "content": "alpha worktree marker"}
            }),
            serde_json::json!({
                "type": "user",
                "cwd": project_b.to_string_lossy(),
                "sessionId": "mixed-worktree-session",
                "uuid": "mixed-b",
                "timestamp": "2026-01-01T00:00:05.000Z",
                "message": {"role": "user", "content": "beta worktree marker"}
            })
        ),
    )
    .unwrap();

    let source = ClaudeSource::with_home(&home);
    let db_a = open_project_session_db(&project_a).await.unwrap();
    let stats_a = try_ingest_claude_source(&db_a, &source, &project_a)
        .await
        .unwrap();
    assert_eq!(stats_a.messages_upserted, 1);
    let hits_a = db_a
        .search_session_messages("claude", None, "worktree marker", 10)
        .await;
    assert_eq!(hits_a.len(), 1);
    assert!(hits_a[0].message.text.contains("alpha worktree marker"));
    drop(db_a);

    let db_b = open_project_session_db(&project_b).await.unwrap();
    let stats_b = try_ingest_claude_source(&db_b, &source, &project_b)
        .await
        .unwrap();
    assert_eq!(stats_b.messages_upserted, 1);
    let hits_b = db_b
        .search_session_messages("claude", None, "worktree marker", 10)
        .await;
    assert_eq!(hits_b.len(), 1);
    assert!(hits_b[0].message.text.contains("beta worktree marker"));
}

/// The real machine has `~/.claude` but no `projects/` dir (no Claude Code
/// sessions); the scan must be a silent no-op, not an error.
#[tokio::test]
async fn claude_missing_projects_dir_is_silent_noop() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    // `~/.claude` exists but holds no `projects/` subdir, like a machine
    // where Claude Code never ran (only backups or settings live there).
    std::fs::create_dir_all(home.join(".claude/backups")).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    assert_eq!(stats.sessions_upserted, 0);
    assert_eq!(stats.messages_upserted, 0);
}

/// Writes a Claude Code transcript with an assistant `tool_use` line and a
/// paired user `tool_result` line, both with recorded `cwd` matching `project`.
fn write_claude_tool_event_transcript(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let cwd = project.to_string_lossy();
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "tool-a1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Running a shell command to list files."},
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}
                ]
            }
        }),
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "tool-a2",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "file listing output"}
                ]
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn claude_tool_use_stays_out_of_searchable_message_text() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_tool_event_transcript(&home, &project, "claude-tool-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    // No new rows beyond the normal two message rows: tool events stay typed
    // facts on the existing assistant/user rows, not separate rows.
    assert_eq!(stats.messages_upserted, 2);
    assert_eq!(stats.sessions_upserted, 1);

    let results = db
        .search_session_messages("claude", None, "shell command", 10)
        .await;
    assert_eq!(results.len(), 1);
    let assistant = &results[0];
    assert_eq!(assistant.message.kind.as_deref(), Some("message"));
    assert_eq!(assistant.message.tool_names.as_deref(), Some("Bash"));
    assert!(
        assistant
            .message
            .text
            .contains("Running a shell command to list files.")
    );
    assert!(
        !assistant.message.text.contains("tool_use"),
        "tool_use must stay typed facts/metadata, not searchable message text"
    );
    let metadata: serde_json::Value =
        serde_json::from_str(assistant.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["tool_use_id"], "toolu_1",
        "the assistant row carries Claude's own tool_use.id"
    );
    // A Bash call edits nothing: the session records no edited_files array.
    let session = db.get_session("claude", "claude-tool-sess").await.unwrap();
    let recorded = session
        .metadata_json
        .as_deref()
        .map(|metadata| serde_json::from_str::<serde_json::Value>(metadata).unwrap());
    assert!(
        recorded
            .as_ref()
            .is_none_or(|metadata| metadata.get("edited_files").is_none()),
        "no edit result, no edited_files: {recorded:?}"
    );
}

/// Writes the two records Claude Code appends for one `Edit` call: the
/// assistant `tool_use` block (with Claude's `toolu_…` id) and the user
/// `tool_result` record whose `toolUseResult` names the edited `filePath`
/// and its `structuredPatch` hunks.
fn write_claude_edit_transcript(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let cwd = project.to_string_lossy();
    let file_path = project.join("src/lib.rs").to_string_lossy().into_owned();
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "edit-a1",
            "parentUuid": null,
            "isSidechain": false,
            "timestamp": "2026-01-01T00:00:00.500Z",
            "message": {
                "id": "msg_edit_1",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [
                    {"type": "text", "text": "Renaming the exported helper."},
                    {"type": "tool_use", "id": "toolu_01DBxBP9umzsnpVGUjGSUeGk", "name": "Edit",
                     "input": {"file_path": file_path, "old_string": "fn old()", "new_string": "fn renamed()"}}
                ]
            }
        }),
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "edit-u1",
            "parentUuid": "edit-a1",
            "isSidechain": false,
            "sourceToolAssistantUUID": "edit-a1",
            "timestamp": "2026-01-01T00:00:01.250Z",
            "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_01DBxBP9umzsnpVGUjGSUeGk",
                     "content": format!("The file {file_path} has been updated.")}
                ]
            },
            "toolUseResult": {
                "filePath": file_path,
                "oldString": "fn old()",
                "newString": "fn renamed()",
                "originalFile": "fn old() {}\n",
                "structuredPatch": [
                    {"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
                     "lines": ["-fn old() {}", "+fn renamed() {}"]}
                ],
                "userModified": false,
                "replaceAll": false
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn claude_edit_result_records_edit_time_and_tool_use_id() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_edit_transcript(&home, &project, "claude-edit-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 2);

    let session = db.get_session("claude", "claude-edit-sess").await.unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["edited_files"],
        serde_json::json!([{
            "path": project.join("src/lib.rs").to_string_lossy(),
            "edited_at_micros": 1_767_225_601_250_000i64,
            "hunks": 1
        }]),
        "the rollup carries the result record's own timestamp; Edit reports no change type"
    );

    let assistant = db
        .search_session_messages("claude", None, "exported helper", 10)
        .await;
    assert_eq!(assistant.len(), 1);
    let tool_use: serde_json::Value =
        serde_json::from_str(assistant[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(tool_use["tool_use_id"], "toolu_01DBxBP9umzsnpVGUjGSUeGk");

    let result = db
        .search_session_messages("claude", None, "has been updated", 10)
        .await;
    assert_eq!(result.len(), 1);
    let result_metadata = result[0]
        .message
        .metadata_json
        .as_deref()
        .map(|metadata| serde_json::from_str::<serde_json::Value>(metadata).unwrap());
    assert!(
        result_metadata
            .as_ref()
            .is_none_or(|metadata| metadata.get("tool_use_id").is_none()),
        "the tool_result row is not a tool use: {result_metadata:?}"
    );
    assert!(
        result_metadata
            .as_ref()
            .is_none_or(|metadata| metadata.get("edited_files").is_none()),
        "the rollup lives on the session row, not the message: {result_metadata:?}"
    );

    // Re-ingesting the same transcript does not duplicate the entry.
    let again = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    assert_eq!(again.messages_upserted, 0);
    let session = db.get_session("claude", "claude-edit-sess").await.unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["edited_files"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn claude_subagent_layout_uses_parent_cwd_fallback() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_transcript(&home, &project, "parent-claude");
    write_claude_subagent_transcript(&home, "parent-claude", "worker");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 3);

    // The cwd-less subagent inherits the parent's cwd, so its message lands
    // in-project.
    let results = db
        .search_session_messages("claude", None, "fallback evidence", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "agent-worker");
}

/// Writes a sidechain transcript shaped like Claude Code's `Agent` subagents:
/// records carry `isSidechain`, `agentId`, and the root `sessionId`; the
/// sibling `agent-<id>.meta.json` sidecar carries `toolUseId` (the spawning
/// `tool_use` block id) and, for a subagent spawned by a subagent,
/// `parentAgentId`.
fn write_claude_sidechain(
    dir: &std::path::Path,
    root_session: &str,
    agent_id: &str,
    meta: Option<serde_json::Value>,
) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(format!("agent-{agent_id}.jsonl")),
        format!(
            "{}\n",
            serde_json::json!({
                "parentUuid": null,
                "isSidechain": true,
                "agentId": agent_id,
                "type": "user",
                "sessionId": root_session,
                "uuid": format!("{agent_id}-u1"),
                "timestamp": "2026-01-01T00:00:10.000Z",
                "message": {"role": "user", "content": format!("Sidechain {agent_id} reviews the writeback journal")}
            })
        ),
    )
    .unwrap();
    if let Some(meta) = meta {
        std::fs::write(
            dir.join(format!("agent-{agent_id}.meta.json")),
            meta.to_string(),
        )
        .unwrap();
    }
}

/// Parentage comes from the child's own host records: the directory above
/// `subagents/` (or the sidecar's `parentAgentId`) names the parent session and
/// the sidecar's `toolUseId` the spawning call. A workflow subagent's sidecar
/// records no `toolUseId`, and a missing sidecar leaves only the directory
/// parent.
#[tokio::test]
async fn claude_sidechain_records_parent_session_and_spawning_tool_use() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let root = "1b206fb6-57ab-4abe-aa7e-51ac8e1168ce";
    write_claude_transcript(&home, &project, root);
    let subagents = home
        .join(".claude/projects/-some-slug")
        .join(root)
        .join("subagents");
    write_claude_sidechain(
        &subagents,
        root,
        "a0dbe36644e553cf1",
        Some(serde_json::json!({
            "agentType": "general-purpose",
            "description": "Simplification review: writeback",
            "toolUseId": "toolu_018BToBNsKzNLGunCcnRrSUR",
            "spawnDepth": 1
        })),
    );
    write_claude_sidechain(
        &subagents,
        root,
        "a270b6e7a135dc633",
        Some(serde_json::json!({
            "agentType": "general-purpose",
            "description": "Simplification review: sftp + nbd + cli",
            "toolUseId": "toolu_01NewiTR1eBNZGXRqwxkHJVM",
            "parentAgentId": "a0dbe36644e553cf1",
            "spawnDepth": 2
        })),
    );
    write_claude_sidechain(
        &subagents.join("workflows").join("wf_run123"),
        root,
        "aworkflow01",
        Some(serde_json::json!({"agentType": "workflow", "spawnDepth": 1})),
    );
    write_claude_sidechain(&subagents, root, "anometa02", None);

    let db = open_project_session_db(&project).await.unwrap();
    let stats = try_ingest_claude_source(&db, &ClaudeSource::with_home(&home), &project)
        .await
        .unwrap();
    assert_eq!(stats.sessions_upserted, 5);

    let mut rows = Vec::new();
    for id in [
        root,
        "agent-a0dbe36644e553cf1",
        "agent-a270b6e7a135dc633",
        "agent-aworkflow01",
        "agent-anometa02",
    ] {
        let session = db.get_session("claude", id).await.unwrap();
        rows.push((
            id,
            session.parent_session_id,
            session.parent_tool_use_id,
            session.is_subagent,
        ));
    }
    let owned = |id: &str| Some(id.to_owned());
    assert_eq!(
        rows,
        vec![
            (root, None, None, false),
            (
                "agent-a0dbe36644e553cf1",
                owned(root),
                owned("toolu_018BToBNsKzNLGunCcnRrSUR"),
                true
            ),
            // A subagent spawned by a subagent forks from that subagent's call.
            (
                "agent-a270b6e7a135dc633",
                owned("agent-a0dbe36644e553cf1"),
                owned("toolu_01NewiTR1eBNZGXRqwxkHJVM"),
                true
            ),
            ("agent-aworkflow01", owned(root), None, true),
            ("agent-anometa02", owned(root), None, true),
        ]
    );
}

/// Writes a cwd-less subagent transcript nested under
/// `subagents/workflows/<workflow_run>/`.
fn write_claude_workflow_subagent(
    home: &std::path::Path,
    parent_session: &str,
    agent_id: &str,
    workflow_run: &str,
) {
    let dir = home
        .join(".claude/projects/-some-slug")
        .join(parent_session)
        .join("subagents")
        .join("workflows")
        .join(workflow_run);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("agent-{agent_id}.jsonl"));
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "sessionId": format!("agent-{agent_id}"),
                "uuid": "nested-u1",
                "timestamp": "2026-01-01T00:00:10.000Z",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "Nested worker verified the fallback evidence trail."}]
                }
            })
        ),
    )
    .unwrap();
}

#[tokio::test]
async fn claude_workflow_nested_subagent_uses_parent_cwd_fallback() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_transcript(&home, &project, "parent-wf");
    write_claude_workflow_subagent(&home, "parent-wf", "nested", "wf_run123");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_claude_source(&db, &source, &project)
        .await
        .unwrap();
    assert_eq!(stats.sessions_upserted, 2);

    // The parent is the directory above `subagents/`, not the file's immediate
    // parent, so the nested subagent inherits the parent's cwd and its message
    // lands in-project.
    let results = db
        .search_session_messages("claude", None, "fallback evidence trail", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "agent-nested");
}

#[tokio::test]
async fn claude_observation_path_conflicting_redelivery_does_not_overwrite() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    init_git_repo(&project);
    mark_test_project(&project);
    let path = write_claude_transcript(&home, &project, "claude-obs-conflict");

    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Claude))
            .await
            .messages_upserted,
        2
    );
    assert!(durable_table_count(&db, "observations").await >= 1);
    let original = db
        .search_session_messages("claude", None, "fixed", 10)
        .await;
    assert_eq!(original.len(), 1);
    // Claude projection identity is the transcript record uuid, not the API
    // message.id (which repeats across streamed rows of one API response).
    assert_eq!(original[0].message.message_id, "u2");
    let original_text = original[0].message.text.clone();
    drop(db);

    // Same record uuid with different content is a conflicting V1 output
    // identity. The observation itself has a distinct byte range, but
    // projection must fail closed and preserve the first durable message row.
    let conflicting = serde_json::json!({
        "type": "assistant",
        "cwd": project,
        "sessionId": "claude-obs-conflict",
        "uuid": "u2",
        "timestamp": "2026-01-01T00:00:06.000Z",
        "message": {
            "id": "msg_claude_1",
            "role": "assistant",
            "content": "Conflicting Claude overwrite attempt."
        }
    });
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap(),
        "{conflicting}"
    )
    .unwrap();

    let again = open_project_session_db(&project).await.unwrap();
    let _ =
        ingest_global_sources_for_provider(&home, &again, &project, Some(SessionProvider::Claude))
            .await;
    let replayed = again
        .search_session_messages("claude", None, "fixed", 10)
        .await;
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].message.message_id, "u2");
    assert_eq!(replayed[0].message.text, original_text);
    assert!(
        again
            .search_session_messages("claude", None, "overwrite", 10)
            .await
            .is_empty()
    );
}
