use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use tracedecay_domain::errors::Result;
use tracedecay_semantic::SemanticModelLifecycleOwnerV1;
use tracedecay_store::ProjectId;

use super::{DaemonSessionRuntimeRegistryV1, ProjectRuntimeOwnerStateV1, session_registry_error};

pub(super) type SemanticLifecycleOwnerCell = Arc<SemanticLifecycleInitialization>;

#[derive(Default)]
pub(super) struct SemanticLifecycleInitialization {
    owner: OnceLock<Arc<SemanticModelLifecycleOwnerV1>>,
    initialization: Mutex<()>,
}

impl SemanticLifecycleInitialization {
    pub(super) fn get(&self) -> Option<&Arc<SemanticModelLifecycleOwnerV1>> {
        self.owner.get()
    }

    pub(super) fn shutdown(&self) -> Result<()> {
        // The initializer keeps this lock inside its blocking worker. Joining
        // the cell also settles an initializer whose async caller was cancelled.
        let _initialization = self.initialization.lock().map_err(|_| {
            session_registry_error(
                "join semantic initializer",
                "semantic initialization lock is poisoned".to_owned(),
            )
        })?;
        if let Some(owner) = self.get() {
            owner
                .cancel_and_join_background_acquisition()
                .map_err(|error| {
                    session_registry_error("join semantic acquisition", format!("{error:?}"))
                })?;
        }
        Ok(())
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    /// Selection belongs to the logical project, so every linked worktree
    /// receives the same owner retained alongside its project stores.
    pub async fn project_semantic_lifecycle(
        &self,
        project_id: &ProjectId,
    ) -> Result<Arc<SemanticModelLifecycleOwnerV1>> {
        let (cell, project_lease) = {
            let entries = self.project_owners.lock().map_err(|_| {
                session_registry_error(
                    "read project semantic owner",
                    "project owner lock is poisoned".to_owned(),
                )
            })?;
            let Some(ProjectRuntimeOwnerStateV1::Ready(owners)) = entries.get(project_id) else {
                return Err(session_registry_error(
                    "read project semantic owner",
                    "logical project owner is not ready".to_owned(),
                ));
            };
            if owners.sessions.is_none() && owners.memory.is_none() {
                return Err(session_registry_error(
                    "read project semantic owner",
                    "logical project stores are retired".to_owned(),
                ));
            }
            // Counted store clients keep terminal retirement from passing while
            // initialization is outside the project owner map's lock.
            let memory = owners
                .memory
                .as_ref()
                .map(|owner| owner.issue_database_lease())
                .transpose()?;
            let sessions = owners
                .sessions
                .as_ref()
                .map(|owner| {
                    owner.database.issue_lease().map_err(|error| {
                        session_registry_error(
                            "retain project during semantic open",
                            format!("{error:?}"),
                        )
                    })
                })
                .transpose()?;
            (
                owners.semantic_lifecycle.clone(),
                Arc::new((memory, sessions)),
            )
        };
        let owner = self
            .open_semantic_lifecycle(&cell, Some(project_id), project_lease.clone())
            .await?;
        let entries = self.project_owners.lock().map_err(|_| {
            session_registry_error(
                "validate project semantic owner",
                "project owner lock is poisoned".to_owned(),
            )
        })?;
        match entries.get(project_id) {
            Some(ProjectRuntimeOwnerStateV1::Ready(owners))
                if Arc::ptr_eq(&owners.semantic_lifecycle, &cell)
                    && (owners.sessions.is_some() || owners.memory.is_some()) =>
            {
                Ok(owner)
            }
            _ => Err(session_registry_error(
                "validate project semantic owner",
                "logical project owner changed during semantic open".to_owned(),
            )),
        }
    }

    /// Projectless commands use the explicit profile owner rather than an
    /// arbitrary project's selection or the first profile opened by a process.
    pub async fn profile_semantic_lifecycle(&self) -> Result<Arc<SemanticModelLifecycleOwnerV1>> {
        self.open_semantic_lifecycle(&self.profile_semantic_lifecycle, None, ())
            .await
    }

    async fn open_semantic_lifecycle<Lease: Send + Sync + 'static>(
        &self,
        cell: &SemanticLifecycleOwnerCell,
        project_id: Option<&ProjectId>,
        project_lease: Lease,
    ) -> Result<Arc<SemanticModelLifecycleOwnerV1>> {
        if self.semantic_lifecycle_closed.load(Ordering::Acquire) {
            return Err(session_registry_error(
                "open semantic selection owner",
                "semantic lifecycle is closed".to_owned(),
            ));
        }
        let root = tracedecay_semantic::default_lifecycle_root_in(self.identity.profile_root());
        let selection_root = match project_id {
            Some(project_id) => root.join("projects").join(project_id.as_str()),
            None => root.clone(),
        };
        let namespace = serde_json::to_string(&(
            self.identity.brain_id(),
            self.identity.profile_id(),
            project_id,
        ))
        .map_err(|error| {
            session_registry_error("identify semantic selection owner", error.to_string())
        })?;
        let cell = Arc::clone(cell);
        let closed = Arc::clone(&self.semantic_lifecycle_closed);
        let owner = tokio::task::spawn_blocking(move || {
            let _project_lease = project_lease;
            // Cancellation of the async waiter cannot release initialization
            // while this worker still loads state or reconciles artifact leases.
            let _initialization = cell.initialization.lock().map_err(|_| {
                session_registry_error(
                    "open semantic selection owner",
                    "semantic initialization lock is poisoned".to_owned(),
                )
            })?;
            if closed.load(Ordering::Acquire) {
                return Err(session_registry_error(
                    "open semantic selection owner",
                    "semantic lifecycle is closed".to_owned(),
                ));
            }
            if let Some(owner) = cell.get() {
                return Ok(Arc::clone(owner));
            }
            let owner = SemanticModelLifecycleOwnerV1::open_scoped_default(
                selection_root,
                root.join("verified-artifacts"),
                &namespace,
            )
            .map(Arc::new)
            .map_err(|error| {
                session_registry_error("open semantic selection owner", format!("{error:?}"))
            })?;
            Ok(Arc::clone(cell.owner.get_or_init(|| owner)))
        })
        .await
        .map_err(|error| {
            session_registry_error("open semantic selection owner", error.to_string())
        })??;
        if self.semantic_lifecycle_closed.load(Ordering::Acquire) {
            owner.cancel_background_acquisition();
            return Err(session_registry_error(
                "open semantic selection owner",
                "semantic lifecycle closed during open".to_owned(),
            ));
        }
        Ok(owner.clone())
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    pub(super) fn retained_semantic_lifecycle_owners(
        &self,
    ) -> Result<Vec<Arc<SemanticModelLifecycleOwnerV1>>> {
        Ok(self
            .retained_semantic_lifecycle_cells()?
            .into_iter()
            .filter_map(|cell| cell.get().cloned())
            .collect())
    }

    pub(super) fn retained_semantic_lifecycle_cells(
        &self,
    ) -> Result<Vec<SemanticLifecycleOwnerCell>> {
        let mut cells = vec![Arc::clone(&self.profile_semantic_lifecycle)];
        let entries = self.project_owners.lock().map_err(|_| {
            session_registry_error(
                "read retained semantic owners",
                "project owner lock is poisoned".to_owned(),
            )
        })?;
        for state in entries.values() {
            let cell = match state {
                ProjectRuntimeOwnerStateV1::Ready(retained) => &retained.semantic_lifecycle,
                ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery) => {
                    &recovery.semantic_lifecycle
                }
                ProjectRuntimeOwnerStateV1::Faulted(faulted) => {
                    &faulted.retained.semantic_lifecycle
                }
                ProjectRuntimeOwnerStateV1::Opening(cell)
                | ProjectRuntimeOwnerStateV1::ReplacingSessions(cell)
                | ProjectRuntimeOwnerStateV1::Recovering(cell)
                | ProjectRuntimeOwnerStateV1::Retiring(cell) => cell,
            };
            cells.push(Arc::clone(cell));
        }
        Ok(cells)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_registry::ProjectRuntimeOwnersV1;
    use tracedecay_daemon_identity::profile_identity;

    #[tokio::test]
    async fn lifecycle_snapshot_retains_transition_owner_and_rejects_storeless_ready() {
        let fixture = tempfile::TempDir::new().expect("lifecycle fixture");
        let profile_root = fixture.path().join("profile");
        let project_root = fixture.path().join("project");
        std::fs::create_dir_all(&project_root).expect("project root");
        let project_id =
            ProjectId::new("project.semantic-lifecycle-transition").expect("project identity");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &project_root,
            project_id.as_str(),
        )
        .expect("project enrollment");
        let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            53,
            "semantic lifecycle transition",
        )
        .expect("daemon database scope");
        let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("registry");
        let memory = registry
            .project_memory(project_id.clone(), [project_root])
            .await
            .expect("project memory");
        let owner = registry
            .project_semantic_lifecycle(&project_id)
            .await
            .expect("project lifecycle");
        let retained = registry
            .project_owners
            .lock()
            .expect("project owners")
            .remove(&project_id)
            .expect("mounted project");
        let ProjectRuntimeOwnerStateV1::Ready(retained) = retained else {
            panic!("mounted project must be ready");
        };
        let cell = retained.semantic_lifecycle.clone();
        for transition in [
            ProjectRuntimeOwnerStateV1::Opening(cell.clone()),
            ProjectRuntimeOwnerStateV1::ReplacingSessions(cell.clone()),
            ProjectRuntimeOwnerStateV1::Recovering(cell.clone()),
            ProjectRuntimeOwnerStateV1::Retiring(cell.clone()),
        ] {
            registry
                .project_owners
                .lock()
                .expect("project owners")
                .insert(project_id.clone(), transition);
            let snapshot = registry
                .retained_semantic_lifecycle_owners()
                .expect("retained lifecycle snapshot");
            assert_eq!(snapshot.len(), 1);
            assert!(Arc::ptr_eq(&snapshot[0], &owner));
            assert!(
                registry
                    .project_semantic_lifecycle(&project_id)
                    .await
                    .is_err()
            );
        }
        registry
            .project_owners
            .lock()
            .expect("project owners")
            .insert(
                project_id.clone(),
                ProjectRuntimeOwnerStateV1::Ready(ProjectRuntimeOwnersV1 {
                    sessions: None,
                    memory: None,
                    semantic_lifecycle: cell,
                }),
            );
        assert!(
            registry
                .project_semantic_lifecycle(&project_id)
                .await
                .is_err()
        );
        registry
            .project_owners
            .lock()
            .expect("project owners")
            .insert(
                project_id.clone(),
                ProjectRuntimeOwnerStateV1::Ready(retained),
            );
        assert!(Arc::ptr_eq(
            &registry
                .project_semantic_lifecycle(&project_id)
                .await
                .expect("restored lifecycle"),
            &owner,
        ));
        drop(memory);
    }
}
