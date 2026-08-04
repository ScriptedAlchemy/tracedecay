//! Destructive lifecycle administration for mounted remote deletion requests.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::errors::{Result, TraceDecayError};

use super::super::remote_deletion::{RemoteDeletionReceipt, RemoteDeletionReceiptTarget};
use super::{
    StoreAdministration, authority, destructive_reservation_error, project_server_lifecycle,
};

impl StoreAdministration {
    /// Applies an authenticated remote account or project deletion through the
    /// profile's one registered authority. The durable tombstone is written
    /// before any runtime is retired or store directory is removed, so a
    /// failed cleanup stays fail-closed and a retry resumes safely.
    pub(in super::super) async fn execute_remote_deletion(
        &self,
        target: RemoteDeletionReceiptTarget,
        project_id: Option<String>,
        tombstone_id: String,
    ) -> Result<RemoteDeletionReceipt> {
        if tombstone_id.trim().is_empty() || tombstone_id.len() > 256 {
            return Err(TraceDecayError::Config {
                message: "remote deletion tombstone id must be non-empty and at most 256 bytes"
                    .to_owned(),
            });
        }
        let profile_identity = self.profile_identity()?.clone();
        let profile_root = authority::canonical_identity_path(profile_identity.profile_root())?;
        let profile_id = profile_identity.profile_id().as_str().to_owned();
        let recorded_at_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| TraceDecayError::Config {
                message: format!("remote deletion clock is before Unix epoch: {error}"),
            })?
            .as_micros()
            .try_into()
            .map_err(|_| TraceDecayError::Config {
                message: "remote deletion timestamp exceeds supported range".to_owned(),
            })?;
        let database = self.registered_profile_database().await?;

        self.with_writer(|| async {
            match target {
                RemoteDeletionReceiptTarget::Project => {
                    let project_id = project_id.ok_or_else(|| TraceDecayError::Config {
                        message: "remote project deletion requires a project id".to_owned(),
                    })?;
                    tracedecay_store::ProjectId::new(project_id.clone()).map_err(|error| {
                        TraceDecayError::Config {
                            message: format!(
                                "remote deletion project identity is invalid: {error}"
                            ),
                        }
                    })?;
                    let tombstone = crate::global_db::RemoteDeletionTombstone {
                        target: crate::global_db::RemoteDeletionTarget::Project,
                        profile_id: profile_id.clone(),
                        project_id: Some(project_id.clone()),
                        tombstone_id,
                        recorded_at_micros,
                    };
                    let tombstone = database.record_remote_deletion_tombstone(tombstone).await?;
                    let removed = self
                        .remove_remote_deleted_project(&database, &profile_root, &project_id)
                        .await?;
                    Ok(RemoteDeletionReceipt {
                        status: "deleted",
                        target,
                        profile_id,
                        tombstone_id: tombstone.tombstone_id,
                        project_id: Some(project_id),
                        removed_project_count: usize::from(removed),
                    })
                }
                RemoteDeletionReceiptTarget::Account => {
                    if project_id.is_some() {
                        return Err(TraceDecayError::Config {
                            message: "remote account deletion must not name a project".to_owned(),
                        });
                    }
                    let tombstone = crate::global_db::RemoteDeletionTombstone {
                        target: crate::global_db::RemoteDeletionTarget::Account,
                        profile_id: profile_id.clone(),
                        project_id: None,
                        tombstone_id,
                        recorded_at_micros,
                    };
                    let tombstone = database.record_remote_deletion_tombstone(tombstone).await?;
                    let projects = self
                        .remote_deletion_project_ids(&database, &profile_root)
                        .await?;
                    let mut removed_project_count = 0_usize;
                    for project_id in projects {
                        removed_project_count += usize::from(
                            self.remove_remote_deleted_project(
                                &database,
                                &profile_root,
                                &project_id,
                            )
                            .await?,
                        );
                    }
                    Ok(RemoteDeletionReceipt {
                        status: "deleted",
                        target,
                        profile_id,
                        tombstone_id: tombstone.tombstone_id,
                        project_id: None,
                        removed_project_count,
                    })
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
        database: &Arc<crate::global_db::RegisteredGlobalDb>,
        profile_root: &Path,
        project_id: &str,
    ) -> Result<bool> {
        let typed_project_id =
            tracedecay_store::ProjectId::new(project_id.to_owned()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("remote deletion project identity is invalid: {error}"),
                }
            })?;
        let data_root = crate::storage::profile_sharded_data_root(profile_root, project_id);
        let project_sessions_path = data_root.join(crate::storage::SESSIONS_DB_FILENAME);

        self.retire_remote_deleted_project_work(profile_root, project_id)
            .await;
        self.project_routes.forget_project(project_id)?;
        self.git_index_transaction_services
            .retire_project_database(&typed_project_id, &project_sessions_path)
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "could not retire remote-deleted project Git transaction actors: {error}"
                ),
            })?;
        let runtime_registry = self.session_runtime_registry().await?;
        runtime_registry
            .drop_project_runtime_caches(&typed_project_id)
            .await;

        let removed_store = if !data_root.exists() {
            false
        } else {
            let metadata =
                std::fs::symlink_metadata(&data_root).map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "could not inspect remote-deleted project store '{}': {error}",
                        data_root.display()
                    ),
                })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "remote-deleted project store '{}' is not a regular directory",
                        data_root.display()
                    ),
                });
            }
            let canonical_data_root = authority::canonical_identity_path(&data_root)?;
            if canonical_data_root != data_root || !canonical_data_root.starts_with(profile_root) {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "remote-deleted project store '{}' is outside its exact profile root",
                        data_root.display()
                    ),
                });
            }
            let database_paths = [
                data_root.join(crate::config::db_filename(&data_root)),
                project_sessions_path.clone(),
            ]
            .into_iter()
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
            if database_paths.is_empty() {
                std::fs::remove_dir_all(&data_root).map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "failed to remove remote-deleted project store '{}': {error}",
                        data_root.display()
                    ),
                })?;
            } else {
                let reservation = runtime_registry
                    .begin_destructive_code_maintenance(&data_root, database_paths.clone())
                    .await?;
                if let Err(error) = self.prove_no_external_branch_store_holders(&database_paths) {
                    reservation
                        .abort_preserved()
                        .map_err(destructive_reservation_error)?;
                    return Err(error);
                }
                if let Err(error) = std::fs::remove_dir_all(&data_root) {
                    reservation
                        .abort_preserved()
                        .map_err(destructive_reservation_error)?;
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "failed to remove remote-deleted project store '{}': {error}",
                            data_root.display()
                        ),
                    });
                }
                reservation
                    .finish_deleted()
                    .map_err(destructive_reservation_error)?;
            }
            true
        };
        database
            .delete_remote_deleted_project_registry_row(project_id)
            .await?;
        Ok(removed_store)
    }

    async fn retire_remote_deleted_project_work(&self, profile_root: &Path, project_id: &str) {
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
        for owner in &owners {
            self.session_temporal_refresh_schedulers
                .retire_project(owner)
                .await;
        }
        #[cfg(unix)]
        self.abort_remote_deleted_maintenance_schedulers(profile_root, project_id)
            .await;
        project_server_lifecycle::shutdown_detached_project_servers(servers).await;
    }

    #[cfg(unix)]
    async fn abort_remote_deleted_maintenance_schedulers(
        &self,
        profile_root: &Path,
        project_id: &str,
    ) {
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
            let _ = task.await;
        }
    }
}
