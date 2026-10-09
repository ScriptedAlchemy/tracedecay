//! Skip unchanged sources after a non-retryable catch-up failure.
//!
//! A terminal refusal (invalid contract, missing identity, rejected privacy)
//! fails the same way on the same bytes. The history worker used to reopen
//! those files every pass, which burned idle CPU on Hermes redaction and
//! Codex metadata fills and flooded the log. Remember the source's mtime and
//! length; skip it until either changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::UNIX_EPOCH;

use crate::runtime::hosts::codex::PendingTranscript;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceRevision {
    mtime_secs: u64,
    len: u64,
}

fn revision(path: &Path) -> Option<SourceRevision> {
    let metadata = std::fs::metadata(path).ok()?;
    let mtime_secs = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())?;
    Some(SourceRevision {
        mtime_secs,
        len: metadata.len(),
    })
}

fn cache() -> &'static Mutex<BTreeMap<PathBuf, SourceRevision>> {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, SourceRevision>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, SourceRevision>> {
    cache().lock().unwrap_or_else(PoisonError::into_inner)
}

pub(in crate::runtime) fn skip_unchanged_terminal_source(path: &Path) -> bool {
    let Some(observed) = revision(path) else {
        return false;
    };
    lock().get(path) == Some(&observed)
}

pub(in crate::runtime) fn remember_terminal_source(path: &Path) {
    if let Some(observed) = revision(path) {
        lock().insert(path.to_path_buf(), observed);
    }
}

pub(in crate::runtime) fn forget_terminal_source(path: &Path) {
    lock().remove(path);
}

/// Record a non-retryable Codex source so later passes skip it until the
/// file's mtime or length changes. Discovery convergence is updated too so a
/// hub consumer does not reopen the same witness.
pub(in crate::runtime) fn remember_non_retryable_codex_source(
    pending: PendingTranscript<'_>,
    path: &Path,
    retryable: bool,
) {
    if retryable {
        return;
    }
    remember_terminal_source(path);
    let _ = pending.finished(path);
}

#[cfg(test)]
pub(in crate::runtime) fn reset_terminal_source_skips_for_test() {
    lock().clear();
}

#[cfg(test)]
mod tests {
    use super::{
        forget_terminal_source, remember_terminal_source, reset_terminal_source_skips_for_test,
        skip_unchanged_terminal_source,
    };

    #[test]
    fn unchanged_terminal_source_is_skipped_until_mtime_or_length_changes() {
        reset_terminal_source_skips_for_test();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rollout.jsonl");
        std::fs::write(&path, "{}\n").unwrap();

        assert!(
            !skip_unchanged_terminal_source(&path),
            "a source is live until a terminal failure records it"
        );
        remember_terminal_source(&path);
        assert!(
            skip_unchanged_terminal_source(&path),
            "the same mtime and length must not be reopened"
        );

        std::fs::write(&path, "{}\n{}\n").unwrap();
        assert!(
            !skip_unchanged_terminal_source(&path),
            "a longer (or newer) file must be retried"
        );

        remember_terminal_source(&path);
        forget_terminal_source(&path);
        assert!(
            !skip_unchanged_terminal_source(&path),
            "a recovered source must be live again"
        );
    }
}
