use std::collections::{HashMap, VecDeque};
use std::io::{BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use super::{CodexMeta, session_meta_from_record, turn_context_from_record};
use crate::runtime::source::{MAX_JSONL_RECORD_BYTES, RawJsonlFrame, RawJsonlFrameReader};

#[derive(Clone)]
pub(super) struct CodexContextState {
    pub(super) turn_id: Option<String>,
    pub(super) model: Option<String>,
    pub(super) cwd: Option<PathBuf>,
    pub(super) git: Option<Value>,
    pub(super) compaction_depth: i64,
}

impl CodexContextState {
    pub(super) fn from_meta(meta: &CodexMeta) -> Self {
        Self {
            turn_id: None,
            model: meta.model.clone(),
            cwd: Some(meta.cwd.clone()),
            git: meta.git.clone(),
            compaction_depth: 0,
        }
    }

    #[hotpath::measure(label = "sessions.hosts.codex.scan_prior")]
    pub(super) fn scan_prior(path: &Path, before_offset: u64, meta: &CodexMeta) -> Self {
        if before_offset == 0 {
            return Self::from_meta(meta);
        }
        let Ok(mut file) = std::fs::File::open(path) else {
            return Self::from_meta(meta);
        };
        #[cfg(test)]
        {
            *PRIOR_CONTEXT_SCANS
                .get_or_init(|| Mutex::new(HashMap::new()))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(path.to_path_buf())
                .or_default() += 1;
        }
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
            // A turn context is a new native correlation boundary. Missing
            // identity/model evidence must become unknown for that turn
            // instead of inheriting a prior turn's values.
            self.turn_id = context.turn_id;
            self.model = context.model;
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

#[cfg(test)]
static PRIOR_CONTEXT_SCANS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

/// Rollout opens that rebuilt `path`'s prior context.
#[cfg(test)]
pub(crate) fn prior_context_scan_count_for_test(path: &Path) -> usize {
    PRIOR_CONTEXT_SCANS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .copied()
        .unwrap_or_default()
}

/// Evicts `path`'s resumable context, as a full cache does for every rollout
/// past its capacity.
#[cfg(test)]
pub(crate) fn evict_prior_context_for_test(path: &Path) {
    if let Some(cache) = PRIOR_CONTEXT_CACHE.get() {
        cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .remove(path);
    }
}

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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::{CodexContextState, CodexMeta};

    fn meta() -> CodexMeta {
        CodexMeta {
            cwd: PathBuf::from("/workspace"),
            session_id: "session.fixture".to_owned(),
            model: None,
            git: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            agent_nickname: None,
            agent_role: None,
            thread_source: None,
        }
    }

    #[test]
    fn new_turn_context_clears_missing_turn_and_model_evidence() {
        let meta = meta();
        let mut state = CodexContextState::from_meta(&meta);
        assert!(state.observe_context_record(
            &json!({
                "type": "turn_context",
                "payload": {
                    "turn_id": "turn.first",
                    "model": "gpt-5.6-codex"
                }
            }),
            Path::new("/tmp/rollout.jsonl"),
            &meta,
        ));
        assert_eq!(state.turn_id.as_deref(), Some("turn.first"));
        assert_eq!(state.model.as_deref(), Some("gpt-5.6-codex"));

        assert!(state.observe_context_record(
            &json!({"type": "turn_context", "payload": {}}),
            Path::new("/tmp/rollout.jsonl"),
            &meta,
        ));
        assert_eq!(state.turn_id, None);
        assert_eq!(state.model, None);
    }
}
