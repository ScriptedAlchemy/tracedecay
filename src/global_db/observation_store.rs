use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use super::{CodeProjectRecord, GlobalDb, StoreInstanceRecord, project_identity_aliases};

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

impl GlobalDb {
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
        let project = self.get_code_project(&project_id).await.ok_or_else(|| {
            ProjectObservationStoreError::ProjectNotRegistered {
                project_root: project_root.clone(),
            }
        })?;
        let mut stores = self.list_store_contexts_for_project(&project_id).await;
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
        match crate::storage::read_repository_identity_marker(project_root) {
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
        let git_common_dir = crate::worktree::git_common_dir(project_root);
        for alias in project_identity_aliases(project_root, git_common_dir.as_deref()) {
            if let Some(project_id) = self.project_id_by_alias_key(&alias).await {
                project_ids.insert(project_id);
            }
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
            crate::storage::STORE_MANIFEST_FILENAME
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
        let canonical_profile_root = profile_root.canonicalize().map_err(|_| {
            ProjectObservationStoreError::UnavailableStore {
                project_id: project.project_id.clone(),
                store_id: store.store_id.clone(),
                path: profile_root.to_path_buf(),
            }
        })?;
        let store_root = profile_root.join(&expected_relpath);
        require_regular_directory(&project, &store, &store_root)?;
        let canonical_store_root = store_root
            .canonicalize()
            .map_err(|_| unavailable(&project, &store, &store_root))?;
        if canonical_store_root != canonical_profile_root.join(&expected_relpath) {
            return Err(noncanonical(format!(
                "store root resolves outside '{}'",
                expected_relpath.display()
            )));
        }

        let manifest_path = profile_root.join(expected_manifest_relpath);
        require_regular_file(&project, &store, &manifest_path)?;
        validate_store_manifest(&project, &store, &canonical_store_root, &manifest_path)?;
        let database_path = store_root.join(crate::storage::SESSIONS_DB_FILENAME);
        require_regular_file(&project, &store, &database_path)?;
        match crate::storage::has_sqlite_database_header(&database_path) {
            Ok(true) => {}
            Ok(false) => {
                return Err(noncanonical(format!(
                    "'{}' is not a SQLite database",
                    database_path.display()
                )));
            }
            Err(_) => return Err(unavailable(&project, &store, &database_path)),
        }

        Ok(ProjectObservationStoreResolution {
            project,
            store,
            store_root: canonical_store_root,
            database_path: database_path
                .canonicalize()
                .map_err(|_| unavailable_path(&project_id, &store_id, &database_path))?,
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

fn validate_store_manifest(
    project: &CodeProjectRecord,
    store: &StoreInstanceRecord,
    store_root: &Path,
    manifest_path: &Path,
) -> Result<(), ProjectObservationStoreError> {
    let manifest = crate::storage::read_store_manifest(manifest_path).map_err(|error| {
        ProjectObservationStoreError::NonCanonicalStore {
            project_id: project.project_id.clone(),
            store_id: store.store_id.clone(),
            reason: format!("store manifest is invalid: {error}"),
        }
    })?;
    let invalid = |reason: String| ProjectObservationStoreError::NonCanonicalStore {
        project_id: project.project_id.clone(),
        store_id: store.store_id.clone(),
        reason,
    };
    if manifest.schema_version != crate::storage::STORE_MANIFEST_SCHEMA_VERSION {
        return Err(invalid(format!(
            "manifest schema must be {}, found {}",
            crate::storage::STORE_MANIFEST_SCHEMA_VERSION,
            manifest.schema_version
        )));
    }
    if manifest.project_id.as_deref() != Some(project.project_id.as_str()) {
        return Err(invalid(
            "manifest project id does not match the registry".to_string(),
        ));
    }
    if manifest.store_kind != crate::storage::StoreKind::CodeProject {
        return Err(invalid(
            "manifest store kind must be 'code_project'".to_string(),
        ));
    }
    if manifest.storage_mode != crate::storage::StorageMode::ProfileSharded {
        return Err(invalid(
            "manifest storage mode must be 'profile_sharded'".to_string(),
        ));
    }
    if manifest.sessions_db_relpath != Path::new(crate::storage::SESSIONS_DB_FILENAME) {
        return Err(invalid(format!(
            "manifest sessions database path must be '{}'",
            crate::storage::SESSIONS_DB_FILENAME
        )));
    }
    let manifest_data_root = manifest
        .data_root
        .canonicalize()
        .map_err(|_| invalid("manifest data root is unavailable".to_string()))?;
    if manifest_data_root != store_root {
        return Err(invalid(
            "manifest data root does not match the registered store".to_string(),
        ));
    }
    Ok(())
}

fn require_regular_directory(
    project: &CodeProjectRecord,
    store: &StoreInstanceRecord,
    path: &Path,
) -> Result<(), ProjectObservationStoreError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| unavailable(project, store, path))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProjectObservationStoreError::NonCanonicalStore {
            project_id: project.project_id.clone(),
            store_id: store.store_id.clone(),
            reason: format!("'{}' is not a regular directory", path.display()),
        });
    }
    Ok(())
}

fn require_regular_file(
    project: &CodeProjectRecord,
    store: &StoreInstanceRecord,
    path: &Path,
) -> Result<(), ProjectObservationStoreError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| unavailable(project, store, path))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ProjectObservationStoreError::NonCanonicalStore {
            project_id: project.project_id.clone(),
            store_id: store.store_id.clone(),
            reason: format!("'{}' is not a regular file", path.display()),
        });
    }
    Ok(())
}

fn unavailable(
    project: &CodeProjectRecord,
    store: &StoreInstanceRecord,
    path: &Path,
) -> ProjectObservationStoreError {
    unavailable_path(&project.project_id, &store.store_id, path)
}

fn unavailable_path(project_id: &str, store_id: &str, path: &Path) -> ProjectObservationStoreError {
    ProjectObservationStoreError::UnavailableStore {
        project_id: project_id.to_string(),
        store_id: store_id.to_string(),
        path: path.to_path_buf(),
    }
}
