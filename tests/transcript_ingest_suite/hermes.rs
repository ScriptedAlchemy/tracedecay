//! Hermes `state.db` transcript ingestion: profile-pin scoping, projection-only
//! writes, and incremental rowid-cursor sweeps. Fixtures mirror the real
//! Hermes schema (`sessions` + `messages` tables) and real row shapes
//! (assistant tool-call turns with empty `content`, JSON `tool_calls`,
//! REAL epoch-second timestamps, session-level token counters).

use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay::global_db::ParseOffset;
use tracedecay::sessions::hermes::{
    ProjectIngestDestination, ingest_for_project as ingest_for_project_with_id,
    ingest_homes as ingest_homes_with_id, ingest_homes_for_projects, ingest_user_homes,
};
use tracedecay::sessions::lcm::{LcmCompressionRequest, LcmSummarizerMode};
use tracedecay::sessions::source::TranscriptIngestStats;
use tracedecay::sessions::{SessionProvider, SessionRecord};
use tracedecay_domain::{
    MAX_OBSERVATION_RECORD_BYTES, ProjectId, ProviderUsageCounterSemanticsV1,
    ProviderUsageCountersV1, ProviderUsageModelV1, ProviderUsageScopeV1,
};

use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
use crate::restart_atomicity::{
    ProjectSessionTestRuntime, assert_secret_absent_from_observation_sinks, durable_table_count,
    mark_test_project, observation_source_cursor, open_project_session_db,
    open_sibling_project_session_db, set_projection_failure,
};
use crate::support::{
    assert_metadata_path_eq, create_git_repo_with_linked_worktree, init_git_repo,
};

const SESSION_ID: &str = "20260101_000000_abc123";

async fn ingest_for_project(
    runtime: &ProjectSessionTestRuntime,
    project_root: &Path,
) -> TranscriptIngestStats {
    let admission = runtime.runtime().facade();
    ingest_for_project_with_id(&admission, project_root, runtime.project_id().clone()).await
}

async fn ingest_homes(
    runtime: &ProjectSessionTestRuntime,
    hermes_homes: &[PathBuf],
    project_root: &Path,
) -> TranscriptIngestStats {
    let admission = runtime.runtime().facade();
    ingest_homes_with_id(
        &admission,
        hermes_homes,
        project_root,
        runtime.project_id().clone(),
    )
    .await
}

async fn ingest_registered_project_provider(
    runtime: &ProjectSessionTestRuntime,
    project_root: &Path,
) -> TranscriptIngestStats {
    runtime
        .runtime()
        .ingest_project_provider_for_test(project_root, Some(SessionProvider::Hermes))
        .await
        .unwrap()
}

fn named_project_id(name: &str) -> ProjectId {
    ProjectId::new(format!("tracedecay-hermes-{name}-fixture")).unwrap()
}

#[tokio::test]
async fn hermes_row_cursor_cannot_regress_during_overlapping_sweeps() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    crate::support::init_project_at(&project);
    let db = open_project_session_db(&project).await.unwrap();
    let cursor = "state.db#turn-project-v2";
    db.runtime()
        .set_project_parse_offset_for_test(
            cursor,
            ParseOffset {
                byte_offset: 200,
                mtime: 20,
                file_id: 0,
            },
        )
        .await
        .unwrap();
    db.runtime()
        .set_project_parse_offset_for_test(
            cursor,
            ParseOffset {
                byte_offset: 100,
                mtime: 10,
                file_id: 0,
            },
        )
        .await
        .unwrap();

    assert_eq!(db.get_parse_offset(cursor).await.unwrap().byte_offset, 200);
}

#[tokio::test]
// Intentional: this test changes HOME/USERPROFILE/HERMES_HOME while storage
// discovery is running, so it must share the profile-environment lock used by
// Cursor's transcript tests.
#[allow(clippy::await_holding_lock)]
async fn hermes_home_env_cannot_redirect_runtime_session_discovery() {
    let _lock = crate::common::GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let tmp = TempDir::new().unwrap();
    let (standard_hermes_home, project) = setup(&tmp);
    let user_home = standard_hermes_home.parent().unwrap();
    let redirected = tmp.path().join("redirected-hermes");
    write_hermes_profile(&redirected, "work", Some(&project)).await;
    let _home = crate::common::EnvVarGuard::set("HOME", user_home);
    let _userprofile = crate::common::EnvVarGuard::set("USERPROFILE", user_home);
    let _hermes_home = crate::common::EnvVarGuard::set("HERMES_HOME", &redirected);
    let db = open_project_session_db(&project).await.unwrap();

    let stats = ingest_for_project(&db, &project).await;

    assert_eq!(stats.messages_upserted, 0);
    assert_eq!(stats.sessions_upserted, 0);
    assert!(db.get_session("hermes", SESSION_ID).await.is_none());
}

/// Like [`crate::support::setup`], but returns the Hermes home
/// (`<home>/.hermes`) instead of the plain test home.
fn setup(tmp: &TempDir) -> (PathBuf, PathBuf) {
    let (home, project) = crate::support::setup(tmp);
    (home.join(".hermes"), project)
}

/// Writes a Hermes profile dir: a `config.yaml` optionally pinning
/// `pinned_project` (the real `plugins.tracedecay.project_root` shape) and a
/// `state.db` with the real Hermes schema. Unpinned profiles (the default
/// since the installer stopped writing storage-home pins) carry only the
/// plugin-enable block.
async fn write_hermes_profile(
    hermes_home: &Path,
    profile: &str,
    pinned_project: Option<&Path>,
) -> PathBuf {
    let profile_dir = hermes_home.join("profiles").join(profile);
    std::fs::create_dir_all(&profile_dir).unwrap();
    let config = match pinned_project {
        Some(pinned_project) => {
            // The pin is JSON-encoded exactly as `tracedecay install --agent
            // hermes` writes it, so Windows backslashes survive the
            // double-quoted YAML scalar.
            let pin = serde_json::to_string(pinned_project.to_string_lossy().as_ref()).unwrap();
            format!(
                "memory:\n  provider: tracedecay\nplugins:\n  enabled:\n    - tracedecay\n  tracedecay:\n    project_root: {pin}\n",
            )
        }
        None => {
            "memory:\n  provider: tracedecay\nplugins:\n  enabled:\n    - tracedecay\n".to_string()
        }
    };
    std::fs::write(profile_dir.join("config.yaml"), config).unwrap();

    let state_db = profile_dir.join("state.db");
    let conn = open_state_db(&state_db);
    conn.execute(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            user_id TEXT,
            model TEXT,
            model_config TEXT,
            system_prompt TEXT,
            parent_session_id TEXT,
            started_at REAL NOT NULL,
            ended_at REAL,
            end_reason TEXT,
            message_count INTEGER DEFAULT 0,
            tool_call_count INTEGER DEFAULT 0,
            input_tokens INTEGER DEFAULT 0,
            output_tokens INTEGER DEFAULT 0,
            cache_read_tokens INTEGER DEFAULT 0,
            cache_write_tokens INTEGER DEFAULT 0,
            reasoning_tokens INTEGER DEFAULT 0,
            cwd TEXT,
            title TEXT,
            archived INTEGER NOT NULL DEFAULT 0
        )",
        (),
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            role TEXT NOT NULL,
            content TEXT,
            tool_call_id TEXT,
            tool_calls TEXT,
            tool_name TEXT,
            timestamp REAL NOT NULL,
            token_count INTEGER,
            finish_reason TEXT,
            reasoning TEXT,
            observed INTEGER DEFAULT 0,
            active INTEGER NOT NULL DEFAULT 1
        )",
        (),
    )
    .unwrap();

    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, ended_at, title,
                               input_tokens, output_tokens, cache_read_tokens,
                               cache_write_tokens, reasoning_tokens)
         VALUES (?1, 'tui', 'gpt-5.5', 1780629300.0, 1780629340.0,
                 'Billing pipeline fix', 96443, 3804, 1064960, 0, 2061)",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();

    // Real Hermes row shapes: a session_meta bootstrap row (must be skipped),
    // a user prompt, an assistant tool-call turn with empty content, a tool
    // result keyed by tool_name, and a final assistant reply.
    let tool_fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/provider_normalization/hermes/assistant_tool_call.input.json"
    ))
    .expect("checked-in Hermes tool-call row");
    let tool_calls = tool_fixture["tool_calls"].to_string();
    for (role, content, tool_calls, tool_name, ts, finish) in [
        (
            "session_meta",
            Some("{\"system_prompt_hash\":\"abc\"}"),
            None,
            None,
            1_780_629_290.1_f64,
            None,
        ),
        (
            "user",
            Some("Help resolve the failing billing pipeline test"),
            None,
            None,
            1_780_629_300.2,
            None,
        ),
        (
            "assistant",
            tool_fixture["content"].as_str(),
            Some(tool_calls.as_str()),
            None,
            tool_fixture["timestamp"].as_f64().unwrap(),
            Some("tool_calls"),
        ),
        (
            "tool",
            Some("{\"output\": \"$ cargo test billing\\nok\", \"exit_code\": 0}"),
            None,
            Some("terminal"),
            1_780_629_320.7,
            None,
        ),
        (
            "assistant",
            Some("The billing pipeline test is fixed."),
            None,
            None,
            1_780_629_330.9,
            Some("stop"),
        ),
    ] {
        conn.execute(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_name,
                                   timestamp, finish_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![SESSION_ID, role, content, tool_calls, tool_name, ts, finish],
        )
        .unwrap();
    }
    state_db
}

fn open_state_db(path: &Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "DELETE").unwrap();
    conn
}

#[tokio::test]
async fn hermes_state_db_populates_projection_for_pinned_project() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    write_hermes_profile(&hermes_home, "test", Some(&linked_worktree)).await;

    let db = open_project_session_db(&project).await.unwrap();
    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &project).await;
    // user + assistant tool-call turn + tool result + assistant reply; the
    // session_meta bootstrap row is skipped.
    assert_eq!(stats.messages_upserted, 4);
    assert_eq!(stats.sessions_upserted, 1);

    let session = db
        .get_session("hermes", SESSION_ID)
        .await
        .expect("hermes session should be stored");
    let results = db
        .search_session_messages(
            "hermes",
            Some(project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
        .await;
    assert!(
        results.iter().any(|hit| hit.message.role == "user"),
        "expected pinned-project user hit; projected session path: {:?}",
        session.project_path
    );
    assert!(results.iter().any(|hit| hit.message.role == "assistant"));
    assert!(results.iter().any(|hit| {
        hit.message.role == "user" && hit.message.model.as_deref() == Some("gpt-5.5")
    }));
    assert!(results.iter().any(|hit| {
        hit.message.kind.as_deref() == Some("message")
            && hit.message.model.as_deref() == Some("gpt-5.5")
            && hit.message.text.contains("billing pipeline test is fixed")
    }));
    // REAL epoch-second timestamps land truncated to whole seconds.
    assert!(
        results
            .iter()
            .any(|hit| hit.message.timestamp == Some(1_780_629_300))
    );

    assert_eq!(session.title.as_deref(), Some("Billing pipeline fix"));
    assert_eq!(session.started_at, Some(1_780_629_300));
    assert_eq!(session.ended_at, Some(1_780_629_340));
    let metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "hermes_state_db");
    assert_eq!(metadata["profile"], "test");
    assert_eq!(metadata["hermes_source"], "tui");
    assert_metadata_path_eq(&metadata["hermes_session_cwd"], &linked_worktree);
    assert_metadata_path_eq(&metadata["hermes_session_worktree"], &linked_worktree);
    assert_eq!(
        metadata["hermes_session_location_provenance"].as_str(),
        Some("profile_pin")
    );
    // Session-cumulative token counters from the Hermes sessions table land
    // in the immutable provider-usage observation family (captured at the
    // session-usage frontier row), not in session metadata. A native zero
    // (cache_write) is measured evidence and survives as exactly zero.
    assert!(metadata.get("usage").is_none());
    let observations = db.provider_usage_observations("hermes").await;
    assert_eq!(observations.len(), 1);
    let usage = &observations[0];
    assert_eq!(usage.session_id.as_str(), SESSION_ID);
    assert_eq!(usage.native_kind, "session");
    assert_eq!(usage.native_field, "sessions.token_counters");
    assert_eq!(usage.native_scope, ProviderUsageScopeV1::Session);
    assert_eq!(
        usage.counter_semantics,
        ProviderUsageCounterSemanticsV1::Cumulative
    );
    assert_eq!(
        usage.model,
        ProviderUsageModelV1::Known {
            model: "gpt-5.5".to_owned(),
        }
    );
    assert_eq!(
        usage.counters,
        ProviderUsageCountersV1::Known {
            input_tokens: Some(96_443),
            output_tokens: Some(3_804),
            cache_read_tokens: Some(1_064_960),
            cache_write_tokens: Some(0),
            reasoning_tokens: Some(2_061),
            total_tokens: None,
        }
    );

    // Empty-content assistant tool-call turns project typed ToolInvocation facts
    // (not a synthesized Message from tool_calls JSON).
    let tool_turn = db
        .get_session_message("hermes", &format!("{SESSION_ID}:3"))
        .await
        .expect("assistant tool-call turn should be stored");
    assert_eq!(tool_turn.role, "assistant");
    assert_eq!(tool_turn.kind.as_deref(), Some("tool_invocation"));
    assert!(tool_turn.text.contains("cargo test billing"));
    assert!(!tool_turn.text.contains("call_FBvwGfCC9lJrXPvOqpDHcjYn"));
    assert_eq!(tool_turn.tool_names.as_deref(), Some("terminal"));
    assert!(tool_turn.model.is_none());
    let tool_metadata: serde_json::Value =
        serde_json::from_str(tool_turn.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&tool_metadata["hermes_session_cwd"], &linked_worktree);
    assert_metadata_path_eq(&tool_metadata["hermes_session_worktree"], &linked_worktree);
    assert_eq!(
        tool_metadata["hermes_session_location_provenance"].as_str(),
        Some("profile_pin")
    );

    // Projection-only: Hermes raw messages are owned by the runtime LCM
    // ingest, so the transcript sweep must never write lcm_raw_messages.
    for ordinal in 2..=5 {
        assert!(
            db.lcm_load_raw_message("hermes", &format!("{SESSION_ID}:{ordinal}"))
                .await
                .is_none()
        );
    }
}

#[tokio::test]
async fn hermes_secret_is_sanitized_before_observation_and_projection() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;
    let secret = "sk-proj-hermes-canary-1234567890";
    open_state_db(&state_db)
        .execute(
            "UPDATE messages SET content = ?1 WHERE role = 'user'",
            rusqlite::params![format!("Hermes sanitizer safe text: {secret}")],
        )
        .unwrap();
    let db = open_project_session_db(&project).await.unwrap();

    assert_eq!(
        ingest_homes(&db, std::slice::from_ref(&hermes_home), &project)
            .await
            .messages_upserted,
        4
    );
    assert_eq!(
        db.search_session_messages("hermes", None, "Hermes sanitizer safe text", 10)
            .await
            .len(),
        1
    );
    assert_secret_absent_from_observation_sinks(&db, "hermes", secret).await;
}

#[tokio::test]
async fn hermes_parent_session_id_marks_subagent_session() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;
    let conn = open_state_db(&state_db);
    conn.execute(
        "UPDATE sessions SET parent_session_id = 'parent-hermes-session' WHERE id = ?1",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    drop(conn);

    let db = open_project_session_db(&project).await.unwrap();
    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &project).await;
    assert_eq!(stats.messages_upserted, 4);
    let session = db
        .get_session("hermes", SESSION_ID)
        .await
        .expect("hermes child session should be stored");
    assert_eq!(
        session.parent_session_id.as_deref(),
        Some("parent-hermes-session")
    );
    assert!(session.is_subagent);
}

#[tokio::test]
async fn hermes_projection_sweep_does_not_mutate_runtime_owned_raw_messages() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    write_hermes_profile(&hermes_home, "test", Some(&project)).await;

    let db = open_project_session_db(&project).await.unwrap();
    assert!(
        db.runtime()
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &SessionRecord {
                    provider: "hermes".to_string(),
                    session_id: SESSION_ID.to_string(),
                    project_key: project.to_string_lossy().to_string(),
                    project_path: project.to_string_lossy().to_string(),
                    title: Some("Runtime-owned raw session".to_string()),
                    started_at: Some(1_780_629_300),
                    ended_at: None,
                    transcript_path: None,
                    metadata_json: Some(r#"{"source":"runtime_preflight"}"#.to_string()),
                    parent_session_id: None,
                    is_subagent: false,
                    agent_id: None,
                    parent_tool_use_id: None,
                },
            )
            .await
            .unwrap()
    );

    let raw_message_id = format!("{SESSION_ID}:3");
    let runtime_owned_raw = "runtime-owned raw message from active replay";
    // Active-message ingest is owned by the compress path; preflight is
    // read-only under the daemon-owned compaction authority.
    db.runtime()
        .lcm_compress_for_test(LcmCompressionRequest {
            provider: "hermes".into(),
            session_id: SESSION_ID.into(),
            messages: vec![json!({
                "id": raw_message_id,
                "role": "assistant",
                "content": runtime_owned_raw,
            })],
            current_tokens: Some(12),
            focus_topic: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            summarizer: LcmSummarizerMode::Noop,
        })
        .await
        .unwrap();

    let raw_before = db
        .lcm_load_raw_message("hermes", &raw_message_id)
        .await
        .expect("runtime-owned raw message should exist before the sweep");
    assert_eq!(raw_before.content, runtime_owned_raw);
    assert!(
        db.get_session_message("hermes", &raw_message_id)
            .await
            .is_none(),
        "LCM active-message ingest should not create a session-message projection"
    );

    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &project).await;
    assert_eq!(stats.messages_upserted, 4);
    // If the Hermes sweep ever switches back to full transcript writes, this
    // message id would overwrite the runtime-owned raw turn with the assistant
    // tool-call JSON from `state.db` row 3.
    let raw_after = db
        .lcm_load_raw_message("hermes", &raw_message_id)
        .await
        .expect("projection-only ingest must leave runtime-owned raw turns intact");
    assert_eq!(raw_after.store_id, raw_before.store_id);
    assert_eq!(raw_after.content, runtime_owned_raw);
    assert_eq!(raw_after.content_hash, raw_before.content_hash);

    let projection = db
        .get_session_message("hermes", &raw_message_id)
        .await
        .expect("projection row should still be searchable");
    assert_eq!(projection.role, "assistant");
    assert_eq!(projection.kind.as_deref(), Some("tool_invocation"));
    assert!(projection.text.contains("cargo test billing"));
    assert_eq!(projection.tool_names.as_deref(), Some("terminal"));
}

#[tokio::test]
async fn hermes_ingest_is_incremental_and_idempotent() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;

    let db = open_project_session_db(&project).await.unwrap();
    let homes = [hermes_home.clone()];
    assert_eq!(
        ingest_homes(&db, &homes, &project).await.messages_upserted,
        4
    );
    // Re-sweep with no new rows is a no-op (rowid cursor already advanced).
    assert_eq!(
        ingest_homes(&db, &homes, &project).await.messages_upserted,
        0
    );

    let conn = open_state_db(&state_db);
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES (?1, 'user', 'Also add a regression test', 1780629400.4)",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    drop(conn);

    let stats = ingest_homes(&db, &homes, &project).await;
    assert_eq!(stats.messages_upserted, 1);
    let appended = db
        .get_session_message("hermes", &format!("{SESSION_ID}:6"))
        .await
        .expect("appended message should be ingested");
    assert_eq!(appended.timestamp, Some(1_780_629_400));
    // The session's original start time survives the incremental sweep and
    // ended_at is not regressed by the partial batch.
    let session = db.get_session("hermes", SESSION_ID).await.unwrap();
    assert_eq!(session.started_at, Some(1_780_629_300));
    assert_eq!(session.ended_at, Some(1_780_629_340));
}

async fn hermes_projection_signature(
    runtime: &HostAdmissionTestRuntimeV1,
) -> Vec<(
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
)> {
    let mut rows = std::collections::BTreeMap::new();
    for query in ["billing", "regression"] {
        for hit in runtime
            .search_project_session_messages_for_test("hermes", None, query, 20)
            .await
            .unwrap()
        {
            let message = hit.message;
            rows.insert(
                message.message_id.clone(),
                (
                    message.message_id,
                    message.role,
                    message.text,
                    message.model,
                    message.timestamp,
                    message.tool_names,
                ),
            );
        }
    }
    rows.into_values().collect()
}

#[tokio::test]
async fn hermes_incremental_ingest_converges_with_full_rebuild() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;
    let homes = [hermes_home];
    let project_id = mark_test_project(&project);

    let incremental = HostAdmissionTestRuntimeV1::project(
        tmp.path().join("incremental-profile"),
        &project,
        project_id.clone(),
    )
    .await
    .unwrap();
    let incremental_admission = incremental.facade();
    assert_eq!(
        ingest_homes_with_id(&incremental_admission, &homes, &project, project_id.clone(),)
            .await
            .messages_upserted,
        4
    );

    let conn = open_state_db(&state_db);
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES (?1, 'user', 'Also add a regression test', 1780629400.4)",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    drop(conn);
    assert_eq!(
        ingest_homes_with_id(&incremental_admission, &homes, &project, project_id.clone(),)
            .await
            .messages_upserted,
        1
    );

    let rebuilt = HostAdmissionTestRuntimeV1::project(
        tmp.path().join("rebuilt-profile"),
        &project,
        project_id.clone(),
    )
    .await
    .unwrap();
    let rebuilt_admission = rebuilt.facade();
    assert_eq!(
        ingest_homes_with_id(&rebuilt_admission, &homes, &project, project_id)
            .await
            .messages_upserted,
        5
    );
    assert_eq!(
        hermes_projection_signature(&incremental).await,
        hermes_projection_signature(&rebuilt).await
    );
}

#[tokio::test]
async fn hermes_replacement_replay_preserves_message_identity() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;
    let homes = [hermes_home.clone()];
    let db = open_project_session_db(&project).await.unwrap();

    assert_eq!(
        ingest_homes(&db, &homes, &project).await.messages_upserted,
        4
    );
    let before = hermes_projection_signature(db.runtime()).await;

    let replacement_root = tmp.path().join("replacement");
    let replacement = write_hermes_profile(&replacement_root, "test", Some(&project)).await;
    let conn = open_state_db(&replacement);
    conn.execute(
        "UPDATE sessions
         SET model = 'gpt-5.6', input_tokens = 100000, output_tokens = 4000
         WHERE id = ?1",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    drop(conn);
    let original_profile = state_db.parent().unwrap();
    let replacement_profile = replacement.parent().unwrap();
    std::fs::remove_dir_all(original_profile).unwrap();
    std::fs::rename(replacement_profile, original_profile).unwrap();

    let replay = ingest_homes(&db, &homes, &project).await;
    assert_eq!(replay.messages_upserted, 0);
    let after = hermes_projection_signature(db.runtime()).await;
    assert_eq!(
        before.iter().map(|row| &row.0).collect::<Vec<_>>(),
        after.iter().map(|row| &row.0).collect::<Vec<_>>()
    );
    assert_eq!(before.len(), after.len());
}

#[tokio::test]
async fn hermes_shared_sweep_routes_one_source_to_multiple_project_stores() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, first_project) = setup(&tmp);
    let second_project = tmp.path().join("second-project");
    crate::support::init_project_at(&second_project);
    let state_db = write_hermes_profile(&hermes_home, "test", None).await;
    let conn = open_state_db(&state_db);
    conn.execute(
        "UPDATE sessions SET cwd = ?1 WHERE id = ?2",
        rusqlite::params![first_project.to_string_lossy().as_ref(), SESSION_ID],
    )
    .unwrap();
    let second_session = "20260101_000100_def456";
    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, cwd, title)
         VALUES (?1, 'telegram', 'gpt-5.5', 1780629500.0, ?2, 'Second project')",
        rusqlite::params![second_session, second_project.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES (?1, 'user', 'Route this message to the second project', 1780629501.0)",
        rusqlite::params![second_session],
    )
    .unwrap();
    drop(conn);

    let first_db = open_project_session_db(&first_project).await.unwrap();
    let second_db = open_sibling_project_session_db(&first_db, &second_project).await;
    let first_admission = first_db.runtime().facade();
    let second_admission = second_db.runtime().facade();
    let destinations = [
        ProjectIngestDestination {
            admission: &first_admission,
            project_root: &first_project,
            project_id: first_db.project_id().clone(),
        },
        ProjectIngestDestination {
            admission: &second_admission,
            project_root: &second_project,
            project_id: second_db.project_id().clone(),
        },
    ];
    let stats = ingest_homes_for_projects(std::slice::from_ref(&hermes_home), &destinations).await;

    assert_eq!(stats.messages_upserted, 5);
    assert!(first_db.get_session("hermes", SESSION_ID).await.is_some());
    assert!(
        first_db
            .get_session("hermes", second_session)
            .await
            .is_none()
    );
    assert!(second_db.get_session("hermes", SESSION_ID).await.is_none());
    assert!(
        second_db
            .get_session("hermes", second_session)
            .await
            .is_some()
    );
    assert_eq!(
        ingest_homes_for_projects(std::slice::from_ref(&hermes_home), &destinations)
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
#[ignore = "manual cold-history benchmark; requires TRACEDECAY_HERMES_BENCH_HOME and TRACEDECAY_HERMES_BENCH_PROJECT"]
async fn hermes_shared_sweep_cold_history_completes_under_sixty_seconds() {
    let hermes_home = PathBuf::from(
        std::env::var("TRACEDECAY_HERMES_BENCH_HOME").expect("Hermes home for benchmark"),
    );
    let project_root = PathBuf::from(
        std::env::var("TRACEDECAY_HERMES_BENCH_PROJECT").expect("project root for benchmark"),
    );
    let output_root = PathBuf::from(
        std::env::var("TRACEDECAY_HERMES_BENCH_OUTPUT").expect("fast output root for benchmark"),
    );
    std::fs::create_dir_all(&output_root).unwrap();
    let temp = tempfile::Builder::new()
        .prefix("hermes-cold-catchup-")
        .tempdir_in(output_root)
        .unwrap();
    let mut project_roots = vec![project_root];
    for index in 1..31 {
        let root = temp.path().join(format!("project-{index}"));
        crate::support::init_project_at(&root);
        project_roots.push(root);
    }
    let mut runtimes = Vec::with_capacity(project_roots.len());
    let mut project_ids = Vec::with_capacity(project_roots.len());
    for (index, project_root) in project_roots.iter().enumerate() {
        let project_id = named_project_id(&format!("benchmark-{index}"));
        runtimes.push(
            HostAdmissionTestRuntimeV1::project(
                temp.path().join(format!("profile-{index}")),
                project_root,
                project_id.clone(),
            )
            .await
            .unwrap(),
        );
        project_ids.push(project_id);
    }
    let admissions = runtimes
        .iter()
        .map(HostAdmissionTestRuntimeV1::facade)
        .collect::<Vec<_>>();
    let destinations = admissions
        .iter()
        .zip(&project_roots)
        .zip(&project_ids)
        .map(
            |((admission, project_root), project_id)| ProjectIngestDestination {
                admission,
                project_root,
                project_id: project_id.clone(),
            },
        )
        .collect::<Vec<_>>();

    let started = std::time::Instant::now();
    let stats = ingest_homes_for_projects(std::slice::from_ref(&hermes_home), &destinations).await;
    let elapsed = started.elapsed();

    assert!(
        stats.messages_upserted > 0,
        "benchmark must ingest real history"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "cold Hermes catch-up took {elapsed:?}"
    );
}

#[tokio::test]
async fn hermes_profile_pinned_elsewhere_is_not_ingested() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let other_project = tmp.path().join("other-project");
    std::fs::create_dir_all(&other_project).unwrap();
    write_hermes_profile(&hermes_home, "test", Some(&other_project)).await;

    let db = open_project_session_db(&project).await.unwrap();
    let stats = ingest_homes(&db, &[hermes_home], &project).await;
    assert_eq!(stats.messages_upserted, 0);
    assert_eq!(stats.sessions_upserted, 0);
    assert!(db.get_session("hermes", SESSION_ID).await.is_none());
}

#[tokio::test]
async fn sweep_skips_rewound_rows_and_surfaces_reasoning_only_turns() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;

    let conn = open_state_db(&state_db);
    let reasoning_fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/provider_normalization/hermes/assistant_reasoning.input.json"
    ))
    .expect("checked-in Hermes reasoning-only row");
    // A rewound (soft-deleted) turn and a reasoning-only assistant turn.
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp, active)
         VALUES (?1, 'user', 'rewound secret prompt', 1780629400.0, 0)",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, reasoning, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            SESSION_ID,
            reasoning_fixture["role"].as_str().unwrap(),
            reasoning_fixture["content"].as_str().unwrap(),
            reasoning_fixture["reasoning"].as_str().unwrap(),
            reasoning_fixture["timestamp"].as_f64().unwrap(),
        ],
    )
    .unwrap();
    drop(conn);

    let db = open_project_session_db(&project).await.unwrap();
    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &project).await;
    // 4 fixture turns + the reasoning-only turn; the rewound row is skipped.
    assert_eq!(stats.messages_upserted, 5);
    assert!(
        db.get_session_message("hermes", &format!("{SESSION_ID}:6"))
            .await
            .is_none()
    );
    let reasoning_turn = db
        .get_session_message("hermes", &format!("{SESSION_ID}:7"))
        .await
        .expect("reasoning-only turn should be searchable");
    assert_eq!(reasoning_turn.kind.as_deref(), Some("reasoning_visible"));
    assert!(
        reasoning_turn
            .text
            .contains("thinking about the billing fix")
    );
    assert!(reasoning_turn.model.is_none());
    let hits = db
        .search_session_messages("hermes", None, "rewound secret prompt", 10)
        .await;
    assert!(hits.is_empty(), "rewound rows must not surface as history");
    let reasoning_hits = db
        .search_session_messages("hermes", None, "thinking about the billing fix", 10)
        .await;
    assert!(
        reasoning_hits
            .iter()
            .any(|hit| hit.message.kind.as_deref() == Some("reasoning_visible")),
        "typed reasoning facts must be independently searchable"
    );
}

#[tokio::test]
async fn sweep_reads_legacy_stores_without_active_or_reasoning_columns() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;

    // Rebuild `messages` with the pre-v12 shape (no active, no reasoning).
    let conn = open_state_db(&state_db);
    conn.execute("DROP TABLE messages", ()).unwrap();
    conn.execute(
        "CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            role TEXT NOT NULL,
            content TEXT,
            tool_calls TEXT,
            tool_name TEXT,
            timestamp REAL NOT NULL
        )",
        (),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES (?1, 'user', 'legacy schema prompt', 1780629300.0)",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    drop(conn);

    let db = open_project_session_db(&project).await.unwrap();
    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &project).await;
    assert_eq!(stats.messages_upserted, 1);
    let turn = db
        .get_session_message("hermes", &format!("{SESSION_ID}:1"))
        .await
        .expect("legacy-store rows should ingest");
    assert!(turn.text.contains("legacy schema prompt"));
}

#[tokio::test]
async fn unpinned_profile_never_maps_to_its_own_home_store() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, unrelated_project) = setup(&tmp);
    write_hermes_profile(&hermes_home, "test", None).await;
    let profile_dir = hermes_home.join("profiles").join("test");

    // Sweeping an unrelated project must not pick up the unpinned profile.
    let db = open_project_session_db(&unrelated_project).await.unwrap();
    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &unrelated_project).await;
    assert_eq!(stats.messages_upserted, 0);

    // A Hermes profile directory is not a code project identity.
    let profile_db = HostAdmissionTestRuntimeV1::project(
        tmp.path().join("profile-directory-destination"),
        &profile_dir,
        named_project_id("profile-directory"),
    )
    .await
    .unwrap();
    let profile_admission = profile_db.facade();
    let stats = ingest_homes_with_id(
        &profile_admission,
        std::slice::from_ref(&hermes_home),
        &profile_dir,
        named_project_id("profile-directory"),
    )
    .await;
    assert_eq!(stats.messages_upserted, 0);
    assert_eq!(stats.sessions_upserted, 0);
    assert!(
        profile_db
            .session_for_test(HostAdmissionScope::Project, "hermes", SESSION_ID)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn unpinned_profile_uses_session_cwd_as_project_provenance() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", None).await;
    let conn = open_state_db(&state_db);
    conn.execute(
        "UPDATE sessions SET cwd = ?1 WHERE id = ?2",
        rusqlite::params![project.to_string_lossy().as_ref(), SESSION_ID],
    )
    .unwrap();
    drop(conn);

    let db = open_project_session_db(&project).await.unwrap();
    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &project).await;
    assert_eq!(stats.messages_upserted, 4);
    let session = db
        .get_session("hermes", SESSION_ID)
        .await
        .expect("session cwd should prove project association");
    let metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["hermes_session_location_provenance"],
        "session_cwd"
    );
    assert_metadata_path_eq(&metadata["hermes_session_cwd"], &project);
}

#[tokio::test]
async fn unpinned_projectless_session_routes_only_a_tool_proven_turn() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", None).await;
    let conn = open_state_db(&state_db);
    let tool_calls = json!([{
        "id": "call_project_context",
        "type": "function",
        "function": {
            "name": "tracedecay_context",
            "arguments": json!({
                "project_path": project.to_string_lossy(),
                "query": "billing pipeline"
            }).to_string()
        }
    }])
    .to_string();
    conn.execute(
        "UPDATE messages SET tool_calls = ?1
         WHERE session_id = ?2 AND tool_calls IS NOT NULL",
        rusqlite::params![tool_calls, SESSION_ID],
    )
    .unwrap();
    drop(conn);

    let db = open_project_session_db(&project).await.unwrap();
    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &project).await;
    assert_eq!(stats.messages_upserted, 4);
    let session = db
        .get_session("hermes", SESSION_ID)
        .await
        .expect("structured tool project path should prove turn association");
    let metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["hermes_session_location_provenance"],
        "tool_project_path"
    );
    assert_metadata_path_eq(&metadata["hermes_session_cwd"], &project);
}

#[tokio::test]
async fn explicit_tool_route_overrides_session_cwd_without_cross_project_duplication() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, session_project) = setup(&tmp);
    let tool_project = tmp.path().join("tool-project");
    crate::support::init_project_at(&tool_project);
    let state_db = write_hermes_profile(&hermes_home, "test", None).await;
    let conn = open_state_db(&state_db);
    conn.execute(
        "UPDATE sessions SET cwd = ?1 WHERE id = ?2",
        rusqlite::params![session_project.to_string_lossy().as_ref(), SESSION_ID],
    )
    .unwrap();
    let tool_calls = json!([{
        "id": "call_other_project",
        "type": "function",
        "function": {
            "name": "tracedecay_context",
            "arguments": json!({
                "project_path": tool_project.to_string_lossy(),
                "query": "other project"
            }).to_string()
        }
    }])
    .to_string();
    conn.execute(
        "UPDATE messages SET tool_calls = ?1
         WHERE session_id = ?2 AND tool_calls IS NOT NULL",
        rusqlite::params![tool_calls, SESSION_ID],
    )
    .unwrap();
    drop(conn);

    let session_db = open_project_session_db(&session_project).await.unwrap();
    let session_stats = ingest_homes(
        &session_db,
        std::slice::from_ref(&hermes_home),
        &session_project,
    )
    .await;
    assert_eq!(session_stats.messages_upserted, 0);
    assert!(session_db.get_session("hermes", SESSION_ID).await.is_none());

    let tool_db = open_sibling_project_session_db(&session_db, &tool_project).await;
    let tool_stats =
        ingest_homes(&tool_db, std::slice::from_ref(&hermes_home), &tool_project).await;
    assert_eq!(tool_stats.messages_upserted, 4);
    let session = tool_db
        .get_session("hermes", SESSION_ID)
        .await
        .expect("explicit tool route should associate the turn with its project");
    let metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["hermes_session_location_provenance"],
        "tool_project_path"
    );
    assert_metadata_path_eq(&metadata["hermes_session_cwd"], &tool_project);
}

#[tokio::test]
async fn user_sweep_keeps_canonical_turns_routed_to_registered_projects() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, registered) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", None).await;
    let conn = open_state_db(&state_db);
    let tool_calls = json!([{
        "function": {
            "name": "tracedecay_context",
            "arguments": json!({"project_path": registered.to_string_lossy()}).to_string()
        }
    }])
    .to_string();
    conn.execute(
        "UPDATE messages SET tool_calls = ?1 WHERE session_id = ?2 AND tool_calls IS NOT NULL",
        rusqlite::params![tool_calls, SESSION_ID],
    )
    .unwrap();
    drop(conn);
    let user_db = HostAdmissionTestRuntimeV1::profile(tmp.path().join("user-profile"))
        .await
        .unwrap();
    let admission = user_db.facade();

    let stats = ingest_user_homes(
        &admission,
        std::slice::from_ref(&hermes_home),
        std::slice::from_ref(&registered),
    )
    .await;

    assert_eq!(stats.messages_upserted, 4);
    assert!(
        user_db
            .session_for_test(HostAdmissionScope::Profile, "hermes", SESSION_ID)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn user_sweep_keeps_registered_session_cwd_as_canonical_history() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, registered) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", None).await;
    let conn = open_state_db(&state_db);
    conn.execute(
        "UPDATE sessions SET cwd = ?1 WHERE id = ?2",
        rusqlite::params![registered.to_string_lossy().as_ref(), SESSION_ID],
    )
    .unwrap();
    conn.execute(
        "UPDATE messages SET tool_calls = NULL WHERE session_id = ?1",
        [SESSION_ID],
    )
    .unwrap();
    drop(conn);
    let user_db = HostAdmissionTestRuntimeV1::profile(tmp.path().join("user-profile"))
        .await
        .unwrap();
    let admission = user_db.facade();

    let stats = ingest_user_homes(
        &admission,
        std::slice::from_ref(&hermes_home),
        std::slice::from_ref(&registered),
    )
    .await;

    assert_eq!(stats.messages_upserted, 3);
    assert!(
        user_db
            .session_for_test(HostAdmissionScope::Profile, "hermes", SESSION_ID)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn hermes_projection_failure_commits_row_frontier_and_replays_once() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;
    let user_home = hermes_home.parent().unwrap();
    let _home = EnvVarGuard::set("HOME", user_home);
    init_git_repo(&project);
    mark_test_project(&project);

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_registered_project_provider(&db, &project).await;
    let before = db.session_message_count().await.unwrap();
    assert_eq!(before, 4);
    let prefix_cursor = observation_source_cursor(&db, "hermes", SESSION_ID, &project)
        .await
        .expect("committed Hermes observation cursor");
    assert!(prefix_cursor.position() >= u64::try_from(before).unwrap());
    drop(db);

    let conn = open_state_db(&state_db);
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES (?1, 'user', 'Hermes projection retry suffix', 1780629410.1)",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    drop(conn);

    let rejected = open_project_session_db(&project).await.unwrap();
    set_projection_failure(&rejected, true).await;
    let _ = ingest_registered_project_provider(&rejected, &project).await;
    let committed_cursor = observation_source_cursor(&rejected, "hermes", SESSION_ID, &project)
        .await
        .expect("committed Hermes observation cursor");
    assert_eq!(committed_cursor.generation(), prefix_cursor.generation());
    assert_eq!(committed_cursor.position(), prefix_cursor.position() + 1);
    assert_eq!(rejected.session_message_count().await.unwrap(), before);
    assert!(
        rejected
            .search_session_messages("hermes", None, "projection retry suffix", 10)
            .await
            .is_empty()
    );
    drop(rejected);

    let recovered = open_project_session_db(&project).await.unwrap();
    set_projection_failure(&recovered, false).await;
    let _ = ingest_registered_project_provider(&recovered, &project).await;
    assert_eq!(recovered.session_message_count().await.unwrap(), before + 1);
    assert_eq!(
        recovered
            .search_session_messages("hermes", None, "projection retry suffix", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        ingest_registered_project_provider(&recovered, &project)
            .await
            .messages_upserted,
        0
    );
    assert_eq!(
        observation_source_cursor(&recovered, "hermes", SESSION_ID, &project).await,
        Some(committed_cursor)
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn hermes_malformed_row_is_covered_and_valid_suffix_resumes_once() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;
    let user_home = hermes_home.parent().unwrap();
    let _home = EnvVarGuard::set("HOME", user_home);
    init_git_repo(&project);
    mark_test_project(&project);

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_registered_project_provider(&db, &project).await;
    let before = db.session_message_count().await.unwrap();
    assert_eq!(before, 4);
    let prefix_cursor = observation_source_cursor(&db, "hermes", SESSION_ID, &project)
        .await
        .expect("committed Hermes observation cursor");
    assert!(prefix_cursor.position() >= u64::try_from(before).unwrap());

    let conn = open_state_db(&state_db);
    // Incomplete/malformed tool_calls JSON on an otherwise complete row.
    conn.execute(
        "INSERT INTO messages (session_id, role, content, tool_calls, timestamp, finish_reason)
         VALUES (?1, 'assistant', '', '{not-json', 1780629420.2, 'tool_calls')",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    drop(conn);

    let covered = ingest_registered_project_provider(&db, &project).await;
    assert_eq!(covered.messages_upserted, 0);
    assert_eq!(db.session_message_count().await.unwrap(), before);
    let malformed_cursor = observation_source_cursor(&db, "hermes", SESSION_ID, &project)
        .await
        .expect("committed Hermes observation cursor");
    assert_eq!(malformed_cursor.position(), prefix_cursor.position() + 1);

    let conn = open_state_db(&state_db);
    conn.execute(
        "UPDATE messages
         SET content = 'Repaired Hermes row at covered identity',
             tool_calls = '[{\"id\":\"repaired-call\",\"type\":\"function\",\"function\":{\"name\":\"repair\",\"arguments\":\"{}\"}}]'
         WHERE id = 5",
        (),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES (?1, 'user', 'Valid Hermes row after malformed tool_calls', 1780629430.3)",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    drop(conn);

    assert_eq!(
        ingest_registered_project_provider(&db, &project)
            .await
            .messages_upserted,
        1
    );
    assert_eq!(
        observation_source_cursor(&db, "hermes", SESSION_ID, &project)
            .await
            .expect("committed Hermes observation cursor")
            .position(),
        malformed_cursor.position() + 1
    );
    assert!(
        db.search_session_messages("hermes", None, "Repaired", 10)
            .await
            .is_empty(),
        "a malformed row already covered at row id 5 must not be reinterpreted in place"
    );
    assert_eq!(
        db.search_session_messages("hermes", None, "Valid", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        ingest_registered_project_provider(&db, &project)
            .await
            .messages_upserted,
        0
    );
    assert_eq!(
        observation_source_cursor(&db, "hermes", SESSION_ID, &project)
            .await
            .expect("committed Hermes observation cursor")
            .position(),
        malformed_cursor.position() + 1
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn hermes_conflicting_identity_does_not_overwrite_committed_observation() {
    // Hermes derives native_record_id from immutable message evidence (content/
    // role/timestamp/tool fields), while the canonical envelope still embeds the
    // generation-local SQLite row range. A later row that reuses that evidence
    // therefore collides on observation identity with a different payload range
    // and must fail closed without overwriting the earlier projection.
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;
    let user_home = hermes_home.parent().unwrap();
    let _home = EnvVarGuard::set("HOME", user_home);
    init_git_repo(&project);
    mark_test_project(&project);

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_registered_project_provider(&db, &project).await;
    let before = db.session_message_count().await.unwrap();
    assert_eq!(before, 4);
    assert_eq!(
        db.search_session_messages("hermes", None, "fixed", 10)
            .await
            .len(),
        1
    );
    let prefix_cursor = observation_source_cursor(&db, "hermes", SESSION_ID, &project)
        .await
        .expect("committed Hermes observation cursor");

    let conn = open_state_db(&state_db);
    // Same immutable message evidence as the final assistant row in write_hermes_profile.
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp, finish_reason)
         VALUES (?1, 'assistant', 'The billing pipeline test is fixed.', 1780629330.9, 'stop')",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    drop(conn);

    let _ = ingest_registered_project_provider(&db, &project).await;
    assert_eq!(db.session_message_count().await.unwrap(), before);
    assert_eq!(
        db.search_session_messages("hermes", None, "fixed", 10)
            .await
            .len(),
        1
    );
    assert!(
        db.search_session_messages("hermes", None, "deterministic conflict winner", 10)
            .await
            .is_empty()
    );
    // Collision fails closed before advancing past the conflicting row.
    assert_eq!(
        observation_source_cursor(&db, "hermes", SESSION_ID, &project).await,
        Some(prefix_cursor)
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn hermes_observation_commit_before_ack_survives_reopen() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let _state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;
    let user_home = hermes_home.parent().unwrap();
    let _home = EnvVarGuard::set("HOME", user_home);
    init_git_repo(&project);
    mark_test_project(&project);

    let rejected = open_project_session_db(&project).await.unwrap();
    set_projection_failure(&rejected, true).await;
    let _ = ingest_registered_project_provider(&rejected, &project).await;
    assert_eq!(rejected.session_message_count().await.unwrap(), 0);
    assert!(
        rejected
            .search_session_messages("hermes", None, "billing pipeline test is fixed", 10)
            .await
            .is_empty()
    );
    let committed_cursor = observation_source_cursor(&rejected, "hermes", SESSION_ID, &project)
        .await
        .expect("Hermes observation frontier commits before projection ack");
    assert!(committed_cursor.position() > 0);
    drop(rejected);

    let committed = open_project_session_db(&project).await.unwrap();
    let observations = durable_table_count(&committed, "observations").await;
    let receipts = durable_table_count(&committed, "sanitization_receipts").await;
    let queued = durable_table_count(&committed, "projection_queue").await;
    assert!(
        observations >= 1,
        "Hermes observation commits before projection ack"
    );
    assert!(
        receipts >= 1,
        "Hermes sanitization receipts commit with observations"
    );
    assert!(
        queued >= 1,
        "Hermes projection work stays queued across the failed ack"
    );

    set_projection_failure(&committed, false).await;
    drop(committed);
    let recovered = open_project_session_db(&project).await.unwrap();
    // Reopen drains the already-committed projection queue before source
    // discovery runs, so the source replay itself is a no-op.
    assert_eq!(
        ingest_registered_project_provider(&recovered, &project)
            .await
            .messages_upserted,
        0
    );
    assert_eq!(recovered.session_message_count().await.unwrap(), 4);
    assert_eq!(
        recovered
            .search_session_messages("hermes", None, "fixed", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        durable_table_count(&recovered, "observations").await,
        observations
    );
    assert_eq!(
        durable_table_count(&recovered, "sanitization_receipts").await,
        receipts
    );
    assert_eq!(durable_table_count(&recovered, "projection_queue").await, 0);
    assert_eq!(
        ingest_registered_project_provider(&recovered, &project)
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn hermes_zeroblob_content_is_covered_without_payload_leak() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;
    let user_home = hermes_home.parent().unwrap();
    let _home = EnvVarGuard::set("HOME", user_home);
    init_git_repo(&project);
    mark_test_project(&project);

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_registered_project_provider(&db, &project).await;
    let before = db.session_message_count().await.unwrap();
    let prefix_cursor = observation_source_cursor(&db, "hermes", SESSION_ID, &project)
        .await
        .expect("committed Hermes observation cursor");

    let conn = open_state_db(&state_db);
    // Hostile payload exists only inside SQLite (zeroblob); Rust never builds it.
    let hostile_bytes = MAX_OBSERVATION_RECORD_BYTES.saturating_add(1);
    conn.execute(
        &format!(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES ('{SESSION_ID}', 'user', zeroblob({hostile_bytes}), 1780629440.0)"
        ),
        (),
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES (?1, 'assistant', 'Hermes row after zeroblob cover', 1780629450.0)",
        rusqlite::params![SESSION_ID],
    )
    .unwrap();
    drop(conn);

    let covered = ingest_registered_project_provider(&db, &project).await;
    let after_count = db.session_message_count().await.unwrap();
    assert_eq!(
        after_count,
        before + 1,
        "zeroblob must not become a durable projected message; only the safe suffix may"
    );
    assert!(
        covered.messages_upserted <= 1,
        "zeroblob coverage must not count as a durable capture (upserted={})",
        covered.messages_upserted
    );
    let after_cursor = observation_source_cursor(&db, "hermes", SESSION_ID, &project)
        .await
        .expect("committed Hermes observation cursor");
    assert_eq!(
        after_cursor.position(),
        prefix_cursor.position() + 2,
        "zeroblob row must advance coverage and the safe suffix must commit"
    );
    assert_eq!(
        db.search_session_messages("hermes", None, "Hermes row after zeroblob cover", 10)
            .await
            .len(),
        1
    );
}
