//! Destructive lifecycle administration for mounted remote deletion requests.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::errors::{Result, TraceDecayError};

use super::super::remote_deletion::{
    RemoteDeletionExecutionError, RemoteDeletionFailureCode, RemoteDeletionPhase,
    RemoteDeletionReceipt, RemoteDeletionReceiptTarget,
};
use super::{
    StoreAdministration, authority, destructive_reservation_error, project_server_lifecycle,
};

struct RemoteDeletionCleanupError {
    code: RemoteDeletionFailureCode,
    phase: RemoteDeletionPhase,
    retryable: bool,
    source: TraceDecayError,
}

impl RemoteDeletionCleanupError {
    fn with_receipt(self, receipt: RemoteDeletionReceipt) -> RemoteDeletionExecutionError {
        RemoteDeletionExecutionError::new(
            receipt,
            self.code,
            self.phase,
            self.retryable,
            self.source,
        )
    }
}

fn cleanup_error(
    code: RemoteDeletionFailureCode,
    phase: RemoteDeletionPhase,
    retryable: bool,
    source: TraceDecayError,
) -> RemoteDeletionCleanupError {
    RemoteDeletionCleanupError {
        code,
        phase,
        retryable,
        source,
    }
}

fn validate_project_id(project_id: &str) -> std::result::Result<(), &'static str> {
    crate::storage::validate_project_id(project_id)
}

impl StoreAdministration {
    /// Applies an authenticated remote account or project deletion through the
    /// profile's one registered authority. The durable tombstone is written
    /// before any runtime is retired or store directory is removed, so a
    /// failed cleanup stays fail-closed and a retry resumes safely.
    pub(in super::super) async fn execute_remote_deletion(
        &self,
        owners: &super::super::remote_deletion::RemoteDeletionRuntimeOwners,
        target: RemoteDeletionReceiptTarget,
        project_id: Option<String>,
        tombstone_id: String,
    ) -> std::result::Result<RemoteDeletionReceipt, RemoteDeletionExecutionError> {
        let mut receipt =
            RemoteDeletionReceipt::pending(target, None, tombstone_id.clone(), project_id.clone());
        if tombstone_id.trim().is_empty() || tombstone_id.len() > 256 {
            return Err(RemoteDeletionExecutionError::new(
                receipt,
                RemoteDeletionFailureCode::InvalidRequest,
                RemoteDeletionPhase::ValidateRequest,
                false,
                TraceDecayError::Config {
                    message: "remote deletion tombstone id must be non-empty and at most 256 bytes"
                        .to_owned(),
                },
            ));
        }
        let profile_identity = self.profile_identity().cloned().map_err(|error| {
            RemoteDeletionExecutionError::new(
                receipt.clone(),
                RemoteDeletionFailureCode::AuthorityUnavailable,
                RemoteDeletionPhase::ResolveAuthority,
                true,
                error,
            )
        })?;
        let profile_root = authority::canonical_identity_path(profile_identity.profile_root())
            .map_err(|error| {
                RemoteDeletionExecutionError::new(
                    receipt.clone(),
                    RemoteDeletionFailureCode::AuthorityUnavailable,
                    RemoteDeletionPhase::ResolveAuthority,
                    true,
                    error,
                )
            })?;
        let profile_id = profile_identity.profile_id().as_str().to_owned();
        receipt.profile_id = Some(profile_id.clone());
        let recorded_at_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| {
                RemoteDeletionExecutionError::new(
                    receipt.clone(),
                    RemoteDeletionFailureCode::AuthorityUnavailable,
                    RemoteDeletionPhase::ResolveAuthority,
                    true,
                    TraceDecayError::Config {
                        message: format!("remote deletion clock is before Unix epoch: {error}"),
                    },
                )
            })?
            .as_micros()
            .try_into()
            .map_err(|_| {
                RemoteDeletionExecutionError::new(
                    receipt.clone(),
                    RemoteDeletionFailureCode::AuthorityUnavailable,
                    RemoteDeletionPhase::ResolveAuthority,
                    true,
                    TraceDecayError::Config {
                        message: "remote deletion timestamp exceeds supported range".to_owned(),
                    },
                )
            })?;
        let database = self
            .raw_registered_profile_database()
            .await
            .map_err(|error| {
                RemoteDeletionExecutionError::new(
                    receipt.clone(),
                    RemoteDeletionFailureCode::AuthorityUnavailable,
                    RemoteDeletionPhase::ResolveAuthority,
                    true,
                    error,
                )
            })?;

        self.with_writer(|| async {
            match target {
                RemoteDeletionReceiptTarget::Project => {
                    let project_id = project_id.ok_or_else(|| {
                        RemoteDeletionExecutionError::new(
                            receipt.clone(),
                            RemoteDeletionFailureCode::InvalidRequest,
                            RemoteDeletionPhase::ValidateRequest,
                            false,
                            TraceDecayError::Config {
                                message: "remote project deletion requires a project id".to_owned(),
                            },
                        )
                    })?;
                    validate_project_id(&project_id).map_err(|error| {
                        RemoteDeletionExecutionError::new(
                            receipt.clone(),
                            RemoteDeletionFailureCode::InvalidRequest,
                            RemoteDeletionPhase::ValidateRequest,
                            false,
                            TraceDecayError::Config {
                                message: format!(
                                    "remote deletion project identity is invalid: {error}"
                                ),
                            },
                        )
                    })?;
                    let existing_tombstone = database
                        .remote_deletion_tombstone(
                            &profile_id,
                            crate::global_db::RemoteDeletionTarget::Project,
                            Some(&project_id),
                        )
                        .await
                        .map_err(|error| {
                            RemoteDeletionExecutionError::new(
                                receipt.clone(),
                                RemoteDeletionFailureCode::AuthorityUnavailable,
                                RemoteDeletionPhase::ResolveTarget,
                                true,
                                error,
                            )
                        })?;
                    let exact_context = database
                        .project_registry_context_by_id(&project_id)
                        .await
                        .map_err(|error| {
                            RemoteDeletionExecutionError::new(
                                receipt.clone(),
                                RemoteDeletionFailureCode::AuthorityUnavailable,
                                RemoteDeletionPhase::ResolveTarget,
                                true,
                                error,
                            )
                        })?;
                    let persisted_identity =
                        tracedecay_runtime_core::storage::ValidatedProfileShard::resolve_existing(
                            &profile_root,
                            &project_id,
                        )
                        .is_ok();
                    if existing_tombstone.is_none()
                        && exact_context
                            .as_ref()
                            .is_none_or(|context| context.project.project_id != project_id)
                        && !persisted_identity
                    {
                        return Err(RemoteDeletionExecutionError::new(
                            receipt,
                            RemoteDeletionFailureCode::TargetNotFound,
                            RemoteDeletionPhase::ResolveTarget,
                            false,
                            TraceDecayError::Config {
                                message: "remote deletion target is not registered to the authenticated profile".to_owned(),
                            },
                        ));
                    }
                    let tombstone = crate::global_db::RemoteDeletionTombstone {
                        target: crate::global_db::RemoteDeletionTarget::Project,
                        profile_id: profile_id.clone(),
                        project_id: Some(project_id.clone()),
                        tombstone_id,
                        recorded_at_micros,
                        cleanup: crate::global_db::RemoteDeletionCleanupState::Pending,
                    };
                    let tombstone_outcome = database
                        .record_remote_deletion_tombstone(tombstone)
                        .await
                        .map_err(|error| {
                            RemoteDeletionExecutionError::new(
                                receipt.clone(),
                                RemoteDeletionFailureCode::TombstoneUnavailable,
                                RemoteDeletionPhase::PersistTombstone,
                                true,
                                error,
                            )
                        })?;
                    let tombstone = match tombstone_outcome {
                        crate::global_db::RemoteDeletionTombstoneRecordOutcome::Recorded(
                            tombstone,
                        )
                        | crate::global_db::RemoteDeletionTombstoneRecordOutcome::Replayed(
                            tombstone,
                        ) => tombstone,
                        crate::global_db::RemoteDeletionTombstoneRecordOutcome::Conflict {
                            existing,
                        } => {
                            return Err(RemoteDeletionExecutionError::new(
                                receipt.clone(),
                                RemoteDeletionFailureCode::TombstoneConflict,
                                RemoteDeletionPhase::PersistTombstone,
                                false,
                                TraceDecayError::Config {
                                    message: format!(
                                        "remote deletion target already has tombstone '{}'",
                                        existing.tombstone_id
                                    ),
                                },
                            ));
                        }
                    };
                    receipt.tombstone_id = Some(tombstone.tombstone_id.clone());
                    receipt.tombstone_recorded = true;
                    if tombstone.cleanup == crate::global_db::RemoteDeletionCleanupState::Deleted {
                        return Ok(receipt.complete());
                    }
                    if let Err(failure) = self.remove_remote_deleted_project(
                        owners,
                        &database,
                        &profile_root,
                        &project_id,
                    )
                    .await
                    {
                        let cleanup = if matches!(
                            failure.code,
                            RemoteDeletionFailureCode::RuntimeOwnersSettling
                                | RemoteDeletionFailureCode::RuntimeRetirementIncomplete
                        ) {
                            crate::global_db::RemoteDeletionCleanupState::Settling {
                                failure_code: failure.code,
                                phase: failure.phase,
                                retryable: failure.retryable,
                            }
                        } else {
                            crate::global_db::RemoteDeletionCleanupState::Partial {
                                failure_code: failure.code,
                                phase: failure.phase,
                                retryable: failure.retryable,
                            }
                        };
                        database
                            .transition_remote_deletion_tombstone(
                                &tombstone,
                                tombstone.cleanup.clone(),
                                cleanup,
                            )
                            .await
                            .map_err(|error| {
                                RemoteDeletionExecutionError::new(
                                    receipt.clone(),
                                    RemoteDeletionFailureCode::TombstoneUnavailable,
                                    RemoteDeletionPhase::PersistTombstone,
                                    true,
                                    error,
                                )
                            })?;
                        return Err(failure.with_receipt(receipt));
                    }
                    database
                        .transition_remote_deletion_tombstone(
                            &tombstone,
                            tombstone.cleanup.clone(),
                            crate::global_db::RemoteDeletionCleanupState::Deleted,
                        )
                        .await
                        .map_err(|error| {
                            RemoteDeletionExecutionError::new(
                                receipt.clone(),
                                RemoteDeletionFailureCode::TombstoneUnavailable,
                                RemoteDeletionPhase::PersistTombstone,
                                true,
                                error,
                            )
                        })?;
                    receipt.removed_project_ids.push(project_id);
                    Ok(receipt.complete())
                }
                RemoteDeletionReceiptTarget::Account => {
                    if project_id.is_some() {
                        return Err(RemoteDeletionExecutionError::new(
                            receipt,
                            RemoteDeletionFailureCode::InvalidRequest,
                            RemoteDeletionPhase::ValidateRequest,
                            false,
                            TraceDecayError::Config {
                                message: "remote account deletion must not name a project"
                                    .to_owned(),
                            },
                        ));
                    }
                    let tombstone = crate::global_db::RemoteDeletionTombstone {
                        target: crate::global_db::RemoteDeletionTarget::Account,
                        profile_id: profile_id.clone(),
                        project_id: None,
                        tombstone_id,
                        recorded_at_micros,
                        cleanup: crate::global_db::RemoteDeletionCleanupState::Pending,
                    };
                    let tombstone_outcome = database
                        .record_remote_deletion_tombstone(tombstone)
                        .await
                        .map_err(|error| {
                            RemoteDeletionExecutionError::new(
                                receipt.clone(),
                                RemoteDeletionFailureCode::TombstoneUnavailable,
                                RemoteDeletionPhase::PersistTombstone,
                                true,
                                error,
                            )
                        })?;
                    let tombstone = match tombstone_outcome {
                        crate::global_db::RemoteDeletionTombstoneRecordOutcome::Recorded(
                            tombstone,
                        )
                        | crate::global_db::RemoteDeletionTombstoneRecordOutcome::Replayed(
                            tombstone,
                        ) => tombstone,
                        crate::global_db::RemoteDeletionTombstoneRecordOutcome::Conflict {
                            existing,
                        } => {
                            return Err(RemoteDeletionExecutionError::new(
                                receipt.clone(),
                                RemoteDeletionFailureCode::TombstoneConflict,
                                RemoteDeletionPhase::PersistTombstone,
                                false,
                                TraceDecayError::Config {
                                    message: format!(
                                        "remote deletion target already has tombstone '{}'",
                                        existing.tombstone_id
                                    ),
                                },
                            ));
                        }
                    };
                    receipt.tombstone_id = Some(tombstone.tombstone_id.clone());
                    receipt.tombstone_recorded = true;
                    if tombstone.cleanup == crate::global_db::RemoteDeletionCleanupState::Deleted {
                        return Ok(receipt.complete());
                    }
                    self.shutdown_host_admission_replay().await;
                    self.session_temporal_refresh_schedulers.shutdown().await;
                    self.host_admission_brokers.lock().await.clear();
                    let projects = match self
                        .remote_deletion_project_ids(&database, &profile_root)
                        .await
                    {
                        Ok(projects) => projects,
                        Err(error) => {
                            database
                                .transition_remote_deletion_tombstone(
                                    &tombstone,
                                    tombstone.cleanup.clone(),
                                    crate::global_db::RemoteDeletionCleanupState::Partial {
                                        failure_code:
                                            RemoteDeletionFailureCode::ProjectEnumerationUnavailable,
                                        phase: RemoteDeletionPhase::EnumerateProjects,
                                        retryable: true,
                                    },
                                )
                                .await
                                .map_err(|transition_error| {
                                    RemoteDeletionExecutionError::new(
                                        receipt.clone(),
                                        RemoteDeletionFailureCode::TombstoneUnavailable,
                                        RemoteDeletionPhase::PersistTombstone,
                                        true,
                                        transition_error,
                                    )
                                })?;
                            return Err(RemoteDeletionExecutionError::new(
                                receipt,
                                RemoteDeletionFailureCode::ProjectEnumerationUnavailable,
                                RemoteDeletionPhase::EnumerateProjects,
                                true,
                                error,
                            ));
                        }
                    };
                    receipt.pending_project_ids = projects.iter().cloned().collect();
                    for project_id in projects {
                        if let Err(failure) = self.remove_remote_deleted_project(
                            owners,
                            &database,
                            &profile_root,
                            &project_id,
                        )
                        .await
                        {
                            let cleanup = if matches!(
                                failure.code,
                                RemoteDeletionFailureCode::RuntimeOwnersSettling
                                    | RemoteDeletionFailureCode::RuntimeRetirementIncomplete
                            ) {
                                crate::global_db::RemoteDeletionCleanupState::Settling {
                                    failure_code: failure.code,
                                    phase: failure.phase,
                                    retryable: failure.retryable,
                                }
                            } else {
                                crate::global_db::RemoteDeletionCleanupState::Partial {
                                    failure_code: failure.code,
                                    phase: failure.phase,
                                    retryable: failure.retryable,
                                }
                            };
                            database
                                .transition_remote_deletion_tombstone(
                                    &tombstone,
                                    tombstone.cleanup.clone(),
                                    cleanup,
                                )
                                .await
                                .map_err(|error| {
                                    RemoteDeletionExecutionError::new(
                                        receipt.clone(),
                                        RemoteDeletionFailureCode::TombstoneUnavailable,
                                        RemoteDeletionPhase::PersistTombstone,
                                        true,
                                        error,
                                    )
                                })?;
                            return Err(failure.with_receipt(receipt));
                        }
                        receipt.removed_project_ids.push(project_id.clone());
                        receipt
                            .pending_project_ids
                            .retain(|pending| pending != &project_id);
                    }
                    database
                        .transition_remote_deletion_tombstone(
                            &tombstone,
                            tombstone.cleanup.clone(),
                            crate::global_db::RemoteDeletionCleanupState::Deleted,
                        )
                        .await
                        .map_err(|error| {
                            RemoteDeletionExecutionError::new(
                                receipt.clone(),
                                RemoteDeletionFailureCode::TombstoneUnavailable,
                                RemoteDeletionPhase::PersistTombstone,
                                true,
                                error,
                            )
                        })?;
                    Ok(receipt.complete())
                }
            }
        })
        .await
    }

    async fn remote_deletion_project_ids(
        &self,
        database: &Arc<crate::global_db::RegisteredGlobalDb>,
        profile_root: &Path,
    ) -> Result<BTreeSet<String>> {
        let mut project_ids = database
            .list_code_projects(usize::MAX)
            .await?
            .into_iter()
            .map(|project| project.project_id)
            .collect::<BTreeSet<_>>();
        let projects_root = profile_root.join("projects");
        let metadata = match std::fs::symlink_metadata(&projects_root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(project_ids),
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "could not inspect authenticated profile project root '{}': {error}",
                        projects_root.display()
                    ),
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "authenticated profile project root '{}' is not a regular directory",
                    projects_root.display()
                ),
            });
        }
        for entry in std::fs::read_dir(&projects_root).map_err(|error| TraceDecayError::Config {
            message: format!(
                "could not enumerate authenticated profile project root '{}': {error}",
                projects_root.display()
            ),
        })? {
            let entry = entry.map_err(|error| TraceDecayError::Config {
                message: format!(
                    "could not read authenticated profile project entry '{}': {error}",
                    projects_root.display()
                ),
            })?;
            let file_type = entry.file_type().map_err(|error| TraceDecayError::Config {
                message: format!(
                    "could not inspect authenticated profile project entry '{}': {error}",
                    entry.path().display()
                ),
            })?;
            if !file_type.is_dir() {
                continue;
            }
            let project_id = entry.file_name().to_string_lossy().into_owned();
            tracedecay_store::ProjectId::new(project_id.clone()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!(
                        "profile project directory '{}' has an invalid project identity: {error}",
                        entry.path().display()
                    ),
                }
            })?;
            project_ids.insert(project_id);
        }
        Ok(project_ids)
    }

    async fn remove_remote_deleted_project(
        &self,
        owners: &super::super::remote_deletion::RemoteDeletionRuntimeOwners,
        database: &Arc<crate::global_db::RegisteredGlobalDb>,
        profile_root: &Path,
        project_id: &str,
    ) -> std::result::Result<(), RemoteDeletionCleanupError> {
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
        let data_root = crate::storage::profile_sharded_data_root(profile_root, project_id);
        let project_sessions_path = data_root.join(crate::storage::SESSIONS_DB_FILENAME);

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
        let open_tasks = super::super::project_open_tasks(owners.project_open_gates.as_ref()).await;
        if !open_tasks
            .shutdown_project_roots(profile_root, &project_roots)
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
            .retire_remote_deleted_project(&typed_project_id, &project_roots)
            .await
            .map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                    RemoteDeletionPhase::CancelRuntimeOwners,
                    true,
                    error,
                )
            })?;
        self.retire_remote_deleted_project_work(profile_root, project_id)
            .await
            .map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                    RemoteDeletionPhase::CancelRuntimeOwners,
                    true,
                    error,
                )
            })?;
        crate::daemon::hook_v2_replay::shutdown_hook_v2_replay_consumer(&data_root).await;
        self.project_routes
            .forget_project(project_id)
            .map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                    RemoteDeletionPhase::CancelRuntimeOwners,
                    true,
                    error,
                )
            })?;
        self.git_index_transaction_services
            .retire_project_database(&typed_project_id, &project_sessions_path)
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
        let runtime_registry = self.session_runtime_registry().await.map_err(|error| {
            cleanup_error(
                RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                RemoteDeletionPhase::CancelRuntimeOwners,
                true,
                error,
            )
        })?;
        runtime_registry
            .drop_project_runtime_caches(&typed_project_id)
            .await;

        if !data_root.exists() {
            // An already-absent exact shard is the idempotent success case.
        } else {
            let metadata = std::fs::symlink_metadata(&data_root).map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::ShardCleanupFailed,
                    RemoteDeletionPhase::RemoveShard,
                    true,
                    TraceDecayError::Config {
                        message: format!(
                            "could not inspect remote-deleted project store '{}': {error}",
                            data_root.display()
                        ),
                    },
                )
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(cleanup_error(
                    RemoteDeletionFailureCode::ShardCleanupFailed,
                    RemoteDeletionPhase::RemoveShard,
                    false,
                    TraceDecayError::Config {
                        message: format!(
                            "remote-deleted project store '{}' is not a regular directory",
                            data_root.display()
                        ),
                    },
                ));
            }
            let canonical_data_root =
                authority::canonical_identity_path(&data_root).map_err(|error| {
                    cleanup_error(
                        RemoteDeletionFailureCode::ShardCleanupFailed,
                        RemoteDeletionPhase::RemoveShard,
                        false,
                        error,
                    )
                })?;
            if canonical_data_root != data_root || !canonical_data_root.starts_with(profile_root) {
                return Err(cleanup_error(
                    RemoteDeletionFailureCode::ShardCleanupFailed,
                    RemoteDeletionPhase::RemoveShard,
                    false,
                    TraceDecayError::Config {
                        message: format!(
                            "remote-deleted project store '{}' is outside its exact profile root",
                            data_root.display()
                        ),
                    },
                ));
            }
            let database_paths = [
                data_root.join(crate::config::db_filename(&data_root)),
                project_sessions_path.clone(),
            ]
            .into_iter()
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
            if database_paths.is_empty() {
                std::fs::remove_dir_all(&data_root).map_err(|error| {
                    cleanup_error(
                        RemoteDeletionFailureCode::ShardCleanupFailed,
                        RemoteDeletionPhase::RemoveShard,
                        true,
                        TraceDecayError::Config {
                            message: format!(
                                "failed to remove remote-deleted project store '{}': {error}",
                                data_root.display()
                            ),
                        },
                    )
                })?;
            } else {
                let reservation = runtime_registry
                    .begin_destructive_code_maintenance(&data_root, database_paths.clone())
                    .await
                    .map_err(|error| {
                        cleanup_error(
                            RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                            RemoteDeletionPhase::CancelRuntimeOwners,
                            true,
                            error,
                        )
                    })?;
                if let Err(error) = self.prove_no_external_branch_store_holders(&database_paths) {
                    reservation
                        .abort_preserved()
                        .map_err(destructive_reservation_error)
                        .map_err(|reservation_error| {
                            cleanup_error(
                                RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                                RemoteDeletionPhase::CancelRuntimeOwners,
                                true,
                                reservation_error,
                            )
                        })?;
                    return Err(cleanup_error(
                        RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                        RemoteDeletionPhase::CancelRuntimeOwners,
                        true,
                        error,
                    ));
                }
                if let Err(error) = std::fs::remove_dir_all(&data_root) {
                    reservation
                        .abort_preserved()
                        .map_err(destructive_reservation_error)
                        .map_err(|reservation_error| {
                            cleanup_error(
                                RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                                RemoteDeletionPhase::CancelRuntimeOwners,
                                true,
                                reservation_error,
                            )
                        })?;
                    return Err(cleanup_error(
                        RemoteDeletionFailureCode::ShardCleanupFailed,
                        RemoteDeletionPhase::RemoveShard,
                        true,
                        TraceDecayError::Config {
                            message: format!(
                                "failed to remove remote-deleted project store '{}': {error}",
                                data_root.display()
                            ),
                        },
                    ));
                }
                reservation
                    .finish_deleted()
                    .map_err(destructive_reservation_error)
                    .map_err(|error| {
                        cleanup_error(
                            RemoteDeletionFailureCode::ShardCleanupFailed,
                            RemoteDeletionPhase::RemoveShard,
                            true,
                            error,
                        )
                    })?;
            }
        }
        database
            .delete_remote_deleted_project_registry_row(project_id)
            .await
            .map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::RegistryCleanupFailed,
                    RemoteDeletionPhase::RemoveRegistryEntry,
                    true,
                    error,
                )
            })?;
        Ok(())
    }

    async fn remote_deleted_project_roots(
        &self,
        database: &Arc<crate::global_db::RegisteredGlobalDb>,
        profile_root: &Path,
        project_id: &str,
    ) -> Result<BTreeSet<std::path::PathBuf>> {
        let mut roots = BTreeSet::new();
        if let Some(context) = database.project_registry_context_by_id(project_id).await? {
            roots.insert(std::path::PathBuf::from(context.project.canonical_root));
            roots.insert(std::path::PathBuf::from(context.project.display_root));
            if let Some(git_common_dir) = context.project.git_common_dir {
                roots.insert(std::path::PathBuf::from(git_common_dir));
            }
            roots.extend(
                context
                    .aliases
                    .into_iter()
                    .map(|alias| std::path::PathBuf::from(alias.alias_path)),
            );
        }
        {
            let registry = self.project_servers.lock().await;
            roots.extend(
                registry
                    .servers
                    .keys()
                    .filter(|key| {
                        key.owner.profile_root == profile_root
                            && key.owner.project_id.as_deref() == Some(project_id)
                    })
                    .map(|key| key.project_root.clone()),
            );
        }
        roots.retain(|root| root.is_absolute());
        Ok(roots)
    }

    async fn retire_remote_deleted_project_work(
        &self,
        profile_root: &Path,
        project_id: &str,
    ) -> Result<()> {
        let (owners, servers) = {
            let mut registry = self.project_servers.lock().await;
            let owners = registry
                .servers
                .keys()
                .filter(|key| {
                    key.owner.profile_root == profile_root
                        && key.owner.project_id.as_deref() == Some(project_id)
                })
                .map(|key| key.owner.clone())
                .collect::<Vec<_>>();
            let servers = owners
                .iter()
                .flat_map(|owner| registry.remove_owner(owner))
                .collect::<Vec<_>>();
            (owners, servers)
        };
        for server in &servers {
            server.revoke_project_server_responses();
            server.cancel_startup_transcript_ingest();
            server.abort_project_server_requests();
        }
        for owner in &owners {
            self.session_temporal_refresh_schedulers
                .retire_project(owner)
                .await;
        }
        #[cfg(unix)]
        self.abort_remote_deleted_maintenance_schedulers(profile_root, project_id)
            .await?;
        project_server_lifecycle::schedule_project_server_retirement(self, servers, None).await;
        if !self
            .settle_project_server_retirements(super::super::DAEMON_TASK_ABORT_DEADLINE)
            .await
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "remote-deleted project '{project_id}' runtime owners are still settling"
                ),
            });
        }
        Ok(())
    }

    #[cfg(unix)]
    async fn abort_remote_deleted_maintenance_schedulers(
        &self,
        profile_root: &Path,
        project_id: &str,
    ) -> Result<()> {
        let mut tasks = Vec::new();
        {
            let mut schedulers = self.automation_schedulers.lock().await;
            let keys = schedulers
                .keys()
                .filter(|key| {
                    key.owner.profile_root == profile_root
                        && key.owner.project_id.as_deref() == Some(project_id)
                })
                .cloned()
                .collect::<Vec<_>>();
            for key in keys {
                if let Some(mut scheduler) = schedulers.remove(&key)
                    && let Some(task) = scheduler.task.take()
                {
                    tasks.push(task);
                }
            }
        }
        {
            let mut schedulers = self.memory_repair_schedulers.lock().await;
            let keys = schedulers
                .keys()
                .filter(|key| {
                    key.owner.profile_root == profile_root
                        && key.owner.project_id.as_deref() == Some(project_id)
                })
                .cloned()
                .collect::<Vec<_>>();
            for key in keys {
                if let Some(mut scheduler) = schedulers.remove(&key)
                    && let Some(task) = scheduler.task.take()
                {
                    tasks.push(task);
                }
            }
        }
        for task in tasks {
            task.abort();
            self.track_project_server_retirement(task).await;
        }
        if !self
            .settle_project_server_retirements(super::super::DAEMON_TASK_ABORT_DEADLINE)
            .await
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "remote-deleted project '{project_id}' maintenance tasks are still settling"
                ),
            });
        }
        Ok(())
    }
}
