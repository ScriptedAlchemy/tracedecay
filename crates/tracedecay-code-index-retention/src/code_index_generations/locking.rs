use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::{
    CodeGenerationRetentionErrorV1, GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
    GRAPH_REPLAY_POOL_ACQUIRE_POLL, SCOPE_RETENTION_LOCK_FILE, STORE_LOCK_FILE, storage,
};
#[cfg(windows)]
use super::{SCOPE_RETENTION_TRANSACTION_FILE, is_code_index_scope_hash, journal, scope_roots};

pub struct CodeGenerationStoreLockV1 {
    file: File,
    store_root: PathBuf,
    generation_store: bool,
    shared: bool,
}

#[cfg(windows)]
enum GenerationScopeFence {
    Unscoped,
    PassBusy,
    Pending,
    Acquired { _file: File },
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
    #[cfg(windows)]
    let _scope_fence = match try_acquire_generation_scope_fence(store_root)? {
        GenerationScopeFence::PassBusy | GenerationScopeFence::Pending => return Ok(None),
        fence => fence,
    };
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
    #[cfg(windows)]
    let _scope_fence = match try_acquire_generation_scope_fence(store_root)? {
        GenerationScopeFence::PassBusy | GenerationScopeFence::Pending => return Ok(None),
        fence => fence,
    };
    try_acquire_code_generation_store_lock_unfenced(store_root)
}

pub(super) fn try_acquire_code_generation_store_lock_during_scope_retention(
    store_root: &Path,
    scope_retention_lock: &CodeGenerationStoreLockV1,
) -> Result<Option<CodeGenerationStoreLockV1>, CodeGenerationRetentionErrorV1> {
    let parent = store_root.parent().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope has no parent for retention lock".to_owned(),
        )
    })?;
    if scope_retention_lock.generation_store
        || scope_retention_lock.shared
        || canonical_store_root(parent)? != scope_retention_lock.store_root
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope collection requires its exact exclusive parent lock".to_owned(),
        ));
    }
    try_acquire_code_generation_store_lock_unfenced(store_root)
}

fn try_acquire_code_generation_store_lock_unfenced(
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
    let deadline = deadline.min(Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET);
    loop {
        if is_cancelled() {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        #[cfg(windows)]
        let scope_fence = if generation_store {
            match try_acquire_generation_scope_fence(store_root)? {
                GenerationScopeFence::Pending => {
                    return Err(CodeGenerationRetentionErrorV1::GenerationStoreBusy);
                }
                GenerationScopeFence::PassBusy => {
                    park_until_retry(deadline)?;
                    continue;
                }
                fence => fence,
            }
        } else {
            GenerationScopeFence::Unscoped
        };
        let store_root = canonical_store_root(store_root)?;
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
                #[cfg(windows)]
                drop(scope_fence);
                park_until_retry(deadline)?;
            }
            Err(error) => return Err(storage(error)),
        }
    }
}

#[cfg(windows)]
fn try_acquire_generation_scope_fence(
    store_root: &Path,
) -> Result<GenerationScopeFence, CodeGenerationRetentionErrorV1> {
    let Some(scope_hash) = store_root.file_name().and_then(std::ffi::OsStr::to_str) else {
        return Ok(GenerationScopeFence::Unscoped);
    };
    if !is_code_index_scope_hash(scope_hash) {
        return Ok(GenerationScopeFence::Unscoped);
    }
    let parent = store_root.parent().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope has no parent for retention journal".to_owned(),
        )
    })?;
    let parent = canonical_store_root(parent)?;
    let lock = open_lock_file(&parent.join(SCOPE_RETENTION_LOCK_FILE))?;
    match lock.try_lock_shared().map_err(std::io::Error::from) {
        Ok(()) if scope_retention_pending(&parent, scope_hash)? => {
            Ok(GenerationScopeFence::Pending)
        }
        Ok(()) => Ok(GenerationScopeFence::Acquired { _file: lock }),
        Err(error) if tracedecay_private_fs::is_lock_contended(&error) => {
            Ok(GenerationScopeFence::PassBusy)
        }
        Err(error) => Err(storage(error)),
    }
}

#[cfg(windows)]
fn scope_retention_pending(
    parent: &Path,
    scope_hash: &str,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    match std::fs::symlink_metadata(parent.join(SCOPE_RETENTION_TRANSACTION_FILE)) {
        Ok(_) => Ok(
            journal::load_journal(parent, &scope_roots::SCOPE_TRANSACTION_JOURNAL)?.is_some_and(
                |transaction| {
                    transaction
                        .receipt
                        .collected_scopes
                        .iter()
                        .any(|scope| scope.scope_hash == scope_hash)
                },
            ),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage(error)),
    }
}

fn park_until_retry(deadline: Instant) -> Result<(), CodeGenerationRetentionErrorV1> {
    if Instant::now() >= deadline {
        return Err(CodeGenerationRetentionErrorV1::GenerationStoreBusy);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    std::thread::park_timeout(remaining.min(GRAPH_REPLAY_POOL_ACQUIRE_POLL));
    Ok(())
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
