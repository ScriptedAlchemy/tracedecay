use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use super::project_registry::ProjectIdentityAliasKind;
use super::{CodeProjectRecord, RegisteredGlobalDb, StoreInstanceRecord};

/// The already-existing project store authorized to persist sanitized observations.
///
/// Resolution is deliberately stricter than the legacy graph/session lookup paths:
/// this type can only name the canonical, verified profile shard registered for the
/// repository. Constructing it never creates a directory, database, or registry row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectObservationStoreResolution {
    project: CodeProjectRecord,
    store: StoreInstanceRecord,
    store_root: PathBuf,
    database_path: PathBuf,
}

impl ProjectObservationStoreResolution {
    pub fn project(&self) -> &CodeProjectRecord {
        &self.project
    }

    pub fn store(&self) -> &StoreInstanceRecord {
        &self.store
    }

    pub fn store_root(&self) -> &Path {
        &self.store_root
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectObservationStoreError {
    UnavailableProject {
        project_root: PathBuf,
    },
    ProjectNotRegistered {
        project_root: PathBuf,
    },
    AmbiguousProjectIdentity {
        project_root: PathBuf,
        project_ids: Vec<String>,
    },
    StoreNotRegistered {
        project_id: String,
    },
    AmbiguousStores {
        project_id: String,
        store_ids: Vec<String>,
    },
    StaleStore {
        project_id: String,
        store_id: String,
    },
    NonCanonicalStore {
        project_id: String,
        store_id: String,
        reason: String,
    },
    UnavailableStore {
        project_id: String,
        store_id: String,
        path: PathBuf,
    },
}

impl fmt::Display for ProjectObservationStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnavailableProject { project_root } => write!(
                formatter,
                "project observation root is unavailable at '{}'",
                project_root.display()
            ),
            Self::ProjectNotRegistered { project_root } => write!(
                formatter,
                "project observation authority is not registered for '{}'",
                project_root.display()
            ),
            Self::AmbiguousProjectIdentity {
                project_root,
                project_ids,
            } => write!(
                formatter,
                "project observation authority for '{}' is ambiguous across project ids: {}",
                project_root.display(),
                project_ids.join(", ")
            ),
            Self::StoreNotRegistered { project_id } => write!(
                formatter,
                "project observation store is not registered for project '{project_id}'"
            ),
            Self::AmbiguousStores {
                project_id,
                store_ids,
            } => write!(
                formatter,
                "project observation store for '{project_id}' is ambiguous across store ids: {}",
                store_ids.join(", ")
            ),
            Self::StaleStore {
                project_id,
                store_id,
            } => write!(
                formatter,
                "project observation store '{store_id}' for '{project_id}' has no verification record"
            ),
            Self::NonCanonicalStore {
                project_id,
                store_id,
                reason,
            } => write!(
                formatter,
                "project observation store '{store_id}' for '{project_id}' is noncanonical: {reason}"
            ),
            Self::UnavailableStore {
                project_id,
                store_id,
                path,
            } => write!(
                formatter,
                "project observation store '{store_id}' for '{project_id}' is unavailable at '{}'",
                path.display()
            ),
        }
    }
}

impl Error for ProjectObservationStoreError {}

impl RegisteredGlobalDb {
    /// Resolve the sole existing store authorized for project observations.
    ///
    /// Repository markers, the canonical checkout path, and Git's common
    /// directory are independent identity evidence. Conflicting evidence or
    /// any noncanonical/unavailable store fails closed. This method never uses
    /// the legacy default-shard, newest-store, or remote-URL fallbacks.
    pub async fn resolve_project_observation_store(
        &self,
        project_root: &Path,
    ) -> Result<ProjectObservationStoreResolution, ProjectObservationStoreError> {
        let project_root = canonical_project_directory(project_root)?;
        let project_ids = self.observation_project_ids(&project_root).await?;
        let project_id = match project_ids.as_slice() {
            [] => {
                return Err(ProjectObservationStoreError::ProjectNotRegistered { project_root });
            }
            [project_id] => project_id.clone(),
            _ => {
                return Err(ProjectObservationStoreError::AmbiguousProjectIdentity {
                    project_root,
                    project_ids,
                });
            }
        };
        let context = self
            .project_registry_context_by_id(&project_id)
            .await
            .map_err(|error| ProjectObservationStoreError::NonCanonicalStore {
                project_id: project_id.clone(),
                store_id: "<unresolved>".to_string(),
                reason: format!("project registry context is unavailable: {error}"),
            })?
            .ok_or_else(|| ProjectObservationStoreError::ProjectNotRegistered {
                project_root: project_root.clone(),
            })?;
        let project = context.project;
        let mut stores = context.stores;
        let store = match stores.len() {
            0 => {
                return Err(ProjectObservationStoreError::StoreNotRegistered { project_id });
            }
            1 => stores
                .pop()
                .ok_or_else(|| ProjectObservationStoreError::StoreNotRegistered {
                    project_id: project_id.clone(),
                })?,
            _ => {
                let mut store_ids = stores
                    .into_iter()
                    .map(|context| context.store.store_id)
                    .collect::<Vec<_>>();
                store_ids.sort();
                return Err(ProjectObservationStoreError::AmbiguousStores {
                    project_id,
                    store_ids,
                });
            }
        };
        let store = store.store;
        self.validate_project_observation_store(project, store)
    }

    async fn observation_project_ids(
        &self,
        project_root: &Path,
    ) -> Result<Vec<String>, ProjectObservationStoreError> {
        let mut project_ids = BTreeSet::new();
        match tracedecay_runtime_core::storage::read_repository_identity_marker(project_root) {
            Ok(Some(marker)) => {
                project_ids.insert(marker.project_id);
            }
            Ok(None) => {}
            Err(error) => {
                return Err(ProjectObservationStoreError::NonCanonicalStore {
                    project_id: "<unresolved>".to_string(),
                    store_id: "<unresolved>".to_string(),
                    reason: format!("repository identity marker is invalid: {error}"),
                });
            }
        }
        if let Some(project_id) = self
            .project_id_by_path_alias(project_root, ProjectIdentityAliasKind::ProjectRoot)
            .await
            .map_err(|error| ProjectObservationStoreError::NonCanonicalStore {
                project_id: "<unresolved>".to_string(),
                store_id: "<unresolved>".to_string(),
                reason: format!("project path alias lookup failed: {error}"),
            })?
        {
            project_ids.insert(project_id);
        }
        if let Some(git_common_dir) =
            tracedecay_runtime_core::worktree::git_common_dir(project_root)
            && let Some(project_id) = self
                .project_id_by_path_alias(&git_common_dir, ProjectIdentityAliasKind::GitCommonDir)
                .await
                .map_err(|error| ProjectObservationStoreError::NonCanonicalStore {
                    project_id: "<unresolved>".to_string(),
                    store_id: "<unresolved>".to_string(),
                    reason: format!("Git common-directory alias lookup failed: {error}"),
                })?
        {
            project_ids.insert(project_id);
        }
        Ok(project_ids.into_iter().collect())
    }

    fn validate_project_observation_store(
        &self,
        project: CodeProjectRecord,
        store: StoreInstanceRecord,
    ) -> Result<ProjectObservationStoreResolution, ProjectObservationStoreError> {
        let project_id = project.project_id.clone();
        let store_id = store.store_id.clone();
        let noncanonical = |reason: String| ProjectObservationStoreError::NonCanonicalStore {
            project_id: project_id.clone(),
            store_id: store_id.clone(),
            reason,
        };
        if store.last_verified_at.is_none() {
            return Err(ProjectObservationStoreError::StaleStore {
                project_id,
                store_id,
            });
        }
        if store.store_kind != "code_project" {
            return Err(noncanonical(format!(
                "store kind must be 'code_project', found '{}'",
                store.store_kind
            )));
        }
        if store.storage_mode != "profile_sharded" {
            return Err(noncanonical(format!(
                "storage mode must be 'profile_sharded', found '{}'",
                store.storage_mode
            )));
        }

        let expected_relpath_text = format!("projects/{}", project.project_id);
        let expected_relpath = PathBuf::from(&expected_relpath_text);
        if store.store_relpath != expected_relpath_text {
            return Err(noncanonical(format!(
                "store path must be '{}'",
                expected_relpath.display()
            )));
        }
        let expected_manifest_relpath_text = format!(
            "{expected_relpath_text}/{}",
            tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME
        );
        let expected_manifest_relpath = PathBuf::from(&expected_manifest_relpath_text);
        if store.manifest_relpath.as_deref() != Some(expected_manifest_relpath_text.as_str()) {
            return Err(noncanonical(format!(
                "manifest path must be '{}'",
                expected_manifest_relpath.display()
            )));
        }

        let profile_root = self
            .db_path()
            .parent()
            .ok_or_else(|| noncanonical("registry database has no profile root".to_string()))?;
        let validated = tracedecay_runtime_core::storage::ValidatedProfileShard::resolve_existing(
            profile_root,
            &project.project_id,
        )
        .map_err(|error| match error {
            tracedecay_runtime_core::storage::ProfileShardValidationError::Unavailable { path } => {
                ProjectObservationStoreError::UnavailableStore {
                    project_id: project_id.clone(),
                    store_id: store_id.clone(),
                    path,
                }
            }
            tracedecay_runtime_core::storage::ProfileShardValidationError::NonCanonical {
                reason,
            } => noncanonical(reason),
        })?;

        Ok(ProjectObservationStoreResolution {
            project,
            store,
            store_root: validated.store_root().to_path_buf(),
            database_path: validated.sessions_db_path().to_path_buf(),
        })
    }
}

fn canonical_project_directory(
    project_root: &Path,
) -> Result<PathBuf, ProjectObservationStoreError> {
    let canonical = project_root.canonicalize().map_err(|_| {
        ProjectObservationStoreError::UnavailableProject {
            project_root: project_root.to_path_buf(),
        }
    })?;
    if !canonical.is_dir() {
        return Err(ProjectObservationStoreError::UnavailableProject {
            project_root: canonical,
        });
    }
    Ok(canonical)
}
