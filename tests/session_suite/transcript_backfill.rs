//! Coverage for the checkpointed transcript-facts backfill: legacy rows
//! ingested with `timestamp = NULL` (or without `metadata_json.usage`
//! counters) are re-derived from their source transcript files by background
//! maintenance, missing files are tolerated, already-dated rows (e.g.
//! Hermes-migrated messages) are left untouched, and the marker makes the
//! pass run once per store.

use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionScope;
use tracedecay::sessions::transcript_backfill::{
    TranscriptFactsBackfillOutcome, TranscriptFactsBackfillTestRuntimeV1,
};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay_domain::ProjectId;

const TEST_PROJECT_ID: &str = "project.transcript-backfill";

// 2026-06-10T09:11:00+02:00 and 2026-06-11T08:00:00+02:00.
const DAY_ONE: i64 = 1_781_075_460;
const DAY_TWO: i64 = 1_781_157_600;

fn init_project(tmp: &TempDir) -> std::path::PathBuf {
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tracedecay")).unwrap();
    std::fs::write(project.join(".tracedecay/tracedecay.db"), "").unwrap();
    project
}

async fn project_runtime(
    tmp: &TempDir,
    project: &std::path::Path,
) -> TranscriptFactsBackfillTestRuntimeV1 {
    TranscriptFactsBackfillTestRuntimeV1::project(
        tmp.path().join("profile"),
        project,
        ProjectId::new(TEST_PROJECT_ID).unwrap(),
    )
    .await
    .unwrap()
}

/// Writes a three-line Cursor transcript with two tag days and returns the
/// path plus each line's starting byte offset.
fn write_tagged_transcript(tmp: &TempDir) -> (std::path::PathBuf, Vec<i64>) {
    let lines = [
        r#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Wednesday, Jun 10, 2026, 9:11 AM (UTC+2)</timestamp>\nFirst day question."}]}}"#,
        r#"{"role":"assistant","message":{"content":[{"type":"text","text":"First day answer."}]}}"#,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Thursday, Jun 11, 2026, 8:00 AM (UTC+2)</timestamp>\nSecond day question."}]}}"#,
    ];
    write_jsonl(tmp, "cursor-session.jsonl", &lines)
}

fn write_jsonl(tmp: &TempDir, name: &str, lines: &[&str]) -> (std::path::PathBuf, Vec<i64>) {
    let mut contents = String::new();
    let mut offsets = Vec::new();
    for line in lines {
        offsets.push(contents.len() as i64);
        contents.push_str(line);
        contents.push('\n');
    }
    let path = tmp.path().join(name);
    std::fs::write(&path, contents).unwrap();
    (path, offsets)
}

fn session(provider: &str) -> SessionRecord {
    SessionRecord {
        provider: provider.to_string(),
        session_id: "legacy-session".to_string(),
        project_key: "legacy".to_string(),
        project_path: "legacy".to_string(),
        title: Some("legacy".to_string()),
        started_at: None,
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

fn legacy_message(
    provider: &str,
    message_id: &str,
    ordinal: i64,
    text: &str,
    source_path: Option<&std::path::Path>,
    source_offset: Option<i64>,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: provider.to_string(),
        message_id: message_id.to_string(),
        session_id: "legacy-session".to_string(),
        role: "user".to_string(),
        timestamp: None,
        ordinal,
        text: text.to_string(),
        kind: Some("message".to_string()),
        model: None,
        tool_names: None,
        source_path: source_path.map(|path| path.to_string_lossy().to_string()),
        source_offset,
        metadata_json: None,
    }
}

/// Seeds legacy rows and clears the backfill marker so background maintenance
/// sees the fixture as pending.
async fn seed_legacy_rows(
    runtime: &TranscriptFactsBackfillTestRuntimeV1,
    provider: &str,
    messages: &[SessionMessageRecord],
) {
    runtime
        .seed_transcript_backfill_for_test(
            HostAdmissionScope::Project,
            &session(provider),
            messages,
        )
        .await
        .unwrap();
}

async fn marker_version(runtime: &TranscriptFactsBackfillTestRuntimeV1) -> Option<i64> {
    runtime
        .transcript_backfill_marker_version_for_test(HostAdmissionScope::Project)
        .await
        .unwrap()
}

async fn drain_backfill(runtime: &TranscriptFactsBackfillTestRuntimeV1) {
    for _ in 0..16 {
        match runtime
            .advance_transcript_facts_backfill_for_test(2)
            .await
            .unwrap()
        {
            TranscriptFactsBackfillOutcome::Pending { .. } => {}
            TranscriptFactsBackfillOutcome::Complete
            | TranscriptFactsBackfillOutcome::NotRequired => return,
        }
    }
    panic!("transcript facts backfill did not drain");
}

fn metadata_usage(metadata_json: Option<&str>) -> Option<serde_json::Value> {
    metadata_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("usage").cloned())
        .filter(|usage| !usage.is_null())
}

#[tokio::test]
async fn admission_publishes_schema_with_transcript_backfill_pending() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let (transcript, offsets) = write_tagged_transcript(&tmp);
    let runtime = project_runtime(&tmp, &project).await;

    seed_legacy_rows(
        &runtime,
        "cursor",
        &[
            legacy_message(
                "cursor",
                "m-0",
                0,
                "First day question.",
                Some(&transcript),
                Some(offsets[0]),
            ),
            legacy_message(
                "cursor",
                "m-1",
                1,
                "First day answer.",
                Some(&transcript),
                Some(offsets[1]),
            ),
            legacy_message(
                "cursor",
                "m-2",
                2,
                "Second day question.",
                Some(&transcript),
                Some(offsets[2]),
            ),
        ],
    )
    .await;
    drop(runtime);

    // Re-opening publishes the schema without running the marker-guarded
    // backfill on the admission path.
    let db = project_runtime(&tmp, &project).await;
    assert_eq!(
        db.transcript_facts_backfill_status_for_test()
            .await
            .unwrap(),
        TranscriptFactsBackfillOutcome::Pending { cursor: 0 }
    );
    assert_eq!(
        db.session_message_for_test(HostAdmissionScope::Project, "cursor", "m-0")
            .await
            .unwrap()
            .unwrap()
            .timestamp,
        None
    );
    assert_eq!(marker_version(&db).await, None);

    drain_backfill(&db).await;
    for (message_id, expected) in [("m-0", DAY_ONE), ("m-1", DAY_ONE), ("m-2", DAY_TWO)] {
        let projected = db
            .session_message_for_test(HostAdmissionScope::Project, "cursor", message_id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("missing {message_id}"));
        assert_eq!(projected.timestamp, Some(expected), "{message_id}");
        let raw = db
            .lcm_load_raw_message_for_test("cursor", message_id)
            .await
            .unwrap_or_else(|| panic!("missing raw {message_id}"));
        assert_eq!(raw.timestamp, Some(expected), "raw {message_id}");
        // Cursor transcripts genuinely carry no token counters; the backfill
        // must not fabricate a usage object for them.
        assert!(
            metadata_usage(projected.metadata_json.as_deref()).is_none(),
            "cursor rows must stay usage-free"
        );
    }

    // Session window is derived from the freshly dated messages.
    let session = db
        .session_for_test(HostAdmissionScope::Project, "cursor", "legacy-session")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.started_at, Some(DAY_ONE));
    assert_eq!(session.ended_at, Some(DAY_TWO));
    assert_eq!(marker_version(&db).await, Some(1));
}

#[tokio::test]
async fn backfill_batches_resume_from_durable_cursor_after_restarts() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let (transcript, offsets) = write_tagged_transcript(&tmp);
    let runtime = project_runtime(&tmp, &project).await;
    seed_legacy_rows(
        &runtime,
        "cursor",
        &[
            legacy_message(
                "cursor",
                "m-0",
                0,
                "First day question.",
                Some(&transcript),
                Some(offsets[0]),
            ),
            legacy_message(
                "cursor",
                "m-1",
                1,
                "First day answer.",
                Some(&transcript),
                Some(offsets[1]),
            ),
            legacy_message(
                "cursor",
                "m-2",
                2,
                "Second day question.",
                Some(&transcript),
                Some(offsets[2]),
            ),
        ],
    )
    .await;
    drop(runtime);

    let first = project_runtime(&tmp, &project).await;
    let TranscriptFactsBackfillOutcome::Pending {
        cursor: first_cursor,
    } = first
        .advance_transcript_facts_backfill_for_test(1)
        .await
        .unwrap()
    else {
        panic!("first bounded batch must remain pending");
    };
    assert!(first_cursor > 0);
    drop(first);

    let second = project_runtime(&tmp, &project).await;
    assert_eq!(
        second
            .transcript_facts_backfill_status_for_test()
            .await
            .unwrap(),
        TranscriptFactsBackfillOutcome::Pending {
            cursor: first_cursor
        }
    );
    let TranscriptFactsBackfillOutcome::Pending {
        cursor: second_cursor,
    } = second
        .advance_transcript_facts_backfill_for_test(1)
        .await
        .unwrap()
    else {
        panic!("second bounded batch must remain pending");
    };
    assert!(second_cursor > first_cursor);
    drop(second);

    let third = project_runtime(&tmp, &project).await;
    drain_backfill(&third).await;
    assert_eq!(marker_version(&third).await, Some(1));
    for (message_id, expected) in [("m-0", DAY_ONE), ("m-1", DAY_ONE), ("m-2", DAY_TWO)] {
        assert_eq!(
            third
                .session_message_for_test(HostAdmissionScope::Project, "cursor", message_id)
                .await
                .unwrap()
                .unwrap()
                .timestamp,
            Some(expected)
        );
    }
}

#[tokio::test]
async fn backfill_adds_claude_usage_counters_from_message_usage() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let runtime = project_runtime(&tmp, &project).await;

    // Claude transcript shape: per-line ISO timestamp, Anthropic-style
    // counters on the assistant `message.usage`.
    let (transcript, offsets) = write_jsonl(
        &tmp,
        "claude-session.jsonl",
        &[
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"Question."}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:05Z","message":{"role":"assistant","content":"Answer.","usage":{"input_tokens":1000,"output_tokens":200,"cache_creation_input_tokens":500,"cache_read_input_tokens":800}}}"#,
        ],
    );

    seed_legacy_rows(
        &runtime,
        "claude",
        &[
            legacy_message(
                "claude",
                "c-0",
                0,
                "Question.",
                Some(&transcript),
                Some(offsets[0]),
            ),
            legacy_message(
                "claude",
                "c-1",
                1,
                "Answer.",
                Some(&transcript),
                Some(offsets[1]),
            ),
        ],
    )
    .await;
    drop(runtime);

    let db = project_runtime(&tmp, &project).await;
    drain_backfill(&db).await;
    let assistant = db
        .session_message_for_test(HostAdmissionScope::Project, "claude", "c-1")
        .await
        .unwrap()
        .unwrap();
    // One re-read populated both facts: the timestamp...
    assert_eq!(assistant.timestamp, Some(1_767_225_605));
    // ...and the usage counters under the keys the savings dashboard reads.
    let usage = metadata_usage(assistant.metadata_json.as_deref())
        .expect("assistant row should gain usage");
    assert_eq!(usage["input_tokens"], 1000);
    assert_eq!(usage["output_tokens"], 200);
    assert_eq!(usage["cache_creation_input_tokens"], 500);
    assert_eq!(usage["cache_read_input_tokens"], 800);
    let raw = db
        .lcm_load_raw_message_for_test("claude", "c-1")
        .await
        .unwrap();
    assert!(metadata_usage(raw.metadata_json.as_deref()).is_some());

    // The user line has no counters; no usage object is fabricated.
    let user = db
        .session_message_for_test(HostAdmissionScope::Project, "claude", "c-0")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.timestamp, Some(1_767_225_600));
    assert!(metadata_usage(user.metadata_json.as_deref()).is_none());
}

#[tokio::test]
async fn backfill_attaches_codex_turn_usage_to_assistant_line() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let runtime = project_runtime(&tmp, &project).await;

    // Codex rollout shape: usage arrives on a separate `token_count` event
    // after the `agent_message`, with OpenAI semantics (input includes
    // cached) that must be split for the savings dashboard's additive math.
    let (transcript, offsets) = write_jsonl(
        &tmp,
        "rollout-codex.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"Question."}}"#,
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"agent_message","message":"Answer."}}"#,
            r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":14662,"cached_input_tokens":6528,"output_tokens":13,"total_tokens":14675},"last_token_usage":{"input_tokens":14662,"cached_input_tokens":6528,"output_tokens":13,"total_tokens":14675}}}}"#,
        ],
    );

    let mut assistant = legacy_message(
        "codex",
        "x-1",
        1,
        "Answer.",
        Some(&transcript),
        Some(offsets[1]),
    );
    assistant.role = "assistant".to_string();
    seed_legacy_rows(&runtime, "codex", &[assistant]).await;
    drop(runtime);

    let db = project_runtime(&tmp, &project).await;
    drain_backfill(&db).await;
    let row = db
        .session_message_for_test(HostAdmissionScope::Project, "codex", "x-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.timestamp, Some(1_767_225_602));
    let usage =
        metadata_usage(row.metadata_json.as_deref()).expect("codex assistant should gain usage");
    // input excludes the cached portion, which lands in cache_read.
    assert_eq!(usage["input_tokens"], 14662 - 6528);
    assert_eq!(usage["cache_read_input_tokens"], 6528);
    assert_eq!(usage["output_tokens"], 13);
    assert_eq!(usage["total_tokens"], 14675);
}

#[tokio::test]
async fn backfill_tolerates_missing_files_and_marks_done() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let gone = tmp.path().join("deleted-transcript.jsonl");
    let runtime = project_runtime(&tmp, &project).await;

    seed_legacy_rows(
        &runtime,
        "cursor",
        &[
            legacy_message(
                "cursor",
                "m-gone",
                0,
                "Source file was deleted.",
                Some(&gone),
                Some(0),
            ),
            legacy_message(
                "cursor",
                "m-nopath",
                1,
                "Never had a source path.",
                None,
                None,
            ),
        ],
    )
    .await;
    drop(runtime);

    let db = project_runtime(&tmp, &project).await;
    drain_backfill(&db).await;
    for message_id in ["m-gone", "m-nopath"] {
        let projected = db
            .session_message_for_test(HostAdmissionScope::Project, "cursor", message_id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("missing {message_id}"));
        assert_eq!(
            projected.timestamp, None,
            "{message_id} must stay undated when its transcript is unavailable"
        );
    }
    // The pass still completes and never re-runs.
    assert_eq!(marker_version(&db).await, Some(1));
}

#[tokio::test]
async fn backfill_leaves_already_dated_rows_untouched() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let (transcript, offsets) = write_tagged_transcript(&tmp);
    let runtime = project_runtime(&tmp, &project).await;

    // A Hermes-migrated message: already dated, pointing at line 0 whose tag
    // would re-derive a *different* value (DAY_ONE).
    let migrated_at = 1_700_000_000;
    let mut migrated = legacy_message(
        "cursor",
        "m-migrated",
        0,
        "First day question.",
        Some(&transcript),
        Some(offsets[0]),
    );
    migrated.timestamp = Some(migrated_at);
    seed_legacy_rows(&runtime, "cursor", &[migrated]).await;
    drop(runtime);

    let db = project_runtime(&tmp, &project).await;
    drain_backfill(&db).await;
    let projected = db
        .session_message_for_test(HostAdmissionScope::Project, "cursor", "m-migrated")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(projected.timestamp, Some(migrated_at));
    let raw = db
        .lcm_load_raw_message_for_test("cursor", "m-migrated")
        .await
        .unwrap();
    assert_eq!(raw.timestamp, Some(migrated_at));
    assert_eq!(marker_version(&db).await, Some(1));
}

#[tokio::test]
async fn backfill_preserves_existing_usage_and_metadata() {
    let tmp = TempDir::new().unwrap();
    let project = init_project(&tmp);
    let runtime = project_runtime(&tmp, &project).await;

    let (transcript, offsets) = write_jsonl(
        &tmp,
        "claude-session.jsonl",
        &[
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:05Z","message":{"role":"assistant","content":"Answer.","usage":{"input_tokens":1000,"output_tokens":200}}}"#,
        ],
    );

    // Row already carries usage (e.g. migrated): the transcript's differing
    // counters must not overwrite it, and sibling metadata keys must survive.
    let mut seeded = legacy_message(
        "claude",
        "c-keep",
        0,
        "Answer.",
        Some(&transcript),
        Some(offsets[0]),
    );
    seeded.metadata_json =
        Some(r#"{"source":"migration","usage":{"input_tokens":42,"output_tokens":7}}"#.to_string());
    seed_legacy_rows(&runtime, "claude", &[seeded]).await;
    drop(runtime);

    let db = project_runtime(&tmp, &project).await;
    drain_backfill(&db).await;
    let row = db
        .session_message_for_test(HostAdmissionScope::Project, "claude", "c-keep")
        .await
        .unwrap()
        .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(row.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "migration");
    assert_eq!(metadata["usage"]["input_tokens"], 42);
    assert_eq!(metadata["usage"]["output_tokens"], 7);
}
