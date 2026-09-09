use super::{RemoteDeletionCleanupError, cleanup_error, validate_project_id};
use crate::daemon::remote_deletion::{RemoteDeletionFailureCode, RemoteDeletionPhase};
use std::path::Path;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::super::{StoreAdministration, remote_recovery_lifecycle};

impl StoreAdministration {
    #[hotpath::skip]
    pub(super) async fn remote_deleted_project_roots(
        &self,
        database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        profile_root: &Path,
        project_id: &str,
    ) -> Result<std::collections::BTreeSet<std::path::PathBuf>> {
        remote_recovery_lifecycle::project_roots(
            database,
            &self.project_servers,
            profile_root,
            project_id,
        )
        .await
    }

    #[hotpath::skip]
    pub(super) async fn retire_remote_deleted_project_work(
        &self,
        profile_root: &Path,
        project_id: &str,
    ) -> Result<()> {
        remote_recovery_lifecycle::retire_runtime_work(
            &self.project_servers,
            &self.session_temporal_refresh_schedulers,
            #[cfg(unix)]
            &self.automation_schedulers,
            &self.project_server_retirements,
            profile_root,
            project_id,
            None,
        )
        .await
    }

    #[hotpath::skip]
    pub(super) async fn retire_remote_deleted_account_owners(
        &self,
        owners: &crate::daemon::remote_deletion::RemoteDeletionRuntimeOwners,
        profile_root: &Path,
    ) -> std::result::Result<(), RemoteDeletionCleanupError> {
        let open_tasks =
            crate::daemon::project_open_tasks(owners.project_open_gates.as_ref()).await;
        if !open_tasks
            .shutdown_profile_with_deadline(profile_root, crate::daemon::DAEMON_TASK_ABORT_DEADLINE)
            .await
        {
            return Err(cleanup_error(
                RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                RemoteDeletionPhase::CancelRuntimeOwners,
                true,
                TraceDecayError::Config {
                    message: "remote-deleted account project opens are still settling".to_owned(),
                },
            ));
        }
        self.shutdown_host_admission_replay().await;
        self.session_temporal_refresh_schedulers.shutdown().await;
        self.host_admission_brokers.lock().await.clear();
        #[cfg(unix)]
        if !self
            .settle_retirement_reapers(crate::daemon::DAEMON_TASK_ABORT_DEADLINE)
            .await
        {
            return Err(cleanup_error(
                RemoteDeletionFailureCode::RuntimeOwnersSettling,
                RemoteDeletionPhase::CancelRuntimeOwners,
                true,
                TraceDecayError::Config {
                    message: "remote-deleted account runtime owners are still settling".to_owned(),
                },
            ));
        }
        Ok(())
    }

    #[hotpath::skip]
    pub(super) async fn retire_remote_deleted_project_owners(
        &self,
        owners: &crate::daemon::remote_deletion::RemoteDeletionRuntimeOwners,
        database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        profile_root: &Path,
        project_id: &str,
    ) -> std::result::Result<
        std::sync::Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>,
        RemoteDeletionCleanupError,
    > {
        let retirement_error = |error| {
            cleanup_error(
                RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                RemoteDeletionPhase::CancelRuntimeOwners,
                true,
                error,
            )
        };
        validate_project_id(project_id).map_err(|error| {
            cleanup_error(
                RemoteDeletionFailureCode::InvalidRequest,
                RemoteDeletionPhase::ValidateRequest,
                false,
                TraceDecayError::Config {
                    message: format!("remote deletion project identity is invalid: {error}"),
                },
            )
        })?;
        let typed_project_id =
            tracedecay_store::ProjectId::new(project_id.to_owned()).map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::InvalidRequest,
                    RemoteDeletionPhase::ValidateRequest,
                    false,
                    TraceDecayError::Config {
                        message: format!("remote deletion project identity is invalid: {error}"),
                    },
                )
            })?;
        let data_root =
            tracedecay_runtime_core::storage::profile_sharded_data_root(profile_root, project_id);
        let identity = self.profile_identity().map_err(retirement_error)?;

        let project_roots = self
            .remote_deleted_project_roots(database, profile_root, project_id)
            .await
            .map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::ProjectEnumerationUnavailable,
                    RemoteDeletionPhase::EnumerateProjects,
                    true,
                    error,
                )
            })?;
        let open_tasks =
            crate::daemon::project_open_tasks(owners.project_open_gates.as_ref()).await;
        if !open_tasks
            .shutdown_project_identity(profile_root, project_id, &project_roots)
            .await
        {
            return Err(cleanup_error(
                RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                RemoteDeletionPhase::CancelRuntimeOwners,
                true,
                TraceDecayError::Config {
                    message: format!(
                        "remote-deleted project '{project_id}' open tasks did not drain"
                    ),
                },
            ));
        }
        owners
            .invocation
            .retire_project_runtime_owners(identity.profile_id(), &typed_project_id, &project_roots)
            .await
            .map_err(retirement_error)?;
        self.retire_remote_deleted_project_work(profile_root, project_id)
            .await
            .map_err(retirement_error)?;
        #[cfg(unix)]
        if !self
            .settle_retirement_reapers_for_project(
                profile_root,
                project_id,
                crate::daemon::DAEMON_TASK_ABORT_DEADLINE,
            )
            .await
        {
            return Err(cleanup_error(
                RemoteDeletionFailureCode::RuntimeOwnersSettling,
                RemoteDeletionPhase::CancelRuntimeOwners,
                true,
                TraceDecayError::Config {
                    message: format!(
                        "remote-deleted project '{project_id}' runtime owners are still settling"
                    ),
                },
            ));
        }
        self.retire_remote_deleted_project_storage(identity, &typed_project_id, &data_root)
            .await?;
        self.retire_remote_deleted_project_graphs(identity, &typed_project_id)
            .await
    }

    #[hotpath::skip]
    async fn retire_remote_deleted_project_storage(
        &self,
        identity: &tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1,
        typed_project_id: &tracedecay_store::ProjectId,
        data_root: &Path,
    ) -> std::result::Result<(), RemoteDeletionCleanupError> {
        let retirement_error = |error| {
            cleanup_error(
                RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                RemoteDeletionPhase::CancelRuntimeOwners,
                true,
                error,
            )
        };
        let project_sessions_path =
            data_root.join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);
        crate::daemon::hook_v2_replay_consumer::shutdown_hook_v2_replay_consumer(data_root).await;
        self.project_routes
            .forget_project(identity.profile_id(), typed_project_id.as_str())
            .map_err(retirement_error)?;
        self.git_index_transaction_services
            .retire_project_database(typed_project_id, &project_sessions_path)
            .await
            .map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                    RemoteDeletionPhase::CancelRuntimeOwners,
                    true,
                    TraceDecayError::Config {
                        message: format!(
                            "could not retire remote-deleted project Git transaction actors: {error}"
                        ),
                    },
                )
            })?;
        self.native_integration_services
            .retire_project_database(typed_project_id, &project_sessions_path)
            .await
            .map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                    RemoteDeletionPhase::CancelRuntimeOwners,
                    true,
                    TraceDecayError::Config {
                        message: format!(
                            "could not retire remote-deleted project native integration actors: {error}"
                        ),
                    },
                )
            })?;
        self.session_sync_service
            .retire_project(identity.profile_id(), typed_project_id)
            .await
            .map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                    RemoteDeletionPhase::CancelRuntimeOwners,
                    true,
                    TraceDecayError::Config {
                        message: format!(
                            "could not retire remote-deleted project session sync: {error}"
                        ),
                    },
                )
            })?;
        Ok(())
    }

    #[hotpath::skip]
    async fn retire_remote_deleted_project_graphs(
        &self,
        identity: &tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1,
        typed_project_id: &tracedecay_store::ProjectId,
    ) -> std::result::Result<
        std::sync::Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>,
        RemoteDeletionCleanupError,
    > {
        let retirement_error = |error| {
            cleanup_error(
                RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                RemoteDeletionPhase::CancelRuntimeOwners,
                true,
                error,
            )
        };
        let runtime_registry = self
            .session_runtime_registry()
            .await
            .map_err(retirement_error)?;
        let memory_shard = tracedecay_store::StoreShardIdV1::project(
            identity.brain_id().clone(),
            identity.profile_id().clone(),
            typed_project_id.clone(),
        );
        runtime_registry
            .retire_memory_graph_reconciliation_task(&memory_shard)
            .await
            .map_err(retirement_error)?;
        runtime_registry
            .retire_project_session_relation_graph(typed_project_id)
            .await
            .map_err(retirement_error)?;
        runtime_registry
            .retire_project_memory_graph(typed_project_id)
            .await
            .map_err(retirement_error)?;
        runtime_registry
            .drop_project_runtime_caches(typed_project_id)
            .await;

        Ok(runtime_registry)
    }
}
