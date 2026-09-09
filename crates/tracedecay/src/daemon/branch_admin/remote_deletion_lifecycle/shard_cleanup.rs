//! Exact shard deletion after runtime owners have drained.

use super::super::destructive_reservation_error;
use super::{RemoteDeletionCleanupError, cleanup_error};
use crate::daemon::remote_deletion::{RemoteDeletionFailureCode, RemoteDeletionPhase};
use std::path::Path;
use tracedecay_daemon_identity::authority;
use tracedecay_domain::errors::TraceDecayError;

fn validate_shard_path(
    profile_root: &Path,
    data_root: &Path,
) -> std::result::Result<(), RemoteDeletionCleanupError> {
    let metadata = std::fs::symlink_metadata(data_root).map_err(|error| {
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
    let canonical_data_root = authority::canonical_identity_path(data_root).map_err(|error| {
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
    Ok(())
}

#[hotpath::skip]
pub(super) async fn remove_project_shard(
    runtime_registry: &tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1,
    profile_root: &Path,
    data_root: &Path,
) -> std::result::Result<(), RemoteDeletionCleanupError> {
    if !data_root.exists() {
        // An already-absent exact shard is the idempotent success case.
        return Ok(());
    }
    validate_shard_path(profile_root, data_root)?;
    let project_sessions_path =
        data_root.join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);
    let database_paths = [
        data_root.join(crate::config::db_filename(data_root)),
        project_sessions_path,
    ]
    .into_iter()
    .filter(|path| path.is_file())
    .collect::<Vec<_>>();
    if database_paths.is_empty() {
        std::fs::remove_dir_all(data_root).map_err(|error| {
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
            .begin_destructive_code_maintenance(data_root, database_paths)
            .await
            .map_err(|error| {
                cleanup_error(
                    RemoteDeletionFailureCode::RuntimeRetirementIncomplete,
                    RemoteDeletionPhase::CancelRuntimeOwners,
                    true,
                    error,
                )
            })?;
        // The reservation proves physical holders are closed and fences
        // stale code authority before deleting the shard.
        if let Err(error) = std::fs::remove_dir_all(data_root) {
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
    Ok(())
}
