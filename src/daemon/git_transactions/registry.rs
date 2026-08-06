//! Process-local singleton ownership for Git index transaction stores.
//!
//! One bounded store actor is retained per registered project-session path.
//! Startup recovery and later mutation services must share that actor so a
//! second queue/journal authority cannot appear for the same database.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tracedecay_store::{GitIndexTransactionStoreError, GitIndexTransactionStoreResult};

#[cfg(test)]
use crate::db::engine::TestConnection;
use crate::global_db::RegisteredGlobalDb;

use super::DaemonGitIndexTransactionStore;
use super::SharedDaemonGitIndexTransactionStore;

/// Retains the one `DaemonGitIndexTransactionStore` actor for each daemon-owned
/// project database. Dropping the registry closes every actor when the daemon
/// store administration shuts down.
#[derive(Default)]
pub(crate) struct GitIndexTransactionStoreRegistry {
    stores: Mutex<HashMap<PathBuf, SharedDaemonGitIndexTransactionStore>>,
}

impl GitIndexTransactionStoreRegistry {
    /// Returns the existing actor for `database`, or opens exactly one.
    pub(crate) fn ensure(
        &self,
        database: Arc<RegisteredGlobalDb>,
    ) -> GitIndexTransactionStoreResult<SharedDaemonGitIndexTransactionStore> {
        // The registered runtime authority already supplies the canonical
        // database identity. Avoid a second filesystem lookup because a fresh
        // SQLite shard may not have materialized its path yet.
        let path = database.db_path().to_path_buf();
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        if let Some(existing) = stores.get(&path) {
            return Ok(existing.clone());
        }
        let store = SharedDaemonGitIndexTransactionStore::from_arc(Arc::new(
            DaemonGitIndexTransactionStore::open(database)?,
        ));
        stores.insert(path, store.clone());
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn ensure_engine_test(
        &self,
        path: PathBuf,
    ) -> GitIndexTransactionStoreResult<SharedDaemonGitIndexTransactionStore> {
        let path = path
            .canonicalize()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        if let Some(existing) = stores.get(&path) {
            return Ok(existing.clone());
        }
        let store = SharedDaemonGitIndexTransactionStore::from_arc(Arc::new(
            DaemonGitIndexTransactionStore::open_engine_test(TestConnection::open(&path))?,
        ));
        stores.insert(path, store.clone());
        Ok(store)
    }
}
