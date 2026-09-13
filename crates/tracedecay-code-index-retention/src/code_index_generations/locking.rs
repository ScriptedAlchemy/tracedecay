use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use super::{CodeGenerationRetentionErrorV1, SCOPE_RETENTION_LOCK_FILE, STORE_LOCK_FILE, storage};

pub struct CodeGenerationStoreLockV1 {
    file: File,
    store_root: PathBuf,
    generation_store: bool,
}

impl CodeGenerationStoreLockV1 {
    pub(super) fn generation_store_root(&self) -> Result<&Path, CodeGenerationRetentionErrorV1> {
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
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn acquire_code_generation_store_lock(
    store_root: &Path,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    lock_file(store_root, STORE_LOCK_FILE, true)
}

/// Hold the generation store stable while reading immutable publication data.
///
/// Readers share this lease with each other, but publication, text-artifact
/// mutation, and retention still acquire the exclusive store lock above.
pub fn acquire_code_generation_store_read_lock(
    store_root: &Path,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    let store_root = canonical_store_root(store_root)?;
    let lock = open_lock_file(&store_root.join(STORE_LOCK_FILE))?;
    FileExt::lock_shared(&lock).map_err(storage)?;
    Ok(CodeGenerationStoreLockV1 {
        file: lock,
        store_root,
        generation_store: false,
    })
}

pub fn try_acquire_code_generation_store_lock(
    store_root: &Path,
) -> Result<Option<CodeGenerationStoreLockV1>, CodeGenerationRetentionErrorV1> {
    let store_root = canonical_store_root(store_root)?;
    let lock = open_lock_file(&store_root.join(STORE_LOCK_FILE))?;
    match lock.try_lock_exclusive() {
        Ok(()) => Ok(Some(CodeGenerationStoreLockV1 {
            file: lock,
            store_root,
            generation_store: true,
        })),
        // Windows LockFileEx reports ERROR_LOCK_VIOLATION (33) instead of
        // WouldBlock. AccessDenied and sharing violations stay Storage.
        Err(error) if tracedecay_private_fs::is_lock_contended(&error) => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

pub fn try_acquire_code_generation_store_read_lock(
    store_root: &Path,
) -> Result<Option<CodeGenerationStoreLockV1>, CodeGenerationRetentionErrorV1> {
    let store_root = canonical_store_root(store_root)?;
    let lock = open_lock_file(&store_root.join(STORE_LOCK_FILE))?;
    match FileExt::try_lock_shared(&lock) {
        Ok(()) => Ok(Some(CodeGenerationStoreLockV1 {
            file: lock,
            store_root,
            generation_store: false,
        })),
        Err(error) if tracedecay_private_fs::is_lock_contended(&error) => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

pub(super) fn acquire_scope_retention_lock(
    store_root: &Path,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    lock_file(store_root, SCOPE_RETENTION_LOCK_FILE, false)
}

#[hotpath::measure(label = "code_index_retention.lock")]
fn lock_file(
    store_root: &Path,
    lock_file: &str,
    generation_store: bool,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    let store_root = canonical_store_root(store_root)?;
    let lock = open_lock_file(&store_root.join(lock_file))?;
    lock.lock_exclusive().map_err(storage)?;
    Ok(CodeGenerationStoreLockV1 {
        file: lock,
        store_root,
        generation_store,
    })
}

fn canonical_store_root(store_root: &Path) -> Result<PathBuf, CodeGenerationRetentionErrorV1> {
    std::fs::canonicalize(store_root).map_err(storage)
}

fn open_lock_file(path: &Path) -> Result<File, CodeGenerationRetentionErrorV1> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(storage)
}

#[cfg(test)]
mod tests {
    use super::{acquire_code_generation_store_read_lock, try_acquire_code_generation_store_lock};

    #[test]
    fn immutable_readers_share_the_store_lock_and_refuse_an_exclusive_writer() {
        let store = tempfile::tempdir().expect("temporary generation store");
        let first =
            acquire_code_generation_store_read_lock(store.path()).expect("first immutable reader");
        let second =
            acquire_code_generation_store_read_lock(store.path()).expect("second immutable reader");

        assert!(
            try_acquire_code_generation_store_lock(store.path())
                .expect("exclusive writer probe")
                .is_none(),
            "an exclusive writer must not overlap immutable readers"
        );

        drop((first, second));
        assert!(
            try_acquire_code_generation_store_lock(store.path())
                .expect("exclusive writer after readers")
                .is_some(),
            "dropping every reader must release the writer"
        );
    }
}
