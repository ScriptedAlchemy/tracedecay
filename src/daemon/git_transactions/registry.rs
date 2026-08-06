//! Process-local singleton ownership for PR11 Git index transaction stores.
//!
//! One bounded store actor is retained per registered project-session path.
//! Startup recovery and later mutation services must share that actor so a
//! second queue/journal authority cannot appear for the same database.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
    closed: AtomicBool,
}

impl GitIndexTransactionStoreRegistry {
    /// Returns the existing actor for `database`, or opens exactly one.
    pub(crate) fn ensure(
        &self,
        database: Arc<RegisteredGlobalDb>,
    ) -> GitIndexTransactionStoreResult<SharedDaemonGitIndexTransactionStore> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionStoreError::Unavailable);
        }
        // The registered runtime authority already supplies the canonical
        // database identity. Avoid a second filesystem lookup because a fresh
        // SQLite shard may not have materialized its path yet.
        let path = database.db_path().to_path_buf();
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionStoreError::Unavailable);
        }
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
        if self.closed.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionStoreError::Unavailable);
        }
        let path = path
            .canonicalize()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionStoreError::Unavailable);
        }
        if let Some(existing) = stores.get(&path) {
            return Ok(existing.clone());
        }
        let store = SharedDaemonGitIndexTransactionStore::from_arc(Arc::new(
            DaemonGitIndexTransactionStore::open_engine_test(TestConnection::open(&path))?,
        ));
        stores.insert(path, store.clone());
        Ok(store)
    }

    pub(crate) async fn shutdown_all(&self) -> GitIndexTransactionStoreResult<usize> {
        self.closed.store(true, Ordering::SeqCst);
        let stores = {
            let mut retained = self
                .stores
                .lock()
                .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
            retained.drain().map(|(_, store)| store).collect::<Vec<_>>()
        };
        tokio::task::spawn_blocking(move || {
            let mut joined = 0usize;
            for store in stores {
                joined = joined.saturating_add(usize::from(store.shutdown()?));
            }
            Ok(joined)
        })
        .await
        .map_err(|_| GitIndexTransactionStoreError::Unavailable)?
    }
}
