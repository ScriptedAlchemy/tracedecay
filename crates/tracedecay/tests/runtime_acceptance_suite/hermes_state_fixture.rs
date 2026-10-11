//! Writes a Hermes host's own `state.db`. It is a foreign SQLite database the
//! host reader opens, so it is built with rusqlite, not the owned-store driver.

use std::path::Path;

use serde_json::Value;

pub(crate) async fn write_hermes_native_fixture(home: &Path, project: &Path) {
    let profile = home.join("profiles/host-event");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(
        profile.join("config.yaml"),
        format!(
            "plugins:\n  enabled:\n    - tracedecay\n  tracedecay:\n    project_root: {}\n",
            serde_json::to_string(project.to_string_lossy().as_ref()).unwrap()
        ),
    )
    .unwrap();
    let conn = rusqlite::Connection::open(profile.join("state.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY, source TEXT NOT NULL, model TEXT, started_at REAL NOT NULL,
            ended_at REAL, cwd TEXT, title TEXT, parent_session_id TEXT,
            input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,
            cache_read_tokens INTEGER DEFAULT 0, cache_write_tokens INTEGER DEFAULT 0,
            reasoning_tokens INTEGER DEFAULT 0
         );
         CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, role TEXT NOT NULL,
            content TEXT, tool_call_id TEXT, tool_calls TEXT, tool_name TEXT,
            timestamp REAL NOT NULL, token_count INTEGER, finish_reason TEXT,
            reasoning TEXT, observed INTEGER DEFAULT 0, active INTEGER NOT NULL DEFAULT 1
         );",
    )
    .unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/provider_normalization/hermes/assistant_tool_call.input.json"
    ))
    .unwrap();
    let session_id = fixture["session_id"].as_str().unwrap();
    conn.execute(
        "INSERT INTO sessions (
            id, source, model, started_at, ended_at, cwd, title,
            input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens
         ) VALUES (?1, 'tui', ?2, ?3, ?3, ?4, 'Host event fixture', ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            session_id,
            fixture["session_model"].as_str(),
            fixture["timestamp"].as_f64(),
            project.to_string_lossy().as_ref(),
            fixture["session_input_tokens"].as_i64(),
            fixture["session_output_tokens"].as_i64(),
            fixture["session_cache_read_tokens"].as_i64(),
            fixture["session_cache_write_tokens"].as_i64(),
            fixture["session_reasoning_tokens"].as_i64(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, tool_calls, timestamp, finish_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, 'tool_calls')",
        rusqlite::params![
            session_id,
            fixture["role"].as_str(),
            fixture["content"].as_str(),
            fixture["tool_calls"].to_string(),
            fixture["timestamp"].as_f64(),
        ],
    )
    .unwrap();
}
