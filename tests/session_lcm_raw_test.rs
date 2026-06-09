use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokensave::global_db::GlobalDb;
use tokensave::sessions::lcm::LcmPreflightRequest;
use tokensave::sessions::source::{
    ingest_source, ParsedTranscript, SessionDraft, StoredCursor, TranscriptSource,
};
use tokensave::sessions::{SessionMessageRecord, SessionRecord};

async fn open_isolated_db(tmp: &TempDir) -> GlobalDb {
    let db_path = tmp.path().join(".tokensave").join("sessions.db");
    GlobalDb::open_at(&db_path).await.expect("session db open")
}

fn sample_session(provider: &str, session_id: &str, project_key: &str) -> SessionRecord {
    SessionRecord {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        project_key: project_key.to_string(),
        project_path: "/tmp/project".to_string(),
        title: Some("LCM raw test".to_string()),
        started_at: Some(1_715_000_000),
        ended_at: None,
        transcript_path: Some("/tmp/project/transcript.jsonl".to_string()),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

fn sample_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    text: &str,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: provider.to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: "assistant".to_string(),
        timestamp: Some(1_715_000_030),
        ordinal: 1,
        text: text.to_string(),
        kind: Some("message".to_string()),
        model: Some("test-model".to_string()),
        tool_names: None,
        source_path: Some("/tmp/project/transcript.jsonl".to_string()),
        source_offset: Some(42),
        metadata_json: None,
    }
}

struct FakeTranscriptSource {
    path: PathBuf,
    content: String,
}

impl TranscriptSource for FakeTranscriptSource {
    fn provider(&self) -> &'static str {
        "fake"
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        vec![self.path.clone()]
    }

    fn parse_new(
        &self,
        path: &Path,
        _prev: StoredCursor,
        project_root: &Path,
        _max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        Some(ParsedTranscript {
            draft: SessionDraft {
                session_id: "fake-session-1".to_string(),
                project_key: project_root.to_string_lossy().to_string(),
                project_path: project_root.to_string_lossy().to_string(),
                title: Some("Fake raw ingest".to_string()),
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            },
            messages: vec![SessionMessageRecord {
                provider: "fake".to_string(),
                message_id: "fake-message-1".to_string(),
                session_id: "fake-session-1".to_string(),
                role: "assistant".to_string(),
                timestamp: Some(1_715_000_030),
                ordinal: 1,
                text: self.content.clone(),
                kind: Some("message".to_string()),
                model: Some("fake-model".to_string()),
                tool_names: None,
                source_path: Some(path.to_string_lossy().to_string()),
                source_offset: Some(0),
                metadata_json: None,
            }],
            new_cursor: StoredCursor {
                position: self.content.len() as u64,
                mtime: 1,
            },
        })
    }
}

#[tokio::test]
async fn active_replay_metadata_namespaces_original_fields_from_storage_metadata() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    assert!(
        db.upsert_session(&sample_session(
            "cursor",
            "session-active-metadata",
            "project-a"
        ))
        .await
    );

    let active_message = json!({
        "id": "active-collision-metadata",
        "role": "assistant",
        "content": [
            {"type": "text", "text": "namespaced active replay"},
            {"type": "input_json", "value": {"ok": true}},
        ],
        "payload_ref": "original-payload-ref",
        "byte_count": 9876,
        "char_count": 543,
        "sha256": "original-sha256",
        "external_payload": {"source": "original-message"},
        "ingest_protection": {"source": "original-message"},
    });

    let preflight = db
        .lcm_preflight(LcmPreflightRequest {
            provider: "cursor".into(),
            session_id: "session-active-metadata".into(),
            messages: vec![active_message.clone()],
            current_tokens: Some(100),
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            ignore_message_patterns: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(preflight.replay_messages[0], active_message);

    let raw = db
        .lcm_load_raw_message("cursor", "active-collision-metadata")
        .await
        .expect("raw active message should exist");
    let metadata: Value = serde_json::from_str(raw.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["lcm_active_replay"], true);
    assert_eq!(metadata["active_replay"], preflight.replay_messages[0]);
    assert!(metadata.get("payload_ref").is_none());
    assert!(metadata.get("byte_count").is_none());
    assert!(metadata.get("char_count").is_none());
    assert!(metadata.get("sha256").is_none());
    assert!(metadata.get("external_payload").is_none());
}

#[tokio::test]
async fn transcript_ingest_preserves_lossless_raw_content() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let transcript = project.join("fake-transcript.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();

    let db = open_isolated_db(&tmp).await;
    let content = format!("{}{}", "a".repeat(300_000), "::lossless-tail");
    let source = FakeTranscriptSource {
        path: transcript,
        content: content.clone(),
    };

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.sessions_upserted, 1);
    assert_eq!(stats.messages_upserted, 1);

    let compatibility = db
        .get_session_message("fake", "fake-message-1")
        .await
        .expect("compatibility message should exist");
    assert!(compatibility.text.chars().count() <= tokensave::sessions::lcm::MAX_DERIVED_TEXT_CHARS);
    assert!(compatibility
        .text
        .contains(tokensave::sessions::lcm::DERIVED_TRUNCATION_MARKER));

    let raw = db
        .lcm_load_raw_message("fake", "fake-message-1")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.content, content);
    assert!(raw.content.ends_with("::lossless-tail"));
    assert!(!raw.legacy_source);
    assert!(!raw.legacy_truncated);
}

#[tokio::test]
async fn search_uses_bounded_projection_but_load_recovers_raw() {
    let tmp = TempDir::new().unwrap();
    let db = open_isolated_db(&tmp).await;
    let session = sample_session("cursor", "session-1", "project-a");
    assert!(db.upsert_session(&session).await);

    let oversized = format!(
        "unique-search-token\n{}::lossless-tail",
        "x".repeat(tokensave::sessions::lcm::MAX_DERIVED_TEXT_CHARS * 5)
    );
    let message = sample_message("cursor", "message-1", "session-1", &oversized);
    assert!(db.upsert_session_message(&message).await);

    let results = db
        .search_session_messages("cursor", Some("project-a"), "unique-search-token", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].message.message_id, "message-1");
    assert!(
        results[0].message.text.chars().count() <= tokensave::sessions::lcm::MAX_DERIVED_TEXT_CHARS
    );
    assert!(results[0]
        .message
        .text
        .contains(tokensave::sessions::lcm::DERIVED_TRUNCATION_MARKER));

    let raw = db
        .lcm_load_raw_message("cursor", "message-1")
        .await
        .expect("raw message should exist");
    assert_eq!(raw.content, oversized);
    assert!(raw.content.ends_with("::lossless-tail"));
}
