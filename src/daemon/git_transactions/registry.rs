//! Process-local singleton ownership for Git index transaction stores.
//!
//! One bounded store actor is retained per registered project-session path.
//! Startup recovery and later mutation services must share that actor so a
//! second queue/journal authority cannot appear for the same database.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_store::{GitIndexTransactionStoreError, GitIndexTransactionStoreResult};

use crate::global_db::RegisteredGlobalDbLeaseV1;

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
        database: RegisteredGlobalDbLeaseV1,
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

    /// Drops the exact project-session actor before its backing shard is
    /// destructively removed by daemon-owned lifecycle administration.
    pub(crate) fn remove(&self, path: &std::path::Path) -> GitIndexTransactionStoreResult<()> {
        let mut stores = self
            .stores
            .lock()
            .map_err(|_| GitIndexTransactionStoreError::Unavailable)?;
        stores.remove(path);
        Ok(())
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
