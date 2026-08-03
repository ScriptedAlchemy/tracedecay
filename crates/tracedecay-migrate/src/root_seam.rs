//! Compatibility paths for migration composition over downward crate APIs.

pub mod global_db {
    pub use tracedecay_global_db::*;
}

pub mod daemon {
    #[cfg(unix)]
    use std::path::PathBuf;

    use tracedecay_runtime_core::errors::Result;

    pub mod profile_identity {
        pub(crate) use crate::profile_identity::load_or_create;
    }

    pub mod store_runtime {
        pub use tracedecay_runtime_core::store_runtime::*;

        pub mod session_registry {
            pub(crate) use crate::session_runtime::DaemonSessionRuntimeRegistryV1;
        }
    }

    /// Offline replacement for the daemon-owned service lifecycle.
    ///
    /// It never attempts service mutation. A running daemon retains the shared
    /// lifecycle lease, so exclusive acquisition fails closed.
    pub struct QuiescedDaemonLifecycle {
        lifecycle_lease: Option<tracedecay_runtime_core::lifecycle_lease::LifecycleLease>,
    }

    impl QuiescedDaemonLifecycle {
        pub fn acquire(operation: &str) -> Result<Self> {
            tracedecay_runtime_core::lifecycle_lease::acquire_exclusive(operation).map(
                |lifecycle_lease| Self {
                    lifecycle_lease: Some(lifecycle_lease),
                },
            )
        }

        pub fn lifecycle_lease(
            &self,
        ) -> Result<&tracedecay_runtime_core::lifecycle_lease::LifecycleLease> {
            self.lifecycle_lease.as_ref().ok_or_else(|| {
                tracedecay_runtime_core::errors::TraceDecayError::Config {
                    message: "migration lifecycle lease was already released".to_owned(),
                }
            })
        }

        pub fn finish(mut self) -> Result<()> {
            drop(self.lifecycle_lease.take());
            Ok(())
        }
    }

    #[cfg(unix)]
    pub fn daemon_reachable() -> bool {
        use std::os::unix::net::UnixStream;

        daemon_socket_path().is_some_and(|path| UnixStream::connect(path).is_ok())
    }

    #[cfg(not(unix))]
    pub fn daemon_reachable() -> bool {
        false
    }

    #[cfg(unix)]
    fn daemon_socket_path() -> Option<PathBuf> {
        std::env::var_os("TRACEDECAY_DAEMON_SOCKET")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                tracedecay_runtime_core::config::user_data_dir()
                    .map(|root| root.join("daemon.sock"))
            })
    }
}

pub mod sessions {
    pub use tracedecay_sessions::runtime::{git_correlation, hermes, lcm, workflow_index};
}

pub mod agents {
    pub mod hermes {
        use std::path::Path;

        pub fn read_config_pinned_project_root(config_path: &Path) -> Option<String> {
            tracedecay_sessions::host_ports::hermes_profile_pin::resolve(config_path)
        }
    }
}

/// Host admission for migration composition.
///
/// This is the same facade the composition root uses
/// (`tracedecay::application::host_admission` re-exports it verbatim). The
/// legacy Hermes `state.db` import drives real observation capture and cursor
/// advance through it, so this must stay the production implementation rather
/// than a locally-defined stand-in.
pub mod application {
    pub mod host_admission {
        pub use tracedecay_usecases::host_admission::{
            HostAdmissionAuthorities, HostAdmissionFacade,
        };
    }
}

pub mod tracedecay_root {}

pub mod storage_adapters {
    use std::path::Path;

    use tracedecay_runtime_core::errors::Result;
    use tracedecay_runtime_core::storage::{
        ProjectStorageLocation, ProjectStorageStatus, classify_project_storage,
        classify_registry_storage_fields,
    };

    use crate::root_seam::global_db::{RegisteredGlobalDb, StoreInstanceRecord};

    pub async fn try_classify_project_storage_with_registry(
        project_root: &Path,
        global_db: &RegisteredGlobalDb,
        profile_root: &Path,
    ) -> Result<ProjectStorageLocation> {
        let location = classify_project_storage(project_root);
        if location.status != ProjectStorageStatus::Stale {
            return Ok(location);
        }
        let Some(store) = global_db
            .try_resolve_project_store_record_by_alias(project_root)
            .await?
        else {
            return Ok(location);
        };
        Ok(classify_registry_storage(project_root, profile_root, &store).unwrap_or(location))
    }

    pub fn classify_registry_storage(
        project_root: &Path,
        profile_root: &Path,
        store: &StoreInstanceRecord,
    ) -> Option<ProjectStorageLocation> {
        classify_registry_storage_fields(
            project_root,
            profile_root,
            &store.storage_mode,
            &store.store_relpath,
            store.manifest_relpath.as_deref(),
        )
    }
}
