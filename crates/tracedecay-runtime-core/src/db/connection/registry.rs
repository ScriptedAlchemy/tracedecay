use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use crate::db::{DatabaseAuthority, engine::Connection};
use crate::errors::TraceDecayError;
// The store-runtime registry moved into this kernel, so the facade retains the
// concrete handle rather than an erased port.
use crate::store_runtime::registry::StoreRuntimeHandle;

pub(super) struct DatabaseInner {
    /// Reader-only channel exposed through the retained database facade.
    pub(super) conn: Connection,
    /// Writer-authorized channel cloned only while the logical writer lane is
    /// held. Read-only facades never retain one.
    pub(super) write_conn: Option<Connection>,
    /// Retains the registry-owned physical runtime. The registry remains the
    /// sole lifecycle owner; this facade never extracts or reopens its
    /// attachment.
    pub(super) _runtime: StoreRuntimeHandle,
    pub(super) writable: bool,
    /// Descriptor-derived identity reported by the physical attachment.
    pub(super) opened_file_identity: u64,
    /// Serializes logical writers sharing this canonical database slot.
    pub(super) writer: tokio::sync::Mutex<()>,
    /// Canonical path from the runtime's verified locator.
    pub(super) canonical_path: PathBuf,
    /// The exact capability retained when this physical attachment was
    /// published writable. Read-only facades never retain write authority.
    pub(super) _authority: Option<DatabaseAuthority>,
    pub(super) _slot: Option<DatabaseSlot>,
}

impl DatabaseInner {
    /// Publishes an already-open canonical registry runtime without reopening
    /// the `SQLite` path.
    pub(super) fn publish(
        runtime: StoreRuntimeHandle,
        writable: bool,
        authority: Option<DatabaseAuthority>,
        slot: Option<DatabaseSlot>,
    ) -> crate::errors::Result<Self> {
        let opened_file_identity = runtime.opened_file_identity().ok_or_else(|| {
            database_registry_error(
                "publish canonical database runtime",
                "registered runtime did not report its opened SQLite file identity",
            )
        })?;
        if let Some(authority) = authority.as_ref()
            && runtime.canonical_path() != authority.canonical_database_path()
        {
            return Err(database_registry_error(
                "publish canonical database runtime",
                format!(
                    "registered locator {} does not match retained database authority {}",
                    runtime.canonical_path().display(),
                    authority.canonical_database_path().display()
                ),
            ));
        }
        runtime
            .validate_registered_read("publish canonical database runtime")
            .map_err(|error| {
                database_registry_error("publish canonical database runtime", format!("{error:?}"))
            })?;

        let write_conn = if writable {
            let authority = authority.clone().ok_or_else(|| {
                database_registry_error(
                    "authorize canonical database engine",
                    "writable database publication requires originating authority",
                )
            })?;
            Some(Connection::attach(
                runtime
                    .authorized_exact_sql_handle(authority)
                    .map_err(|error| {
                        database_registry_error(
                            "authorize canonical database engine",
                            format!("{error:?}"),
                        )
                    })?,
            ))
        } else {
            None
        };
        let read_conn = Connection::attach(runtime.telemetry_read_handle().map_err(|error| {
            database_registry_error("attach canonical database reader", format!("{error:?}"))
        })?);

        Ok(Self {
            conn: read_conn,
            write_conn,
            canonical_path: runtime.canonical_path().to_path_buf(),
            _runtime: runtime,
            writable,
            opened_file_identity,
            writer: tokio::sync::Mutex::new(()),
            _authority: authority,
            _slot: slot,
        })
    }
}

type DatabaseWeak = Weak<DatabaseInner>;
pub(super) type DatabaseSlot = Arc<tokio::sync::Mutex<DatabaseWeak>>;
type WeakDatabaseSlot = Weak<tokio::sync::Mutex<DatabaseWeak>>;
type OpenDatabases = HashMap<PathBuf, WeakDatabaseSlot>;

static OPEN_DATABASES: LazyLock<Mutex<OpenDatabases>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn database_slot(identity_key: &Path) -> DatabaseSlot {
    let mut databases = OPEN_DATABASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    databases.retain(|_, slot| slot.strong_count() > 0);
    if let Some(slot) = databases.get(identity_key).and_then(Weak::upgrade) {
        return slot;
    }
    let slot = Arc::new(tokio::sync::Mutex::new(Weak::new()));
    databases.insert(identity_key.to_path_buf(), Arc::downgrade(&slot));
    slot
}

fn database_registry_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}
