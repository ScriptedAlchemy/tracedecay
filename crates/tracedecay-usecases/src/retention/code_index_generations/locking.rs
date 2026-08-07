use std::fs::{File, OpenOptions};
use std::path::Path;

use fs2::FileExt;

use super::{CodeGenerationRetentionErrorV1, SCOPE_RETENTION_LOCK_FILE, STORE_LOCK_FILE, storage};

pub struct CodeGenerationStoreLockV1(File);

impl Drop for CodeGenerationStoreLockV1 {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub fn acquire_code_generation_store_lock(
    store_root: &Path,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    lock_file(store_root.join(STORE_LOCK_FILE))
}

pub fn try_acquire_code_generation_store_lock(
    store_root: &Path,
) -> Result<Option<CodeGenerationStoreLockV1>, CodeGenerationRetentionErrorV1> {
    let lock = open_lock_file(&store_root.join(STORE_LOCK_FILE))?;
    match lock.try_lock_exclusive() {
        Ok(()) => Ok(Some(CodeGenerationStoreLockV1(lock))),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

pub(super) fn acquire_scope_retention_lock(
    store_root: &Path,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    lock_file(store_root.join(SCOPE_RETENTION_LOCK_FILE))
}

fn lock_file(
    path: impl AsRef<Path>,
) -> Result<CodeGenerationStoreLockV1, CodeGenerationRetentionErrorV1> {
    let lock = open_lock_file(path.as_ref())?;
    lock.lock_exclusive().map_err(storage)?;
    Ok(CodeGenerationStoreLockV1(lock))
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
