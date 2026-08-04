use std::io::Write;

use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
#[cfg(all(unix, not(target_os = "macos")))]
use tracedecay::global_db::ParseOffset;
use tracedecay::sessions::SessionProvider;
use tracedecay::sessions::claude::ClaudeSource;
use tracedecay::sessions::git_correlation::{
    CommitEvidence, CommitRelation, GitRefFilter, SessionsForQuery, SpanOverlapKind,
};
#[cfg(all(unix, not(target_os = "macos")))]
use tracedecay::sessions::source::TranscriptSource;

use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
use crate::restart_atomicity::{
    durable_table_count, ingest_global_sources_for_provider, mark_test_project,
    open_project_session_db, try_ingest_source,
};
use crate::support::{assert_metadata_path_eq, init_git_repo, init_project_at, run_git, setup};

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
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 1);

    let cursor_key = source.cursor_key(&path).durable_text();
    let offset = db
        .get_parse_offset(&cursor_key)
        .await
        .expect("lossless cursor key persisted");
    assert_eq!(offset.byte_offset, std::fs::metadata(&path).unwrap().len());
    assert_eq!(
        db.get_parse_offset(&path.to_string_lossy()).await,
        None,
        "lossy path aliases are not persisted"
    );

    drop(db);
    let reopened = open_project_session_db(&project).await.unwrap();
    let replay = try_ingest_source(&reopened, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(replay, Default::default());
    assert_eq!(
        reopened.get_parse_offset(&cursor_key).await,
        Some(offset),
        "canonical cursor survives restart"
    );
}

// macOS filesystems reject invalid UTF-8 path components with EILSEQ.
#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
async fn claude_non_utf8_cursor_key_ignores_lossy_path_alias() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".claude/projects/-non-utf8-migration");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(OsString::from_vec(b"session-\xfe.jsonl".to_vec()));
    let row = |uuid: &str, content: &str| {
        serde_json::json!({
            "type": "user",
            "cwd": project,
            "sessionId": "native-migration-session",
            "uuid": uuid,
            "timestamp": "2026-01-01T00:00:00Z",
            "message": {"role": "user", "content": content}
        })
    };
    let prefix = format!("{}\n", row("legacy-row", "Already ingested legacy row"));
    let suffix = format!("{}\n", row("new-row", "New native path evidence"));
    std::fs::write(&path, format!("{prefix}{suffix}")).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let legacy_key = path.to_string_lossy().into_owned();
    db.runtime()
        .set_project_parse_offset_for_test(
            &legacy_key,
            ParseOffset {
                byte_offset: prefix.len() as u64,
                mtime: 0,
                file_id: 0,
            },
        )
        .await
        .unwrap();

    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 2);
    assert!(
        db.get_session_message("claude", "legacy-row")
            .await
            .is_some()
    );
    assert!(db.get_session_message("claude", "new-row").await.is_some());

    let final_offset = std::fs::metadata(&path).unwrap().len();
    let durable_key = source.cursor_key(&path).durable_text();
    assert_eq!(
        db.get_parse_offset(&durable_key).await.unwrap().byte_offset,
        final_offset
    );
    assert_eq!(
        db.get_parse_offset(&legacy_key).await.unwrap().byte_offset,
        prefix.len() as u64,
        "lossy path aliases are neither read nor advanced"
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
    std::fs::create_dir_all(&profile).unwrap();

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
    let stats = runtime
        .ingest_profile_transcript_source_for_test(&source, &profile, None)
        .await
        .unwrap();
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
    std::fs::create_dir_all(&profile).unwrap();
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
    let stats = runtime
        .ingest_profile_transcript_source_for_test(&source, &profile, None)
        .await
        .unwrap();
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

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 2);
    assert_eq!(stats.sessions_upserted, 1);

    let results = db
        .search_session_messages(
            "claude",
            Some(project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
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

    // Anthropic-style `message.usage` counters land in metadata under the
    // keys the savings dashboard reads; non-counter fields are dropped.
    let assistant = results
        .iter()
        .find(|hit| hit.message.role == "assistant")
        .expect("assistant message should be searchable");
    let metadata: serde_json::Value =
        serde_json::from_str(assistant.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata["claude_message_cwd"], &project);
    assert_metadata_path_eq(&metadata["claude_message_worktree"], &project);
    assert_eq!(
        metadata["claude_message_location_provenance"].as_str(),
        Some("transcript_record")
    );
    assert!(metadata.get("claude_git_branch").is_none());
    assert_eq!(metadata["usage"]["input_tokens"], 1200);
    assert_eq!(metadata["usage"]["output_tokens"], 340);
    assert_eq!(metadata["usage"]["cache_creation_input_tokens"], 500);
    assert_eq!(metadata["usage"]["cache_read_input_tokens"], 8000);
    assert!(metadata["usage"].get("service_tier").is_none());
    let user = results
        .iter()
        .find(|hit| hit.message.role == "user")
        .expect("user message should be searchable");
    let user_metadata: serde_json::Value =
        serde_json::from_str(user.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&user_metadata["claude_message_cwd"], &project);
    assert_metadata_path_eq(&user_metadata["claude_message_worktree"], &project);
    assert_eq!(
        user_metadata["claude_message_location_provenance"].as_str(),
        Some("transcript_record")
    );
    assert!(user_metadata.get("usage").is_none());
    let session_metadata: serde_json::Value =
        serde_json::from_str(results[0].session.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&session_metadata["claude_session_cwd"], &project);
    assert_metadata_path_eq(&session_metadata["claude_session_worktree"], &project);
    assert_eq!(
        session_metadata["claude_session_location_provenance"].as_str(),
        Some("transcript_session")
    );

    // Privacy contract: Message facts carry only authored text. Tool use is a
    // typed ToolInvocation fact / tool_events metadata, never searchable JSON.
    let raw = db
        .lcm_load_raw_message("claude", "msg_claude_1")
        .await
        .expect("authored Claude content should be in raw LCM storage");
    assert_eq!(raw.content, "The billing pipeline regression is fixed.");
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

    // Two provider-authored visible messages project as ordinary rows. The
    // plaintext thinking block remains a separately typed reasoning row.
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 3);

    let reasoning_results = db
        .search_session_messages(
            "claude",
            Some(project.to_string_lossy().as_ref()),
            "reasoning breadcrumb",
            10,
        )
        .await;
    assert_eq!(reasoning_results.len(), 1);
    assert!(
        reasoning_results
            .iter()
            .all(|hit| hit.message.kind.as_deref() == Some("reasoning"))
    );
    assert!(
        reasoning_results
            .iter()
            .all(|hit| hit.message.kind.as_deref() != Some("message"))
    );

    let visible_results = db
        .search_session_messages(
            "claude",
            Some(project.to_string_lossy().as_ref()),
            "Traced it",
            10,
        )
        .await;
    let message = visible_results
        .iter()
        .find(|hit| hit.message.kind.as_deref() == Some("message"))
        .expect("assistant authored message row");
    assert_eq!(message.message.message_id, "msg_thinking_1");
    assert_eq!(message.message.text, "Traced it.");
    assert_eq!(message.message.tool_names.as_deref(), Some("Read"));
    let redacted_results = db
        .search_session_messages(
            "claude",
            Some(project.to_string_lossy().as_ref()),
            "ENCRYPTED_SHOULD_NEVER_INDEX",
            10,
        )
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

    // Re-ingesting the unchanged transcript is a no-op: the reasoning row's
    // stable `:thinking` id keeps the insert idempotent.
    let second = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(second.messages_upserted, 0);
}

#[tokio::test]
async fn claude_transcript_ingest_is_incremental() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_claude_transcript(&home, &project, "claude-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let first = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(first.messages_upserted, 2);
    // Re-ingesting the unchanged file is a no-op.
    let second = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(second.messages_upserted, 0);

    // Appending one line ingests only that line.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        f,
        "{}",
        serde_json::json!({
            "type": "user",
            "cwd": project.to_string_lossy(),
            "sessionId": "claude-sess",
            "uuid": "u3",
            "timestamp": "2026-01-01T00:01:00.000Z",
            "message": {"role": "user", "content": "Add a regression test for billing"}
        })
    )
    .unwrap();
    drop(f);

    let third = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(third.messages_upserted, 1);
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

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(
        stats.messages_upserted, 0,
        "a transcript whose cwd is a different project must be skipped"
    );

    // The cursor must still advance past the filtered-out content, or every
    // future sweep re-reads and re-filters the whole foreign transcript.
    let file_size = std::fs::metadata(&path).unwrap().len();
    let path_str = path.to_string_lossy();
    let mut offset = db.get_parse_offset(path_str.as_ref()).await;
    if offset.is_none() && cfg!(windows) {
        // The scanner stores native separators; the helper built this path
        // with embedded forward slashes.
        offset = db.get_parse_offset(&path_str.replace('/', "\\")).await;
    }
    let offset = offset.expect("skipped foreign transcript should persist a parse offset");
    assert_eq!(
        offset.byte_offset, file_size,
        "parse cursor should sit at EOF for a fully filtered transcript"
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
    let stats_a = try_ingest_source(&db_a, &source, &project_a, None)
        .await
        .unwrap();
    assert_eq!(stats_a.messages_upserted, 1);
    let hits_a = db_a
        .search_session_messages("claude", None, "worktree marker", 10)
        .await;
    assert_eq!(hits_a.len(), 1);
    assert!(hits_a[0].message.text.contains("alpha worktree marker"));
    let metadata_a: serde_json::Value =
        serde_json::from_str(hits_a[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata_a["claude_message_cwd"], &project_a);
    assert_metadata_path_eq(&metadata_a["claude_message_worktree"], &project_a);
    assert_eq!(
        metadata_a["claude_message_location_provenance"].as_str(),
        Some("transcript_record")
    );
    drop(db_a);

    let db_b = open_project_session_db(&project_b).await.unwrap();
    let stats_b = try_ingest_source(&db_b, &source, &project_b, None)
        .await
        .unwrap();
    assert_eq!(stats_b.messages_upserted, 1);
    let hits_b = db_b
        .search_session_messages("claude", None, "worktree marker", 10)
        .await;
    assert_eq!(hits_b.len(), 1);
    assert!(hits_b[0].message.text.contains("beta worktree marker"));
    let metadata_b: serde_json::Value =
        serde_json::from_str(hits_b[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata_b["claude_message_cwd"], &project_b);
    assert_metadata_path_eq(&metadata_b["claude_message_worktree"], &project_b);
    assert_eq!(
        metadata_b["claude_message_location_provenance"].as_str(),
        Some("transcript_record")
    );
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

    let stats = try_ingest_source(&db, &source, &project, None)
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
async fn claude_tool_use_and_results_populate_tool_event_metadata() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_tool_event_transcript(&home, &project, "claude-tool-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    // No new rows beyond the normal two message rows: tool events are metadata
    // on the existing assistant/user rows, not separate rows.
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
    let assistant_metadata: serde_json::Value =
        serde_json::from_str(assistant.message.metadata_json.as_deref().unwrap()).unwrap();
    let tool_events = assistant_metadata["tool_events"]
        .as_array()
        .expect("assistant row should carry tool_events metadata");
    assert_eq!(tool_events.len(), 1);
    assert_eq!(tool_events[0]["type"], "tool_use");
    assert_eq!(tool_events[0]["tool_name"], "Bash");
    assert_eq!(tool_events[0]["call_id"], "toolu_1");
    assert!(tool_events[0]["input_bytes"].as_u64().unwrap() > 0);

    let user_results = db
        .search_session_messages("claude", None, "listing output", 10)
        .await;
    assert_eq!(user_results.len(), 1);
    let user = &user_results[0];
    assert_eq!(user.message.kind.as_deref(), Some("message"));
    let user_metadata: serde_json::Value =
        serde_json::from_str(user.message.metadata_json.as_deref().unwrap()).unwrap();
    let user_tool_events = user_metadata["tool_events"]
        .as_array()
        .expect("user row should carry tool_events metadata");
    assert_eq!(user_tool_events.len(), 1);
    assert_eq!(user_tool_events[0]["type"], "tool_result");
    assert_eq!(user_tool_events[0]["call_id"], "toolu_1");
    assert!(user_tool_events[0]["output_bytes"].as_u64().unwrap() > 0);
}

/// Writes a Claude Code transcript with a `type=="system"` hook record that
/// carries `hookErrors`, a routine `type=="system"` record with no signal, and
/// one normal user message.
fn write_claude_system_hook_transcript(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let cwd = project.to_string_lossy();
    let contents = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({
            "type": "system",
            "cwd": cwd,
            "sessionId": session,
            "subtype": "stop_hook_summary",
            "uuid": "hook-1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "toolUseID": "tu-1",
            "hookCount": 2,
            "hookInfos": [{"command": "lint.sh", "durationMs": 12}],
            "hookErrors": ["hook boom failed"],
            "hookAdditionalContext": [],
            "preventedContinuation": false,
            "stopReason": "",
            "level": "error"
        }),
        serde_json::json!({
            "type": "system",
            "cwd": cwd,
            "sessionId": session,
            "subtype": "stop_hook_summary",
            "uuid": "hook-2",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "toolUseID": "tu-2",
            "hookCount": 1,
            "hookInfos": [{"command": "routine-marker-command"}],
            "hookErrors": [],
            "hookAdditionalContext": [],
            "preventedContinuation": false,
            "stopReason": "",
            "level": "info"
        }),
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "hook-3",
            "timestamp": "2026-01-01T00:00:02.000Z",
            "message": {"role": "user", "content": "Continue the billing investigation"}
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn claude_system_hook_errors_become_searchable_hook_events() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_system_hook_transcript(&home, &project, "claude-hook-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    // The routine system record produces no row; only the user message and
    // one hook-event row are ingested.
    assert_eq!(stats.messages_upserted, 2);

    let results = db.search_session_messages("claude", None, "boom", 10).await;
    assert_eq!(results.len(), 1);
    let hit = &results[0];
    assert_eq!(hit.message.role, "tool");
    assert_eq!(hit.message.kind.as_deref(), Some("hook_event"));
    assert!(
        hit.message
            .text
            .contains("Claude hook event: stop_hook_summary")
    );
    assert!(hit.message.text.contains("tool_use_id: tu-1"));
    let metadata: serde_json::Value =
        serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "claude_system_record");
    assert!(metadata.get("hook_count").is_some());

    // Durable message identity does not leak an absolute checkout/cache path.
    let source_path = hit.message.source_path.as_deref().unwrap();
    assert!(source_path.starts_with("tracedecay-claude-observation-source-v1-sha256-"));
    assert!(!source_path.contains(tmp.path().to_string_lossy().as_ref()));
    assert!(hit.message.source_offset.is_some());

    let routine = db
        .search_session_messages("claude", None, "routine-marker-command", 10)
        .await;
    assert!(
        routine.is_empty(),
        "routine system record without signal must not produce a row"
    );
}

#[tokio::test]
async fn claude_subagent_layout_uses_parent_link_and_parent_cwd_fallback() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_transcript(&home, &project, "parent-claude");
    write_claude_subagent_transcript(&home, "parent-claude", "worker");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 3);

    let child = db
        .get_session("claude", "agent-worker")
        .await
        .expect("subagent session should be stored");
    assert_eq!(child.parent_session_id.as_deref(), Some("parent-claude"));
    assert!(child.is_subagent);
    assert_eq!(child.agent_id.as_deref(), Some("worker"));

    let results = db
        .search_session_messages("claude", None, "fallback evidence", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "agent-worker");

    // Regression: a plain session with no PR links, edits, or subagent facts
    // must not gain any of the new session-metadata keys.
    let session = db
        .get_session("claude", "parent-claude")
        .await
        .expect("parent session should be stored");
    let metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    for absent in [
        "pr_links",
        "edited_files",
        "agent_type",
        "agent_description",
        "spawn_depth",
        "workflow_run_id",
    ] {
        assert!(
            metadata.get(absent).is_none(),
            "plain session metadata should not carry `{absent}`"
        );
    }
}

/// Writes a transcript with a normal user turn (so the session cwd resolves)
/// followed by a `type=="pr-link"` record that carries no cwd of its own.
fn write_claude_pr_link_transcript(
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
            "uuid": "pr-u1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {"role": "user", "content": "Open the pull request for the billing fix"}
        }),
        serde_json::json!({
            "type": "pr-link",
            "sessionId": session,
            "uuid": "pr-link-1",
            "timestamp": "2026-01-01T00:00:05.000Z",
            "prNumber": 42,
            "prUrl": "https://github.com/acme/widgets/pull/42",
            "prRepository": "acme/widgets"
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn claude_pr_link_record_becomes_marker_row_and_session_summary() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_pr_link_transcript(&home, &project, "claude-pr-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    // The user turn plus a dedicated pr_link marker row.
    assert_eq!(stats.messages_upserted, 2);

    // The marker row is retrievable by its stable, kind-scoped id.
    let marker = db
        .get_session_message("claude", "pr_link:pr-link-1")
        .await
        .expect("pr-link record should produce a marker row");
    assert_eq!(marker.kind.as_deref(), Some("pr_link"));
    assert_eq!(marker.session_id, "claude-pr-sess");
    assert!(marker.text.contains("acme/widgets"));
    let marker_metadata: serde_json::Value =
        serde_json::from_str(marker.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(marker_metadata["source"], "claude_pr_link");
    assert_eq!(marker_metadata["pr_number"], 42);
    assert_eq!(
        marker_metadata["pr_url"],
        "https://github.com/acme/widgets/pull/42"
    );
    assert_eq!(marker_metadata["pr_repository"], "acme/widgets");

    // message_search finds the marker by its human-readable text.
    let hits = db
        .search_session_messages("claude", None, "PR link", 10)
        .await;
    assert!(
        hits.iter()
            .any(|hit| hit.message.kind.as_deref() == Some("pr_link"))
    );

    // The session draft carries the PR link in its pr_links[] summary.
    let session = db
        .get_session("claude", "claude-pr-sess")
        .await
        .expect("session should be stored");
    let session_metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    let pr_links = session_metadata["pr_links"]
        .as_array()
        .expect("session should carry a pr_links summary");
    assert_eq!(pr_links.len(), 1);
    assert_eq!(pr_links[0]["pr_number"], 42);
    assert_eq!(pr_links[0]["pr_repository"], "acme/widgets");
}

#[tokio::test]
async fn claude_assistant_attribution_fields_land_in_metadata() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("claude-attrib.jsonl");
    let cwd = project.to_string_lossy();
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "cwd": cwd,
                "sessionId": "claude-attrib",
                "uuid": "attrib-1",
                "timestamp": "2026-01-01T00:00:00.000Z",
                "attributionMcpServer": "tracedecay",
                "attributionMcpTool": "tracedecay_context",
                "attributionSkill": "exploring-code",
                "promptSource": "user",
                "origin": "cli",
                "message": {
                    "id": "msg_attrib_1",
                    "role": "assistant",
                    "model": "claude-opus-4-8",
                    "content": [{"type": "text", "text": "Adoption ground truth turn"}]
                }
            })
        ),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 1);

    let assistant = db
        .get_session_message("claude", "msg_attrib_1")
        .await
        .expect("assistant row should be stored");
    let metadata: serde_json::Value =
        serde_json::from_str(assistant.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["attribution_mcp_server"], "tracedecay");
    assert_eq!(metadata["attribution_mcp_tool"], "tracedecay_context");
    assert_eq!(metadata["attribution_skill"], "exploring-code");
    assert_eq!(metadata["prompt_source"], "user");
    assert_eq!(metadata["origin"], "cli");
}

#[tokio::test]
async fn claude_tool_use_result_edited_files_populate_metadata_and_summary() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("claude-edits.jsonl");
    let cwd = project.to_string_lossy();
    let contents = format!(
        "{}\n{}\n",
        // Edit result: no explicit `type`, two structured patch hunks.
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": "claude-edits",
            "uuid": "edit-1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "toolUseResult": {
                "filePath": "/repo/src/lib.rs",
                "oldString": "a",
                "newString": "b",
                "structuredPatch": [{"lines": ["-a", "+b"]}, {"lines": ["-c", "+d"]}]
            },
            "message": {
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "toolu_edit", "content": "edit applied"}]
            }
        }),
        // Write result: explicit `type` "create".
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": "claude-edits",
            "uuid": "edit-2",
            "timestamp": "2026-01-01T00:00:01.000Z",
            "toolUseResult": {
                "type": "create",
                "filePath": "/repo/src/new.rs",
                "content": "fn main() {}",
                "structuredPatch": [{"lines": ["+fn main() {}"]}]
            },
            "message": {
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "toolu_write", "content": "file created"}]
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 2);

    let edit = db
        .get_session_message("claude", "edit-1")
        .await
        .expect("edit tool_result row should be stored");
    let edit_metadata: serde_json::Value =
        serde_json::from_str(edit.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(edit_metadata["edited_file"]["path"], "/repo/src/lib.rs");
    assert_eq!(edit_metadata["edited_file"]["change_type"], "edit");
    assert_eq!(edit_metadata["edited_file"]["hunks"], 2);

    let write = db
        .get_session_message("claude", "edit-2")
        .await
        .expect("write tool_result row should be stored");
    let write_metadata: serde_json::Value =
        serde_json::from_str(write.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(write_metadata["edited_file"]["path"], "/repo/src/new.rs");
    assert_eq!(write_metadata["edited_file"]["change_type"], "create");
    assert_eq!(write_metadata["edited_file"]["hunks"], 1);

    // Session draft carries the deduped edited-files summary.
    let session = db
        .get_session("claude", "claude-edits")
        .await
        .expect("session should be stored");
    let session_metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    let edited_files = session_metadata["edited_files"]
        .as_array()
        .expect("session should carry an edited_files summary");
    assert_eq!(edited_files.len(), 2);
    let paths: Vec<&str> = edited_files
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .collect();
    assert!(paths.contains(&"/repo/src/lib.rs"));
    assert!(paths.contains(&"/repo/src/new.rs"));
}

#[tokio::test]
async fn claude_compact_boundary_record_becomes_marker_row() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("claude-compact.jsonl");
    let cwd = project.to_string_lossy();
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": "claude-compact",
            "uuid": "compact-u1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {"role": "user", "content": "Keep working after compaction"}
        }),
        serde_json::json!({
            "type": "system",
            "subtype": "compact_boundary",
            "sessionId": "claude-compact",
            "uuid": "compact-1",
            "timestamp": "2026-01-01T00:00:05.000Z",
            "logicalParentUuid": "pre-compact-parent",
            "compactMetadata": {"trigger": "auto", "preTokens": 150000}
        }),
    );
    std::fs::write(&path, contents).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 2);

    let marker = db
        .get_session_message("claude", "compact_boundary:compact-1")
        .await
        .expect("compact_boundary record should produce a marker row");
    assert_eq!(marker.kind.as_deref(), Some("compact_boundary"));
    assert_eq!(marker.role, "system");
    let metadata: serde_json::Value =
        serde_json::from_str(marker.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "claude_compact_boundary");
    assert_eq!(metadata["trigger"], "auto");
    assert_eq!(metadata["pre_tokens"], 150000);
    assert_eq!(metadata["logical_parent_uuid"], "pre-compact-parent");
}

#[tokio::test]
async fn claude_model_refusal_fallback_record_becomes_marker_row() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("claude-fallback.jsonl");
    let cwd = project.to_string_lossy();
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": "claude-fallback",
            "uuid": "fallback-u1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {"role": "user", "content": "Draft the release note"}
        }),
        serde_json::json!({
            "type": "system",
            "subtype": "model_refusal_fallback",
            "sessionId": "claude-fallback",
            "uuid": "fallback-1",
            "timestamp": "2026-01-01T00:00:05.000Z",
            "originalModel": "claude-opus-4-8",
            "fallbackModel": "claude-sonnet-4-6",
            "trigger": "refusal",
            "apiRefusalCategory": "policy"
        }),
    );
    std::fs::write(&path, contents).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 2);

    let marker = db
        .get_session_message("claude", "model_fallback:fallback-1")
        .await
        .expect("model_refusal_fallback record should produce a marker row");
    assert_eq!(marker.kind.as_deref(), Some("model_fallback"));
    assert_eq!(marker.model.as_deref(), Some("claude-sonnet-4-6"));
    let metadata: serde_json::Value =
        serde_json::from_str(marker.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "claude_model_fallback");
    assert_eq!(metadata["original_model"], "claude-opus-4-8");
    assert_eq!(metadata["fallback_model"], "claude-sonnet-4-6");
    assert_eq!(metadata["trigger"], "refusal");
    assert_eq!(metadata["api_refusal_category"], "policy");
}

/// Writes a subagent transcript plus its sibling `agent-<id>.meta.json`. When
/// `workflow_run` is `Some`, the subagent is nested under
/// `subagents/workflows/wf_<run>/` (the layout that used to ingest as an orphan
/// standalone session).
fn write_claude_subagent_with_meta(
    home: &std::path::Path,
    parent_session: &str,
    agent_id: &str,
    workflow_run: Option<&str>,
) -> std::path::PathBuf {
    let mut dir = home
        .join(".claude/projects/-some-slug")
        .join(parent_session)
        .join("subagents");
    if let Some(run) = workflow_run {
        dir = dir.join("workflows").join(run);
    }
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
    // Sibling meta.json carrying spawn provenance.
    let meta_path = dir.join(format!("agent-{agent_id}.meta.json"));
    std::fs::write(
        &meta_path,
        serde_json::json!({
            "agentType": "Explore",
            "description": "Investigate the billing fallback path",
            "toolUseId": "toolu_spawn_42",
            "spawnDepth": 1
        })
        .to_string(),
    )
    .unwrap();
    path
}

#[tokio::test]
async fn claude_subagent_meta_json_enriches_draft() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_transcript(&home, &project, "parent-meta");
    write_claude_subagent_with_meta(&home, "parent-meta", "worker", None);

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.sessions_upserted, 2);

    let child = db
        .get_session("claude", "agent-worker")
        .await
        .expect("subagent session should be stored");
    assert!(child.is_subagent);
    assert_eq!(child.parent_session_id.as_deref(), Some("parent-meta"));
    assert_eq!(child.agent_id.as_deref(), Some("worker"));
    // toolUseId rides the dedicated parent_tool_use_id column.
    assert_eq!(child.parent_tool_use_id.as_deref(), Some("toolu_spawn_42"));

    let metadata: serde_json::Value =
        serde_json::from_str(child.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["agent_type"], "Explore");
    assert_eq!(
        metadata["agent_description"],
        "Investigate the billing fallback path"
    );
    assert_eq!(metadata["spawn_depth"], 1);
    // Not a workflow-nested subagent: no run id.
    assert!(metadata.get("workflow_run_id").is_none());
}

#[tokio::test]
async fn claude_workflow_nested_subagent_links_to_parent_not_orphan() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_transcript(&home, &project, "parent-wf");
    write_claude_subagent_with_meta(&home, "parent-wf", "nested", Some("wf_run123"));

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    // Parent session plus the workflow-nested subagent (not an orphan third).
    assert_eq!(stats.sessions_upserted, 2);

    let child = db
        .get_session("claude", "agent-nested")
        .await
        .expect("workflow-nested subagent session should be stored");
    assert!(
        child.is_subagent,
        "workflow-nested subagent must be flagged as a subagent, not an orphan standalone session"
    );
    assert_eq!(child.parent_session_id.as_deref(), Some("parent-wf"));
    assert_eq!(child.agent_id.as_deref(), Some("nested"));
    assert_eq!(child.parent_tool_use_id.as_deref(), Some("toolu_spawn_42"));

    let metadata: serde_json::Value =
        serde_json::from_str(child.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["workflow_run_id"], "wf_run123");
    assert_eq!(metadata["agent_type"], "Explore");
    assert_eq!(metadata["spawn_depth"], 1);

    // The subagent inherits the parent's cwd, so its message lands in-project.
    let results = db
        .search_session_messages("claude", None, "fallback evidence trail", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "agent-nested");
}

#[tokio::test]
async fn claude_git_operation_becomes_direct_producer_evidence_atomically() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    init_git_repo(&project);
    std::fs::write(project.join("commit.txt"), "commit evidence\n").unwrap();
    run_git(&project, &["add", "commit.txt"]);
    run_git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Tests",
            "-c",
            "user.email=tests@example.invalid",
            "commit",
            "-m",
            "commit evidence",
        ],
    );
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&project)
        .output()
        .unwrap();
    let sha = String::from_utf8(output.stdout).unwrap().trim().to_string();

    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let cwd = project.to_string_lossy();
    std::fs::write(
        dir.join("claude-commit.jsonl"),
        format!(
            "{}\n",
            serde_json::json!({
                "type": "user",
                "cwd": cwd,
                "gitBranch": "main",
                "sessionId": "claude-commit",
                "uuid": "commit-result-1",
                "timestamp": "2026-01-01T00:00:00.000Z",
                "message": {"role": "user", "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tool-commit",
                    "is_error": false,
                    "content": "commit complete"
                }]},
                "toolUseResult": {"gitOperation": {"commit": {
                    "sha": &sha[..8],
                    "kind": "committed"
                }}}
            })
        ),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 1);

    let message = db
        .get_session_message("claude", "commit-result-1")
        .await
        .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["produced_commit_candidates"],
        serde_json::json!([&sha[..8]])
    );
    assert_eq!(metadata["produced_commit_evidence"], "host_event");
    assert_eq!(metadata["produced_commit_kind"], "committed");

    let hits = db
        .runtime()
        .project_git_sessions_for_test(&SessionsForQuery {
            git_ref: GitRefFilter::Commit(sha[..8].to_string()),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].relation, Some(CommitRelation::Produced));
    assert_eq!(hits[0].evidence, Some(CommitEvidence::HostEvent));
    assert_eq!(hits[0].span_overlap_kind, Some(SpanOverlapKind::Direct));
    assert_eq!(
        hits[0].evidence_message_id.as_deref(),
        Some("commit-result-1")
    );
    let branch_hits = db
        .runtime()
        .project_git_sessions_for_test(&SessionsForQuery {
            git_ref: GitRefFilter::Branch("main".to_string()),
            since: None,
            until: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(branch_hits.len(), 1);
    assert_eq!(branch_hits[0].session_id, "claude-commit");
    assert_eq!(branch_hits[0].sources, vec!["ingest".to_string()]);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn claude_observation_path_conflicting_redelivery_does_not_overwrite() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let path = write_claude_transcript(&home, &project, "claude-obs-conflict");

    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Claude))
            .await
            .messages_upserted,
        2
    );
    assert!(durable_table_count(&db, "observations").await >= 1);
    let original = db
        .search_session_messages("claude", None, "fixed", 10)
        .await;
    assert_eq!(original.len(), 1);
    assert_eq!(original[0].message.message_id, "msg_claude_1");
    let original_text = original[0].message.text.clone();
    drop(db);

    // Same parser-evidenced message.id with different content is a conflicting
    // V1 output identity. The observation itself has a distinct byte range, but
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
        ingest_global_sources_for_provider(&again, &project, Some(SessionProvider::Claude)).await;
    let replayed = again
        .search_session_messages("claude", None, "fixed", 10)
        .await;
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].message.message_id, "msg_claude_1");
    assert_eq!(replayed[0].message.text, original_text);
    assert!(
        again
            .search_session_messages("claude", None, "overwrite", 10)
            .await
            .is_empty()
    );
}
