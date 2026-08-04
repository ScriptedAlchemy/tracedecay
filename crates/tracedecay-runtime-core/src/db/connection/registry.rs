use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use libsql::{Connection, Database as LibsqlDatabase};

use crate::db::DatabaseAuthority;

pub(super) struct DatabaseInner {
    pub(super) conn: Connection,
    /// Kept alive so the underlying database is not dropped.
    pub(super) db: LibsqlDatabase,
    pub(super) writable: bool,
    pub(super) _authority: DatabaseAuthority,
    pub(super) _slot: Option<DatabaseSlot>,
}

type DatabaseWeak = Weak<DatabaseInner>;
pub(super) type DatabaseSlot = Arc<tokio::sync::Mutex<DatabaseWeak>>;
type WeakDatabaseSlot = Weak<tokio::sync::Mutex<DatabaseWeak>>;
type OpenDatabases = HashMap<PathBuf, WeakDatabaseSlot>;

static OPEN_DATABASES: LazyLock<Mutex<OpenDatabases>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn database_slot(path: &Path) -> DatabaseSlot {
    let mut databases = OPEN_DATABASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    databases.retain(|_, slot| slot.strong_count() > 0);
    if let Some(slot) = databases.get(path).and_then(Weak::upgrade) {
        return slot;
    }
    let slot = Arc::new(tokio::sync::Mutex::new(Weak::new()));
    databases.insert(path.to_path_buf(), Arc::downgrade(&slot));
    slot
}
