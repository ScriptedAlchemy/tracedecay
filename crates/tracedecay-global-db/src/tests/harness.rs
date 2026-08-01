use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tempfile::TempDir;

use crate::RegisteredGlobalDb;
use crate::host_ports::profile_sessions::{self, ProfileSessionsRuntime};
use tracedecay_runtime_core::db::DaemonDatabaseScope;

static TEST_RUNTIME_NONCE: AtomicU64 = AtomicU64::new(1);

/// Message shown when the composition root never installed the opener.
pub(crate) const UNWIRED_PROFILE_SESSIONS: &str = "tracedecay_global_db::host_ports::profile_sessions::register must be called by the \
     composition root before a registered harness can open";

pub struct RegisteredGlobalDbHarness {
    pub registered: Arc<RegisteredGlobalDb>,
    _directory: TempDir,
    scope: Option<DaemonDatabaseScope>,
    registry: Box<dyn ProfileSessionsRuntime>,
}

impl RegisteredGlobalDbHarness {
    pub async fn open(label: &str) -> Self {
        let directory = tempfile::tempdir().expect("temporary registered global database");
        let profile_root = directory.path().join("profile");
        let nonce = TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
        // The scope guard is entered before the runtime opens; the root opener
        // creates the profile identity on its way to the session registry.
        let scope =
            tracedecay_runtime_core::db::enter_daemon_database_scope(&profile_root, nonce, label)
                .expect("daemon database scope");
        let registry = profile_sessions::open(profile_root)
            .expect(UNWIRED_PROFILE_SESSIONS)
            .await;
        let registered = registry.mount().await;
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
        self.registry.mount().await
    }

    pub(super) fn revoke(&mut self) {
        drop(self.scope.take());
    }
}
