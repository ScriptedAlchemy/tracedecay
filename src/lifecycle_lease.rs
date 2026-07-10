use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::errors::{Result, TraceDecayError};

const LIFECYCLE_LOCK_FILENAME: &str = "lifecycle.lock";
static LEASE_NONCE: AtomicU64 = AtomicU64::new(0);
static PROCESS_LEASE_TOKENS: LazyLock<Mutex<Vec<String>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

#[derive(Debug)]
enum LeaseHold {
    File(File),
    Inherited,
}

/// Cross-process guard for operations that may replace binaries, restart the
/// daemon, or inspect stores while those mutations are in progress.
#[derive(Debug)]
pub struct LifecycleLease {
    hold: LeaseHold,
    token: Option<String>,
    lock_path: PathBuf,
    exclusive: bool,
}

#[derive(Debug)]
pub enum SharedLeaseAttempt {
    Acquired(LifecycleLease),
    Busy,
}

impl LifecycleLease {
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub fn is_exclusive(&self) -> bool {
        self.exclusive
    }

    pub fn guards_profile(&self, profile_root: &Path) -> bool {
        let expected = profile_root.join(LIFECYCLE_LOCK_FILENAME);
        canonical_or_original(&self.lock_path) == canonical_or_original(&expected)
    }
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

impl Drop for LifecycleLease {
    fn drop(&mut self) {
        if let Some(token) = self.token.as_deref() {
            unregister_process_token(token);
        }
        if let LeaseHold::File(file) = &self.hold {
            let _ = fs2::FileExt::unlock(file);
        }
    }
}

pub fn acquire_exclusive(operation: &str) -> Result<LifecycleLease> {
    acquire_exclusive_at(&lifecycle_lock_path()?, operation)
}

/// Acquires the lifecycle lease rooted in an explicit profile. Migration
/// commands use this instead of ambient HOME so synthetic profiles and
/// user-selected profile roots cannot accidentally lock a different store.
pub fn acquire_exclusive_for_profile(
    profile_root: &Path,
    operation: &str,
) -> Result<LifecycleLease> {
    acquire_exclusive_at(&lifecycle_lock_path_for_profile(profile_root)?, operation)
}

pub fn acquire_shared(operation: &str) -> Result<LifecycleLease> {
    acquire_shared_at(&lifecycle_lock_path()?, operation)
}

/// Attempts to acquire a non-inherited shared lease without blocking.
pub fn try_acquire_shared(operation: &str) -> Result<SharedLeaseAttempt> {
    try_acquire_shared_at(&lifecycle_lock_path()?, operation)
}

pub fn try_acquire_shared_for_profile(
    profile_root: &Path,
    operation: &str,
) -> Result<SharedLeaseAttempt> {
    try_acquire_shared_at(&lifecycle_lock_path_for_profile(profile_root)?, operation)
}

/// Acquires a shared diagnostic lease, or joins the exclusive lease held by
/// this process's post-update parent.
pub fn acquire_shared_or_inherited(operation: &str) -> Result<LifecycleLease> {
    let path = lifecycle_lock_path()?;
    acquire_shared_or_inherited_at(&path, operation)
}

/// Attempts to acquire a shared lifecycle lease without blocking. A live
/// unrelated exclusive owner is reported as [`SharedLeaseAttempt::Busy`];
/// lock-file and profile configuration failures remain errors.
pub fn try_acquire_shared_or_inherited(operation: &str) -> Result<SharedLeaseAttempt> {
    let path = lifecycle_lock_path()?;
    try_acquire_shared_or_inherited_at(&path, operation)
}

/// Explicit-profile counterpart used when ambient HOME/profile resolution is
/// not authoritative.
pub fn try_acquire_shared_or_inherited_for_profile(
    profile_root: &Path,
    operation: &str,
) -> Result<SharedLeaseAttempt> {
    try_acquire_shared_or_inherited_at(&lifecycle_lock_path_for_profile(profile_root)?, operation)
}

fn acquire_shared_or_inherited_at(path: &Path, operation: &str) -> Result<LifecycleLease> {
    let mut file = open_lock_file(path)?;
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(LifecycleLease {
            hold: LeaseHold::File(file),
            token: None,
            lock_path: path.to_path_buf(),
            exclusive: false,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            let owner = read_owner(&mut file);
            let owner_token = owner.as_deref().and_then(|line| line.split('\t').next());
            if owner_token.is_some_and(process_owns_token) {
                Ok(LifecycleLease {
                    hold: LeaseHold::Inherited,
                    token: None,
                    lock_path: path.to_path_buf(),
                    exclusive: false,
                })
            } else {
                Err(busy_error(operation, owner.as_deref()))
            }
        }
        Err(error) => Err(lock_error(path, operation, &error)),
    }
}

fn try_acquire_shared_or_inherited_at(path: &Path, operation: &str) -> Result<SharedLeaseAttempt> {
    let mut file = open_lock_file(path)?;
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(SharedLeaseAttempt::Acquired(LifecycleLease {
            hold: LeaseHold::File(file),
            token: None,
            lock_path: path.to_path_buf(),
            exclusive: false,
        })),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            let owner = read_owner(&mut file);
            let owner_token = owner.as_deref().and_then(|line| line.split('\t').next());
            if owner_token.is_some_and(process_owns_token) {
                Ok(SharedLeaseAttempt::Acquired(LifecycleLease {
                    hold: LeaseHold::Inherited,
                    token: None,
                    lock_path: path.to_path_buf(),
                    exclusive: false,
                }))
            } else {
                Ok(SharedLeaseAttempt::Busy)
            }
        }
        Err(error) => Err(lock_error(path, operation, &error)),
    }
}

/// Acquires the lifecycle lease, or proves that this process is the
/// post-update child of the process that still owns it.
pub fn acquire_exclusive_or_inherited(
    operation: &str,
    inherited_token: Option<&str>,
) -> Result<LifecycleLease> {
    acquire_exclusive_or_inherited_at(
        &lifecycle_lock_path()?,
        operation,
        inherited_token.map(str::to_string),
    )
}

fn acquire_exclusive_or_inherited_at(
    path: &Path,
    operation: &str,
    inherited: Option<String>,
) -> Result<LifecycleLease> {
    let mut file = open_lock_file(path)?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => own_exclusive(file, path, operation),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            let owner = read_owner(&mut file);
            if let Some(token) = inherited.filter(|token| {
                owner.as_deref().and_then(|line| line.split('\t').next()) == Some(token.as_str())
            }) {
                register_process_token(&token);
                Ok(LifecycleLease {
                    hold: LeaseHold::Inherited,
                    token: Some(token),
                    lock_path: path.to_path_buf(),
                    exclusive: true,
                })
            } else {
                Err(busy_error(operation, owner.as_deref()))
            }
        }
        Err(error) => Err(lock_error(path, operation, &error)),
    }
}

fn lifecycle_lock_path() -> Result<PathBuf> {
    let root = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory for lifecycle lease"
            .to_string(),
    })?;
    std::fs::create_dir_all(&root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create TraceDecay user data directory '{}': {error}",
            root.display()
        ),
    })?;
    Ok(root.join(LIFECYCLE_LOCK_FILENAME))
}

fn lifecycle_lock_path_for_profile(profile_root: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(profile_root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create TraceDecay profile root '{}': {error}",
            profile_root.display()
        ),
    })?;
    Ok(profile_root.join(LIFECYCLE_LOCK_FILENAME))
}

fn acquire_exclusive_at(path: &Path, operation: &str) -> Result<LifecycleLease> {
    let file = open_lock_file(path)?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => own_exclusive(file, path, operation),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            let mut file = file;
            let owner = read_owner(&mut file);
            Err(busy_error(operation, owner.as_deref()))
        }
        Err(error) => Err(lock_error(path, operation, &error)),
    }
}

fn acquire_shared_at(path: &Path, operation: &str) -> Result<LifecycleLease> {
    let mut file = open_lock_file(path)?;
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(LifecycleLease {
            hold: LeaseHold::File(file),
            token: None,
            lock_path: path.to_path_buf(),
            exclusive: false,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            let owner = read_owner(&mut file);
            Err(busy_error(operation, owner.as_deref()))
        }
        Err(error) => Err(lock_error(path, operation, &error)),
    }
}

fn try_acquire_shared_at(path: &Path, operation: &str) -> Result<SharedLeaseAttempt> {
    let file = open_lock_file(path)?;
    match fs2::FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(SharedLeaseAttempt::Acquired(LifecycleLease {
            hold: LeaseHold::File(file),
            token: None,
            lock_path: path.to_path_buf(),
            exclusive: false,
        })),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Ok(SharedLeaseAttempt::Busy)
        }
        Err(error) => Err(lock_error(path, operation, &error)),
    }
}

fn own_exclusive(mut file: File, path: &Path, operation: &str) -> Result<LifecycleLease> {
    let token = lease_token();
    file.set_len(0).map_err(|error| owner_write_error(&error))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| owner_write_error(&error))?;
    writeln!(file, "{token}\t{operation}\t{}", std::process::id())
        .map_err(|error| owner_write_error(&error))?;
    file.flush().map_err(|error| owner_write_error(&error))?;
    register_process_token(&token);
    Ok(LifecycleLease {
        hold: LeaseHold::File(file),
        token: Some(token),
        lock_path: path.to_path_buf(),
        exclusive: true,
    })
}

fn register_process_token(token: &str) {
    PROCESS_LEASE_TOKENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(token.to_string());
}

fn unregister_process_token(token: &str) {
    let mut tokens = PROCESS_LEASE_TOKENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(index) = tokens.iter().rposition(|candidate| candidate == token) {
        tokens.swap_remove(index);
    }
}

fn process_owns_token(token: &str) -> bool {
    PROCESS_LEASE_TOKENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|candidate| candidate == token)
}

fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| lock_error(path, "open", &error))?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| lock_error(path, "open", &error))
}

fn read_owner(file: &mut File) -> Option<String> {
    let mut owner = String::new();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.read_to_string(&mut owner).ok()?;
    let owner = owner.trim();
    (!owner.is_empty()).then(|| owner.to_string())
}

fn lease_token() -> String {
    let epoch_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = LEASE_NONCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}:{}:{epoch_nanos}:{nonce}",
        crate::runtime_identity::process_run_id(),
        std::process::id()
    )
}

fn busy_error(operation: &str, owner: Option<&str>) -> TraceDecayError {
    let owner_operation = owner
        .and_then(|line| line.split('\t').nth(1))
        .unwrap_or("another lifecycle operation");
    TraceDecayError::Config {
        message: format!(
            "cannot start {operation}: {owner_operation} is already active; retry after it finishes"
        ),
    }
}

fn lock_error(path: &Path, operation: &str, error: &std::io::Error) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "failed to acquire lifecycle lease for {operation} at '{}': {error}",
            path.display()
        ),
    }
}

fn owner_write_error(error: &std::io::Error) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("failed to record TraceDecay lifecycle lease owner: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::{
        SharedLeaseAttempt, acquire_exclusive_at, acquire_exclusive_or_inherited_at,
        acquire_shared_at, acquire_shared_or_inherited_at, try_acquire_shared_at,
        try_acquire_shared_or_inherited_at, try_acquire_shared_or_inherited_for_profile,
    };

    #[test]
    fn exclusive_lease_rejects_a_concurrent_mutator() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lifecycle.lock");
        let held = acquire_exclusive_at(&path, "upgrade").unwrap();

        let error = acquire_exclusive_at(&path, "update").unwrap_err();

        assert!(error.to_string().contains("upgrade"));
        drop(held);
        acquire_exclusive_at(&path, "update").unwrap();
    }

    #[test]
    fn shared_doctor_lease_blocks_mutation_but_not_another_reader() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lifecycle.lock");
        let first = acquire_shared_at(&path, "doctor").unwrap();
        let second = acquire_shared_at(&path, "doctor").unwrap();

        let error = acquire_exclusive_at(&path, "upgrade").unwrap_err();

        assert!(error.to_string().contains("lifecycle operation"));
        drop((first, second));
        acquire_exclusive_at(&path, "upgrade").unwrap();
    }

    #[test]
    fn nested_doctor_joins_the_process_owned_exclusive_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lifecycle.lock");
        let _parent = acquire_exclusive_at(&path, "post-update").unwrap();

        acquire_shared_or_inherited_at(&path, "doctor").unwrap();
        assert!(matches!(
            try_acquire_shared_or_inherited_at(&path, "hook").unwrap(),
            SharedLeaseAttempt::Acquired(_)
        ));
    }

    #[test]
    fn nonblocking_shared_attempt_reports_an_unrelated_exclusive_owner_as_busy() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lifecycle.lock");
        let mut external = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&external).unwrap();
        writeln!(external, "external-token\tmigration\t999").unwrap();
        external.flush().unwrap();

        assert!(matches!(
            try_acquire_shared_or_inherited_at(&path, "hook").unwrap(),
            SharedLeaseAttempt::Busy
        ));
    }

    #[test]
    fn noninherited_shared_attempt_does_not_join_a_process_owned_exclusive_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lifecycle.lock");
        let _parent = acquire_exclusive_at(&path, "update").unwrap();

        assert!(matches!(
            try_acquire_shared_at(&path, "hook").unwrap(),
            SharedLeaseAttempt::Busy
        ));
    }

    #[test]
    fn nonblocking_shared_attempt_preserves_profile_io_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let not_a_directory = tmp.path().join("profile-file");
        std::fs::write(&not_a_directory, "not a directory").unwrap();

        let error =
            try_acquire_shared_or_inherited_for_profile(&not_a_directory, "hook").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("failed to create TraceDecay profile root")
        );
    }

    #[test]
    fn post_update_child_must_present_the_live_parent_token() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lifecycle.lock");
        let parent = acquire_exclusive_at(&path, "update").unwrap();
        let token = parent.token().unwrap().to_string();

        acquire_exclusive_or_inherited_at(&path, "post-update", Some(token)).unwrap();
        let error = acquire_exclusive_or_inherited_at(
            &path,
            "post-update",
            Some("stale-token".to_string()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("update"));
    }
}
