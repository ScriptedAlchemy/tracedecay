//! Process-local singleton ownership for Git index transaction stores.
//!
//! One store is retained per registered project-session path. Startup
//! recovery and later mutation services must share it so a second
//! queue/journal authority cannot appear for the same database.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_store::{GitIndexTransactionStoreError, GitIndexTransactionStoreResult};

use tracedecay_global_db::RegisteredGlobalDbLeaseV1;

use super::DaemonGitIndexTransactionStore;

/// Retains the one `DaemonGitIndexTransactionStore` for each daemon-owned
/// project database. Shutting the registry down closes every store when the
/// daemon store administration shuts down.
pub struct GitIndexTransactionStoreRegistry {
    stores: tokio::sync::Mutex<HashMap<PathBuf, DaemonGitIndexTransactionStore>>,
    closed: AtomicBool,
}

impl Default for GitIndexTransactionStoreRegistry {
    fn default() -> Self {
        Self {
            stores: tokio::sync::Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        }
    }
}

impl GitIndexTransactionStoreRegistry {
    /// Returns the existing store for `database`, or opens exactly one.
    #[tracing::instrument(name = "daemon.git.tx.store_ensure", level = "trace", skip_all)]
    pub async fn ensure(
        &self,
        database: RegisteredGlobalDbLeaseV1,
    ) -> GitIndexTransactionStoreResult<DaemonGitIndexTransactionStore> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionStoreError::unavailable(
                "git index transaction store registry is shut down",
            ));
        }
        // The registered runtime authority already supplies the canonical
        // database identity. Avoid a second filesystem lookup because a fresh
        // SQLite shard may not have materialized its path yet.
        let path = database.db_path().to_path_buf();
        let mut stores = self.stores.lock().await;
        if self.closed.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionStoreError::unavailable(
                "git index transaction store registry is shut down",
            ));
        }
        if let Some(existing) = stores.get(&path) {
            return Ok(existing.clone());
        }
        let store = DaemonGitIndexTransactionStore::open(database).await?;
        stores.insert(path, store.clone());
        Ok(store)
    }

    /// Drops the exact project-session store before its backing shard is
    /// destructively removed by daemon-owned lifecycle administration.
    pub async fn remove(&self, path: &std::path::Path) {
        self.stores.lock().await.remove(path);
    }

    #[tracing::instrument(name = "daemon.git.tx.store_shutdown", level = "trace", skip_all)]
    pub async fn shutdown_all(&self) -> usize {
        self.closed.store(true, Ordering::SeqCst);
        self.stores
            .lock()
            .await
            .drain()
            .map(|(_, store)| usize::from(store.shutdown()))
            .sum()
    }
}
