//! Process-local singleton ownership for Git index transaction stores.
//!
//! One bounded store actor is retained per registered project-session path.
//! Startup recovery and later mutation services must share that actor so a
//! second queue/journal authority cannot appear for the same database.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_store::{GitIndexTransactionStoreError, GitIndexTransactionStoreResult};

use tracedecay_global_db::RegisteredGlobalDbLeaseV1;

use super::DaemonGitIndexTransactionStore;
use super::SharedDaemonGitIndexTransactionStore;

type ProfiledStdMutex<T> = hotpath::mutexes::Mutex<T>;

/// Retains the one `DaemonGitIndexTransactionStore` actor for each daemon-owned
/// project database. Dropping the registry closes every actor when the daemon
/// store administration shuts down.
pub struct GitIndexTransactionStoreRegistry {
    stores: ProfiledStdMutex<HashMap<PathBuf, SharedDaemonGitIndexTransactionStore>>,
    closed: AtomicBool,
}

impl Default for GitIndexTransactionStoreRegistry {
    fn default() -> Self {
        Self {
            stores: hotpath::mutex!(
                std::sync::Mutex::new(HashMap::new()),
                label = "daemon.git.tx.stores"
            ),
            closed: AtomicBool::new(false),
        }
    }
}

impl GitIndexTransactionStoreRegistry {
    /// Returns the existing actor for `database`, or opens exactly one.
    #[hotpath::measure(label = "daemon.git.tx.store_ensure")]
    pub fn ensure(
        &self,
        database: RegisteredGlobalDbLeaseV1,
    ) -> GitIndexTransactionStoreResult<SharedDaemonGitIndexTransactionStore> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionStoreError::unavailable(
                "git index transaction store registry is shut down",
            ));
        }
        // The registered runtime authority already supplies the canonical
        // database identity. Avoid a second filesystem lookup because a fresh
        // SQLite shard may not have materialized its path yet.
        let path = database.db_path().to_path_buf();
        let mut stores = self
            .stores
            .lock()
            .map_err(GitIndexTransactionStoreError::unavailable)?;
        if self.closed.load(Ordering::SeqCst) {
            return Err(GitIndexTransactionStoreError::unavailable(
                "git index transaction store registry is shut down",
            ));
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
    pub fn remove(&self, path: &std::path::Path) -> GitIndexTransactionStoreResult<()> {
        let mut stores = self
            .stores
            .lock()
            .map_err(GitIndexTransactionStoreError::unavailable)?;
        stores.remove(path);
        Ok(())
    }

    #[hotpath::measure(label = "daemon.git.tx.store_shutdown", future = true)]
    pub async fn shutdown_all(&self) -> GitIndexTransactionStoreResult<usize> {
        self.closed.store(true, Ordering::SeqCst);
        let stores = {
            let mut retained = self
                .stores
                .lock()
                .map_err(GitIndexTransactionStoreError::unavailable)?;
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
        .map_err(GitIndexTransactionStoreError::unavailable)?
    }
}
