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
use crate::sessions::shared::{
    append_location_metadata, append_tool_calls_metadata, append_usage_metadata,
    content_storage_text_and_tools, path_belongs_to_project, title_from_messages, StoredCursor,
    TranscriptLocation, TranscriptLocationMetadataKeys,
};
use crate::sessions::source::{
    collect_files_with_ext, stream_new_jsonl, ParsedTranscript, SessionDraft, TranscriptSource,
};
use crate::sessions::SessionMessageRecord;

const PROVIDER: &str = "claude";
const CLAUDE_SESSION_LOCATION_KEYS: TranscriptLocationMetadataKeys =
    TranscriptLocationMetadataKeys::new(
        "claude_session_cwd",
        "claude_session_worktree",
        "claude_session_location_provenance",
    );
const CLAUDE_MESSAGE_LOCATION_KEYS: TranscriptLocationMetadataKeys =
    TranscriptLocationMetadataKeys::new(
        "claude_message_cwd",
        "claude_message_worktree",
        "claude_message_location_provenance",
    );
/// `~/.claude/projects/<slug>/<…>.jsonl` is at most a few levels deep.
const MAX_SCAN_DEPTH: u8 = 6;
/// `cwd` should appear on an early line; scan a few in case the first is a
/// `summary`/meta line without one.
pub(crate) const CWD_PROBE_LINES: usize = 8;

/// Claude Code transcript locator + parser.
pub struct ClaudeSource {
    projects_dir: PathBuf,
}

impl ClaudeSource {
    /// Source rooted at the real `~/.claude/projects`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::sessions::home_dir()?;
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
        // Cheap session scoping: the first/parent cwd describes where the
        // session began, but individual Claude rows can carry their own cwd.
        // Filter messages per row so sessions that cross worktrees are split
        // into the right project stores without losing transcript truth.
        let session_cwd = transcript_cwd(path).or_else(|| {
            subagent
                .as_ref()
                .and_then(|info| transcript_cwd(&info.parent_transcript_path))
        });

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
            let line_cwd = record_cwd(&line.value).or_else(|| session_cwd.clone());
            if !line_cwd
                .as_deref()
                .is_some_and(|cwd| path_belongs_to_project(cwd, project_root))
            {
                continue;
            }
            if let Some(message) = message_from_line(
                &line.value,
                &session_id,
                path,
                line.offset,
                session_cwd.as_deref(),
            ) {
                messages.push(message);
            }
        }
        // No early return when `messages` is empty: this source scans every
        // ~/.claude/projects slug and relies on the per-row cwd filter above,
        // so transcripts belonging to other projects legitimately parse to
        // zero messages. Returning the (empty) transcript lets `ingest_one`
        // persist the advanced cursor; returning `None` would pin the cursor
        // at 0 and re-read + re-filter the whole file on every sweep.

        let project = project_root.to_string_lossy().to_string();
        let draft = SessionDraft {
            session_id,
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&session_metadata(session_cwd.as_deref())).ok(),
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
pub(crate) fn transcript_cwd(path: &Path) -> Option<PathBuf> {
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
    session_cwd: Option<&Path>,
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
    let (text, tool_names) = content_storage_text_and_tools(
        content,
        message
            .get("tool_calls")
            .or_else(|| record.get("tool_calls")),
    );
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
        metadata_json: serde_json::to_string(&message_metadata(kind, record, message, session_cwd))
            .ok(),
    })
}

fn session_metadata(session_cwd: Option<&Path>) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_transcript".to_string()),
    );
    append_location_metadata(
        &mut metadata,
        CLAUDE_SESSION_LOCATION_KEYS,
        TranscriptLocation::new(session_cwd, "transcript_session"),
    );
    Value::Object(metadata)
}

fn message_metadata(
    kind: &str,
    record: &Value,
    message: &Value,
    session_cwd: Option<&Path>,
) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("claude_transcript".to_string()),
    );
    metadata.insert("raw_type".to_string(), Value::String(kind.to_string()));
    let record_cwd = record_cwd(record);
    let (location_cwd, location_provenance) = if record_cwd.is_some() {
        (record_cwd.as_deref(), "transcript_record")
    } else {
        (session_cwd, "transcript_session")
    };
    append_location_metadata(
        &mut metadata,
        CLAUDE_MESSAGE_LOCATION_KEYS,
        TranscriptLocation::new(location_cwd, location_provenance),
    );
    append_tool_calls_metadata(&mut metadata, message);
    // Anthropic-style per-message counters: `message.usage.{input_tokens,
    // output_tokens, cache_creation_input_tokens, cache_read_input_tokens}`.
    append_usage_metadata(&mut metadata, &[message]);
    Value::Object(metadata)
}

fn record_cwd(record: &Value) -> Option<PathBuf> {
    record
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
}
