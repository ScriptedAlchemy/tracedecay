#![allow(dead_code)] // in-flight feature APIs not yet wired; see clippy sweep
//! Process-local singleton ownership for PR11 Git index transaction stores.
//!
//! One bounded store actor is retained per canonical project `GlobalDb` path.
//! Startup recovery and later mutation services must share that actor so a
//! second queue/journal authority cannot appear for the same database.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tracedecay_store::{GitIndexTransactionStoreError, GitIndexTransactionStoreResult};

use crate::global_db::GlobalDb;

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
        database: Arc<GlobalDb>,
    ) -> GitIndexTransactionStoreResult<SharedDaemonGitIndexTransactionStore> {
        let path = database
            .db_path()
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
            DaemonGitIndexTransactionStore::open(database)?,
        ));
        stores.insert(path, store.clone());
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, path: &std::path::Path) -> bool {
        self.stores
            .lock()
            .is_ok_and(|stores| stores.contains_key(path))
    }
}
