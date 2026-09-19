use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::{
    CodeGenerationRetentionErrorV1, GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
    GRAPH_REPLAY_POOL_ACQUIRE_POLL, SCOPE_RETENTION_LOCK_FILE, STORE_LOCK_FILE, storage,
};

pub struct CodeGenerationStoreLockV1 {
    file: File,
    store_root: PathBuf,
    generation_store: bool,
    shared: bool,
}

impl CodeGenerationStoreLockV1 {
    pub(super) fn generation_store_root(&self) -> Result<&Path, CodeGenerationRetentionErrorV1> {
        if self.shared {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "text-artifact attachment requires an exclusive generation-store lock".to_owned(),
            ));
        }
        if self.generation_store {
            Ok(&self.store_root)
        } else {
            Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "text-artifact attachment requires the generation-store lock".to_owned(),
            ))
        }
    }
}

impl Drop for CodeGenerationStoreLockV1 {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn acquire_code_generation_store_lock(
    store_root: &Path,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    acquire_code_generation_store_lock_checked(
        store_root,
        Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
        &|| false,
    )
}

/// Exclusive generation-store lock that stops at `deadline` or cancellation.
///
/// A free lock is taken even when the deadline has already elapsed, so a
/// caller that only needs one uncontended critical section is not refused.
/// A held lock returns [`CodeGenerationRetentionErrorV1::GenerationStoreBusy`]
/// or [`CodeGenerationRetentionErrorV1::Cancelled`] instead of blocking in
/// `File::lock`, which cannot observe either signal.
pub(super) fn acquire_code_generation_store_lock_checked(
    store_root: &Path,
    deadline: Instant,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    lock_file(store_root, STORE_LOCK_FILE, true, deadline, is_cancelled)
}

/// Try to hold the generation store as a reader for one bounded read of
/// immutable, content-addressed evidence. The caller owns cancellation and
/// deadline-aware retry while an exclusive writer is active.
pub fn try_acquire_code_generation_store_read_lock(
    store_root: &Path,
) -> Result<Option<CodeGenerationStoreLockV1>, CodeGenerationRetentionErrorV1> {
    let store_root = canonical_store_root(store_root)?;
    let lock = open_lock_file(&store_root.join(STORE_LOCK_FILE))?;
    match lock.try_lock_shared().map_err(std::io::Error::from) {
        Ok(()) => Ok(Some(CodeGenerationStoreLockV1 {
            file: lock,
            store_root,
            generation_store: true,
            shared: true,
        })),
        Err(error) if tracedecay_private_fs::is_lock_contended(&error) => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

pub fn try_acquire_code_generation_store_lock(
    store_root: &Path,
) -> Result<Option<CodeGenerationStoreLockV1>, CodeGenerationRetentionErrorV1> {
    let store_root = canonical_store_root(store_root)?;
    let lock = open_lock_file(&store_root.join(STORE_LOCK_FILE))?;
    match lock.try_lock().map_err(std::io::Error::from) {
        Ok(()) => Ok(Some(CodeGenerationStoreLockV1 {
            file: lock,
            store_root,
            generation_store: true,
            shared: false,
        })),
        // Windows LockFileEx reports ERROR_LOCK_VIOLATION (33) instead of
        // WouldBlock. AccessDenied and sharing violations stay Storage.
        Err(error) if tracedecay_private_fs::is_lock_contended(&error) => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

pub(super) fn acquire_scope_retention_lock(
    store_root: &Path,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    lock_file(
        store_root,
        SCOPE_RETENTION_LOCK_FILE,
        false,
        Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
        &|| false,
    )
}

#[hotpath::measure(label = "code_index_retention.lock")]
fn lock_file(
    store_root: &Path,
    lock_file: &str,
    generation_store: bool,
    deadline: Instant,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    let store_root = canonical_store_root(store_root)?;
    let deadline = deadline.min(Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET);
    loop {
        if is_cancelled() {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        let lock = open_lock_file(&store_root.join(lock_file))?;
        match lock.try_lock().map_err(std::io::Error::from) {
            Ok(()) => {
                return Ok(CodeGenerationStoreLockV1 {
                    file: lock,
                    store_root,
                    generation_store,
                    shared: false,
                });
            }
            Err(error) if tracedecay_private_fs::is_lock_contended(&error) => {
                if Instant::now() >= deadline {
                    return Err(CodeGenerationRetentionErrorV1::GenerationStoreBusy);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::park_timeout(remaining.min(GRAPH_REPLAY_POOL_ACQUIRE_POLL));
            }
            Err(error) => return Err(storage(error)),
        }
    }
}

fn canonical_store_root(store_root: &Path) -> Result<PathBuf, CodeGenerationRetentionErrorV1> {
    std::fs::canonicalize(store_root).map_err(super::deferred_if_absent)
}

fn open_lock_file(path: &Path) -> Result<File, CodeGenerationRetentionErrorV1> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(super::deferred_if_absent)
}
