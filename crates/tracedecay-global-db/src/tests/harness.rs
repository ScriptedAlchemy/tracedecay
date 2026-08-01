use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tempfile::TempDir;

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::DaemonDatabaseScope;
use crate::RegisteredGlobalDb;

static TEST_RUNTIME_NONCE: AtomicU64 = AtomicU64::new(1);

pub struct RegisteredGlobalDbHarness {
    pub registered: Arc<RegisteredGlobalDb>,
    _directory: TempDir,
    scope: Option<DaemonDatabaseScope>,
    registry: DaemonSessionRuntimeRegistryV1,
}

impl RegisteredGlobalDbHarness {
    pub async fn open(label: &str) -> Self {
        let directory = tempfile::tempdir().expect("temporary registered global database");
        let profile_root = directory.path().join("profile");
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)
            .expect("profile identity");
        let nonce = TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
        let scope = crate::db::enter_daemon_database_scope(&profile_root, nonce, label)
            .expect("daemon database scope");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("session runtime registry");
        let registered = registry
            .profile_sessions()
            .await
            .expect("registered profile sessions");
        Self {
            registered,
            _directory: directory,
            scope: Some(scope),
            registry,
        }
    }

    pub(super) fn storage_root(&self) -> &std::path::Path {
        self.registered
            .db_path()
            .parent()
            .expect("registered database storage root")
    }

    pub async fn mount(&self) -> Arc<RegisteredGlobalDb> {
        self.registry
            .profile_sessions()
            .await
            .expect("registered profile sessions")
    }

    pub(super) fn revoke(&mut self) {
        drop(self.scope.take());
    }
}
