#![allow(dead_code)]

use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::{SessionMessageRecord, SessionRecord};

pub fn isolated_lcm_db_path(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join(".tokensave").join("sessions.db")
}

pub fn isolated_global_db_path(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join(".tokensave").join("global.db")
}

pub async fn open_lcm_db(tmp: &TempDir) -> GlobalDb {
    GlobalDb::open_at(&isolated_lcm_db_path(tmp))
        .await
        .expect("session db open")
}

pub async fn open_global_db(tmp: &TempDir) -> GlobalDb {
    GlobalDb::open_at(&isolated_global_db_path(tmp))
        .await
        .expect("global db open")
}

pub fn session_record(
    provider: &str,
    session_id: &str,
    project_key: &str,
    title: &str,
    transcript_path: Option<&str>,
    metadata_json: Option<&str>,
) -> SessionRecord {
    SessionRecord {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        project_key: project_key.to_string(),
        project_path: "/tmp/project".to_string(),
        title: Some(title.to_string()),
        started_at: Some(1_715_000_000),
        ended_at: None,
        transcript_path: transcript_path.map(str::to_string),
        metadata_json: metadata_json.map(str::to_string),
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

pub fn lcm_payload_session(provider: &str, session_id: &str) -> SessionRecord {
    session_record(
        provider,
        session_id,
        "/tmp/project",
        "LCM payload test",
        None,
        None,
    )
}

pub fn lcm_dag_session(provider: &str, session_id: &str) -> SessionRecord {
    session_record(
        provider,
        session_id,
        "/tmp/project",
        "LCM DAG test",
        None,
        None,
    )
}

pub fn lcm_raw_session(provider: &str, session_id: &str, project_key: &str) -> SessionRecord {
    session_record(
        provider,
        session_id,
        project_key,
        "LCM raw test",
        Some("/tmp/project/transcript.jsonl"),
        None,
    )
}

pub fn global_session(provider: &str, session_id: &str, project_key: &str) -> SessionRecord {
    session_record(
        provider,
        session_id,
        project_key,
        "Initial title",
        Some("/tmp/project/transcript.jsonl"),
        Some(r#"{"source":"test"}"#),
    )
}

pub fn message_record(
    provider: &str,
    message_id: &str,
    session_id: &str,
    role: &str,
    ordinal: i64,
    text: &str,
    kind: &str,
    tool_names: Option<&str>,
    source_path: Option<&str>,
    source_offset: Option<i64>,
    metadata_json: Option<&str>,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: provider.to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        timestamp: Some(1_715_000_030),
        ordinal,
        text: text.to_string(),
        kind: Some(kind.to_string()),
        model: Some("test-model".to_string()),
        tool_names: tool_names.map(str::to_string),
        source_path: source_path.map(str::to_string),
        source_offset,
        metadata_json: metadata_json.map(str::to_string),
    }
}

pub fn lcm_payload_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    role: &str,
    text: &str,
) -> SessionMessageRecord {
    message_record(
        provider,
        message_id,
        session_id,
        role,
        1,
        text,
        "tool_result",
        None,
        None,
        None,
        None,
    )
}

pub fn lcm_dag_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    ordinal: i64,
    text: &str,
) -> SessionMessageRecord {
    let mut message = message_record(
        provider,
        message_id,
        session_id,
        "assistant",
        ordinal,
        text,
        "message",
        None,
        None,
        None,
        None,
    );
    message.timestamp = Some(1_715_000_000 + ordinal);
    message
}

pub fn lcm_raw_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    text: &str,
) -> SessionMessageRecord {
    message_record(
        provider,
        message_id,
        session_id,
        "assistant",
        1,
        text,
        "message",
        None,
        Some("/tmp/project/transcript.jsonl"),
        Some(42),
        None,
    )
}

pub fn global_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    text: &str,
) -> SessionMessageRecord {
    message_record(
        provider,
        message_id,
        session_id,
        "assistant",
        1,
        text,
        "message",
        Some("tokensave_context,tokensave_search"),
        Some("/tmp/project/transcript.jsonl"),
        Some(42),
        Some(r#"{"finish_reason":"stop"}"#),
    )
}
