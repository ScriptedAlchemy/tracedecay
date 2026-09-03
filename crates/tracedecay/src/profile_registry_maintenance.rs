//! Exact-final profile registry maintenance.
//!
//! This composition boundary opens the daemon-owned final registry for
//! explicit offline maintenance. Orphan inspection, relinking, and retirement
//! semantics live with the registry store in `tracedecay-global-db`; this
//! wrapper owns only profile/runtime composition.

use std::path::{Component, Path, PathBuf};
use tracedecay_global_db::{
    ProjectRegistryContext, RegisteredGlobalDb, RegisteredGlobalDbLeaseV1,
    registry_maintenance::ForgetRegistryProjectRows, registry_maintenance::RegistryGcReport,
    registry_maintenance::RegistryOrphanRelinkApplyReport,
    registry_maintenance::RegistryOrphanRelinkReport,
    registry_maintenance::forget_registry_project,
};

fn store_removal_error(
    operation: &str,
    path: &Path,
    error: &std::io::Error,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("failed to {operation} '{}': {error}", path.display()),
    }
}

/// Verifies a removed store path left no namespace entry behind (a dangling
/// symlink still occupies the name and must be reported, not read as gone).
pub fn verify_store_path_absent(path: &Path) -> tracedecay_domain::errors::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "store removal did not remove expected namespace entry '{}'",
                path.display()
            ),
        }),
        Err(error) => Err(store_removal_error(
            "verify store path namespace absence",
            path,
            &error,
        )),
    }
}

/// Removes one registered store directory with the destructive-detachment
/// ritual shared by `wipe` and `projects forget`: symlink components are
/// refused, the parent directory is durably synced, and the absence of the
/// namespace entry is verified. Returns `false` when nothing existed.
#[hotpath::measure(label = "registry_maintenance.remove_store_directory")]
pub fn remove_store_directory(path: &Path) -> tracedecay_domain::errors::Result<bool> {
    use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_directory};

    tracedecay_runtime_core::storage::reject_symlink_components(path, "store removal target")
        .map_err(|error| store_removal_error("validate store removal target", path, &error))?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            // `remove_dir_all` does not follow directory symlinks, and the
            // exact target plus every existing parent were rejected above
            // when symlinked; the caller's exclusive lifecycle lease keeps a
            // cooperating TraceDecay writer from racing the removal.
            std::fs::remove_dir_all(path)
                .map_err(|error| store_removal_error("remove store directory", path, &error))?;
            let parent = path.parent().ok_or_else(|| {
                tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!("store removal target '{}' has no parent", path.display()),
                }
            })?;
            sync_directory(parent, DirectorySyncPolicy::Strict).map_err(|error| {
                store_removal_error("sync store removal parent", parent, &error)
            })?;
            verify_store_path_absent(path)?;
            Ok(true)
        }
        Ok(_) => Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "store removal target '{}' is not a regular directory",
                path.display()
            ),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(store_removal_error(
            "inspect store removal target",
            path,
            &error,
        )),
    }
}

/// Resolves one registered store-instance relpath under the profile root,
/// refusing anything but a plain relative path so a corrupted registry row
/// can never direct a destructive command outside the profile.
fn resolve_store_data_root(
    profile_root: &Path,
    store_relpath: &str,
) -> tracedecay_domain::errors::Result<PathBuf> {
    let relpath = Path::new(store_relpath);
    if relpath.as_os_str().is_empty()
        || !relpath
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "registered store relpath '{store_relpath}' is not a plain relative path inside \
                 the profile; refusing the destructive operation"
            ),
        });
    }
    Ok(profile_root.join(relpath))
}

/// Outcome of forgetting one registered project.
#[derive(Debug)]
pub struct ForgetProjectReport {
    pub project_id: String,
    /// Store directories that existed and were removed.
    pub removed_store_dirs: Vec<PathBuf>,
    /// Registered store directories that were already absent.
    pub absent_store_dirs: Vec<PathBuf>,
    /// Store directories preserved by `--keep-store`.
    pub kept_store_dirs: Vec<PathBuf>,
    pub rows: ForgetRegistryProjectRows,
}

pub struct ProfileRegistryMaintenanceRuntime {
    profile_database: RegisteredGlobalDbLeaseV1,
}

impl ProfileRegistryMaintenanceRuntime {
    /// Opens an existing exact-final profile registry without creating one.
    #[hotpath::measure(label = "daemon.profile_registry.open_existing", future = true)]
    pub async fn try_open_existing(
        profile_root: &Path,
    ) -> tracedecay_domain::errors::Result<Option<Self>> {
        if !profile_root.try_exists().map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Database {
                operation: "inspect existing profile root".to_string(),
                message: error.to_string(),
            }
        })? {
            return Ok(None);
        }
        let profile_root = profile_root.canonicalize().map_err(|error| {
            tracedecay_domain::errors::TraceDecayError::Database {
                operation: "resolve existing profile registry".to_string(),
                message: error.to_string(),
            }
        })?;
        if !profile_root
            .join("global.db")
            .try_exists()
            .map_err(
                |error| tracedecay_domain::errors::TraceDecayError::Database {
                    operation: "inspect existing profile registry".to_string(),
                    message: error.to_string(),
                },
            )?
        {
            return Ok(None);
        }
        Self::open(&profile_root).await.map(Some)
    }

    #[hotpath::measure(label = "daemon.profile_registry.open", future = true)]
    pub async fn open(profile_root: &Path) -> tracedecay_domain::errors::Result<Self> {
        let identity = tracedecay_daemon_identity::profile_identity::load_existing(profile_root)?;
        let registry =
            tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(identity).await?;
        let profile_database = registry.profile_database().await?;
        Ok(Self { profile_database })
    }

    #[hotpath::measure(label = "daemon.profile_registry.list_projects", future = true)]
    pub async fn registered_project_paths(
        &self,
    ) -> tracedecay_domain::errors::Result<Vec<PathBuf>> {
        self.profile_database
            .try_list_code_project_paths(usize::MAX)
            .await
    }

    #[hotpath::measure(label = "daemon.profile_registry.classify_storage", future = true)]
    pub async fn classify_project_storage(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> tracedecay_domain::errors::Result<tracedecay_runtime_core::storage::ProjectStorageLocation>
    {
        let location = tracedecay_runtime_core::storage::classify_project_storage(project_root);
        if location.status != tracedecay_runtime_core::storage::ProjectStorageStatus::Stale {
            return Ok(location);
        }
        let Some(store) = self
            .profile_database
            .try_resolve_project_store_record_by_alias(project_root)
            .await?
        else {
            return Ok(location);
        };
        Ok(store
            .classify_storage(project_root, profile_root)
            .unwrap_or(location))
    }

    pub fn canonical_project_key(project_root: &Path) -> String {
        RegisteredGlobalDb::canonical_project_key(project_root)
    }

    #[hotpath::measure(label = "daemon.profile_registry.retire_paths", future = true)]
    pub async fn delete_project_paths(
        &self,
        project_paths: &[PathBuf],
    ) -> tracedecay_domain::errors::Result<usize> {
        tracedecay_global_db::registry_maintenance::retire_registry_project_paths(
            self.profile_database.as_ref(),
            project_paths,
        )
        .await
    }

    /// Resolves one registered project from an operator selector (project id,
    /// registered alias path, or repository root). See
    /// [`RegisteredGlobalDb::project_registry_context_by_selector`].
    pub async fn resolve_registered_project(
        &self,
        selector: &Path,
    ) -> tracedecay_domain::errors::Result<Option<ProjectRegistryContext>> {
        self.profile_database
            .as_ref()
            .project_registry_context_by_selector(selector)
            .await
    }

    /// Forgets exactly one registered project: detaches its store directories
    /// from disk (unless `keep_store`), then retires its registry rows in one
    /// transaction. Every store relpath is validated before anything is
    /// deleted, and a directory that fails to delete aborts before the row
    /// retirement so the registry keeps naming the store for a retry.
    #[hotpath::measure(label = "registry_maintenance.forget_project", future = true)]
    pub async fn forget_project(
        &self,
        profile_root: &Path,
        context: &ProjectRegistryContext,
        keep_store: bool,
    ) -> tracedecay_domain::errors::Result<ForgetProjectReport> {
        let mut store_dirs = Vec::with_capacity(context.stores.len());
        for store in &context.stores {
            store_dirs.push(resolve_store_data_root(
                profile_root,
                &store.store.store_relpath,
            )?);
        }
        store_dirs.sort();
        store_dirs.dedup();
        let mut removed_store_dirs = Vec::new();
        let mut absent_store_dirs = Vec::new();
        let mut kept_store_dirs = Vec::new();
        for store_dir in store_dirs {
            if keep_store {
                kept_store_dirs.push(store_dir);
            } else if remove_store_directory(&store_dir)? {
                removed_store_dirs.push(store_dir);
            } else {
                absent_store_dirs.push(store_dir);
            }
        }
        let mut project_paths = vec![
            PathBuf::from(&context.project.canonical_root),
            PathBuf::from(&context.project.display_root),
        ];
        project_paths.extend(
            context
                .aliases
                .iter()
                .map(|alias| PathBuf::from(&alias.alias_path)),
        );
        project_paths.sort();
        project_paths.dedup();
        let rows = forget_registry_project(
            self.profile_database.as_ref(),
            &context.project.project_id,
            &project_paths,
        )
        .await?;
        Ok(ForgetProjectReport {
            project_id: context.project.project_id.clone(),
            removed_store_dirs,
            absent_store_dirs,
            kept_store_dirs,
            rows,
        })
    }

    #[hotpath::measure(label = "daemon.profile_registry.apply_orphan_relink", future = true)]
    pub async fn apply_orphan_relink(
        &self,
        report: &RegistryOrphanRelinkReport,
    ) -> std::result::Result<RegistryOrphanRelinkApplyReport, Vec<String>> {
        tracedecay_global_db::registry_maintenance::apply_registry_orphan_relink_report(
            self.profile_database.as_ref(),
            report,
        )
        .await
    }

    #[hotpath::measure(label = "daemon.profile_registry.gc", future = true)]
    pub async fn registry_gc(
        &self,
        profile_root: &Path,
        prefix: Option<String>,
        apply: bool,
    ) -> tracedecay_domain::errors::Result<RegistryGcReport> {
        if apply {
            tracedecay_global_db::registry_maintenance::apply_registry_gc(
                self.profile_database.as_ref(),
                profile_root,
                prefix,
            )
            .await
        } else {
            tracedecay_global_db::registry_maintenance::registry_gc_report(
                self.profile_database.as_ref(),
                profile_root,
                prefix,
            )
            .await
        }
    }

    pub fn database(&self) -> &RegisteredGlobalDb {
        self.profile_database.as_ref()
    }
}

// Test-only fixtures use unwrap/expect so setup failures abort immediately.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod store_removal_tests {
    use super::*;

    #[test]
    fn removes_a_store_directory_and_reports_a_missing_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = tmp.path().join("projects").join("proj_a");
        std::fs::create_dir_all(store.join("branches")).unwrap();
        std::fs::write(store.join("tracedecay.db"), b"store bytes").unwrap();

        assert!(remove_store_directory(&store).unwrap());
        assert!(!store.exists());
        assert!(
            !remove_store_directory(&store).unwrap(),
            "an absent store is reported, not invented"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_store_targets_and_dangling_namespace_entries() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let real = tmp.path().join("real-store");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("store-link");
        symlink(&real, &link).unwrap();
        let dangling = tmp.path().join("dangling");
        symlink(tmp.path().join("missing"), &dangling).unwrap();

        assert!(remove_store_directory(&link).is_err());
        assert!(real.exists(), "the symlink target must survive the refusal");
        assert!(
            verify_store_path_absent(&dangling).is_err(),
            "a dangling symlink remains a namespace entry"
        );
    }

    #[test]
    fn store_relpath_validation_refuses_escapes_and_absolute_paths() {
        let profile = Path::new("/profile");

        assert_eq!(
            resolve_store_data_root(profile, "projects/proj_a").unwrap(),
            profile.join("projects/proj_a")
        );
        for malformed in ["", "..", "projects/../..", "/etc", "./projects/proj_a"] {
            assert!(
                resolve_store_data_root(profile, malformed).is_err(),
                "malformed store relpath was admitted: {malformed:?}"
            );
        }
    }
}
