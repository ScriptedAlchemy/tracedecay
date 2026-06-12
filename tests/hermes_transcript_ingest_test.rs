//! Hermes `state.db` transcript ingestion: profile-pin scoping, projection-only
//! writes, and incremental rowid-cursor sweeps. Fixtures mirror the real
//! Hermes schema (`sessions` + `messages` tables) and real row shapes
//! (assistant tool-call turns with empty `content`, JSON `tool_calls`,
//! REAL epoch-second timestamps, session-level token counters).

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tokensave::sessions::cursor::open_project_session_db;
use tokensave::sessions::hermes::ingest_homes;

const SESSION_ID: &str = "20260101_000000_abc123";

fn setup(tmp: &TempDir) -> (PathBuf, PathBuf) {
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tokensave")).unwrap();
    std::fs::write(project.join(".tokensave/tokensave.db"), "").unwrap();
    (home.join(".hermes"), project)
}

/// Writes a Hermes profile dir: a `config.yaml` optionally pinning
/// `pinned_project` (the real `plugins.tokensave.project_root` shape) and a
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
            // The pin is JSON-encoded exactly as `tokensave install --agent
            // hermes` writes it, so Windows backslashes survive the
            // double-quoted YAML scalar.
            let pin = serde_json::to_string(pinned_project.to_string_lossy().as_ref()).unwrap();
            format!(
                "memory:\n  provider: tokensave\nplugins:\n  enabled:\n    - tokensave\n  tokensave:\n    project_root: {pin}\n",
            )
        }
        None => {
            "memory:\n  provider: tokensave\nplugins:\n  enabled:\n    - tokensave\n".to_string()
        }
    };
    std::fs::write(profile_dir.join("config.yaml"), config).unwrap();

    let state_db = profile_dir.join("state.db");
    let conn = open_state_db(&state_db).await;
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
    .await
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
    .await
    .unwrap();

    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, ended_at, title,
                               input_tokens, output_tokens, cache_read_tokens,
                               cache_write_tokens, reasoning_tokens)
         VALUES (?1, 'tui', 'gpt-5.5', 1780629300.0, 1780629340.0,
                 'Billing pipeline fix', 96443, 3804, 1064960, 0, 2061)",
        libsql::params![SESSION_ID],
    )
    .await
    .unwrap();

    // Real Hermes row shapes: a session_meta bootstrap row (must be skipped),
    // a user prompt, an assistant tool-call turn with empty content, a tool
    // result keyed by tool_name, and a final assistant reply.
    let tool_calls = serde_json::json!([{
        "id": "call_FBvwGfCC9lJrXPvOqpDHcjYn",
        "call_id": "call_FBvwGfCC9lJrXPvOqpDHcjYn",
        "type": "function",
        "function": {"name": "terminal", "arguments": "{\"command\":\"cargo test billing\"}"}
    }])
    .to_string();
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
            Some(""),
            Some(tool_calls.as_str()),
            None,
            1_780_629_310.5,
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
            libsql::params![SESSION_ID, role, content, tool_calls, tool_name, ts, finish],
        )
        .await
        .unwrap();
    }
    state_db
}

async fn open_state_db(path: &Path) -> libsql::Connection {
    libsql::Builder::new_local(path)
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

#[tokio::test]
async fn hermes_state_db_populates_projection_for_pinned_project() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    write_hermes_profile(&hermes_home, "test", Some(&project)).await;

    let db = open_project_session_db(&project).await.unwrap();
    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &project).await;
    // user + assistant tool-call turn + tool result + assistant reply; the
    // session_meta bootstrap row is skipped.
    assert_eq!(stats.messages_upserted, 4);
    assert_eq!(stats.sessions_upserted, 1);

    let results = db
        .search_session_messages(
            "hermes",
            Some(project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
        .await;
    assert!(results.iter().any(|hit| hit.message.role == "user"));
    assert!(results.iter().any(|hit| hit.message.role == "assistant"));
    assert!(results
        .iter()
        .all(|hit| hit.message.model.as_deref() == Some("gpt-5.5")));
    // REAL epoch-second timestamps land truncated to whole seconds.
    assert!(results
        .iter()
        .any(|hit| hit.message.timestamp == Some(1_780_629_300)));

    let session = db
        .get_session("hermes", SESSION_ID)
        .await
        .expect("hermes session should be stored");
    assert_eq!(session.title.as_deref(), Some("Billing pipeline fix"));
    assert_eq!(session.started_at, Some(1_780_629_300));
    assert_eq!(session.ended_at, Some(1_780_629_340));
    let metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["source"], "hermes_state_db");
    assert_eq!(metadata["profile"], "test");
    assert_eq!(metadata["hermes_source"], "tui");
    // Session-cumulative token counters from the Hermes sessions table map to
    // dashboard counter names; zero counters (cache_write) are omitted.
    assert_eq!(metadata["usage"]["input_tokens"], 96443);
    assert_eq!(metadata["usage"]["output_tokens"], 3804);
    assert_eq!(metadata["usage"]["cache_read_input_tokens"], 1_064_960);
    assert_eq!(metadata["usage"]["reasoning_tokens"], 2061);
    assert!(metadata["usage"]
        .get("cache_creation_input_tokens")
        .is_none());

    // The assistant tool-call turn has no content; its text falls back to the
    // tool_calls JSON and the tool name is extracted.
    let tool_turn = db
        .get_session_message("hermes", &format!("{SESSION_ID}:3"))
        .await
        .expect("assistant tool-call turn should be stored");
    assert_eq!(tool_turn.role, "assistant");
    assert!(tool_turn.text.contains("call_FBvwGfCC9lJrXPvOqpDHcjYn"));
    assert_eq!(tool_turn.tool_names.as_deref(), Some("terminal"));

    // Projection-only: Hermes raw messages are owned by the runtime LCM
    // ingest, so the transcript sweep must never write lcm_raw_messages.
    for ordinal in 2..=5 {
        assert!(db
            .lcm_load_raw_message("hermes", &format!("{SESSION_ID}:{ordinal}"))
            .await
            .is_none());
    }
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

    let conn = open_state_db(&state_db).await;
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES (?1, 'user', 'Also add a regression test', 1780629400.4)",
        libsql::params![SESSION_ID],
    )
    .await
    .unwrap();

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

    let conn = open_state_db(&state_db).await;
    // A rewound (soft-deleted) turn and a reasoning-only assistant turn.
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp, active)
         VALUES (?1, 'user', 'rewound secret prompt', 1780629400.0, 0)",
        libsql::params![SESSION_ID],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, reasoning, timestamp)
         VALUES (?1, 'assistant', '', 'thinking about the billing fix', 1780629410.0)",
        libsql::params![SESSION_ID],
    )
    .await
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &project).await;
    // 4 fixture turns + the reasoning-only turn; the rewound row is skipped.
    assert_eq!(stats.messages_upserted, 5);
    assert!(db
        .get_session_message("hermes", &format!("{SESSION_ID}:6"))
        .await
        .is_none());
    let reasoning_turn = db
        .get_session_message("hermes", &format!("{SESSION_ID}:7"))
        .await
        .expect("reasoning-only turn should be searchable");
    assert!(reasoning_turn
        .text
        .contains("thinking about the billing fix"));
    let hits = db
        .search_session_messages("hermes", None, "rewound secret prompt", 10)
        .await;
    assert!(hits.is_empty(), "rewound rows must not surface as history");
}

#[tokio::test]
async fn sweep_reads_legacy_stores_without_active_or_reasoning_columns() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, project) = setup(&tmp);
    let state_db = write_hermes_profile(&hermes_home, "test", Some(&project)).await;

    // Rebuild `messages` with the pre-v12 shape (no active, no reasoning).
    let conn = open_state_db(&state_db).await;
    conn.execute("DROP TABLE messages", ()).await.unwrap();
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
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp)
         VALUES (?1, 'user', 'legacy schema prompt', 1780629300.0)",
        libsql::params![SESSION_ID],
    )
    .await
    .unwrap();

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
async fn unpinned_profile_maps_to_its_own_home_store() {
    let tmp = TempDir::new().unwrap();
    let (hermes_home, unrelated_project) = setup(&tmp);
    write_hermes_profile(&hermes_home, "test", None).await;
    let profile_dir = hermes_home.join("profiles").join("test");

    // Sweeping an unrelated project must not pick up the unpinned profile.
    let db = open_project_session_db(&unrelated_project).await.unwrap();
    let stats = ingest_homes(&db, std::slice::from_ref(&hermes_home), &unrelated_project).await;
    assert_eq!(stats.messages_upserted, 0);

    // Sweeping the profile home itself ingests into the profile-scoped store
    // (`<profile>/.tokensave/sessions.db`) — the store the generated
    // plugin's hermes_profile storage scope serves.
    let profile_db = open_project_session_db(&profile_dir).await.unwrap();
    let stats = ingest_homes(
        &profile_db,
        std::slice::from_ref(&hermes_home),
        &profile_dir,
    )
    .await;
    assert_eq!(stats.messages_upserted, 4);
    assert_eq!(stats.sessions_upserted, 1);
    let session = profile_db
        .get_session("hermes", SESSION_ID)
        .await
        .expect("unpinned profile history should land in its own home store");
    assert_eq!(session.title.as_deref(), Some("Billing pipeline fix"));
}
