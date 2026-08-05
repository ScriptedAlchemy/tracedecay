use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use crate::errors::{Result, TraceDecayError};

use super::super::{StoreAdministration, project_server_lifecycle};

impl StoreAdministration {
    pub(super) async fn remote_deleted_project_roots(
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

    pub(super) async fn retire_remote_deleted_project_work(
        &self,
        profile_root: &Path,
        project_id: &str,
    ) -> Result<()> {
        let retirements = {
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
            owners
                .iter()
                .map(|owner| (owner.clone(), registry.remove_owner(owner)))
                .collect::<Vec<_>>()
        };
        for server in retirements.iter().flat_map(|(_, servers)| servers) {
            server.revoke_project_server_responses();
            server.cancel_startup_transcript_ingest();
            server.abort_project_server_requests();
        }
        for (owner, _) in &retirements {
            self.session_temporal_refresh_schedulers
                .retire_project(owner)
                .await;
        }
        #[cfg(unix)]
        self.abort_remote_deleted_maintenance_schedulers(profile_root, project_id)
            .await?;
        #[cfg(unix)]
        if !self
            .settle_retirement_reapers_for_project(
                profile_root,
                project_id,
                super::super::super::DAEMON_TASK_ABORT_DEADLINE,
            )
            .await
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "remote-deleted project '{project_id}' maintenance reapers are still settling"
                ),
            });
        }
        for (owner, servers) in retirements {
            project_server_lifecycle::schedule_project_server_retirement(
                self, owner, servers, None,
            )
            .await;
        }
        if !self
            .settle_project_server_retirements_for_project(
                profile_root,
                project_id,
                super::super::super::DAEMON_TASK_ABORT_DEADLINE,
            )
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
                    tasks.push((key.owner, task));
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
                    tasks.push((key.owner, task));
                }
            }
        }
        for (owner, task) in tasks {
            task.abort();
            self.track_project_server_retirement(owner, task).await;
        }
        if !self
            .settle_project_server_retirements_for_project(
                profile_root,
                project_id,
                super::super::super::DAEMON_TASK_ABORT_DEADLINE,
            )
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
