//! Destructive lifecycle administration for mounted remote deletion requests.

mod runtime_retirement;
mod shard_cleanup;

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_daemon_identity::authority;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::super::remote_deletion::{
    RemoteDeletionExecutionError, RemoteDeletionFailureCode, RemoteDeletionPhase,
    RemoteDeletionReceipt, RemoteDeletionReceiptTarget,
};
use super::StoreAdministration;

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
    tracedecay_runtime_core::storage::validate_project_id(project_id)
}

impl StoreAdministration {
    /// Applies an authenticated remote account or project deletion through the
    /// profile's one registered authority. The durable tombstone is written
    /// before any runtime is retired or store directory is removed, so a
    /// failed cleanup stays fail-closed and a retry resumes safely.
    #[hotpath::measure(label = "daemon.branch_admin.remote_deletion", future = true)]
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

        let tombstone = tracedecay_global_db::RemoteDeletionTombstone {
            target: match target {
                RemoteDeletionReceiptTarget::Project => {
                    tracedecay_global_db::RemoteDeletionTarget::Project
                }
                RemoteDeletionReceiptTarget::Account => {
                    tracedecay_global_db::RemoteDeletionTarget::Account
                }
            },
            profile_id,
            project_id,
            tombstone_id,
            recorded_at_micros,
            cleanup: tracedecay_global_db::RemoteDeletionCleanupState::Pending,
        };
        self.with_writer(|| {
            self.execute_remote_deletion_at_profile(
                owners,
                &database,
                &profile_root,
                tombstone,
                receipt,
            )
        })
        .await
    }

    #[hotpath::skip]
    async fn execute_remote_deletion_at_profile(
        &self,
        owners: &super::super::remote_deletion::RemoteDeletionRuntimeOwners,
        database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        profile_root: &Path,
        tombstone: tracedecay_global_db::RemoteDeletionTombstone,
        mut receipt: RemoteDeletionReceipt,
    ) -> std::result::Result<RemoteDeletionReceipt, RemoteDeletionExecutionError> {
        match tombstone.target {
            tracedecay_global_db::RemoteDeletionTarget::Project => {
                validate_project_deletion_target(database, profile_root, &tombstone, &receipt)
                    .await?;
            }
            tracedecay_global_db::RemoteDeletionTarget::Account
                if tombstone.project_id.is_some() =>
            {
                return Err(RemoteDeletionExecutionError::new(
                    receipt,
                    RemoteDeletionFailureCode::InvalidRequest,
                    RemoteDeletionPhase::ValidateRequest,
                    false,
                    TraceDecayError::Config {
                        message: "remote account deletion must not name a project".to_owned(),
                    },
                ));
            }
            tracedecay_global_db::RemoteDeletionTarget::Account => {}
        }
        let tombstone = record_deletion_tombstone(database, tombstone, &mut receipt).await?;
        let cleanup = match tombstone.target {
            tracedecay_global_db::RemoteDeletionTarget::Project => {
                if tombstone.cleanup == tracedecay_global_db::RemoteDeletionCleanupState::Deleted {
                    return Ok(receipt.complete());
                }
                // Validation above established the exact project identity before admission.
                match tombstone.project_id.as_deref() {
                    Some(project_id) => {
                        self.remove_remote_deleted_project(
                            owners,
                            database,
                            profile_root,
                            project_id,
                        )
                        .await
                    }
                    None => Err(cleanup_error(
                        RemoteDeletionFailureCode::InvalidRequest,
                        RemoteDeletionPhase::ValidateRequest,
                        false,
                        TraceDecayError::Config {
                            message: "remote project deletion requires a project id".to_owned(),
                        },
                    )),
                }
            }
            tracedecay_global_db::RemoteDeletionTarget::Account => {
                self.settle_remote_account_deletion_tombstone_persist(&tombstone);
                self.remove_remote_deleted_account(owners, database, profile_root, &mut receipt)
                    .await
            }
        };
        finish_deletion_cleanup(database, &tombstone, receipt, cleanup).await
    }

    #[hotpath::skip]
    async fn remove_remote_deleted_account(
        &self,
        owners: &super::super::remote_deletion::RemoteDeletionRuntimeOwners,
        database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        profile_root: &Path,
        receipt: &mut RemoteDeletionReceipt,
    ) -> std::result::Result<(), RemoteDeletionCleanupError> {
        self.retire_remote_deleted_account_owners(owners, profile_root)
            .await?;
        let projects = self
            .remote_deletion_project_ids(database, profile_root)
            .await
            .map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::ProjectEnumerationUnavailable,
                    RemoteDeletionPhase::EnumerateProjects,
                    true,
                    error,
                )
            })?;
        receipt.pending_project_ids = projects.iter().cloned().collect();
        for project_id in projects {
            self.remove_remote_deleted_project(owners, database, profile_root, &project_id)
                .await?;
            receipt.removed_project_ids.push(project_id.clone());
            receipt
                .pending_project_ids
                .retain(|pending| pending != &project_id);
        }
        Ok(())
    }

    #[hotpath::skip]
    async fn remote_deletion_project_ids(
        &self,
        database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
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

    #[hotpath::skip]
    async fn remove_remote_deleted_project(
        &self,
        owners: &super::super::remote_deletion::RemoteDeletionRuntimeOwners,
        database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
        profile_root: &Path,
        project_id: &str,
    ) -> std::result::Result<(), RemoteDeletionCleanupError> {
        let runtime_registry = self
            .retire_remote_deleted_project_owners(owners, database, profile_root, project_id)
            .await?;
        let data_root =
            tracedecay_runtime_core::storage::profile_sharded_data_root(profile_root, project_id);
        shard_cleanup::remove_project_shard(&runtime_registry, profile_root, &data_root).await?;
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
}

#[hotpath::skip]
async fn validate_project_deletion_target(
    database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    profile_root: &Path,
    tombstone: &tracedecay_global_db::RemoteDeletionTombstone,
    receipt: &RemoteDeletionReceipt,
) -> std::result::Result<(), RemoteDeletionExecutionError> {
    let project_id = tombstone.project_id.as_deref().ok_or_else(|| {
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
    validate_project_id(project_id).map_err(|error| {
        RemoteDeletionExecutionError::new(
            receipt.clone(),
            RemoteDeletionFailureCode::InvalidRequest,
            RemoteDeletionPhase::ValidateRequest,
            false,
            TraceDecayError::Config {
                message: format!("remote deletion project identity is invalid: {error}"),
            },
        )
    })?;
    let existing_tombstone = database
        .remote_deletion_tombstone(
            &tombstone.profile_id,
            tracedecay_global_db::RemoteDeletionTarget::Project,
            Some(project_id),
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
        .project_registry_context_by_id(project_id)
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
            profile_root,
            project_id,
        )
        .is_ok();
    if existing_tombstone.is_none()
        && exact_context
            .as_ref()
            .is_none_or(|context| context.project.project_id != project_id)
        && !persisted_identity
    {
        return Err(RemoteDeletionExecutionError::new(
            receipt.clone(),
            RemoteDeletionFailureCode::TargetNotFound,
            RemoteDeletionPhase::ResolveTarget,
            false,
            TraceDecayError::Config {
                message: "remote deletion target is not registered to the authenticated profile"
                    .to_owned(),
            },
        ));
    }
    Ok(())
}

#[hotpath::skip]
async fn record_deletion_tombstone(
    database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    tombstone: tracedecay_global_db::RemoteDeletionTombstone,
    receipt: &mut RemoteDeletionReceipt,
) -> std::result::Result<tracedecay_global_db::RemoteDeletionTombstone, RemoteDeletionExecutionError>
{
    let outcome = database
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
    let tombstone = match outcome {
        tracedecay_global_db::RemoteDeletionTombstoneRecordOutcome::Recorded(tombstone)
        | tracedecay_global_db::RemoteDeletionTombstoneRecordOutcome::Replayed(tombstone) => {
            tombstone
        }
        tracedecay_global_db::RemoteDeletionTombstoneRecordOutcome::Conflict { existing } => {
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
    Ok(tombstone)
}

#[hotpath::skip]
async fn finish_deletion_cleanup(
    database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    tombstone: &tracedecay_global_db::RemoteDeletionTombstone,
    mut receipt: RemoteDeletionReceipt,
    result: std::result::Result<(), RemoteDeletionCleanupError>,
) -> std::result::Result<RemoteDeletionReceipt, RemoteDeletionExecutionError> {
    let cleanup = match &result {
        Ok(()) => tracedecay_global_db::RemoteDeletionCleanupState::Deleted,
        Err(failure)
            if matches!(
                failure.code,
                RemoteDeletionFailureCode::RuntimeOwnersSettling
                    | RemoteDeletionFailureCode::RuntimeRetirementIncomplete
            ) =>
        {
            tracedecay_global_db::RemoteDeletionCleanupState::Settling {
                failure_code: failure.code,
                phase: failure.phase,
                retryable: failure.retryable,
            }
        }
        Err(failure) => tracedecay_global_db::RemoteDeletionCleanupState::Partial {
            failure_code: failure.code,
            phase: failure.phase,
            retryable: failure.retryable,
        },
    };
    database
        .transition_remote_deletion_tombstone(tombstone, tombstone.cleanup.clone(), cleanup)
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
    match result {
        Ok(()) => {
            if tombstone.target == tracedecay_global_db::RemoteDeletionTarget::Project
                && let Some(project_id) = &tombstone.project_id
            {
                receipt.removed_project_ids.push(project_id.clone());
            }
            Ok(receipt.complete())
        }
        Err(failure) => Err(failure.with_receipt(receipt)),
    }
}
