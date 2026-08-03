use std::collections::HashMap;
#[cfg(any(test, feature = "test-transport"))]
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, LazyLock, Mutex};

#[cfg(any(test, feature = "test-transport"))]
use fs2::FileExt;

use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
};

use crate::errors::{Result, TraceDecayError};

mod bootstrap;
mod lease;
mod owner_io;
mod path_layout;

use bootstrap::reject_hard_linked_database;
#[cfg(windows)]
pub use bootstrap::windows_hard_link_count;
pub use lease::enter_maintenance_database_scope;
#[cfg(not(test))]
pub use lease::enter_owned_maintenance_database_scope;
#[cfg(test)]
use lease::fallback_scoped_runtime_role;
use lease::{acquire_process_lease, exact_scoped_runtime_role, scoped_runtime_role};
pub use lease::{enter_daemon_database_scope, probe_writer_owner};
pub use owner_io::is_lock_contended;
use owner_io::{
    authority_token, epoch_ms, publish_record_atomically, read_record_strict, writer_owner,
};
use path_layout::{
    canonical_profile_root, database_profile_root, is_legacy_repository_database,
    platform_identity_key,
};

static PROCESS_LEASES: LazyLock<Mutex<HashMap<PathBuf, ProcessLease>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DAEMON_SCOPES: LazyLock<Mutex<HashMap<PathBuf, DaemonScopeState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static MAINTENANCE_SCOPES: LazyLock<Mutex<HashMap<PathBuf, MaintenanceScopeState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static TOKEN_NONCE: AtomicU64 = AtomicU64::new(0);
static PROCESS_STARTED_EPOCH_MS: LazyLock<u128> = LazyLock::new(epoch_ms);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseAuthorityRole {
    Daemon,
    Maintenance,
    #[doc(hidden)]
    Test,
}

#[cfg_attr(
    not(any(feature = "test-helpers", feature = "test-transport")),
    doc = r#"
Production builds do not expose the integration-fixture authority escape hatch.

```compile_fail
use std::path::Path;
use tracedecay::db::DatabaseAuthority;

let _ = DatabaseAuthority::acquire_test(Path::new("/tmp/fixture.db"), "fixture");
```

Debug assertions alone do not enable daemonless writable authority:

```
use tracedecay::db::DatabaseAuthority;
use tracedecay_rusqlite_runtime::exact_sql::{
    ExactSqlError, ExactSqlWriteAuthority, ExactSqlWriteIntent,
};

let path = std::env::temp_dir().join(format!(
    "tracedecay-production-authority-boundary-{}.db",
    std::process::id()
));
let error = DatabaseAuthority::for_runtime(&path, "production boundary")
    .expect_err("temp paths must not grant production write authority");
assert!(error.to_string().contains(
    "database access requires managed-daemon or exclusive-maintenance authority"
));
```
"#
)]
#[derive(Clone, Debug)]
pub struct DatabaseAuthority {
    inner: Arc<AuthorityInner>,
}

#[derive(Debug)]
pub struct DaemonDatabaseScope {
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

/// Maintenance scope that owns the lifecycle lease it validates.
///
/// Dropping the scope revokes database authority before its retained lifecycle
/// lease is released.
#[derive(Debug)]
pub struct OwnedMaintenanceDatabaseScope {
    profile_root: PathBuf,
    token: String,
    _lifecycle: crate::lifecycle_lease::LifecycleLease,
}

impl MaintenanceDatabaseScope<'_> {
    /// Issues database authority for one exact artifact beneath this retained
    /// exclusive-maintenance profile scope.
    pub fn database_authority(&self, db_path: &Path, intent: &str) -> Result<DatabaseAuthority> {
        let identity = DatabaseIdentity::for_path(db_path)?;
        if identity.profile_root != self.profile_root {
            return Err(access_error(
                intent,
                db_path,
                "database artifact belongs to a different maintenance profile",
            ));
        }
        let active = MAINTENANCE_SCOPES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&self.profile_root)
            .is_some_and(|scope| scope.token == self.token);
        if !active {
            return Err(access_error(
                intent,
                db_path,
                "exclusive-maintenance database scope was revoked",
            ));
        }
        DatabaseAuthority::acquire_identity(identity, DatabaseAuthorityRole::Maintenance, intent)
    }
}

impl OwnedMaintenanceDatabaseScope {
    #[cfg(not(test))]
    pub fn lifecycle(&self) -> &crate::lifecycle_lease::LifecycleLease {
        &self._lifecycle
    }
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DatabaseIdentity {
    database_path: PathBuf,
    database_key: PathBuf,
    profile_root: PathBuf,
    allows_ambient_profile_scope: bool,
}

#[derive(Debug)]
struct ProcessLease {
    token: String,
    refs: usize,
    role: DatabaseAuthorityRole,
    owner: WriterOwner,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriterOwner {
    pub token: String,
    pub pid: u32,
    pub started_epoch_ms: u128,
    pub version: String,
    pub intent: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriterOwnership {
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

    /// Acquires only authority already owned by the active daemon or an
    /// exclusive maintenance scope. Unlike [`Self::for_runtime`], this never
    /// falls back to fixture-only `Test` authority.
    pub fn for_owned_runtime(db_path: &Path, intent: &str) -> Result<Self> {
        let identity = DatabaseIdentity::for_path(db_path)?;
        if let Some(role) = exact_scoped_runtime_role(&identity.profile_root, intent)? {
            return Self::acquire_identity(identity, role, intent);
        }
        let maintenance_active = PROCESS_LEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&identity.database_key)
            .is_some_and(|lease| lease.role == DatabaseAuthorityRole::Maintenance);
        if maintenance_active {
            return Self::acquire_identity(identity, DatabaseAuthorityRole::Maintenance, intent);
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

    #[doc(hidden)]
    pub fn for_runtime(db_path: &Path, intent: &str) -> Result<Self> {
        let identity = DatabaseIdentity::for_path(db_path)?;
        #[cfg(any(test, feature = "test-transport"))]
        if is_isolated_test_path(&identity.database_path) {
            let existing_role = PROCESS_LEASES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&identity.database_key)
                .map(|lease| match lease.role {
                    DatabaseAuthorityRole::Maintenance => DatabaseAuthorityRole::Maintenance,
                    DatabaseAuthorityRole::Daemon | DatabaseAuthorityRole::Test => {
                        DatabaseAuthorityRole::Test
                    }
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
            .is_some_and(|lease| lease.role == DatabaseAuthorityRole::Maintenance);
        if maintenance_active {
            return Self::acquire_identity(identity, DatabaseAuthorityRole::Maintenance, intent);
        }
        // Debug fixtures may use ambient Test authority on isolated temp paths,
        // but never while another process already holds exclusive daemon
        // authority for the profile. Rejected contenders and brokered clients
        // must fail closed with zero durable SQLite opens/writes.
        #[cfg(any(test, feature = "test-transport"))]
        if is_isolated_test_path(&identity.database_path)
            && !foreign_daemon_authority_held(&identity.profile_root)
        {
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
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
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

    /// Verifies that the authority retained by an open database still has an
    /// active runtime scope before it can start a mutation.
    ///
    /// Holding an authority keeps the operating-system writer lease alive, but
    /// it must not let a clone of a daemon-owned `Database` outlive the daemon
    /// election or an exclusive maintenance scope. Test authorities remain the
    /// explicit fixture escape hatch.
    pub fn require_active_write_scope(&self, intent: &str) -> Result<()> {
        if self.inner.role == DatabaseAuthorityRole::Test {
            return Ok(());
        }

        let active = match exact_scoped_runtime_role(&self.inner.identity.profile_root, intent)? {
            Some(role) => role,
            None => scoped_runtime_role(&self.inner.identity, intent)?.ok_or_else(|| {
                access_error(
                    intent,
                    &self.inner.identity.database_path,
                    "database write requires an active daemon or exclusive maintenance scope",
                )
            })?,
        };
        if active == self.inner.role {
            return Ok(());
        }

        Err(access_error(
            intent,
            &self.inner.identity.database_path,
            "active database scope does not match the retained write authority",
        ))
    }

    pub fn token(&self) -> &str {
        &self.inner.token
    }

    pub fn publish_record_atomically(
        temporary: &Path,
        destination: &Path,
        payload: &[u8],
        record_name: &str,
    ) -> Result<()> {
        publish_record_atomically(temporary, destination, payload, record_name)
    }

    pub fn read_record_strict(path: &Path, record_name: &str) -> Result<Option<String>> {
        read_record_strict(path, record_name)
    }

    pub fn replace_file_atomically(
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
        identity: DatabaseIdentity,
        role: DatabaseAuthorityRole,
        intent: &str,
    ) -> Result<Self> {
        let token = acquire_process_lease(&identity, role, intent)?;
        Ok(Self {
            inner: Arc::new(AuthorityInner {
                identity,
                role,
                token,
            }),
        })
    }

    pub fn hold_for(&self, db_path: &Path, operation: &str) -> Result<Self> {
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

    pub fn canonical_database_path(&self) -> &Path {
        &self.inner.identity.database_path
    }

    pub(crate) fn database_identity_key(&self) -> &Path {
        &self.inner.identity.database_key
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
            .map_err(|error| access_io_error("create database parent directory", parent, &error))?;

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
        let profile_root = database_profile_root(&database_path, parent);
        Ok(Self {
            allows_ambient_profile_scope: is_legacy_repository_database(&database_path),
            database_path,
            database_key,
            profile_root: platform_identity_key(&profile_root),
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

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub fn is_isolated_test_path(path: &Path) -> bool {
    let root = std::env::temp_dir();
    if path.starts_with(root.canonicalize().unwrap_or(root)) {
        return true;
    }
    std::env::var_os("TRACEDECAY_DATA_DIR")
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

/// Matches `src/daemon/authority.rs` `LOCK_FILE`. Kept local so the access
/// layer can fail closed without depending on the daemon module. Its only
/// consumer is the ambient-Test authority probe below, so it shares that
/// probe's cfg surface — without the gate the default-feature build sees it
/// as dead.
#[cfg(any(test, feature = "test-transport"))]
const DAEMON_AUTHORITY_LOCK_FILE: &str = "daemon-authority.lock";
/// Matches the Windows-only private state directory in `src/daemon/authority.rs`.
#[cfg(all(windows, any(test, feature = "test-transport")))]
const DAEMON_AUTHORITY_DIRECTORY: &str = "daemon-authority";

/// Returns true when another process currently holds the profile's exclusive
/// daemon-authority lock. Used to keep ambient Test opens from mutating a
/// store while a live daemon owner is elected — its sole caller is the
/// cfg-gated ambient-Test branch, so the probe carries the same gate.
#[cfg(any(test, feature = "test-transport"))]
fn foreign_daemon_authority_held(profile_root: &Path) -> bool {
    #[cfg(windows)]
    let lock_path = profile_root
        .join(DAEMON_AUTHORITY_DIRECTORY)
        .join(DAEMON_AUTHORITY_LOCK_FILE);
    #[cfg(not(windows))]
    let lock_path = profile_root.join(DAEMON_AUTHORITY_LOCK_FILE);
    // Probe with a read-only open so a rejected contender mutates zero bytes
    // before exclusive authority is granted.
    let mut options = OpenOptions::new();
    options.read(true).write(false).create(false);
    let Ok(file) = options.open(&lock_path) else {
        return false;
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            false
        }
        Err(error) if is_lock_contended(&error) => true,
        Err(_) => false,
    }
}

/// Registered databases authorize every exact-SQL write through the
/// retained authority. The impl lives beside `DatabaseAuthority` because the
/// trait belongs to `tracedecay_rusqlite_runtime` and the orphan rule allows
/// it only in the crate that owns the type.
impl ExactSqlWriteAuthority for DatabaseAuthority {
    fn verify(&self, intent: ExactSqlWriteIntent) -> std::result::Result<(), ExactSqlError> {
        let intent = match intent {
            ExactSqlWriteIntent::Validate => "validate registered global database statement",
            ExactSqlWriteIntent::Execute => "execute registered global database statement",
            ExactSqlWriteIntent::Query => "query registered global database writer",
            ExactSqlWriteIntent::ExecuteBatch => {
                "execute registered global database statement batch"
            }
            ExactSqlWriteIntent::Vacuum => {
                if self.role() != DatabaseAuthorityRole::Maintenance {
                    return Err(ExactSqlError::AuthorityDenied(
                        "whole-database vacuum requires exclusive maintenance authority".to_owned(),
                    ));
                }
                "vacuum registered global database under exclusive maintenance"
            }
            ExactSqlWriteIntent::BeginTransaction => "begin registered global database transaction",
            ExactSqlWriteIntent::Commit => "commit registered global database transaction",
        };
        self.require_active_write_scope(intent)
            .map_err(|error| ExactSqlError::AuthorityDenied(error.to_string()))
    }
}

#[cfg(test)]
mod tests;
