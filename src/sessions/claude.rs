//! Claude Code transcript source.
//!
//! Claude Code appends one JSON object per line to
//! `~/.claude/projects/<slug>/<session-uuid>.jsonl` (with subagent transcripts
//! under `…/<session>/subagents/*.jsonl`). Each line carries a top-level `type`
//! (`"user"`/`"assistant"`/…), a `message` object (`role`, `content`, `model`,
//! `id`), an ISO-8601 `timestamp`, the session `cwd`, and `sessionId`/`uuid`.
//!
//! The accounting parser already reads these files for cost `turns`; this source
//! reuses the **same** append-only byte-offset machinery to also populate the
//! provider-neutral `session_messages` table. Files are scoped to the current
//! project by their recorded `cwd`, so a project only ingests its own sessions.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::accounting::parser::parse_timestamp;
use crate::sessions::source::{
    collect_files_with_ext, paths_equal, stream_new_jsonl, title_from_messages, ParsedTranscript,
    SessionDraft, StoredCursor, TranscriptSource,
};
use crate::sessions::SessionMessageRecord;

const PROVIDER: &str = "claude";
/// `~/.claude/projects/<slug>/<…>.jsonl` is at most a few levels deep.
const MAX_SCAN_DEPTH: u8 = 6;
/// `cwd` should appear on an early line; scan a few in case the first is a
/// `summary`/meta line without one.
const CWD_PROBE_LINES: usize = 8;

/// Claude Code transcript locator + parser.
pub struct ClaudeSource {
    projects_dir: PathBuf,
}

impl ClaudeSource {
    /// Source rooted at the real `~/.claude/projects`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = dirs::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.claude/projects` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        Self {
            projects_dir: home.join(".claude").join("projects"),
        }
    }
}

impl TranscriptSource for ClaudeSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        // Scan every project slug; `parse_new` filters by recorded `cwd` so each
        // project only ingests its own sessions without us having to replicate
        // Claude's slug-encoding scheme.
        collect_files_with_ext(&self.projects_dir, "jsonl", MAX_SCAN_DEPTH)
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let subagent = claude_subagent_identity(path);
        // Cheap project scoping: a transcript belongs to exactly one cwd. Claude
        // subagent files in the verified nested layout may omit cwd, so fall
        // back to the parent transcript's cwd when the path proves parentage.
        match transcript_cwd(path).or_else(|| {
            subagent
                .as_ref()
                .and_then(|info| transcript_cwd(&info.parent_transcript_path))
        }) {
            Some(cwd) if paths_equal(&cwd, project_root) => {}
            _ => return None,
        }

        let new = stream_new_jsonl(path, prev, max_new_bytes)?;
        let session_id = subagent.as_ref().map_or_else(
            || {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            },
            |info| info.session_id.clone(),
        );

        let mut messages = Vec::new();
        for line in &new.lines {
            if let Some(message) = message_from_line(&line.value, &session_id, path, line.offset) {
                messages.push(message);
            }
        }

        let project = project_root.to_string_lossy().to_string();
        let draft = SessionDraft {
            session_id,
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&serde_json::json!({
                "source": "claude_transcript",
            }))
            .ok(),
            parent_session_id: subagent.as_ref().map(|info| info.parent_session_id.clone()),
            is_subagent: subagent.is_some(),
            agent_id: subagent.as_ref().map(|info| info.agent_id.clone()),
            parent_tool_use_id: None,
        };

        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: new.new_cursor,
        })
    }
}

struct ClaudeSubagentInfo {
    session_id: String,
    parent_session_id: String,
    agent_id: String,
    parent_transcript_path: PathBuf,
}

fn claude_subagent_identity(path: &Path) -> Option<ClaudeSubagentInfo> {
    if path.parent()?.file_name().and_then(|name| name.to_str()) != Some("subagents") {
        return None;
    }
    let parent_dir = path.parent()?.parent()?;
    let parent_session_id = parent_dir.file_name()?.to_str()?.to_string();
    let session_id = path.file_stem()?.to_str()?.to_string();
    let agent_id = session_id
        .strip_prefix("agent-")
        .unwrap_or(&session_id)
        .to_string();
    Some(ClaudeSubagentInfo {
        session_id,
        parent_session_id: parent_session_id.clone(),
        agent_id,
        parent_transcript_path: parent_dir.with_file_name(format!("{parent_session_id}.jsonl")),
    })
}

/// Reads the session `cwd` from an early line of a Claude transcript.
fn transcript_cwd(path: &Path) -> Option<PathBuf> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines().take(CWD_PROBE_LINES).map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
                if !cwd.is_empty() {
                    return Some(PathBuf::from(cwd));
                }
            }
        }
    }
    None
}

/// Map one Claude transcript line to a provider-neutral message, or `None` for
/// lines that carry no conversational text (tool-result-only, meta lines, …).
fn message_from_line(
    record: &Value,
    session_id: &str,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let kind = record.get("type").and_then(Value::as_str)?;
    if kind != "user" && kind != "assistant" {
        return None;
    }
    let message = record.get("message").unwrap_or(record);
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or(kind)
        .to_string();

    let content = message.get("content").unwrap_or(message);
    let (text, tool_names) = content_text_and_tools(content);
    if text.trim().is_empty() {
        return None;
    }

    let message_id = message
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| record.get("uuid").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .map_or_else(|| format!("{session_id}:{offset}"), ToString::to_string);
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let timestamp = record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64);

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id,
        session_id: session_id.to_string(),
        role,
        timestamp,
        ordinal: offset,
        text,
        kind: Some("message".to_string()),
        model,
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&serde_json::json!({
            "source": "claude_transcript",
            "raw_type": kind,
        }))
        .ok(),
    })
}

/// Extract the concatenated text and tool-use names from a Claude `content`
/// field, which is either a plain string (user turns) or an array of typed
/// blocks (`text`, `tool_use`, `tool_result`, …) for assistant turns.
fn content_text_and_tools(content: &Value) -> (String, Vec<String>) {
    if let Some(text) = content.as_str() {
        return (text.to_string(), Vec::new());
    }
    let Some(items) = content.as_array() else {
        return (String::new(), Vec::new());
    };

    let mut texts = Vec::new();
    let mut tools = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    texts.push(text.to_string());
                }
            }
            Some("tool_use") => {
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    tools.push(name.to_string());
                }
            }
            _ => {}
        }
    }
    (texts.join("\n\n"), tools)
}
