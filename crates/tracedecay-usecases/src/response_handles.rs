//! Canonical local response-handle storage.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::storage::{
    DURABLE_REMOVAL_TOMBSTONE_PREFIX, PrivateStoreIo, reject_symlink_components,
    resolve_response_handle_root,
};

pub const RESPONSE_HANDLE_TTL_SECS: i64 = 86_400;

const HANDLE_HEX_CHARS: usize = 24;
const HANDLE_PREFIX: &str = "rh_";
const LOCK_SUFFIX: &str = ".lock";
const STAGING_PREFIX: &str = ".response-handle-staging-";

#[derive(Debug, Clone)]
pub struct ResponseHandleRecord {
    pub handle: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub content: String,
}

impl ResponseHandleRecord {
    pub fn original_chars(&self) -> usize {
        self.content.chars().count()
    }
}

#[derive(Debug, Clone)]
pub enum ResponseHandleLookup {
    Found(ResponseHandleRecord),
    Missing,
    Expired { created_at: i64, expires_at: i64 },
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct ResponseHandleCleanup {
    pub scanned: usize,
    pub removed_expired: usize,
    pub removed_staging: usize,
    pub removed_tombstones: usize,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct ResponseHandleInventory {
    pub file_count: u64,
    pub total_bytes: u64,
    pub oldest_expires_at: Option<i64>,
    pub newest_expires_at: Option<i64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredResponseHandleRecord {
    created_at: i64,
    expires_at: i64,
    content: String,
}

struct StoredFile {
    path: PathBuf,
    bytes: u64,
    record: StoredResponseHandleRecord,
}

pub fn is_valid_response_handle(handle: &str) -> bool {
    let Some(hex) = handle.strip_prefix(HANDLE_PREFIX) else {
        return false;
    };
    hex.len() == HANDLE_HEX_CHARS
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub fn store_response_handle(
    project_root: &Path,
    content: &str,
    now: i64,
) -> Result<ResponseHandleRecord> {
    let root = resolve_response_handle_root(project_root)?;
    PrivateStoreIo::create_dir_all_durable(&root)
        .map_err(|error| file_error(&root, "create durable directory", error))?;
    store_response_handle_in_root(&root, content, now)
}

fn store_response_handle_in_root(
    root: &Path,
    content: &str,
    now: i64,
) -> Result<ResponseHandleRecord> {
    with_exclusive_lock(root, || store_response_handle_locked(root, content, now))
}

fn store_response_handle_locked(
    root: &Path,
    content: &str,
    now: i64,
) -> Result<ResponseHandleRecord> {
    let handle = response_handle_for(content);
    let path = response_handle_path(root, &handle)?;
    let stored = StoredResponseHandleRecord {
        created_at: now,
        expires_at: now.saturating_add(RESPONSE_HANDLE_TTL_SECS),
        content: content.to_owned(),
    };
    let payload = serde_json::to_vec_pretty(&stored)?;

    let rollback_payload = match retrieve_from_root_locked(root, &handle, now) {
        Ok(ResponseHandleLookup::Found(existing)) if existing.content == content => {
            Some(serde_json::to_vec_pretty(&StoredResponseHandleRecord {
                created_at: existing.created_at,
                expires_at: existing.expires_at,
                content: existing.content,
            })?)
        }
        Ok(ResponseHandleLookup::Found(_)) => {
            return Err(corrupt_record_error(
                &path,
                "digest collision with different stored content",
            ));
        }
        Ok(ResponseHandleLookup::Missing | ResponseHandleLookup::Expired { .. }) => None,
        Err(error) if is_corrupt_record_error(&error) => None,
        Err(error) => return Err(error),
    };
    if let Err(error) = publish_record_durable(root, &path, &payload) {
        if let Some(rollback_payload) = rollback_payload {
            return match publish_record_durable(root, &path, &rollback_payload) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(TraceDecayError::File {
                    message: format!(
                        "{error}; additionally failed to restore the prior response-handle record: {rollback_error}"
                    ),
                    path: path.display().to_string(),
                }),
            };
        }
        return match remove_failed_fresh_publish(&path, &stored) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(TraceDecayError::File {
                message: format!(
                    "{error}; additionally failed to clean up the rejected response-handle publication: {cleanup_error}"
                ),
                path: path.display().to_string(),
            }),
        };
    }
    Ok(ResponseHandleRecord {
        handle,
        created_at: stored.created_at,
        expires_at: stored.expires_at,
        content: stored.content,
    })
}

pub fn retrieve_response_handle(
    project_root: &Path,
    handle: &str,
    now: i64,
) -> Result<ResponseHandleLookup> {
    validate_handle(handle)?;
    let root = resolve_response_handle_root(project_root)?;
    retrieve_from_root(&root, handle, now)
}

fn retrieve_from_root(root: &Path, handle: &str, now: i64) -> Result<ResponseHandleLookup> {
    validate_handle(handle)?;
    validate_response_handle_path(root)?;
    if !path_exists(root)? {
        return Ok(ResponseHandleLookup::Missing);
    }
    with_exclusive_lock(root, || retrieve_from_root_locked(root, handle, now))
}

fn retrieve_from_root_locked(root: &Path, handle: &str, now: i64) -> Result<ResponseHandleLookup> {
    let path = response_handle_path(root, handle)?;
    let Some(stored) = read_record(&path)? else {
        return Ok(ResponseHandleLookup::Missing);
    };
    validate_record(handle, &stored, &path)?;
    if stored.expires_at <= now {
        PrivateStoreIo::remove_file_durable(&path)
            .map_err(|error| file_error(&path, "durably delete expired record", error))?;
        return Ok(ResponseHandleLookup::Expired {
            created_at: stored.created_at,
            expires_at: stored.expires_at,
        });
    }
    Ok(ResponseHandleLookup::Found(ResponseHandleRecord {
        handle: handle.to_owned(),
        created_at: stored.created_at,
        expires_at: stored.expires_at,
        content: stored.content,
    }))
}

pub fn cleanup_expired_response_handles(
    project_root: &Path,
    now: i64,
) -> Result<ResponseHandleCleanup> {
    let root = resolve_response_handle_root(project_root)?;
    cleanup_expired_response_handles_in_root(&root, now)
}

fn cleanup_expired_response_handles_in_root(
    root: &Path,
    now: i64,
) -> Result<ResponseHandleCleanup> {
    validate_response_handle_path(root)?;
    if !path_exists(root)? {
        return Ok(ResponseHandleCleanup::default());
    }
    with_exclusive_lock(root, || {
        let removed_staging = remove_abandoned_staging_files(root)?;
        let removed_tombstones = remove_orphaned_removal_tombstones(root)?;
        let mut cleanup = ResponseHandleCleanup {
            scanned: 0,
            removed_expired: 0,
            removed_staging,
            removed_tombstones,
        };
        cleanup.scanned = visit_stored_files(root, |file| {
            if file.record.expires_at <= now
                && PrivateStoreIo::remove_file_durable(&file.path).map_err(|error| {
                    file_error(&file.path, "durably delete expired record", error)
                })?
            {
                cleanup.removed_expired += 1;
            }
            Ok(())
        })?;
        Ok(cleanup)
    })
}

pub fn inventory_response_handles(project_root: &Path) -> Result<ResponseHandleInventory> {
    let root = resolve_response_handle_root(project_root)?;
    inventory_response_handles_in_root(&root)
}

fn inventory_response_handles_in_root(root: &Path) -> Result<ResponseHandleInventory> {
    validate_response_handle_path(root)?;
    if !path_exists(root)? {
        return Ok(ResponseHandleInventory::default());
    }
    with_exclusive_lock(root, || {
        let mut inventory = ResponseHandleInventory::default();
        visit_stored_files(root, |file| {
            inventory.file_count += 1;
            inventory.total_bytes = inventory.total_bytes.saturating_add(file.bytes);
            inventory.oldest_expires_at = Some(
                inventory
                    .oldest_expires_at
                    .map_or(file.record.expires_at, |value| {
                        value.min(file.record.expires_at)
                    }),
            );
            inventory.newest_expires_at = Some(
                inventory
                    .newest_expires_at
                    .map_or(file.record.expires_at, |value| {
                        value.max(file.record.expires_at)
                    }),
            );
            Ok(())
        })?;
        Ok(inventory)
    })
}

fn visit_stored_files(
    root: &Path,
    mut visit: impl FnMut(StoredFile) -> Result<()>,
) -> Result<usize> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(file_error(root, "read response-handle directory", error)),
    };
    let mut scanned = 0;
    for entry in entries {
        let entry = entry.map_err(|error| file_error(root, "read directory entry", error))?;
        let path = entry.path();
        let Some(handle) = handle_from_record_path(&path)? else {
            continue;
        };
        if !entry
            .file_type()
            .map_err(|error| file_error(&path, "read file type", error))?
            .is_file()
        {
            return Err(corrupt_record_error(&path, "record is not a regular file"));
        }
        let bytes = entry
            .metadata()
            .map_err(|error| file_error(&path, "read metadata", error))?
            .len();
        let record = read_record(&path)?.ok_or_else(|| {
            corrupt_record_error(&path, "record disappeared during directory scan")
        })?;
        validate_record(&handle, &record, &path)?;
        visit(StoredFile {
            path,
            bytes,
            record,
        })?;
        scanned += 1;
    }
    Ok(scanned)
}

fn remove_abandoned_staging_files(root: &Path) -> Result<usize> {
    let entries = fs::read_dir(root)
        .map_err(|error| file_error(root, "read response-handle directory", error))?;
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|error| file_error(root, "read directory entry", error))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(STAGING_PREFIX) {
            continue;
        }
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| file_error(&path, "read staging file type", error))?
            .is_file()
        {
            return Err(corrupt_record_error(
                &path,
                "staging record is not a regular file",
            ));
        }
        if PrivateStoreIo::remove_file_durable(&path)
            .map_err(|error| file_error(&path, "durably remove staging record", error))?
        {
            removed += 1;
        }
    }
    Ok(removed)
}

fn remove_orphaned_removal_tombstones(root: &Path) -> Result<usize> {
    let entries = fs::read_dir(root)
        .map_err(|error| file_error(root, "read response-handle directory", error))?;
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|error| file_error(root, "read directory entry", error))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(DURABLE_REMOVAL_TOMBSTONE_PREFIX) {
            continue;
        }
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|error| file_error(&path, "read removal tombstone type", error))?
            .is_file()
        {
            return Err(corrupt_record_error(
                &path,
                "durable-removal tombstone is not a regular file",
            ));
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(file_error(&path, "remove durable-removal tombstone", error));
            }
        }
    }
    Ok(removed)
}

fn read_record(path: &Path) -> Result<Option<StoredResponseHandleRecord>> {
    validate_response_handle_path(path)?;
    let payload = match fs::read(path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(file_error(path, "read response handle", error)),
    };
    serde_json::from_slice(&payload)
        .map(Some)
        .map_err(|error| corrupt_record_error(path, &error.to_string()))
}

fn handle_from_record_path(path: &Path) -> Result<Option<String>> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
        return Ok(None);
    }
    let handle = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| corrupt_record_error(path, "record filename is not valid UTF-8"))?;
    validate_handle(handle).map_err(|error| corrupt_record_error(path, &error.to_string()))?;
    Ok(Some(handle.to_owned()))
}

fn validate_record(handle: &str, record: &StoredResponseHandleRecord, path: &Path) -> Result<()> {
    if record.expires_at != record.created_at.saturating_add(RESPONSE_HANDLE_TTL_SECS) {
        return Err(corrupt_record_error(
            path,
            "record has a noncanonical expiry",
        ));
    }
    if response_handle_for(&record.content) != handle {
        return Err(corrupt_record_error(
            path,
            "record digest does not match its handle",
        ));
    }
    Ok(())
}

fn response_handle_for(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    format!(
        "{HANDLE_PREFIX}{}",
        hex::encode(&digest[..HANDLE_HEX_CHARS / 2])
    )
}

fn response_handle_path(root: &Path, handle: &str) -> Result<PathBuf> {
    validate_handle(handle)?;
    Ok(root.join(format!("{handle}.json")))
}

fn validate_handle(handle: &str) -> Result<()> {
    if is_valid_response_handle(handle) {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!(
            "invalid response handle: expected `{HANDLE_PREFIX}` followed by {HANDLE_HEX_CHARS} hex characters copied from a truncated MCP response envelope"
        ),
    })
}

fn path_exists(path: &Path) -> Result<bool> {
    path.try_exists()
        .map_err(|error| file_error(path, "check response-handle root", error))
}

fn validate_response_handle_path(path: &Path) -> Result<()> {
    reject_symlink_components(path, "response-handle cache")
        .map_err(|error| file_error(path, "validate response-handle path", error))
}

fn with_exclusive_lock<T>(root: &Path, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    validate_response_handle_path(root)?;
    let parent = root.parent().ok_or_else(|| TraceDecayError::File {
        message: "response-handle root has no stable lock parent".to_string(),
        path: root.display().to_string(),
    })?;
    let leaf = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| TraceDecayError::File {
            message: "response-handle root name is not valid UTF-8".to_string(),
            path: root.display().to_string(),
        })?;
    let path = parent.join(format!(".{leaf}{LOCK_SUFFIX}"));
    validate_response_handle_path(&path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| file_error(&path, "open response-handle lock", error))?;
    lock.try_lock_exclusive()
        .map_err(|error| file_error(&path, "acquire response-handle lock", error))?;
    let result = operation();
    let unlock = FileExt::unlock(&lock)
        .map_err(|error| file_error(&path, "release response-handle lock", error));
    match (result, unlock) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(operation_error), Err(unlock_error)) => Err(TraceDecayError::File {
            message: format!("{operation_error}; additionally {unlock_error}"),
            path: path.display().to_string(),
        }),
    }
}

fn publish_record_durable(root: &Path, path: &Path, payload: &[u8]) -> Result<()> {
    let temporary = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempfile_in(root)
        .map_err(|error| file_error(root, "create temporary record", error))?;
    let temporary_path = temporary
        .into_temp_path()
        .keep()
        .map_err(|error| file_error(root, "retain temporary record", error.into()))?;
    match PrivateStoreIo::write_file_atomically_durable(path, &temporary_path, payload) {
        Ok(()) => Ok(()),
        Err(error) => {
            let failure = file_error(path, "durably publish response handle", error);
            match PrivateStoreIo::remove_file_durable(&temporary_path) {
                Ok(_) => Err(failure),
                Err(cleanup_error) => Err(TraceDecayError::File {
                    message: format!(
                        "{failure}; additionally failed to durably remove temporary record: {cleanup_error}"
                    ),
                    path: temporary_path.display().to_string(),
                }),
            }
        }
    }
}

fn remove_failed_fresh_publish(path: &Path, expected: &StoredResponseHandleRecord) -> Result<()> {
    let Some(actual) = read_record(path)? else {
        return Ok(());
    };
    if &actual != expected {
        return Err(corrupt_record_error(
            path,
            "refused to remove a failed publication whose record material does not match",
        ));
    }
    PrivateStoreIo::remove_file_durable(path)
        .map(|_| ())
        .map_err(|error| file_error(path, "durably remove failed publication", error))
}

fn file_error(path: &Path, operation: &str, error: std::io::Error) -> TraceDecayError {
    TraceDecayError::File {
        message: format!("failed to {operation}: {error}"),
        path: path.display().to_string(),
    }
}

fn corrupt_record_error(path: &Path, reason: &str) -> TraceDecayError {
    TraceDecayError::File {
        message: format!("corrupt response-handle record: {reason}"),
        path: path.display().to_string(),
    }
}

fn is_corrupt_record_error(error: &TraceDecayError) -> bool {
    matches!(
        error,
        TraceDecayError::File { message, .. }
            if message.starts_with("corrupt response-handle record:")
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;
    use tracedecay_runtime_core::storage::{
        DurableAtomicWriteFaultForTest, with_durable_atomic_write_fault_for_test,
    };

    #[test]
    fn handles_and_character_counts_are_canonical() {
        assert!(!is_valid_response_handle("rh_ABCDEF000000000000000000"));
        let record = ResponseHandleRecord {
            handle: "rh_000000000000000000000000".into(),
            created_at: 0,
            expires_at: RESPONSE_HANDLE_TTL_SECS,
            content: "é🦀".into(),
        };
        assert_eq!(record.original_chars(), 2);
    }

    #[test]
    fn same_content_renews_the_expiry() {
        let root = tempfile::tempdir().unwrap();
        let first = store_response_handle_in_root(root.path(), "payload", 10).unwrap();
        let second = store_response_handle_in_root(root.path(), "payload", 20).unwrap();

        assert_eq!(second.handle, first.handle);
        assert_eq!(first.created_at, 10);
        assert_eq!(second.created_at, 20);
        assert_eq!(second.expires_at, 20 + RESPONSE_HANDLE_TTL_SECS);
        assert_eq!(
            inventory_response_handles_in_root(root.path())
                .unwrap()
                .file_count,
            1
        );
        let ResponseHandleLookup::Found(persisted) =
            retrieve_from_root(root.path(), &second.handle, 20).unwrap()
        else {
            panic!("renewed response handle was not retrievable");
        };
        assert_eq!(persisted.created_at, 20);

        let other_root = tempfile::tempdir().unwrap();
        assert!(matches!(
            retrieve_from_root(other_root.path(), &second.handle, 20).unwrap(),
            ResponseHandleLookup::Missing
        ));
    }

    #[test]
    fn concurrent_identical_stores_publish_one_complete_record() {
        let root = tempfile::tempdir().unwrap();
        let root = Arc::new(root.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(2));
        let workers = [100, 100].map(|now| {
            let root = Arc::clone(&root);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                store_response_handle_in_root(&root, "shared payload", now)
            })
        });
        for result in workers.map(|worker| worker.join().unwrap()) {
            if let Err(error) = result {
                assert!(matches!(
                    error,
                    TraceDecayError::File { message, .. }
                        if message.contains("acquire response-handle lock")
                ));
            }
        }
        let first = store_response_handle_in_root(&root, "shared payload", 100).unwrap();

        assert_eq!(
            inventory_response_handles_in_root(&root)
                .unwrap()
                .file_count,
            1
        );
        let ResponseHandleLookup::Found(persisted) =
            retrieve_from_root(&root, &first.handle, 100).unwrap()
        else {
            panic!("concurrent response handle was not retrievable");
        };
        assert_eq!(persisted.content, "shared payload");

        use fs2::FileExt as _;
        use std::sync::mpsc;
        use std::time::Duration;

        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().file_name().unwrap().to_str().unwrap();
        let lock_path = root
            .path()
            .parent()
            .unwrap()
            .join(format!(".{leaf}{LOCK_SUFFIX}"));
        let held = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        held.lock_exclusive().unwrap();

        let worker_root = root.path().to_path_buf();
        let (sent, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = sent.send(inventory_response_handles_in_root(&worker_root));
        });
        let early = received.recv_timeout(Duration::from_millis(250));
        FileExt::unlock(&held).unwrap();
        let result = match early {
            Ok(result) => result,
            Err(error) => {
                worker.join().unwrap();
                panic!("lock acquisition blocked instead of failing closed: {error}");
            }
        };
        worker.join().unwrap();

        assert!(matches!(
            result,
            Err(TraceDecayError::File { message, .. })
                if message.contains("acquire response-handle lock")
        ));
    }

    #[test]
    fn expiry_cleanup_removes_the_exact_record() {
        let root = tempfile::tempdir().unwrap();
        let record = store_response_handle_in_root(root.path(), "expired", 10).unwrap();
        let cleanup =
            cleanup_expired_response_handles_in_root(root.path(), 10 + RESPONSE_HANDLE_TTL_SECS)
                .unwrap();

        assert_eq!(cleanup.scanned, 1);
        assert_eq!(cleanup.removed_expired, 1);
        assert!(matches!(
            retrieve_from_root(root.path(), &record.handle, 20).unwrap(),
            ResponseHandleLookup::Missing
        ));
    }

    #[test]
    fn cleanup_and_inventory_reject_corrupt_records() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rh_000000000000000000000000.json");
        fs::write(&path, b"{}").unwrap();

        assert!(matches!(
            inventory_response_handles_in_root(root.path()),
            Err(TraceDecayError::File { message, path: error_path })
                if message.contains("corrupt response-handle record")
                    && error_path == path.display().to_string()
        ));
        assert!(matches!(
            cleanup_expired_response_handles_in_root(root.path(), 10),
            Err(TraceDecayError::File { message, path: error_path })
                if message.contains("corrupt response-handle record")
                    && error_path == path.display().to_string()
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let owner = tempfile::tempdir().unwrap();
            let attacker = tempfile::tempdir().unwrap();
            let stored =
                store_response_handle_in_root(owner.path(), "private payload", 10).unwrap();
            let linked_root = attacker.path().join("response-handles");
            symlink(owner.path(), &linked_root).unwrap();

            assert!(matches!(
                retrieve_from_root(&linked_root, &stored.handle, 10),
                Err(TraceDecayError::File { message, .. })
                    if message.contains("must not contain symlinks")
            ));
            assert!(inventory_response_handles_in_root(&linked_root).is_err());
            assert!(cleanup_expired_response_handles_in_root(&linked_root, 10).is_err());
        }
    }

    #[test]
    fn cleanup_durably_removes_abandoned_staging_records() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join(format!("{STAGING_PREFIX}orphan"));
        fs::write(&staging, b"partial").unwrap();
        let tombstone = root
            .path()
            .join(format!("{DURABLE_REMOVAL_TOMBSTONE_PREFIX}orphan"));
        fs::write(&tombstone, b"private response payload").unwrap();

        let cleanup = cleanup_expired_response_handles_in_root(root.path(), 10).unwrap();

        assert_eq!(cleanup.scanned, 0);
        assert_eq!(cleanup.removed_expired, 0);
        assert_eq!(cleanup.removed_staging, 1);
        assert_eq!(cleanup.removed_tombstones, 1);
        assert!(!staging.exists());
        assert!(!tombstone.exists());
    }

    #[test]
    fn identical_store_replaces_a_corrupt_record() {
        let root = tempfile::tempdir().unwrap();
        let original = store_response_handle_in_root(root.path(), "recover me", 10).unwrap();
        let path = root.path().join(format!("{}.json", original.handle));
        fs::write(&path, b"{not-json").unwrap();

        let restored = store_response_handle_in_root(root.path(), "recover me", 20).unwrap();

        assert_eq!(restored.handle, original.handle);
        assert_eq!(restored.created_at, 20);
        assert_eq!(restored.expires_at, 20 + RESPONSE_HANDLE_TTL_SECS);
        let ResponseHandleLookup::Found(persisted) =
            retrieve_from_root(root.path(), &restored.handle, 20).unwrap()
        else {
            panic!("restored response handle was not retrievable");
        };
        assert_eq!(persisted.content, "recover me");
        assert_eq!(persisted.created_at, 20);
    }

    #[test]
    fn interrupted_publish_exposes_no_record_and_retry_recovers() {
        let root = tempfile::tempdir().unwrap();

        assert!(
            with_durable_atomic_write_fault_for_test(
                DurableAtomicWriteFaultForTest::AfterTempSync,
                || store_response_handle_in_root(root.path(), "retry me", 10),
            )
            .is_err()
        );
        let handle = response_handle_for("retry me");
        assert!(matches!(
            retrieve_from_root(root.path(), &handle, 10).unwrap(),
            ResponseHandleLookup::Missing
        ));

        let recovered = store_response_handle_in_root(root.path(), "retry me", 20).unwrap();
        assert_eq!(recovered.handle, handle);
        let ResponseHandleLookup::Found(persisted) =
            retrieve_from_root(root.path(), &handle, 20).unwrap()
        else {
            panic!("retried response handle was not retrievable");
        };
        assert_eq!(persisted.content, "retry me");
    }

    #[test]
    fn post_rename_failure_durably_removes_record_before_retry() {
        let root = tempfile::tempdir().unwrap();

        assert!(
            with_durable_atomic_write_fault_for_test(
                DurableAtomicWriteFaultForTest::AfterRename,
                || store_response_handle_in_root(root.path(), "retry after rename", 10),
            )
            .is_err()
        );
        let handle = response_handle_for("retry after rename");
        assert!(matches!(
            retrieve_from_root(root.path(), &handle, 10).unwrap(),
            ResponseHandleLookup::Missing
        ));

        let recovered =
            store_response_handle_in_root(root.path(), "retry after rename", 20).unwrap();
        assert_eq!(recovered.handle, handle);
        assert!(matches!(
            retrieve_from_root(root.path(), &handle, 20).unwrap(),
            ResponseHandleLookup::Found(_)
        ));
    }

    #[test]
    fn failed_renewal_restores_the_previously_issued_record() {
        let root = tempfile::tempdir().unwrap();
        let original = store_response_handle_in_root(root.path(), "renew safely", 10).unwrap();

        assert!(
            with_durable_atomic_write_fault_for_test(
                DurableAtomicWriteFaultForTest::AfterRename,
                || store_response_handle_in_root(root.path(), "renew safely", 20),
            )
            .is_err()
        );

        let ResponseHandleLookup::Found(restored) =
            retrieve_from_root(root.path(), &original.handle, 11).unwrap()
        else {
            panic!("failed renewal must preserve the previously issued record");
        };
        assert_eq!(restored.created_at, original.created_at);
        assert_eq!(restored.expires_at, original.expires_at);
        assert_eq!(restored.content, original.content);
    }
}
