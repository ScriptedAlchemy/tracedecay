#[cfg(test)]
use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::Instant;

use tracedecay_private_fs::FileLease;

use super::{
    CodeGenerationRetentionErrorV1, GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
    GRAPH_REPLAY_POOL_ACQUIRE_POLL, SCOPE_RETENTION_LOCK_FILE, STORE_LOCK_FILE, storage,
};
#[cfg(windows)]
use super::{SCOPE_RETENTION_TRANSACTION_FILE, is_code_index_scope_hash, journal, scope_roots};

const CODE_GENERATION_LEASE_LABEL: &str = "code_index_retention.store";

pub struct CodeGenerationStoreLockV1 {
    _file: FileLease,
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

/// Reuses the same lock-file descriptor for successive bounded historical reads.
///
/// Every read still obtains a fresh kernel shared lock. The mutex is acquired
/// without blocking so concurrent callers and cancelled requests can retry.
/// The pathname and opened inode are authenticated before and after locking;
/// a replaced lock authority must never silently authorize an old inode.
#[cfg(unix)]
pub struct CachedCodeGenerationStoreReadLockV1 {
    store_root: PathBuf,
    file: Mutex<Option<File>>,
}

#[cfg(unix)]
pub struct CachedCodeGenerationStoreReadGuardV1<'a> {
    file: MutexGuard<'a, Option<File>>,
}

#[cfg(unix)]
impl Drop for CachedCodeGenerationStoreReadGuardV1<'_> {
    fn drop(&mut self) {
        if let Some(file) = self.file.as_ref() {
            if let Err(error) = file.unlock() {
                tracing::warn!(lease = CODE_GENERATION_LEASE_LABEL, %error, "cached read lock release failed");
            }
        }
    }
}

#[cfg(unix)]
impl CachedCodeGenerationStoreReadLockV1 {
    pub fn new(store_root: &Path) -> Self {
        Self {
            store_root: store_root.to_path_buf(),
            file: Mutex::new(None),
        }
    }

    fn verify_named_inode(file: &File, path: &Path) -> Result<(), CodeGenerationRetentionErrorV1> {
        let opened = file.metadata().map_err(storage)?;
        // A symlink to the retired inode is not the original lock authority.
        // lstat authenticates the directory entry without another open().
        let named = std::fs::symlink_metadata(path).map_err(super::deferred_if_absent)?;
        if !named.is_file() || (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "generation-store lock pathname no longer names the opened inode".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn try_acquire(
        &self,
    ) -> Result<Option<CachedCodeGenerationStoreReadGuardV1<'_>>, CodeGenerationRetentionErrorV1>
    {
        let mut cached = match self.file.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Poisoned(_)) => {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                    "cached generation-store read lock is poisoned".to_owned(),
                ));
            }
        };
        let root = canonical_store_root(&self.store_root)?;
        let path = root.join(STORE_LOCK_FILE);
        if cached.is_none() {
            *cached = Some(open_lock_file(&path)?);
        }
        let Some(file) = cached.as_ref() else {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "cached generation-store lock was not initialized".to_owned(),
            ));
        };
        Self::verify_named_inode(file, &path)?;
        match file.try_lock_shared().map_err(std::io::Error::from) {
            Ok(()) => {
                let lease = CachedCodeGenerationStoreReadGuardV1 { file: cached };
                // Recheck after kernel admission, before any pointer/segment read.
                // On an error the guard's Drop explicitly unlocks the descriptor.
                let Some(file) = lease.file.as_ref() else {
                    return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                        "cached generation-store lock disappeared".to_owned(),
                    ));
                };
                Self::verify_named_inode(file, &path)?;
                Ok(Some(lease))
            }
            Err(error) if tracedecay_private_fs::is_lock_contended(&error) => Ok(None),
            Err(error) => Err(storage(error)),
        }
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
            _file: FileLease::held(lock, CODE_GENERATION_LEASE_LABEL),
            store_root,
            generation_store: true,
            shared: true,
        })),
        Err(error) if tracedecay_private_fs::is_lock_contended(&error) => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

/// Block the calling thread until no holder, in this or any other process,
/// holds the generation store, then let go at once.
///
/// The kernel wakes this wait on the holders' release itself, so a caller
/// refused by a held lock retries exactly when the lock becomes free. The
/// wait is exclusive because a refused writer must also outlast readers. On
/// Windows it first waits out a retention pass fencing the scope; a scope a
/// pending retention transaction collected stays busy until that transaction
/// resolves, so it is reported as
/// [`CodeGenerationRetentionErrorV1::GenerationStoreBusy`] instead of a
/// release.
pub fn wait_for_code_generation_store_release(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    #[cfg(windows)]
    wait_for_generation_scope_fence_release(store_root)?;
    let store_root = canonical_store_root(store_root)?;
    open_lock_file(&store_root.join(STORE_LOCK_FILE))?
        .lock()
        .map_err(storage)
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
            _file: FileLease::held(lock, CODE_GENERATION_LEASE_LABEL),
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

/// Exclusive hold on the project's shared generation segments.
///
/// It is the project's scope-retention lock: every change that can make a
/// manifest stop or start naming segments, retiring generations, sweeping
/// segments, and collecting or restoring whole scopes, runs under it, so a
/// sweep's mark phase observes every manifest that can still be read.
pub(super) fn acquire_generation_segments_lock_checked(
    store_root: &Path,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    lock_file(
        super::generation_segment_project_root(store_root),
        SCOPE_RETENTION_LOCK_FILE,
        false,
        Instant::now() + GRAPH_REPLAY_POOL_ACQUIRE_BUDGET,
        is_cancelled,
    )
}

/// Shared hold a publication keeps from its first segment write until its
/// manifest is durable, so no sweep can collect a segment a manifest is about
/// to name. Publications of different worktrees share it; the exclusive
/// holders are bounded passes, so this waits for them instead of failing the
/// publication, observing `is_cancelled` between probes.
pub fn acquire_generation_segments_publication_lock(
    store_root: &Path,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    let project_root = canonical_store_root(super::generation_segment_project_root(store_root))?;
    loop {
        if is_cancelled() {
            return Err(CodeGenerationRetentionErrorV1::Cancelled);
        }
        let lock = open_lock_file(&project_root.join(SCOPE_RETENTION_LOCK_FILE))?;
        match lock.try_lock_shared().map_err(std::io::Error::from) {
            Ok(()) => {
                return Ok(CodeGenerationStoreLockV1 {
                    _file: FileLease::held(lock, CODE_GENERATION_LEASE_LABEL),
                    store_root: project_root,
                    generation_store: false,
                    shared: true,
                });
            }
            Err(error) if tracedecay_private_fs::is_lock_contended(&error) => {
                if is_cancelled() {
                    return Err(CodeGenerationRetentionErrorV1::Cancelled);
                }
                std::thread::park_timeout(GRAPH_REPLAY_POOL_ACQUIRE_POLL);
            }
            Err(error) => return Err(storage(error)),
        }
    }
}

#[tracing::instrument(name = "code_index_retention.lock", level = "trace", skip_all)]
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
                    park_until_retry(deadline, is_cancelled)?;
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
                    _file: FileLease::held(lock, CODE_GENERATION_LEASE_LABEL),
                    store_root,
                    generation_store,
                    shared: false,
                });
            }
            Err(error) if tracedecay_private_fs::is_lock_contended(&error) => {
                #[cfg(windows)]
                drop(scope_fence);
                park_until_retry(deadline, is_cancelled)?;
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
fn wait_for_generation_scope_fence_release(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let Some(scope_hash) = store_root.file_name().and_then(std::ffi::OsStr::to_str) else {
        return Ok(());
    };
    if !is_code_index_scope_hash(scope_hash) {
        return Ok(());
    }
    let parent = store_root.parent().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope has no parent for retention journal".to_owned(),
        )
    })?;
    let parent = canonical_store_root(parent)?;
    open_lock_file(&parent.join(SCOPE_RETENTION_LOCK_FILE))?
        .lock_shared()
        .map_err(storage)?;
    if scope_retention_pending(&parent, scope_hash)? {
        return Err(CodeGenerationRetentionErrorV1::GenerationStoreBusy);
    }
    Ok(())
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

fn park_until_retry(
    deadline: Instant,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if is_cancelled() {
        return Err(CodeGenerationRetentionErrorV1::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(CodeGenerationRetentionErrorV1::GenerationStoreBusy);
    }
    #[cfg(test)]
    STORE_LOCK_PARKS.with(|parks| parks.set(parks.get() + 1));
    let remaining = deadline.saturating_duration_since(Instant::now());
    std::thread::park_timeout(remaining.min(GRAPH_REPLAY_POOL_ACQUIRE_POLL));
    Ok(())
}

// Per-thread for the same reason as the replay-pool acquire observation:
// parallel tests park on their own threads.
#[cfg(test)]
thread_local! {
    static STORE_LOCK_PARKS: Cell<usize> = const { Cell::new(0) };
}

/// Park slices [`park_until_retry`] has taken on this thread.
#[cfg(test)]
pub(super) fn store_lock_park_count() -> usize {
    STORE_LOCK_PARKS.with(Cell::get)
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

#[cfg(all(test, unix))]
mod cached_read_lock_tests {
    use super::*;

    #[test]
    fn cached_reader_keeps_exclusive_fencing_and_refuses_replaced_lock_file() {
        let store = tempfile::tempdir().expect("isolated generation-store root");
        let cached = CachedCodeGenerationStoreReadLockV1::new(store.path());

        let read = cached
            .try_acquire()
            .expect("first shared kernel lock")
            .expect("uncontended shared lease");
        assert!(
            try_acquire_code_generation_store_lock(store.path())
                .expect("probe a real exclusive kernel lock")
                .is_none(),
            "a writer must not enter a protected historical page read"
        );
        assert!(
            cached.try_acquire().expect("second in-process probe").is_none(),
            "the single descriptor must not be concurrently unlocked by another reader"
        );
        drop(read);

        let writer = try_acquire_code_generation_store_lock(store.path())
            .expect("exclusive acquisition after reader release")
            .expect("read guard must explicitly unlock");
        assert!(
            cached.try_acquire().expect("shared probe under writer").is_none(),
            "a real exclusive lock must prevent a cached shared lease"
        );
        drop(writer);
        drop(
            cached
                .try_acquire()
                .expect("reader recovers from write lock")
                .expect("shared lease after exclusive release"),
        );

        let path = store.path().join(STORE_LOCK_FILE);
        let original = std::fs::metadata(&path).expect("cached lock inode");
        let retired = store.path().join("retired-lock-inode");
        std::fs::rename(&path, &retired)
            .expect("replace the lock pathname without closing the original inode");
        std::fs::File::create(&path).expect("different lock file at the same pathname");
        let replacement = std::fs::metadata(&path).expect("new lock inode");
        assert_ne!(
            (original.dev(), original.ino()),
            (replacement.dev(), replacement.ino()),
            "replacement must be a different real inode"
        );
        assert!(
            matches!(
                cached.try_acquire(),
                Err(CodeGenerationRetentionErrorV1::UnsafeState(_))
            ),
            "a cached descriptor must not lock a retired inode to authorize a new scope"
        );

        // A symlink back to the retired inode has the same dev/ino when
        // followed. The cached reader must still refuse this replacement:
        // verifying metadata(path) would wrongly accept it.
        std::fs::remove_file(&path).expect("remove replacement lock inode");
        std::os::unix::fs::symlink(&retired, &path)
            .expect("replace the lock with an alias of the retired inode");
        let followed = std::fs::metadata(&path).expect("follow symlink as positive control");
        assert_eq!(
            (followed.dev(), followed.ino()),
            (original.dev(), original.ino()),
            "the symlink must resolve to exactly the still-open cached inode"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("inspect replacement type")
                .file_type()
                .is_symlink(),
            "the replacement must be a real symlink"
        );
        assert!(
            matches!(
                cached.try_acquire(),
                Err(CodeGenerationRetentionErrorV1::UnsafeState(_))
            ),
            "a lock-file symlink must not authorize a retired cached inode"
        );

        // Recovery through the original pathname must not require another
        // cache entry or skip the real shared kernel-lock acquisition.
        std::fs::remove_file(&path).expect("remove lock symlink");
        std::fs::rename(&retired, &path).expect("restore exact original lock inode");
        let read = cached
            .try_acquire()
            .expect("recheck restored original lock")
            .expect("reacquire real shared kernel lock");
        assert!(
            try_acquire_code_generation_store_lock(store.path())
                .expect("probe exclusive lock after recovery")
                .is_none(),
            "restored cached read must still fence exclusive writers"
        );
        drop(read);
    }
}
