//! Mistral Vibe transcript source.
//!
//! Vibe stores sessions under `$VIBE_HOME/logs/session/` or
//! `~/.vibe/logs/session/`. Each session directory contains:
//!
//! * `meta.json` - cumulative metadata, including session id, active model, and
//!   the working directory (`environment.working_directory` in current releases).
//! * `messages.jsonl` - append-only line-delimited LLM messages.
//!
//! This source uses the shared **`ByteOffset`** reader for `messages.jsonl` and
//! scopes sessions to a tracedecay project by matching the working directory in
//! `meta.json` to `project_root`.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::SessionMessageRecord;
use crate::runtime::shared::{
    ProjectMembership, ProjectRootMatcherCache, StoredCursor, TranscriptLocation,
    TranscriptLocationMetadataKeys, append_location_metadata, append_tool_calls_metadata,
    append_usage_metadata, content_storage_text_and_tools, title_from_messages,
};
use crate::runtime::source::{
    ParsedTranscript, SessionDraft, TranscriptSource, collect_files_with_ext, stream_new_jsonl,
};

const PROVIDER: &str = "vibe";
const MAX_SCAN_DEPTH: u8 = 4;
/// Bound global history enumeration so one large Vibe profile cannot stall ingest.
const MAX_SESSION_FILES: usize = 512;
const VIBE_LOCATION_KEYS: TranscriptLocationMetadataKeys = TranscriptLocationMetadataKeys::new(
    "vibe_session_cwd",
    "vibe_session_worktree",
    "vibe_session_location_provenance",
);

/// Vibe session locator + parser.
pub struct VibeSource {
    session_root: PathBuf,
    user_registered_roots: Option<Vec<PathBuf>>,
    project_matchers: ProjectRootMatcherCache,
}

impl VibeSource {
    /// Source rooted at the real Vibe home. Returns `None` when the home
    /// directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = super::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.vibe/logs/session` (used by tests). This does
    /// not read `VIBE_HOME`; tests can pass the desired base explicitly.
    pub fn with_home(home: &Path) -> Self {
        Self::with_vibe_home(&home.join(".vibe"))
    }

    /// Source rooted at `<vibe_home>/logs/session`.
    pub fn with_vibe_home(vibe_home: &Path) -> Self {
        Self {
            session_root: vibe_home.join("logs").join("session"),
            user_registered_roots: None,
            project_matchers: ProjectRootMatcherCache::default(),
        }
    }

    #[must_use]
    pub fn for_user_scope(mut self, registered_roots: Vec<PathBuf>) -> Self {
        self.user_registered_roots = Some(registered_roots);
        self
    }
}

impl TranscriptSource for VibeSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        let mut paths = collect_files_with_ext(&self.session_root, "jsonl", MAX_SCAN_DEPTH)
            .into_iter()
            .filter(|path| {
                path.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
            })
            .map(|path| {
                let mtime = std::fs::metadata(&path)
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |duration| duration.as_secs());
                (mtime, path)
            })
            .collect::<Vec<_>>();
        paths.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
        paths.truncate(MAX_SESSION_FILES);
        paths.into_iter().map(|(_, path)| path).collect()
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let meta_path = path.parent()?.join("meta.json");
        let meta = read_meta(&meta_path)?;
        if let Some(roots) = &self.user_registered_roots {
            if self
                .project_matchers
                .membership_against_roots(&meta.working_directory, roots)
                != ProjectMembership::NoMatch
            {
                return None;
            }
        } else if self
            .project_matchers
            .membership(&meta.working_directory, project_root)
            != ProjectMembership::Match
        {
            return None;
        }

        let new = stream_new_jsonl(path, prev, max_new_bytes)?;
        let mut messages = Vec::new();
        for line in &new.lines {
            if let Some(message) = message_from_line(&line.value, &meta, path, line.offset) {
                messages.push(message);
            }
        }

        let project = self.user_registered_roots.as_ref().map_or_else(
            || project_root.to_string_lossy().to_string(),
            |_| "user".to_string(),
        );
        let draft = SessionDraft {
            session_id: meta.session_id.clone(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&session_metadata(&meta)).ok(),
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };

        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: new.new_cursor,
        })
    }
}

struct VibeMeta {
    session_id: String,
    working_directory: PathBuf,
    model: Option<String>,
}

fn read_meta(path: &Path) -> Option<VibeMeta> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let session_id = value
        .get("session_id")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || {
                path.parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            },
            ToString::to_string,
        );
    let working_directory = value
        .pointer("/environment/working_directory")
        .or_else(|| value.pointer("/environment/workdir"))
        .or_else(|| value.pointer("/config/working_directory"))
        .or_else(|| value.pointer("/config/workdir"))
        .or_else(|| value.get("working_directory"))
        .or_else(|| value.get("cwd"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)?;
    let model = value
        .pointer("/config/active_model")
        .or_else(|| value.get("active_model"))
        .or_else(|| value.get("model"))
        .and_then(Value::as_str)
        .map(str::to_string);

    Some(VibeMeta {
        session_id,
        working_directory,
        model,
    })
}

fn message_from_line(
    record: &Value,
    meta: &VibeMeta,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    let role = record
        .get("role")
        .or_else(|| record.pointer("/message/role"))
        .and_then(Value::as_str)
        .filter(|role| matches!(*role, "user" | "assistant" | "model"))?;
    let normalized_role = if role == "model" { "assistant" } else { role };
    let content = record
        .get("content")
        .or_else(|| record.pointer("/message/content"))
        .unwrap_or(record);
    let (text, tool_names) = content_storage_text_and_tools(
        content,
        record
            .get("tool_calls")
            .or_else(|| record.pointer("/message/tool_calls")),
    );
    if text.trim().is_empty() {
        return None;
    }
    let timestamp = record
        .get("timestamp")
        .or_else(|| record.get("created_at"))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
        });

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: normalized_role.to_string(),
        timestamp,
        ordinal: offset,
        text,
        kind: Some("message".to_string()),
        model: meta.model.clone(),
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&message_metadata(record, meta)).ok(),
    })
}

fn session_metadata(meta: &VibeMeta) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("vibe_messages".to_string()),
    );
    append_location_metadata(
        &mut metadata,
        VIBE_LOCATION_KEYS,
        TranscriptLocation::new(Some(&meta.working_directory), "session_meta"),
    );
    Value::Object(metadata)
}

fn message_metadata(record: &Value, meta: &VibeMeta) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("vibe_messages".to_string()),
    );
    append_location_metadata(
        &mut metadata,
        VIBE_LOCATION_KEYS,
        TranscriptLocation::new(Some(&meta.working_directory), "session_meta"),
    );
    append_tool_calls_metadata(&mut metadata, record);
    if let Some(message) = record.get("message") {
        append_tool_calls_metadata(&mut metadata, message);
        append_usage_metadata(&mut metadata, &[record, message]);
    } else {
        append_usage_metadata(&mut metadata, &[record]);
    }
    Value::Object(metadata)
}
