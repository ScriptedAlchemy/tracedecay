use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use super::{CodexMeta, session_meta_from_record, turn_context_from_record};
use crate::runtime::jsonl_observation_admission::jsonl_frame_hints;
use crate::runtime::source::MAX_JSONL_RECORD_BYTES;

/// Chunk the prior-context walk reads backwards from the resume offset.
const PRIOR_CONTEXT_CHUNK_BYTES: u64 = 8 * 1024;

/// The rollout context a resumed record inherits from the records before it.
/// The model may be absent: real rollouts carry it on `turn_context` records,
/// which not every rollout contains.
#[derive(Clone)]
pub(super) struct CodexContextState {
    pub(super) cwd: PathBuf,
    pub(super) model: Option<String>,
}

/// What a context record updates; each field stays put when the record does
/// not carry it.
#[derive(Default)]
struct ContextValues {
    cwd: Option<PathBuf>,
    model: Option<String>,
}

impl CodexContextState {
    pub(super) fn from_meta(meta: &CodexMeta) -> Self {
        Self {
            cwd: meta.cwd.clone(),
            model: meta.model.clone(),
        }
    }

    /// Context in effect at `before_offset`, with the bytes read to rebuild it.
    ///
    /// Each context field comes from the last record before the cursor that
    /// sets it, normally the current turn's, so the walk runs backwards from
    /// the cursor and stops once every field is found instead of replaying
    /// the prefix. A cached context for an earlier offset of the same JSONL
    /// `generation` also stops it at that offset: one generation is one byte
    /// stream, so the bytes before that offset are the ones the cached
    /// context was read from.
    #[tracing::instrument(name = "sessions.hosts.codex.scan_prior", level = "trace", skip_all)]
    pub(super) fn scan_prior(
        path: &Path,
        generation: u64,
        before_offset: u64,
        meta: &CodexMeta,
    ) -> (Self, u64) {
        if before_offset == 0 {
            return (Self::from_meta(meta), 0);
        }
        let Ok(mut file) = File::open(path) else {
            return (Self::from_meta(meta), 0);
        };
        let (floor, floor_state) = cached_prior_context(path, generation, before_offset)
            .map_or_else(
                || (0, Self::from_meta(meta)),
                |(state, offset)| (offset, state),
            );
        let (state, read) =
            match Self::last_context_values(&mut file, floor, before_offset, path, meta) {
                Ok((values, read)) => (
                    Self {
                        cwd: values.cwd.unwrap_or(floor_state.cwd),
                        model: values.model.or(floor_state.model),
                    },
                    read,
                ),
                Err(read) => return (Self::from_meta(meta), read),
            };
        store_prior_context(path, generation, before_offset, state.clone());
        (state, read)
    }

    /// The values the last context records of `[floor, before_offset)` set,
    /// with the bytes read to find them. `floor` is a record boundary. The
    /// walk stops once every field resolves; a field found nowhere before the
    /// floor stays `None` for the caller to merge with the floor state.
    fn last_context_values(
        file: &mut File,
        floor: u64,
        before_offset: u64,
        path: &Path,
        meta: &CodexMeta,
    ) -> Result<(ContextValues, u64), u64> {
        let mut read = 0_u64;
        let mut found = ContextValues::default();
        let mut chunk = Vec::new();
        // Pieces of the line whose start the walk has not reached, last first.
        let mut pieces: Vec<Vec<u8>> = Vec::new();
        let mut pieces_len = 0_usize;
        // Bytes after the last newline belong to the record the cursor sits
        // inside, which no prior context includes.
        let mut in_cursor_record = true;
        let mut end = before_offset;
        while end > floor && (found.cwd.is_none() || found.model.is_none()) {
            let start = end.saturating_sub(PRIOR_CONTEXT_CHUNK_BYTES).max(floor);
            chunk.resize(usize::try_from(end - start).map_err(|_| read)?, 0);
            file.seek(SeekFrom::Start(start)).map_err(|_| read)?;
            file.read_exact(&mut chunk).map_err(|_| read)?;
            read += end - start;
            let mut rest = &chunk[..];
            while let Some(newline) = rest.iter().rposition(|byte| *byte == b'\n') {
                if !in_cursor_record {
                    let values =
                        line_context_values(&rest[newline + 1..], &pieces, pieces_len, path, meta);
                    if found.cwd.is_none() {
                        found.cwd = values.cwd;
                    }
                    if found.model.is_none() {
                        found.model = values.model;
                    }
                    if found.cwd.is_some() && found.model.is_some() {
                        return Ok((found, read));
                    }
                }
                in_cursor_record = false;
                pieces.clear();
                pieces_len = 0;
                rest = &rest[..newline];
            }
            pieces_len += rest.len();
            // The forward reader skips a record this long, newline included.
            if pieces_len < MAX_JSONL_RECORD_BYTES {
                pieces.push(rest.to_vec());
            } else {
                pieces.clear();
            }
            end = start;
        }
        if in_cursor_record {
            return Ok((found, read));
        }
        let first = line_context_values(&[], &pieces, pieces_len, path, meta);
        if found.cwd.is_none() {
            found.cwd = first.cwd;
        }
        if found.model.is_none() {
            found.model = first.model;
        }
        Ok((found, read))
    }

    pub(super) fn observe_context_record(
        &mut self,
        record: &Value,
        path: &Path,
        meta: &CodexMeta,
    ) -> bool {
        match context_values(record, path, meta) {
            Some(values) => {
                if let Some(cwd) = values.cwd {
                    self.cwd = cwd;
                }
                if let Some(model) = values.model {
                    self.model = Some(model);
                }
                true
            }
            None => false,
        }
    }
}

/// What `record` does to the rollout context: `None` when it is no context
/// record, `Some` of the fields it sets when it is one.
fn context_values(record: &Value, path: &Path, meta: &CodexMeta) -> Option<ContextValues> {
    if let Some(updated) = session_meta_from_record(record, path) {
        let same_session = updated.session_id == meta.session_id;
        return Some(ContextValues {
            cwd: same_session.then_some(updated.cwd),
            model: same_session.then_some(updated.model).flatten(),
        });
    }
    turn_context_from_record(record).map(|context| ContextValues {
        cwd: context.cwd,
        model: context.model,
    })
}

/// The values the line `head` + `pieces` (stored last first) sets, if it is a
/// context record.
fn line_context_values(
    head: &[u8],
    pieces: &[Vec<u8>],
    pieces_len: usize,
    path: &Path,
    meta: &CodexMeta,
) -> ContextValues {
    if head.len() + pieces_len >= MAX_JSONL_RECORD_BYTES {
        return ContextValues::default();
    }
    let assembled;
    let line = if pieces.is_empty() {
        head
    } else {
        let mut bytes = Vec::with_capacity(head.len() + pieces_len);
        bytes.extend_from_slice(head);
        pieces
            .iter()
            .rev()
            .for_each(|piece| bytes.extend_from_slice(piece));
        assembled = bytes;
        &assembled[..]
    };
    if !jsonl_frame_hints(line).may_change_codex_context {
        return ContextValues::default();
    }
    let Some(record) = serde_json::from_slice::<Value>(line).ok() else {
        return ContextValues::default();
    };
    context_values(&record, path, meta).unwrap_or_default()
}

/// Bounded cache of resumed prior-context state keyed by rollout path, so the
/// next window of the same JSONL generation walks back no further than its
/// last resume offset.
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
mod tests;
