//! AWS Kiro IDE transcript source.
//!
//! Kiro persists chat history under VS Code-style globalStorage at
//! `Kiro/User/globalStorage/kiro.kiroagent`. Two layouts are supported:
//!
//! * **Legacy** — `<workspace-hash>/<execution-id>.chat` JSON with a `chat`
//!   array (`human`/`bot` roles) and `metadata` (model, workflow id, times).
//! * **Modern** — extensionless execution JSON under workspace hash dirs or
//!   `workspace-sessions/<encoded-workspace-path>/<session-id>.json` with a
//!   top-level `messages`/`conversation`/`chat` array.
//!
//! Project scoping resolves each workspace hash via
//! `Kiro/User/workspaceStorage/<hash>/workspace.json` (`folder` field) or, for
//! `workspace-sessions`, by base64-decoding the directory name. The source uses
//! the shared **`ContentHash`** reader because Kiro writes full snapshot files.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::sessions::source::{
    append_tool_calls_metadata, append_usage_metadata, collect_files_with_ext,
    content_storage_text_and_tools, paths_equal, read_changed_file, title_from_messages,
    ParsedTranscript, SessionDraft, StoredCursor, TranscriptSource,
};
use crate::sessions::SessionMessageRecord;

const PROVIDER: &str = "kiro";
/// Workspace hash dirs plus one level of session nesting.
const MAX_SCAN_DEPTH: u8 = 3;
/// Bound workspace hash enumeration on large installs.
const MAX_WORKSPACE_DIRS: usize = 256;

/// Kiro IDE transcript locator + parser.
pub struct KiroSource {
    agent_dir: PathBuf,
    workspace_storage_dir: PathBuf,
}

impl KiroSource {
    /// Source rooted at the real Kiro IDE storage. Returns `None` when home
    /// cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = dirs::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.config/Kiro` (or macOS equivalent).
    pub fn with_home(home: &Path) -> Self {
        let data_dir = crate::agents::kiro_data_dir(home);
        Self {
            agent_dir: data_dir.join("User/globalStorage/kiro.kiroagent"),
            workspace_storage_dir: data_dir.join("User/workspaceStorage"),
        }
    }
}

impl TranscriptSource for KiroSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        out.extend(collect_workspace_session_files(
            &self.agent_dir.join("workspace-sessions"),
            project_root,
        ));
        out.extend(collect_agent_storage_files(
            &self.agent_dir,
            &self.workspace_storage_dir,
            project_root,
        ));
        out
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        _max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        if !transcript_belongs_to_project(path, &self.workspace_storage_dir, project_root) {
            return None;
        }

        let changed = read_changed_file(path, prev)?;
        let value: Value = serde_json::from_str(&changed.contents).ok()?;
        if value.get("executions").and_then(Value::as_array).is_some() {
            return None;
        }

        let session_id = session_id_from_transcript(path, &value);
        let model = model_from_transcript(&value);
        let messages = messages_from_transcript(&value, &session_id, path, model.as_deref());
        if messages.is_empty() {
            return None;
        }

        let project = project_root.to_string_lossy().to_string();
        let draft = SessionDraft {
            session_id: session_id.clone(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&serde_json::json!({
                "source": "kiro_transcript",
            }))
            .ok(),
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };

        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: changed.new_cursor,
        })
    }
}

/// Incrementally ingests Kiro transcripts for `project_root` into `db`.
pub async fn ingest_kiro_for_project(
    db: &crate::global_db::GlobalDb,
    project_root: &Path,
    max_new_bytes: Option<u64>,
) -> crate::sessions::source::TranscriptIngestStats {
    let Some(source) = KiroSource::new() else {
        return crate::sessions::source::TranscriptIngestStats::default();
    };
    crate::sessions::source::ingest_source(db, &source, project_root, max_new_bytes).await
}

fn collect_workspace_session_files(sessions_root: &Path, project_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(sessions_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let encoded_dir = entry.path();
        if !encoded_dir.is_dir() {
            continue;
        }
        let Some(workspace) =
            decode_workspace_sessions_dir(entry.file_name().to_string_lossy().as_ref())
        else {
            continue;
        };
        if !paths_equal(&workspace, project_root) {
            continue;
        }
        let Ok(session_entries) = std::fs::read_dir(&encoded_dir) else {
            continue;
        };
        for session_entry in session_entries.flatten() {
            let path = session_entry.path();
            if path.is_file() && path.extension().is_none_or(|ext| ext == "json") {
                out.push(path);
            }
        }
    }
    out
}

fn collect_agent_storage_files(
    agent_dir: &Path,
    workspace_storage_dir: &Path,
    project_root: &Path,
) -> Vec<PathBuf> {
    let mut workspace_dirs: Vec<(u64, PathBuf, PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(agent_dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "workspace-sessions" || name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() || name.len() != 32 {
            continue;
        }
        let Some(workspace) = workspace_path_from_hash(workspace_storage_dir, &name) else {
            continue;
        };
        if !paths_equal(&workspace, project_root) {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs());
        workspace_dirs.push((mtime, path, workspace));
    }
    workspace_dirs.sort_by_key(|b| std::cmp::Reverse(b.0));
    workspace_dirs.truncate(MAX_WORKSPACE_DIRS);

    let mut out = Vec::new();
    for (_, workspace_dir, _) in workspace_dirs {
        out.extend(
            collect_files_with_ext(&workspace_dir, "chat", MAX_SCAN_DEPTH)
                .into_iter()
                .filter(|path| path.is_file()),
        );
        collect_extensionless_execution_files(&workspace_dir, MAX_SCAN_DEPTH, &mut out);
    }
    out
}

fn collect_extensionless_execution_files(dir: &Path, max_depth: u8, out: &mut Vec<PathBuf>) {
    if max_depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_extensionless_execution_files(&path, max_depth - 1, out);
            continue;
        }
        if path.extension().is_some() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == "sessions.json") {
            continue;
        }
        out.push(path);
    }
}

fn transcript_belongs_to_project(
    path: &Path,
    workspace_storage_dir: &Path,
    project_root: &Path,
) -> bool {
    if let Some(workspace) = workspace_from_sessions_path(path) {
        return paths_equal(&workspace, project_root);
    }
    let Some(hash) = workspace_hash_from_path(path) else {
        return false;
    };
    workspace_path_from_hash(workspace_storage_dir, &hash)
        .is_some_and(|workspace| paths_equal(&workspace, project_root))
}

fn workspace_from_sessions_path(path: &Path) -> Option<PathBuf> {
    let components = path.components().collect::<Vec<_>>();
    let idx = components
        .iter()
        .position(|component| component.as_os_str() == "workspace-sessions")?;
    let encoded = components.get(idx + 1)?.as_os_str().to_str()?;
    decode_workspace_sessions_dir(encoded)
}

fn workspace_hash_from_path(path: &Path) -> Option<String> {
    path.ancestors().find_map(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.len() == 32 && name.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_string)
    })
}

fn workspace_path_from_hash(workspace_storage_dir: &Path, hash: &str) -> Option<PathBuf> {
    let workspace_json = workspace_storage_dir.join(hash).join("workspace.json");
    let contents = std::fs::read_to_string(workspace_json).ok()?;
    let value: Value = serde_json::from_str(&contents).ok()?;
    folder_field_to_path(value.get("folder").and_then(Value::as_str)?)
}

fn folder_field_to_path(folder: &str) -> Option<PathBuf> {
    let stripped = folder
        .strip_prefix("file://")
        .or_else(|| folder.strip_prefix("file:"))
        .unwrap_or(folder);
    let decoded = percent_decode_path(stripped);
    if decoded.as_os_str().is_empty() {
        None
    } else {
        Some(decoded)
    }
}

fn percent_decode_path(value: &str) -> PathBuf {
    let mut out = String::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""),
                16,
            ) {
                out.push(byte as char);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    PathBuf::from(out)
}

fn decode_workspace_sessions_dir(name: &str) -> Option<PathBuf> {
    let trimmed = name.trim_end_matches('_');
    if trimmed.is_empty() {
        return None;
    }
    let mut padded = trimmed.replace('-', "+").replace('_', "/");
    let rem = padded.len() % 4;
    if rem > 0 {
        padded.push_str(&"=".repeat(4 - rem));
    }
    let decoded = base64_decode(&padded)?;
    let path = String::from_utf8(decoded).ok()?;
    let path = path.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf = 0_u32;
    let mut bits = 0_u32;
    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let val = TABLE.iter().position(|&c| c == byte)? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn session_id_from_transcript(path: &Path, value: &Value) -> String {
    string_field(value, &["sessionId", "conversationId", "workflowId", "id"])
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|meta| string_field(meta, &["workflowId", "sessionId"]))
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("unknown")
                .to_string()
        })
}

fn model_from_transcript(value: &Value) -> Option<String> {
    string_field(value, &["modelId", "modelID", "modelName", "model"]).or_else(|| {
        value
            .get("metadata")
            .and_then(|meta| string_field(meta, &["modelId", "modelID"]))
            .map(|model| model.replace('.', "-"))
    })
}

fn messages_from_transcript(
    value: &Value,
    session_id: &str,
    path: &Path,
    model: Option<&str>,
) -> Vec<SessionMessageRecord> {
    if let Some(chat) = value.get("chat").and_then(Value::as_array) {
        return legacy_chat_messages(chat, session_id, path, model, value.get("metadata"));
    }
    for key in [
        "messages",
        "conversation",
        "transcript",
        "entries",
        "events",
    ] {
        if let Some(messages) = value.get(key).and_then(Value::as_array) {
            return modern_messages(messages, session_id, path, model);
        }
    }
    Vec::new()
}

fn legacy_chat_messages(
    chat: &[Value],
    session_id: &str,
    path: &Path,
    model: Option<&str>,
    metadata: Option<&Value>,
) -> Vec<SessionMessageRecord> {
    let base_ts = metadata
        .and_then(|meta| meta.get("startTime"))
        .and_then(parse_timestamp_secs);
    let mut out = Vec::new();
    for (index, entry) in chat.iter().enumerate() {
        let role = match entry.get("role").and_then(Value::as_str) {
            Some("human" | "user") => "user",
            Some("bot" | "assistant" | "model") => "assistant",
            _ => continue,
        };
        let content = entry.get("content").unwrap_or(entry);
        let (text, tool_names) = content_storage_text_and_tools(content, entry.get("tool_calls"));
        if text.trim().is_empty() {
            continue;
        }
        out.push(SessionMessageRecord {
            provider: PROVIDER.to_string(),
            message_id: format!("{session_id}:{index}"),
            session_id: session_id.to_string(),
            role: role.to_string(),
            timestamp: base_ts.map(|ts| ts + index as i64),
            ordinal: index as i64,
            text,
            kind: Some("message".to_string()),
            model: model.map(str::to_string),
            tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
            source_path: Some(path.to_string_lossy().to_string()),
            source_offset: Some(index as i64),
            metadata_json: serde_json::to_string(&message_metadata(entry)).ok(),
        });
    }
    out
}

fn modern_messages(
    messages: &[Value],
    session_id: &str,
    path: &Path,
    model: Option<&str>,
) -> Vec<SessionMessageRecord> {
    let mut out = Vec::new();
    for (index, entry) in messages.iter().enumerate() {
        let Some(role) = normalized_role(entry) else {
            continue;
        };
        let content = entry
            .get("content")
            .or_else(|| entry.get("text"))
            .or_else(|| entry.get("message"))
            .unwrap_or(entry);
        let (text, tool_names) = content_storage_text_and_tools(content, entry.get("tool_calls"));
        if text.trim().is_empty() {
            continue;
        }
        let timestamp = entry
            .get("timestamp")
            .or_else(|| entry.get("createdAt"))
            .or_else(|| entry.get("startTime"))
            .and_then(parse_timestamp_secs);
        out.push(SessionMessageRecord {
            provider: PROVIDER.to_string(),
            message_id: format!("{session_id}:{index}"),
            session_id: session_id.to_string(),
            role: role.to_string(),
            timestamp,
            ordinal: index as i64,
            text,
            kind: Some("message".to_string()),
            model: model.map(str::to_string),
            tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
            source_path: Some(path.to_string_lossy().to_string()),
            source_offset: Some(index as i64),
            metadata_json: serde_json::to_string(&message_metadata(entry)).ok(),
        });
    }
    out
}

fn normalized_role(entry: &Value) -> Option<&'static str> {
    let role = entry
        .get("role")
        .or_else(|| entry.get("type"))
        .or_else(|| entry.get("author"))
        .and_then(Value::as_str)?
        .to_ascii_lowercase();
    match role.as_str() {
        "human" | "user" => Some("user"),
        "bot" | "assistant" | "model" | "ai" => Some("assistant"),
        _ => None,
    }
}

fn parse_timestamp_secs(value: &Value) -> Option<i64> {
    if let Some(ts) = value.as_i64() {
        return Some(if ts >= 1_000_000_000_000 {
            ts / 1000
        } else {
            ts
        });
    }
    value
        .as_str()
        .and_then(crate::accounting::parser::parse_timestamp)
        .map(|secs| secs as i64)
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn message_metadata(entry: &Value) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("kiro_transcript".to_string()),
    );
    append_tool_calls_metadata(&mut metadata, entry);
    append_usage_metadata(&mut metadata, &[entry]);
    Value::Object(metadata)
}
