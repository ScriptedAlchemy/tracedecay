//! Cline/Roo Code/Kilo Code task-history transcript sources.
//!
//! These VS Code extension-family adapters persist each task in a directory with
//! JSON files such as:
//!
//! * `api_conversation_history.json` (or Roo's `api_messages.json`) - the
//!   Anthropic-compatible conversation sent to/received from the model.
//! * `ui_messages.json` - webview-oriented messages; `say`/`api_req_started`
//!   events carry token counters in the `text` JSON payload.
//! * `task_metadata.json` / `history_item.json` - task metadata.
//!
//! The API conversation file is a **full-rewrite** JSON array, so the source uses
//! the shared `ContentHash` reader and deterministic `<task-id>:<index>` message
//! ids. To avoid mixing global VS Code extension history across projects, a task
//! is ingested only when its metadata contains a project/workspace/cwd path that
//! resolves to the current tracedecay project root.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value};

use crate::sessions::source::{
    append_tool_calls_metadata, append_usage_metadata, content_storage_text_and_tools, paths_equal,
    read_changed_with_companion, title_from_messages, ParsedTranscript, SessionDraft, StoredCursor,
    TranscriptSource,
};
use crate::sessions::SessionMessageRecord;

/// Cap task-directory scans so a long VS Code globalStorage history cannot
/// block dashboard startup.
const MAX_TASK_DIRS_PER_ROOT: usize = 512;

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
            out.extend(collect_task_api_paths(root));
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

        let ui_path = task_dir.join("ui_messages.json");
        let changed = read_changed_with_companion(path, &ui_path, prev)?;
        let document: Value = serde_json::from_str(&changed.contents).ok()?;
        let entries = document.as_array()?;
        let task_id = task_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        let usage_by_assistant = usage_counters_by_assistant_index(&ui_path);

        let mut messages = Vec::new();
        let mut assistant_index = 0_usize;
        for (index, entry) in entries.iter().enumerate() {
            let usage = if entry.get("role").and_then(Value::as_str) == Some("assistant")
                || entry.get("role").and_then(Value::as_str) == Some("model")
            {
                let usage = usage_by_assistant.get(assistant_index).cloned();
                assistant_index += 1;
                usage
            } else {
                None
            };
            if let Some(message) =
                message_from_entry(self.provider, entry, task_id, path, index, usage.as_ref())
            {
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

fn collect_task_api_paths(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut task_dirs: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs());
            Some((mtime, path))
        })
        .collect();
    task_dirs.sort_by_key(|b| std::cmp::Reverse(b.0));
    task_dirs.truncate(MAX_TASK_DIRS_PER_ROOT);

    let mut out = Vec::new();
    for (_, task_dir) in task_dirs {
        for name in ["api_conversation_history.json", "api_messages.json"] {
            let path = task_dir.join(name);
            if path.is_file() {
                out.push(path);
            }
        }
    }
    out
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

/// Ordered usage counters extracted from `ui_messages.json` `api_req_started`
/// events — one entry per assistant turn, in file order.
fn usage_counters_by_assistant_index(ui_path: &Path) -> Vec<Value> {
    let Ok(contents) = std::fs::read_to_string(ui_path) else {
        return Vec::new();
    };
    let Ok(events) = serde_json::from_str::<Value>(&contents) else {
        return Vec::new();
    };
    let Some(events) = events.as_array() else {
        return Vec::new();
    };

    events
        .iter()
        .filter_map(|event| {
            if event.get("type").and_then(Value::as_str) != Some("say") {
                return None;
            }
            if event.get("say").and_then(Value::as_str) != Some("api_req_started") {
                return None;
            }
            let text = event.get("text").and_then(Value::as_str)?;
            usage_from_api_req_started(text)
        })
        .collect()
}

fn usage_from_api_req_started(text: &str) -> Option<Value> {
    let payload: Value = serde_json::from_str(text).ok()?;
    let mut counters = Map::new();
    map_counter(
        &mut counters,
        "input_tokens",
        &payload,
        &["tokensIn", "tokens_in"],
    );
    map_counter(
        &mut counters,
        "output_tokens",
        &payload,
        &["tokensOut", "tokens_out"],
    );
    map_counter(
        &mut counters,
        "cache_read_input_tokens",
        &payload,
        &["cacheReads", "cache_reads"],
    );
    map_counter(
        &mut counters,
        "cache_creation_input_tokens",
        &payload,
        &["cacheWrites", "cache_writes"],
    );
    if let Some(total) = payload
        .get("totalTokens")
        .or_else(|| payload.get("total_tokens"))
        .and_then(Value::as_i64)
    {
        counters.insert("total_tokens".to_string(), Value::from(total));
    }
    (!counters.is_empty()).then_some(Value::Object(counters))
}

fn map_counter(
    counters: &mut Map<String, Value>,
    target_key: &str,
    payload: &Value,
    source_keys: &[&str],
) {
    for key in source_keys {
        if let Some(count) = payload.get(*key).and_then(Value::as_i64) {
            counters.insert(target_key.to_string(), Value::from(count));
            return;
        }
    }
}

fn message_from_entry(
    provider: &str,
    entry: &Value,
    task_id: &str,
    path: &Path,
    index: usize,
    ui_usage: Option<&Value>,
) -> Option<SessionMessageRecord> {
    let role = match entry.get("role").and_then(Value::as_str)? {
        "user" => "user",
        "assistant" | "model" => "assistant",
        _ => return None,
    };
    let content = entry.get("content").unwrap_or(entry);
    let (text, tool_names) = content_storage_text_and_tools(content, entry.get("tool_calls"));
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
        metadata_json: serde_json::to_string(&message_metadata(provider, entry, ui_usage)).ok(),
    })
}

fn message_metadata(provider: &str, entry: &Value, ui_usage: Option<&Value>) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String(format!("{provider}_task_history")),
    );
    append_tool_calls_metadata(&mut metadata, entry);
    if let Some(usage) = ui_usage {
        metadata.insert("usage".to_string(), usage.clone());
    } else {
        append_usage_metadata(&mut metadata, &[entry]);
    }
    Value::Object(metadata)
}
