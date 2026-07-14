use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, LazyLock, Mutex};

use crate::errors::{Result, TraceDecayError};

mod bootstrap;
mod lease;
mod owner_io;
mod path_layout;

use bootstrap::{BootstrapAuthority, acquire_bootstrap_authority, reject_hard_linked_database};
pub use lease::enter_maintenance_database_scope;
use lease::{acquire_process_lease, exact_scoped_runtime_role, scoped_runtime_role};
pub(crate) use lease::{enter_daemon_database_scope, probe_writer_owner};
use owner_io::{
    authority_token, epoch_ms, is_lock_contended, open_lock_file, publish_record_atomically,
    read_owner, write_owner, writer_owner,
};
use path_layout::{
    bootstrap_database_key, canonical_profile_root, database_lock_root,
    is_legacy_repository_database, platform_identity_key, stable_path_hash,
};

static PROCESS_LEASES: LazyLock<Mutex<HashMap<PathBuf, ProcessLease>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DAEMON_SCOPES: LazyLock<Mutex<HashMap<PathBuf, DaemonScopeState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static MAINTENANCE_SCOPES: LazyLock<Mutex<HashMap<PathBuf, MaintenanceScopeState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static AUTHORITY_NONCE: AtomicU64 = AtomicU64::new(0);
static PROCESS_STARTED_EPOCH_MS: LazyLock<u128> = LazyLock::new(epoch_ms);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseAuthorityRole {
    Daemon,
    Maintenance,
    #[doc(hidden)]
    Test,
}

#[derive(Clone, Debug)]
pub struct DatabaseAuthority {
    inner: Arc<AuthorityInner>,
}

#[derive(Debug)]
pub(crate) struct DaemonDatabaseScope {
    profile_root: PathBuf,
    token: String,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct MaintenanceDatabaseScope<'lease> {
    profile_root: PathBuf,
    token: String,
    _lifecycle: std::marker::PhantomData<&'lease crate::lifecycle_lease::LifecycleLease>,
}

#[derive(Debug)]
struct DaemonScopeState {
    token: String,
    refs: usize,
}

#[derive(Debug)]
struct MaintenanceScopeState {
    token: String,
    refs: usize,
}

#[derive(Debug)]
struct AuthorityInner {
    identity: DatabaseIdentity,
    role: DatabaseAuthorityRole,
    token: String,
    _bootstrap: Option<BootstrapAuthority>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DatabaseIdentity {
    database_path: PathBuf,
    database_key: PathBuf,
    profile_root: PathBuf,
    allows_ambient_profile_scope: bool,
    access_lock_path: PathBuf,
    writer_lock_path: PathBuf,
    writer_owner_path: PathBuf,
    bootstrap_lock_path: Option<PathBuf>,
}

#[derive(Debug)]
struct ProcessLease {
    token: String,
    refs: usize,
    held: HeldLocks,
}

#[derive(Debug)]
enum HeldLocks {
    Daemon {
        access: File,
        writer: File,
        owner: WriterOwner,
    },
    Maintenance {
        access: File,
        writer: File,
        owner: WriterOwner,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WriterOwner {
    pub(crate) token: String,
    pub(crate) pid: u32,
    pub(crate) started_epoch_ms: u128,
    pub(crate) version: String,
    pub(crate) intent: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WriterOwnership {
    Idle,
    Active(WriterOwner),
    ActiveUnknown,
}

impl DatabaseAuthority {
    #[cfg(test)]
    pub(crate) fn acquire_daemon(db_path: &Path, intent: &str) -> Result<Self> {
        let identity = DatabaseIdentity::for_path(db_path)?;
        if !DAEMON_SCOPES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&identity.profile_root)
        {
            return Err(access_error(
                intent,
                db_path,
                "database access is restricted to the elected managed daemon",
            ));
        }
        Self::acquire_identity(identity, DatabaseAuthorityRole::Daemon, intent)
    }

    #[cfg(test)]
    pub(crate) fn acquire_maintenance(db_path: &Path, intent: &str) -> Result<Self> {
        Self::acquire(db_path, DatabaseAuthorityRole::Maintenance, intent)
    }

    #[doc(hidden)]
    pub fn for_runtime(db_path: &Path, intent: &str) -> Result<Self> {
        let identity = DatabaseIdentity::for_path(db_path)?;
        if cfg!(debug_assertions) && is_isolated_test_path(&identity.database_path) {
            let existing_role = PROCESS_LEASES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&identity.database_key)
                .map(|lease| match &lease.held {
                    HeldLocks::Maintenance { .. } => DatabaseAuthorityRole::Maintenance,
                    HeldLocks::Daemon { .. } => DatabaseAuthorityRole::Test,
                });
            if let Some(role) = existing_role {
                return Self::acquire_identity(identity, role, intent);
            }
        }
        if let Some(role) = exact_scoped_runtime_role(&identity.profile_root, intent)? {
            return Self::acquire_identity(identity, role, intent);
        }
        let maintenance_active = PROCESS_LEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&identity.database_key)
            .is_some_and(|lease| matches!(&lease.held, HeldLocks::Maintenance { .. }));
        if maintenance_active {
            return Self::acquire_identity(identity, DatabaseAuthorityRole::Maintenance, intent);
        }
        if cfg!(debug_assertions) && is_isolated_test_path(&identity.database_path) {
            return Self::acquire_identity(identity, DatabaseAuthorityRole::Test, intent);
        }
        if let Some(role) = scoped_runtime_role(&identity, intent)? {
            return Self::acquire_identity(identity, role, intent);
        }
        Err(access_error(
            intent,
            &identity.database_path,
            "database access requires managed-daemon or exclusive-maintenance authority",
        ))
    }

    /// Test escape hatch for integration fixtures. Production paths are
    /// rejected even when a caller can reach this hidden API.
    #[doc(hidden)]
    pub fn acquire_test(db_path: &Path, intent: &str) -> Result<Self> {
        let identity = DatabaseIdentity::for_path(db_path)?;
        if !is_isolated_test_path(&identity.database_path) {
            return Err(access_error(
                "test authority",
                &identity.database_path,
                "test database must be inside the system temporary directory",
            ));
        }
        Self::acquire_identity(identity, DatabaseAuthorityRole::Test, intent)
    }

    pub fn role(&self) -> DatabaseAuthorityRole {
        self.inner.role
    }

    pub fn token(&self) -> &str {
        &self.inner.token
    }

    pub(crate) fn publish_record_atomically(
        temporary: &Path,
        destination: &Path,
        payload: &[u8],
        record_name: &str,
    ) -> Result<()> {
        publish_record_atomically(temporary, destination, payload, record_name)
    }

    pub(crate) fn replace_file_atomically(
        temporary: &Path,
        destination: &Path,
        record_name: &str,
    ) -> Result<()> {
        owner_io::replace_file_atomically(temporary, destination, record_name)
    }

    #[cfg(test)]
    fn acquire(db_path: &Path, role: DatabaseAuthorityRole, intent: &str) -> Result<Self> {
        Self::acquire_identity(DatabaseIdentity::for_path(db_path)?, role, intent)
    }

    fn acquire_identity(
        mut identity: DatabaseIdentity,
        role: DatabaseAuthorityRole,
        intent: &str,
    ) -> Result<Self> {
        let bootstrap = acquire_bootstrap_authority(&identity, intent)?;
        if bootstrap.is_some() {
            identity = DatabaseIdentity::for_path(&identity.database_path)?;
        }
        let token = acquire_process_lease(&identity, role, intent)?;
        Ok(Self {
            inner: Arc::new(AuthorityInner {
                identity,
                role,
                token,
                _bootstrap: bootstrap,
            }),
        })
    }

    pub(crate) fn hold_for(&self, db_path: &Path, operation: &str) -> Result<Self> {
        let identity = DatabaseIdentity::for_path(db_path)?;
        if identity.database_key != self.inner.identity.database_key {
            return Err(access_error(
                operation,
                &identity.database_path,
                "database authority belongs to a different database",
            ));
        }
        Ok(self.clone())
    }

    pub(crate) fn canonical_database_path(&self) -> &Path {
        &self.inner.identity.database_path
    }
}

impl DatabaseIdentity {
    fn for_path(db_path: &Path) -> Result<Self> {
        let absolute = if db_path.is_absolute() {
            db_path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| access_io_error("resolve", db_path, &error))?
                .join(db_path)
        };
        let file_name = absolute
            .file_name()
            .ok_or_else(|| access_error("resolve", db_path, "database path has no file name"))?;
        let parent = absolute.parent().ok_or_else(|| {
            access_error("resolve", db_path, "database path has no parent directory")
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|error| access_io_error("create lock directory", parent, &error))?;

        let entry = match std::fs::symlink_metadata(&absolute) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(access_io_error("inspect", &absolute, &error)),
        };
        let database_path = match entry.as_ref() {
            Some(metadata) if metadata.file_type().is_symlink() => absolute
                .canonicalize()
                .map_err(|_| access_error("resolve", &absolute, "database symlink is dangling"))?,
            Some(_) => absolute
                .canonicalize()
                .map_err(|error| access_io_error("resolve", &absolute, &error))?,
            None => parent
                .canonicalize()
                .map_err(|error| access_io_error("resolve parent", parent, &error))?
                .join(file_name),
        };
        if entry.is_some() {
            reject_hard_linked_database(&database_path)?;
        }
        let database_key = platform_identity_key(&database_path);
        let lock_root = database_lock_root(&database_path, parent);
        std::fs::create_dir_all(&lock_root).map_err(|error| {
            access_io_error("create database lock directory", &lock_root, &error)
        })?;
        let lock_id = stable_path_hash(&database_key);
        let bootstrap_lock_path = if entry.is_none() {
            bootstrap_database_key(
                database_path.parent().unwrap_or(parent),
                database_path.file_name().unwrap_or(file_name),
            )
            .map(|key| lock_root.join(format!("{:016x}.bootstrap.lock", stable_path_hash(&key))))
        } else {
            None
        };
        let profile_root = lock_root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| parent.to_path_buf());
        Ok(Self {
            allows_ambient_profile_scope: is_legacy_repository_database(&database_path),
            database_path,
            database_key,
            profile_root: platform_identity_key(&profile_root),
            access_lock_path: lock_root.join(format!("{lock_id:016x}.access.lock")),
            writer_lock_path: lock_root.join(format!("{lock_id:016x}.writer.lock")),
            writer_owner_path: lock_root.join(format!("{lock_id:016x}.writer.owner")),
            bootstrap_lock_path,
        })
    }
}

fn access_error(operation: &str, path: &Path, message: &str) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{message} at '{}'", path.display()),
        operation: operation.to_string(),
    }
}

fn access_io_error(operation: &str, path: &Path, error: &std::io::Error) -> TraceDecayError {
    access_error(operation, path, &error.to_string())
}

fn is_isolated_test_path(path: &Path) -> bool {
    let root = std::env::temp_dir();
    if path.starts_with(root.canonicalize().unwrap_or(root)) {
        return true;
    }
    cfg!(debug_assertions)
        && std::env::var_os("TRACEDECAY_DATA_DIR")
            .filter(|root| !root.is_empty())
            .map(PathBuf::from)
            .is_some_and(|root| {
                let root = if root.is_absolute() {
                    root
                } else {
                    std::env::current_dir()
                        .unwrap_or_else(|_| PathBuf::from("."))
                        .join(root)
                };
                path.starts_with(root.canonicalize().unwrap_or(root))
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    static SCOPE_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn canonical_identity_collapses_parent_aliases() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("nested")).unwrap();
        let direct = DatabaseIdentity::for_path(&temp.path().join("graph.db")).unwrap();
        let aliased = DatabaseIdentity::for_path(&temp.path().join("nested/../graph.db")).unwrap();
        assert_eq!(direct, aliased);
    }

    #[test]
    fn identity_key_preserves_unproven_case_variants() {
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("MixedCase.DB");
        let lower = temp.path().join("mixedcase.db");

        assert_ne!(platform_identity_key(&upper), platform_identity_key(&lower));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn case_distinct_database_files_have_distinct_identities() {
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("MixedCase.DB");
        let lower = temp.path().join("mixedcase.db");
        std::fs::write(&upper, []).unwrap();
        std::fs::write(&lower, []).unwrap();

        let upper = DatabaseIdentity::for_path(&upper).unwrap();
        let lower = DatabaseIdentity::for_path(&lower).unwrap();

        assert_ne!(upper.database_key, lower.database_key);
        assert_ne!(upper.writer_lock_path, lower.writer_lock_path);

        let upper_authority = DatabaseAuthority::acquire_test(
            &temp.path().join("MixedCase.DB"),
            "upper case-sensitive database",
        )
        .unwrap();
        let lower_authority = DatabaseAuthority::acquire_test(
            &temp.path().join("mixedcase.db"),
            "lower case-sensitive database",
        )
        .unwrap();
        assert_ne!(upper_authority.token(), lower_authority.token());
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn fresh_case_variants_cannot_hold_concurrent_first_create_authorities() {
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("MixedCase.DB");
        let lower = temp.path().join("mixedcase.db");

        let first = DatabaseAuthority::acquire_test(&upper, "first case variant").unwrap();
        let error = DatabaseAuthority::acquire_test(&lower, "second case variant").unwrap_err();
        assert!(error.to_string().contains("case-variant first-create"));

        std::fs::write(&upper, []).unwrap();
        drop(first);
        let second = DatabaseAuthority::acquire_test(&lower, "second case variant").unwrap();
        if lower.exists() {
            assert_eq!(
                second.canonical_database_path(),
                upper.canonicalize().unwrap()
            );
        } else {
            std::fs::write(&lower, []).unwrap();
            let upper_identity = DatabaseIdentity::for_path(&upper).unwrap();
            let lower_identity = DatabaseIdentity::for_path(&lower).unwrap();
            assert_ne!(upper_identity.database_key, lower_identity.database_key);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_share_one_database_identity() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("database.db");
        let alias = temp.path().join("database-alias.db");
        std::fs::write(&database, []).unwrap();
        std::os::unix::fs::symlink(&database, &alias).unwrap();

        let database = DatabaseIdentity::for_path(&database).unwrap();
        let alias = DatabaseIdentity::for_path(&alias).unwrap();

        assert_eq!(database.database_key, alias.database_key);
        assert_eq!(database.writer_lock_path, alias.writer_lock_path);
    }

    #[test]
    fn profile_databases_share_one_exact_profile_scope() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let expected_profile = platform_identity_key(&profile.canonicalize().unwrap());
        let paths = [
            profile.join("global.db"),
            profile.join("user-memory.db"),
            profile.join("user-sessions.db"),
            profile.join("projects/project/tracedecay.db"),
            profile.join("projects/project/sessions.db"),
            profile.join("projects/project/branches/feature.db"),
        ];

        for path in paths {
            let identity = DatabaseIdentity::for_path(&path).unwrap();
            assert_eq!(
                identity.profile_root,
                expected_profile,
                "{}",
                path.display()
            );
            assert!(
                !identity.allows_ambient_profile_scope,
                "{} must require its exact profile authority",
                path.display()
            );
            assert_eq!(
                identity.access_lock_path.parent(),
                Some(profile.join(".tracedecay-database-locks").as_path())
            );
        }
    }

    #[test]
    fn projects_directory_in_repository_path_is_not_a_profile_shard() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("projects/repository/.tracedecay");
        let path = data_root.join("tracedecay.db");
        let identity = DatabaseIdentity::for_path(&path).unwrap();

        assert_eq!(
            identity.profile_root,
            platform_identity_key(&data_root.canonicalize().unwrap())
        );
        assert!(identity.allows_ambient_profile_scope);
    }

    #[test]
    fn fs2_contention_is_classified_as_an_active_lease() {
        let temp = tempfile::tempdir().unwrap();
        let lock_path = temp.path().join("authority.lock");
        let first = open_lock_file(&lock_path).unwrap();
        let second = open_lock_file(&lock_path).unwrap();
        fs2::FileExt::try_lock_exclusive(&first).unwrap();

        let error = fs2::FileExt::try_lock_exclusive(&second).unwrap_err();

        assert!(is_lock_contended(&error), "unexpected lock error: {error}");
        fs2::FileExt::unlock(&first).unwrap();
    }

    #[test]
    fn writer_owner_replacement_is_complete_and_leaves_no_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("writer.owner");
        let first = writer_owner("first", "first owner");
        let second = writer_owner("second", "replacement owner");
        write_owner(&path, &first).unwrap();

        write_owner(&path, &second).unwrap();

        assert_eq!(read_owner(&path), Some(second));
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn atomic_record_publication_preserves_a_colliding_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("authority.record");
        let temporary = temp.path().join("authority.record.tmp");
        std::fs::write(&temporary, b"other publisher").unwrap();

        let error = DatabaseAuthority::publish_record_atomically(
            &temporary,
            &destination,
            b"replacement",
            "test authority record",
        )
        .unwrap_err();

        assert!(error.to_string().contains("create test authority record"));
        assert_eq!(std::fs::read(&temporary).unwrap(), b"other publisher");
        assert!(!destination.exists());
    }

    #[test]
    fn daemon_authority_is_same_process_reentrant() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let first = DatabaseAuthority::acquire_test(&path, "first").unwrap();
        let second = DatabaseAuthority::acquire_test(&path, "second").unwrap();
        assert_eq!(first.token(), second.token());
        assert_eq!(
            probe_writer_owner(&path).unwrap(),
            WriterOwnership::Active(
                read_owner(&DatabaseIdentity::for_path(&path).unwrap().writer_owner_path).unwrap()
            )
        );
        drop(first);
        assert!(matches!(
            probe_writer_owner(&path).unwrap(),
            WriterOwnership::Active(_)
        ));
        drop(second);
        assert_eq!(probe_writer_owner(&path).unwrap(), WriterOwnership::Idle);
    }

    #[test]
    fn maintenance_and_daemon_authorities_are_mutually_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let daemon = DatabaseAuthority::acquire_test(&path, "daemon").unwrap();
        let error = DatabaseAuthority::acquire_maintenance(&path, "replace").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("incompatible database authority")
        );
        drop(daemon);

        let maintenance = DatabaseAuthority::acquire_maintenance(&path, "replace").unwrap();
        let error = DatabaseAuthority::acquire_test(&path, "daemon").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("incompatible database authority")
        );
        drop(maintenance);
    }

    #[test]
    fn stale_owner_metadata_never_establishes_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let identity = DatabaseIdentity::for_path(&path).unwrap();
        std::fs::write(
            &identity.writer_owner_path,
            "token=stale\tpid=1\tstarted_epoch_ms=1\tversion=old\tintent=old\n",
        )
        .unwrap();
        assert_eq!(probe_writer_owner(&path).unwrap(), WriterOwnership::Idle);
        assert!(identity.writer_owner_path.exists());
    }

    #[test]
    fn authority_is_bound_to_one_canonical_database() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.db");
        let second = temp.path().join("second.db");
        let authority = DatabaseAuthority::acquire_test(&first, "test").unwrap();
        let error = authority.hold_for(&second, "open").unwrap_err();
        assert!(error.to_string().contains("different database"));
    }

    #[test]
    fn daemon_authority_inherits_live_election_scope() {
        let _lock = SCOPE_TEST_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let scope = enter_daemon_database_scope(temp.path(), 7, "election-token").unwrap();
        let authority = DatabaseAuthority::acquire_daemon(&path, "daemon").unwrap();
        assert_eq!(authority.role(), DatabaseAuthorityRole::Daemon);
        drop(authority);
        drop(scope);
    }

    #[test]
    fn sole_daemon_scope_authorizes_only_legacy_repo_local_database() {
        let _lock = SCOPE_TEST_LOCK.lock().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let repository = tempfile::tempdir().unwrap();
        let scope = enter_daemon_database_scope(profile.path(), 1, "daemon").unwrap();
        let identity =
            DatabaseIdentity::for_path(&repository.path().join(".tracedecay/tracedecay.db"))
                .unwrap();

        assert!(identity.allows_ambient_profile_scope);
        assert_eq!(
            scoped_runtime_role(&identity, "legacy repository database").unwrap(),
            Some(DatabaseAuthorityRole::Daemon)
        );

        drop(scope);
    }

    #[test]
    fn sole_daemon_scope_rejects_standard_databases_from_another_profile() {
        let _lock = SCOPE_TEST_LOCK.lock().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let scope = enter_daemon_database_scope(first.path(), 1, "first").unwrap();
        let paths = [
            second.path().join("global.db"),
            second.path().join("user-memory.db"),
            second.path().join("user-sessions.db"),
            second.path().join("projects/project/tracedecay.db"),
            second.path().join("projects/project/sessions.db"),
            second.path().join("projects/project/branches/feature.db"),
        ];

        for path in paths {
            let identity = DatabaseIdentity::for_path(&path).unwrap();
            assert_eq!(
                exact_scoped_runtime_role(&identity.profile_root, "other profile").unwrap(),
                None
            );
            assert_eq!(
                scoped_runtime_role(&identity, "other profile").unwrap(),
                None,
                "{} used an unrelated ambient profile scope",
                path.display()
            );
        }

        drop(scope);
    }

    #[test]
    fn maintenance_scope_requires_and_inherits_exclusive_profile_lease() {
        let _lock = SCOPE_TEST_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("projects/p1/tracedecay.db");
        let lifecycle =
            crate::lifecycle_lease::acquire_exclusive_for_profile(temp.path(), "maintenance test")
                .unwrap();
        let scope =
            enter_maintenance_database_scope(&lifecycle, temp.path(), "maintenance test").unwrap();
        let authority = DatabaseAuthority::for_runtime(&path, "repair").unwrap();
        assert_eq!(authority.role(), DatabaseAuthorityRole::Maintenance);
        drop(authority);
        drop(scope);
        drop(lifecycle);
    }

    #[test]
    fn daemon_scopes_are_isolated_by_profile() {
        let _lock = SCOPE_TEST_LOCK.lock().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_scope = enter_daemon_database_scope(first.path(), 1, "first").unwrap();
        let second_scope = enter_daemon_database_scope(second.path(), 1, "second").unwrap();

        let first_authority = DatabaseAuthority::for_runtime(
            &first.path().join("projects/one/tracedecay.db"),
            "first profile",
        )
        .unwrap();
        let second_authority = DatabaseAuthority::for_runtime(
            &second.path().join("projects/two/tracedecay.db"),
            "second profile",
        )
        .unwrap();
        assert_eq!(first_authority.role(), DatabaseAuthorityRole::Daemon);
        assert_eq!(second_authority.role(), DatabaseAuthorityRole::Daemon);

        drop((first_authority, second_authority, first_scope, second_scope));
    }

    #[test]
    fn maintenance_scope_is_reentrant_across_nested_intents() {
        let _lock = SCOPE_TEST_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let lifecycle =
            crate::lifecycle_lease::acquire_exclusive_for_profile(temp.path(), "outer").unwrap();
        let outer = enter_maintenance_database_scope(&lifecycle, temp.path(), "plan").unwrap();
        let inner = enter_maintenance_database_scope(&lifecycle, temp.path(), "apply").unwrap();
        let authority = DatabaseAuthority::for_runtime(
            &temp.path().join("projects/p1/tracedecay.db"),
            "nested operation",
        )
        .unwrap();
        assert_eq!(authority.role(), DatabaseAuthorityRole::Maintenance);

        drop(authority);
        drop(inner);
        drop(outer);
        drop(lifecycle);
    }
}
