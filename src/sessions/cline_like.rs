//! Cline/Roo Code/Kilo Code task-history transcript sources.
//!
//! These VS Code extension-family adapters persist each task in a directory with
//! JSON files such as:
//!
//! * `api_conversation_history.json` (or Roo's `api_messages.json`) - the
//!   Anthropic-compatible conversation sent to/received from the model.
//! * `ui_messages.json` - webview-oriented messages.
//! * `task_metadata.json` / `history_item.json` - task metadata.
//!
//! The API conversation file is a **full-rewrite** JSON array, so the source uses
//! the shared `ContentHash` reader and deterministic `<task-id>:<index>` message
//! ids. To avoid mixing global VS Code extension history across projects, a task
//! is ingested only when its metadata contains a project/workspace/cwd path that
//! resolves to the current tokensave project root.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::sessions::source::{
    paths_equal, read_changed_file, title_from_messages, ParsedTranscript, SessionDraft,
    StoredCursor, TranscriptSource,
};
use crate::sessions::SessionMessageRecord;

/// One Cline-family provider configuration.
#[derive(Clone)]
pub struct ClineLikeSource {
    provider: &'static str,
    storage_roots: Vec<PathBuf>,
}

impl ClineLikeSource {
    /// Cline VS Code extension storage:
    /// `Code/User/globalStorage/saoudrizwan.claude-dev/tasks`.
    pub fn cline() -> Option<Self> {
        let home = dirs::home_dir()?;
        Some(Self::cline_with_home(&home))
    }

    /// Roo Code VS Code extension storage:
    /// `Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks`.
    pub fn roo_code() -> Option<Self> {
        let home = dirs::home_dir()?;
        Some(Self::roo_code_with_home(&home))
    }

    /// Kilo Code storage. Current docs mention both the VS Code extension root
    /// and the CLI root (`~/.kilocode/cli/global/tasks`), so scan both.
    pub fn kilo() -> Option<Self> {
        let home = dirs::home_dir()?;
        Some(Self::kilo_with_home(&home))
    }

    pub fn cline_with_home(home: &Path) -> Self {
        Self {
            provider: "cline",
            storage_roots: vec![crate::agents::vscode_data_dir(home)
                .join("User/globalStorage/saoudrizwan.claude-dev/tasks")],
        }
    }

    pub fn roo_code_with_home(home: &Path) -> Self {
        Self {
            provider: "roo-code",
            storage_roots: vec![crate::agents::vscode_data_dir(home)
                .join("User/globalStorage/rooveterinaryinc.roo-cline/tasks")],
        }
    }

    pub fn kilo_with_home(home: &Path) -> Self {
        Self {
            provider: "kilo",
            storage_roots: vec![
                crate::agents::vscode_data_dir(home)
                    .join("User/globalStorage/kilocode.kilo-code/tasks"),
                home.join(".kilocode/cli/global/tasks"),
            ],
        }
    }
}

impl TranscriptSource for ClineLikeSource {
    fn provider(&self) -> &'static str {
        self.provider
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in &self.storage_roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let task_dir = entry.path();
                if !task_dir.is_dir() {
                    continue;
                }
                for name in ["api_conversation_history.json", "api_messages.json"] {
                    let path = task_dir.join(name);
                    if path.is_file() {
                        out.push(path);
                    }
                }
            }
        }
        out
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        _max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let task_dir = path.parent()?;
        let metadata = read_task_metadata(task_dir)?;
        if !metadata_belongs_to_project(&metadata, project_root) {
            return None;
        }

        let changed = read_changed_file(path, prev)?;
        let document: Value = serde_json::from_str(&changed.contents).ok()?;
        let entries = document.as_array()?;
        let task_id = task_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");

        let mut messages = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            if let Some(message) = message_from_entry(self.provider, entry, task_id, path, index) {
                messages.push(message);
            }
        }

        let project = project_root.to_string_lossy().to_string();
        let draft = SessionDraft {
            session_id: task_id.to_string(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages)
                .or_else(|| metadata_task_title(&metadata).map(str::to_string)),
            metadata_json: serde_json::to_string(&serde_json::json!({
                "source": format!("{}_task_history", self.provider),
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

fn read_task_metadata(task_dir: &Path) -> Option<Value> {
    for name in ["task_metadata.json", "history_item.json", "history.json"] {
        let path = task_dir.join(name);
        if !path.is_file() {
            continue;
        }
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<Value>(&contents) {
                return Some(value);
            }
        }
    }
    None
}

fn metadata_belongs_to_project(metadata: &Value, project_root: &Path) -> bool {
    metadata_project_paths(metadata)
        .iter()
        .any(|path| paths_equal(path, project_root))
}

fn metadata_project_paths(value: &Value) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_metadata_project_paths(value, None, &mut out);
    out
}

fn collect_metadata_project_paths(value: &Value, key: Option<&str>, out: &mut Vec<PathBuf>) {
    match value {
        Value::Object(map) => {
            for (child_key, child_value) in map {
                collect_metadata_project_paths(child_value, Some(child_key), out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_metadata_project_paths(item, key, out);
            }
        }
        Value::String(s) => {
            let key = key.unwrap_or_default().to_ascii_lowercase();
            let looks_like_project_path = key.contains("workspace")
                || key.contains("project")
                || key.contains("cwd")
                || key.contains("workdir")
                || key.contains("directory")
                || key == "root";
            if looks_like_project_path && !s.is_empty() {
                out.push(PathBuf::from(s));
            }
        }
        _ => {}
    }
}

fn metadata_task_title(metadata: &Value) -> Option<&str> {
    metadata
        .get("task")
        .or_else(|| metadata.get("title"))
        .or_else(|| metadata.get("summary"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn message_from_entry(
    provider: &str,
    entry: &Value,
    task_id: &str,
    path: &Path,
    index: usize,
) -> Option<SessionMessageRecord> {
    let role = match entry.get("role").and_then(Value::as_str)? {
        "user" => "user",
        "assistant" | "model" => "assistant",
        _ => return None,
    };
    let content = entry.get("content").unwrap_or(entry);
    let (text, tool_names) = content_text_and_tools(content);
    if text.trim().is_empty() {
        return None;
    }
    let timestamp = entry
        .get("ts")
        .or_else(|| entry.get("timestamp"))
        .or_else(|| entry.get("createdAt"))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
        });
    let model = entry
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let message_id = entry
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(|| format!("{task_id}:{index}"), ToString::to_string);

    Some(SessionMessageRecord {
        provider: provider.to_string(),
        message_id,
        session_id: task_id.to_string(),
        role: role.to_string(),
        timestamp,
        ordinal: index as i64,
        text,
        kind: Some("message".to_string()),
        model,
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(index as i64),
        metadata_json: serde_json::to_string(&serde_json::json!({
            "source": format!("{provider}_task_history"),
        }))
        .ok(),
    })
}

fn content_text_and_tools(content: &Value) -> (String, Vec<String>) {
    if let Some(text) = content.as_str() {
        return (text.to_string(), Vec::new());
    }
    let Some(items) = content.as_array() else {
        return (
            content
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Vec::new(),
        );
    };

    let mut texts = Vec::new();
    let mut tools = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") | None => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    texts.push(text.to_string());
                }
            }
            Some("tool_use") => {
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    tools.push(name.to_string());
                }
            }
            Some("tool_result") => {
                if let Some(text) = item.get("content").and_then(Value::as_str) {
                    texts.push(text.to_string());
                }
            }
            _ => {}
        }
    }
    (texts.join("\n\n"), tools)
}
