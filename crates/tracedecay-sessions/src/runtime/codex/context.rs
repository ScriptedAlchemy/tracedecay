use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use super::{CodexMeta, session_meta_from_record, turn_context_from_record};
use crate::runtime::SessionMessageRecord;
use crate::runtime::shared::{
    TranscriptLocation, TranscriptLocationMetadataKeys, append_location_metadata,
};
use crate::runtime::source::{MAX_JSONL_RECORD_BYTES, RawJsonlFrame, RawJsonlFrameReader};

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

#[derive(Clone)]
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
        if before_offset == 0 {
            return Self::from_meta(meta);
        }
        let Ok(mut file) = std::fs::File::open(path) else {
            return Self::from_meta(meta);
        };
        let generation = prior_context_generation(&file);
        let (mut state, mut offset) = match generation
            .and_then(|generation| cached_prior_context(path, generation, before_offset))
        {
            Some(resumed) => resumed,
            None => (Self::from_meta(meta), 0),
        };
        if offset > 0 && file.seek(SeekFrom::Start(offset)).is_err() {
            state = Self::from_meta(meta);
            offset = 0;
            if file.seek(SeekFrom::Start(0)).is_err() {
                return state;
            }
        }
        let mut frames = RawJsonlFrameReader::new(BufReader::new(file), MAX_JSONL_RECORD_BYTES);
        while let Ok(frame) = frames.next_frame() {
            if matches!(frame, RawJsonlFrame::Eof) || offset >= before_offset {
                break;
            }
            let line_offset = offset;
            let byte_len = match frame {
                RawJsonlFrame::Complete { byte_len }
                | RawJsonlFrame::Partial { byte_len }
                | RawJsonlFrame::Oversized { byte_len, .. }
                | RawJsonlFrame::BudgetExhausted { byte_len, .. } => byte_len,
                RawJsonlFrame::Eof => 0,
            };
            offset = offset.saturating_add(byte_len);
            if line_offset >= before_offset {
                break;
            }
            if !matches!(frame, RawJsonlFrame::Complete { .. }) {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<Value>(frames.record()) else {
                continue;
            };
            state.observe_prior_record(&value, path, meta);
        }
        if let Some(generation) = generation {
            store_prior_context(path, generation, before_offset, state.clone());
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

/// Bounded cache of resumed prior-context state keyed by rollout path so an
/// incremental scan of an active session only parses the delta beyond its last
/// resume offset instead of the whole prefix.
const PRIOR_CONTEXT_CACHE_CAPACITY: usize = 512;

struct CachedPriorContext {
    generation: u64,
    offset: u64,
    state: CodexContextState,
}

#[derive(Default)]
struct PriorContextCache {
    entries: HashMap<PathBuf, CachedPriorContext>,
    order: VecDeque<PathBuf>,
}

static PRIOR_CONTEXT_CACHE: OnceLock<Mutex<PriorContextCache>> = OnceLock::new();

fn prior_context_generation(file: &std::fs::File) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let meta = file.metadata().ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.dev().hash(&mut hasher);
        meta.ino().hash(&mut hasher);
    }
    #[cfg(not(unix))]
    {
        meta.created()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
            .hash(&mut hasher);
    }
    Some(hasher.finish())
}

fn cached_prior_context(
    path: &Path,
    generation: u64,
    before_offset: u64,
) -> Option<(CodexContextState, u64)> {
    let cache = PRIOR_CONTEXT_CACHE.get()?;
    let cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = cache.entries.get(path)?;
    (entry.generation == generation && entry.offset <= before_offset)
        .then(|| (entry.state.clone(), entry.offset))
}

fn store_prior_context(path: &Path, generation: u64, offset: u64, state: CodexContextState) {
    let cache = PRIOR_CONTEXT_CACHE.get_or_init(|| Mutex::new(PriorContextCache::default()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = cache.entries.get_mut(path) {
        entry.generation = generation;
        entry.offset = offset;
        entry.state = state;
        return;
    }
    if cache.entries.len() >= PRIOR_CONTEXT_CACHE_CAPACITY
        && let Some(evicted) = cache.order.pop_front()
    {
        cache.entries.remove(&evicted);
    }
    cache.order.push_back(path.to_path_buf());
    cache.entries.insert(
        path.to_path_buf(),
        CachedPriorContext {
            generation,
            offset,
            state,
        },
    );
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
