use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use tokio::sync::{Mutex, OwnedMutexGuard};
use tracedecay_store::{CodeShardScopeV1, ProjectId, StoreShardIdV1};

use super::{
    DaemonSessionRuntimeRegistryV1, Database, DatabaseAccessMode, DatabaseAuthority,
    LocalCodeStoreAuthorityRegistrationOutcomeV1, LocalCodeStoreAuthorityV1, RefId, Result,
    StoreRuntimeHandle, StoreRuntimeKey, open_runtime, session_registry_error,
};

impl DaemonSessionRuntimeRegistryV1 {
    pub(crate) async fn begin_destructive_code_maintenance(
        &self,
        root: &Path,
        database_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<super::DestructiveMaintenanceReservation> {
        let target =
            super::DestructiveMaintenanceTarget::new(root, database_paths).map_err(|error| {
                session_registry_error(
                    "construct destructive code-store reservation",
                    format!("{error:?}"),
                )
            })?;
        let reservation = self
            .registry
            .begin_destructive_maintenance(target)
            .await
            .map_err(|error| {
                session_registry_error(
                    "reserve destructive code-store maintenance",
                    format!("{error:?}"),
                )
            })?;
        for closed in reservation.closed() {
            self.resolver
                .retire_code_authority(&closed.binding().shard_id, closed.path())
                .map_err(|error| {
                    session_registry_error(
                        "retire destructively closed code-shard authority",
                        format!("{error:?}"),
                    )
                })?;
        }
        Ok(reservation)
    }

    pub(crate) async fn close_code_graph_paths(
        &self,
        database_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<()> {
        for database_path in database_paths {
            let closed = self
                .registry
                .close_path(&database_path)
                .await
                .map_err(|error| {
                    session_registry_error(
                        "close registered code-shard runtime",
                        format!("{error:?}"),
                    )
                })?;
            if let Some(closed) = closed {
                self.resolver
                    .retire_code_authority(&closed.binding().shard_id, closed.path())
                    .map_err(|error| {
                        session_registry_error(
                            "retire registered code-shard authority",
                            format!("{error:?}"),
                        )
                    })?;
            }
        }
        Ok(())
    }

    pub(super) async fn code_graph_with_authority(
        &self,
        shard_id: StoreShardIdV1,
        database_path: PathBuf,
        mut database_authority: Option<DatabaseAuthority>,
        initialize_if_missing: bool,
    ) -> Result<StoreRuntimeHandle> {
        // Registration and rollback are one per-shard transaction. Without
        // this gate, an identical caller can observe the new authority between
        // a failed open and its rollback, then lose that authority underneath
        // its own open.
        let _open_guard = self.code_graph_open_guard(&shard_id).await;
        let registration =
            self.resolver
                .register_code_authority(
                    LocalCodeStoreAuthorityV1::new(shard_id.clone(), database_path.clone())
                        .map_err(|error| {
                            session_registry_error(
                                "construct code-shard authority",
                                format!("{error:?}"),
                            )
                        })?,
                )
                .map_err(|error| {
                    session_registry_error("register code-shard authority", format!("{error:?}"))
                })?;
        let runtime = async {
            if database_authority.is_none() && !initialize_if_missing {
                let key = StoreRuntimeKey::new(shard_id.clone(), self.incarnation);
                if let Some(runtime) = self.registry.retained_runtime_for_read(&key) {
                    if runtime.locator().path() != database_path {
                        return Err(session_registry_error(
                            "reuse read-only code-shard runtime",
                            "retained runtime locator differs from the registered database path"
                                .to_owned(),
                        ));
                    }
                    return Ok(runtime);
                }
                database_authority = Some(DatabaseAuthority::for_owned_runtime(
                    &database_path,
                    "publish registered read-only code-shard runtime",
                )?);
            }
            open_runtime(
                &self.registry,
                self.resolver.as_ref(),
                shard_id.clone(),
                self.incarnation,
                Some(self.profile_pin.clone()),
                database_authority,
                initialize_if_missing,
                "mount code-shard store",
            )
            .await
        }
        .await;

        if runtime.is_err()
            && registration == LocalCodeStoreAuthorityRegistrationOutcomeV1::Registered
        {
            let key = StoreRuntimeKey::new(shard_id.clone(), self.incarnation);
            if self.registry.retained_runtime_for_read(&key).is_none() {
                self.resolver
                    .retire_code_authority(&shard_id, &database_path)
                    .map_err(|error| {
                        session_registry_error(
                            "roll back failed code-shard authority registration",
                            format!("{error:?}"),
                        )
                    })?;
            }
        }
        runtime
    }

    pub(super) async fn code_graph_open_guard(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> OwnedMutexGuard<()> {
        let gate = {
            let mut gates = self.code_graph_open_gates.lock().await;
            gates.retain(|_, gate| gate.strong_count() > 0);
            match gates.get(shard_id).and_then(Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    let gate = Arc::new(Mutex::new(()));
                    gates.insert(shard_id.clone(), Arc::downgrade(&gate));
                    gate
                }
            }
        };
        gate.lock_owned().await
    }

    /// Mounts the mutable graph for this exact project/repository/worktree
    /// identity. The checkout path is used only by the Git identity authority;
    /// it is never itself the shard identity.
    pub(crate) async fn code_graph_worktree(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        database_path: PathBuf,
        database_authority: DatabaseAuthority,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        // Mutable graph storage exists for non-Git projects too. Its structural
        // shard identity needs only the stable repository/worktree components;
        // resolving HEAD here would incorrectly make ordinary project open
        // depend on a Git repository being present.
        let repository_id =
            crate::daemon::code_index_scheduler::identity::repository_id_for(project_root)
                .map_err(|error| {
                    session_registry_error("resolve code-shard repository", error.to_string())
                })?;
        let worktree_id =
            crate::daemon::code_index_scheduler::identity::worktree_id_for(project_root).map_err(
                |error| session_registry_error("resolve code-shard worktree", error.to_string()),
            )?;
        let shard_id = StoreShardIdV1::code(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
            repository_id,
            CodeShardScopeV1::Worktree { worktree_id },
        );
        let runtime = self
            .code_graph_with_authority(
                shard_id,
                database_path,
                Some(database_authority),
                matches!(access, DatabaseAccessMode::ReadWrite),
            )
            .await?;
        Database::publish_runtime(runtime, access).await
    }

    /// Mounts the mutable graph for an exact named Git ref in this worktree.
    /// The ref is normalized to its full `refs/heads/*` identity before it
    /// enters the shard key.
    pub(crate) async fn code_graph_branch(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        branch_name: &str,
        database_path: PathBuf,
        database_authority: DatabaseAuthority,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        self.code_graph_branch_with_authority(
            project_root,
            project_id,
            branch_name,
            database_path,
            Some(database_authority),
            access,
        )
        .await
    }

    pub(crate) async fn code_graph_branch_registered(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        branch_name: &str,
        database_path: PathBuf,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        self.code_graph_branch_with_authority(
            project_root,
            project_id,
            branch_name,
            database_path,
            None,
            access,
        )
        .await
    }

    async fn code_graph_branch_with_authority(
        &self,
        project_root: &Path,
        project_id: ProjectId,
        branch_name: &str,
        database_path: PathBuf,
        database_authority: Option<DatabaseAuthority>,
        access: DatabaseAccessMode,
    ) -> Result<Database> {
        let identity = crate::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(
            project_root,
        )
        .map_err(|error| {
            session_registry_error("resolve code-branch identity", error.to_string())
        })?;
        let ref_name = if branch_name.starts_with("refs/heads/") {
            branch_name.to_owned()
        } else if branch_name.starts_with("refs/") {
            return Err(session_registry_error(
                "construct code-branch ref identity",
                "branch ref must be under refs/heads/".to_owned(),
            ));
        } else {
            format!("refs/heads/{branch_name}")
        };
        let ref_id = RefId::new(ref_name).map_err(|error| {
            session_registry_error("construct code-branch ref identity", error.to_string())
        })?;
        let shard_id = StoreShardIdV1::code(
            self.identity.brain_id().clone(),
            self.identity.profile_id().clone(),
            project_id,
            identity.repository_id().clone(),
            CodeShardScopeV1::Branch {
                worktree_id: identity.worktree_id().clone(),
                ref_id,
            },
        );
        let runtime = self
            .code_graph_with_authority(
                shard_id,
                database_path,
                database_authority,
                matches!(access, DatabaseAccessMode::ReadWrite),
            )
            .await?;
        Database::publish_runtime(runtime, access).await
    }
}
