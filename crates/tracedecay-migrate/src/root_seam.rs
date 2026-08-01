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

    pub mod code_index_scheduler {
        pub mod identity {
            use std::path::Path;

            use sha2::{Digest, Sha256};
            use tracedecay_domain::{RepositoryId, WorktreeId};
            use tracedecay_runtime_core::errors::{Result, TraceDecayError};

            pub(crate) struct IndexingIdentityV1 {
                repository_id: RepositoryId,
                worktree_id: WorktreeId,
            }

            impl IndexingIdentityV1 {
                pub(crate) fn resolve(project_root: &Path) -> Result<Self> {
                    let common = tracedecay_runtime_core::worktree::git_common_dir(project_root)
                        .unwrap_or_else(|| project_root.to_path_buf());
                    let repository_id = RepositoryId::new(format!(
                        "repository.daemon.{}",
                        sha256_hex(common.to_string_lossy().as_bytes())
                    ))
                    .map_err(identity_error)?;
                    let worktree_id = WorktreeId::new(format!(
                        "worktree.daemon.{}",
                        sha256_hex(project_root.to_string_lossy().as_bytes())
                    ))
                    .map_err(identity_error)?;
                    Ok(Self {
                        repository_id,
                        worktree_id,
                    })
                }

                pub(crate) fn repository_id(&self) -> &RepositoryId {
                    &self.repository_id
                }

                pub(crate) fn worktree_id(&self) -> &WorktreeId {
                    &self.worktree_id
                }
            }

            fn sha256_hex(bytes: &[u8]) -> String {
                hex::encode(Sha256::digest(bytes))
            }

            fn identity_error(error: impl std::fmt::Display) -> TraceDecayError {
                TraceDecayError::Config {
                    message: format!("code-index identity: {error}"),
                }
            }
        }
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

pub mod application {
    pub mod host_admission {
        use tracedecay_domain::{
            BrainId, ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceIdentityV1,
            ProjectId, UserProfileId,
        };
        use tracedecay_sessions::admission::{
            AdmissionFuture, HostAdmission, HostAdmissionOutcome, HostProjectionDrainOutcome,
        };
        use tracedecay_sessions::observation::{
            CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
        };
        use tracedecay_store::ParseOffset;
        use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};

        use crate::root_seam::global_db::RegisteredGlobalDb;

        pub struct HostAdmissionAuthorities<'a> {
            project_id: ProjectId,
            registered: &'a RegisteredGlobalDb,
            _brain_id: BrainId,
            _profile_id: UserProfileId,
        }

        impl<'a> HostAdmissionAuthorities<'a> {
            pub fn for_project(
                brain_id: BrainId,
                profile_id: UserProfileId,
                project_id: ProjectId,
                registered: &'a RegisteredGlobalDb,
            ) -> Self {
                Self {
                    project_id,
                    registered,
                    _brain_id: brain_id,
                    _profile_id: profile_id,
                }
            }
        }

        pub struct HostAdmissionFacade<'a> {
            authorities: HostAdmissionAuthorities<'a>,
        }

        impl<'a> HostAdmissionFacade<'a> {
            pub const fn new(authorities: HostAdmissionAuthorities<'a>) -> Self {
                Self { authorities }
            }

            fn validate_scope(
                &self,
                scope: &ObservationScopeV1,
            ) -> Result<(), HostAdmissionOutcome> {
                match scope {
                    ObservationScopeV1::Project { project_id }
                        if project_id == &self.authorities.project_id =>
                    {
                        Ok(())
                    }
                    ObservationScopeV1::Project { .. } => {
                        Err(HostAdmissionOutcome::project_authority_mismatch())
                    }
                    _ => Err(HostAdmissionOutcome::project_authority_unbound()),
                }
            }
        }

        impl HostAdmission for HostAdmissionFacade<'_> {
            fn capture_observation<'a>(
                &'a self,
                request: CaptureObservationRequest,
            ) -> AdmissionFuture<'a, CaptureObservationOutcome> {
                Box::pin(async move {
                    self.validate_scope(request.scope())?;
                    Err(HostAdmissionOutcome::retained_unavailable(
                        "migration_host_admission_unavailable",
                    ))
                })
            }

            fn advance_non_durable_source_cursor<'a>(
                &'a self,
                advance: ObservationCursorAdvance,
                _cancellation: ObservationCancellation,
            ) -> AdmissionFuture<'a, CursorAdvanceOutcome> {
                Box::pin(async move {
                    self.validate_scope(advance.next_cursor().scope())?;
                    Err(HostAdmissionOutcome::retained_unavailable(
                        "migration_host_admission_unavailable",
                    ))
                })
            }

            fn get_source_cursor<'a>(
                &'a self,
                _source: &'a ObservationSourceIdentityV1,
                scope: &'a ObservationScopeV1,
            ) -> AdmissionFuture<'a, Option<ObservationSourceCursorV1>> {
                Box::pin(async move {
                    self.validate_scope(scope)?;
                    Err(HostAdmissionOutcome::retained_unavailable(
                        "migration_host_admission_unavailable",
                    ))
                })
            }

            fn drain_projection_queue<'a>(
                &'a self,
                _provider: &'a str,
                scope: &'a ObservationScopeV1,
                _cancellation: &'a ObservationCancellation,
                _max: usize,
            ) -> AdmissionFuture<'a, HostProjectionDrainOutcome> {
                Box::pin(async move {
                    self.validate_scope(scope)?;
                    Err(HostAdmissionOutcome::retained_unavailable(
                        "migration_host_admission_unavailable",
                    ))
                })
            }

            fn has_session_message<'a>(
                &'a self,
                scope: &'a ObservationScopeV1,
                provider: &'a str,
                message_id: &'a str,
            ) -> AdmissionFuture<'a, bool> {
                Box::pin(async move {
                    self.validate_scope(scope)?;
                    self.authorities
                        .registered
                        .has_session_message(provider, message_id)
                        .await
                        .map_err(|_| HostAdmissionOutcome::registered_authority_unavailable())
                })
            }

            fn get_parse_offset<'a>(
                &'a self,
                scope: &'a ObservationScopeV1,
                path: &'a str,
            ) -> AdmissionFuture<'a, Option<ParseOffset>> {
                Box::pin(async move {
                    self.validate_scope(scope)?;
                    self.authorities
                        .registered
                        .get_parse_offset_result(path)
                        .await
                        .map_err(|_| HostAdmissionOutcome::registered_authority_unavailable())
                })
            }

            fn advance_parse_offset<'a>(
                &'a self,
                scope: &'a ObservationScopeV1,
                path: &'a str,
                offset: ParseOffset,
            ) -> AdmissionFuture<'a, ()> {
                Box::pin(async move {
                    self.validate_scope(scope)?;
                    self.authorities
                        .registered
                        .advance_parse_offset_result(path, offset)
                        .await
                        .map_err(|_| HostAdmissionOutcome::registered_authority_unavailable())
                })
            }
        }
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
