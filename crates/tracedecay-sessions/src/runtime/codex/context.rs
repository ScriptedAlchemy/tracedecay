use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{CodexMeta, session_meta_from_record, turn_context_from_record};
use crate::SessionMessageRecord;
use crate::runtime::shared::{
    TranscriptLocation, TranscriptLocationMetadataKeys, append_location_metadata,
};

const CODEX_SESSION_LOCATION_KEYS: TranscriptLocationMetadataKeys =
    TranscriptLocationMetadataKeys::new(
        "codex_session_cwd",
        "codex_session_worktree",
        "codex_session_location_provenance",
    );
const CODEX_TURN_LOCATION_KEYS: TranscriptLocationMetadataKeys =
    TranscriptLocationMetadataKeys::new(
        "codex_turn_cwd",
        "codex_turn_worktree",
        "codex_turn_location_provenance",
    );

pub(super) struct CodexContextState {
    pub(super) model: Option<String>,
    pub(super) cwd: Option<PathBuf>,
    pub(super) git: Option<Value>,
    pub(super) compaction_depth: i64,
}

impl CodexContextState {
    pub(super) fn from_meta(meta: &CodexMeta) -> Self {
        Self {
            model: meta.model.clone(),
            cwd: Some(meta.cwd.clone()),
            git: meta.git.clone(),
            compaction_depth: 0,
        }
    }

    pub(super) fn scan_prior(path: &Path, before_offset: u64, meta: &CodexMeta) -> Self {
        let mut state = Self::from_meta(meta);
        if before_offset == 0 {
            return state;
        }
        let Ok(file) = std::fs::File::open(path) else {
            return state;
        };
        let mut reader = std::io::BufReader::new(file);
        let mut line = String::new();
        let mut offset = 0_u64;
        loop {
            line.clear();
            let Ok(n) = reader.read_line(&mut line) else {
                break;
            };
            if n == 0 || offset >= before_offset {
                break;
            }
            let line_offset = offset;
            offset = offset.saturating_add(n as u64);
            if line_offset >= before_offset {
                break;
            }
            let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            state.observe_prior_record(&value, path, meta);
        }
        state
    }

    pub(super) fn observe_context_record(
        &mut self,
        record: &Value,
        path: &Path,
        meta: &CodexMeta,
    ) -> bool {
        if let Some(updated) = session_meta_from_record(record, path) {
            if updated.session_id == meta.session_id {
                self.cwd = Some(updated.cwd);
                if updated.git.is_some() {
                    self.git = updated.git;
                }
                if updated.model.is_some() {
                    self.model = updated.model;
                }
            }
            return true;
        }
        if let Some(context) = turn_context_from_record(record) {
            if context.model.is_some() {
                self.model = context.model;
            }
            if context.cwd.is_some() {
                self.cwd = context.cwd;
            }
            return true;
        }
        false
    }

    fn observe_prior_record(&mut self, record: &Value, path: &Path, meta: &CodexMeta) {
        if record.get("type").and_then(Value::as_str) == Some("compacted") {
            self.compaction_depth += 1;
            return;
        }
        self.observe_context_record(record, path, meta);
    }
}

pub(super) fn session_metadata_json(
    meta: &CodexMeta,
    summary: Option<&super::events::CodexSessionSummary>,
) -> Option<String> {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_rollout".to_string()),
    );
    if let Some(thread_source) = &meta.thread_source {
        metadata.insert(
            "thread_source".to_string(),
            Value::String(thread_source.clone()),
        );
    }
    if let Some(agent_role) = &meta.agent_role {
        metadata.insert("agent_role".to_string(), Value::String(agent_role.clone()));
    }
    if let Some(agent_nickname) = &meta.agent_nickname {
        metadata.insert(
            "agent_nickname".to_string(),
            Value::String(agent_nickname.clone()),
        );
    }
    append_location_metadata(
        &mut metadata,
        CODEX_SESSION_LOCATION_KEYS,
        TranscriptLocation::new(Some(&meta.cwd), "session_meta"),
    );
    insert_git_metadata(&mut metadata, meta.git.as_ref());
    if let Some(summary) = summary {
        summary.apply(&mut metadata);
    }
    serde_json::to_string(&Value::Object(metadata)).ok()
}

pub(super) fn annotate_message(
    message: &mut SessionMessageRecord,
    cwd: Option<&Path>,
    git: Option<&Value>,
) {
    let mut metadata = message
        .metadata_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();

    append_location_metadata(
        &mut metadata,
        CODEX_TURN_LOCATION_KEYS,
        TranscriptLocation::new(cwd, "codex_context"),
    );
    insert_git_metadata(&mut metadata, git);
    message.metadata_json = serde_json::to_string(&Value::Object(metadata)).ok();
}

fn insert_git_metadata(metadata: &mut serde_json::Map<String, Value>, git: Option<&Value>) {
    let Some(git) = git.and_then(Value::as_object) else {
        return;
    };
    if let Some(branch) = git.get("branch").and_then(Value::as_str) {
        metadata.insert(
            "codex_git_branch".to_string(),
            Value::String(branch.to_string()),
        );
    }
    if let Some(commit) = git.get("commit_hash").and_then(Value::as_str) {
        metadata.insert(
            "codex_git_commit_hash".to_string(),
            Value::String(commit.to_string()),
        );
    }
    if let Some(remote) = git.get("repository_url").and_then(Value::as_str) {
        metadata.insert(
            "codex_git_repository_url".to_string(),
            Value::String(remote.to_string()),
        );
    }
}
