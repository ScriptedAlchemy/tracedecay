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
#[derive(Clone)]
pub(super) struct CodexContextState {
    pub(super) cwd: PathBuf,
}

impl CodexContextState {
    pub(super) fn from_meta(meta: &CodexMeta) -> Self {
        Self {
            cwd: meta.cwd.clone(),
        }
    }

    /// Context in effect at `before_offset`, with the bytes read to rebuild it.
    ///
    /// The cwd is set by the last context record before the cursor, normally
    /// the current turn's, so the walk runs backwards from the cursor and stops
    /// there instead of replaying the prefix. A cached context for an earlier
    /// offset of the same JSONL `generation` also stops it at that offset:
    /// one generation is one byte stream, so the bytes before that offset are
    /// the ones the cached context was read from.
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
            match Self::last_context_cwd(&mut file, floor, before_offset, path, meta) {
                Ok((cwd, read)) => (cwd.map_or(floor_state, |cwd| Self { cwd }), read),
                Err(read) => return (Self::from_meta(meta), read),
            };
        store_prior_context(path, generation, before_offset, state.clone());
        crate::runtime::pipeline_metrics::add("sessions.hosts.codex.prior_context_bytes", read);
        (state, read)
    }

    /// The cwd the last context record of `[floor, before_offset)` sets, with
    /// the bytes read to find it. `floor` is a record boundary.
    fn last_context_cwd(
        file: &mut File,
        floor: u64,
        before_offset: u64,
        path: &Path,
        meta: &CodexMeta,
    ) -> Result<(Option<PathBuf>, u64), u64> {
        let mut read = 0_u64;
        let mut chunk = Vec::new();
        // Pieces of the line whose start the walk has not reached, last first.
        let mut pieces: Vec<Vec<u8>> = Vec::new();
        let mut pieces_len = 0_usize;
        // Bytes after the last newline belong to the record the cursor sits
        // inside, which no prior context includes.
        let mut in_cursor_record = true;
        let mut end = before_offset;
        while end > floor {
            let start = end.saturating_sub(PRIOR_CONTEXT_CHUNK_BYTES).max(floor);
            chunk.resize(usize::try_from(end - start).map_err(|_| read)?, 0);
            file.seek(SeekFrom::Start(start)).map_err(|_| read)?;
            file.read_exact(&mut chunk).map_err(|_| read)?;
            read += end - start;
            let mut rest = &chunk[..];
            while let Some(newline) = rest.iter().rposition(|byte| *byte == b'\n') {
                if !in_cursor_record
                    && let Some(cwd) =
                        line_context_cwd(&rest[newline + 1..], &pieces, pieces_len, path, meta)
                {
                    return Ok((Some(cwd), read));
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
        let first = (!in_cursor_record)
            .then(|| line_context_cwd(&[], &pieces, pieces_len, path, meta))
            .flatten();
        Ok((first, read))
    }

    pub(super) fn observe_context_record(
        &mut self,
        record: &Value,
        path: &Path,
        meta: &CodexMeta,
    ) -> bool {
        match context_cwd(record, path, meta) {
            Some(cwd) => {
                if let Some(cwd) = cwd {
                    self.cwd = cwd;
                }
                true
            }
            None => false,
        }
    }
}

/// What `record` does to the rollout cwd: `None` when it is no context record,
/// `Some(None)` when it is one that leaves the cwd as it was.
fn context_cwd(record: &Value, path: &Path, meta: &CodexMeta) -> Option<Option<PathBuf>> {
    if let Some(updated) = session_meta_from_record(record, path) {
        return Some((updated.session_id == meta.session_id).then_some(updated.cwd));
    }
    turn_context_from_record(record).map(|context| context.cwd)
}

/// The cwd the line `head` + `pieces` (stored last first) sets, if it is a
/// context record that sets one.
fn line_context_cwd(
    head: &[u8],
    pieces: &[Vec<u8>],
    pieces_len: usize,
    path: &Path,
    meta: &CodexMeta,
) -> Option<PathBuf> {
    if head.len() + pieces_len >= MAX_JSONL_RECORD_BYTES {
        return None;
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
        return None;
    }
    let record = serde_json::from_slice::<Value>(line).ok()?;
    context_cwd(&record, path, meta).flatten()
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
