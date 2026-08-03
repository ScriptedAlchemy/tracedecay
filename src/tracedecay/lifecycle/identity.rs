//! Store-layout identity resolution: mapping a project root to its
//! authoritative store layout.

use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::storage::{self, StoreLayout};
use tracedecay_store::ProjectId;

use super::{TraceDecay, TraceDecayOpenOptions};

impl TraceDecay {
    pub(in crate::tracedecay) fn registered_project_id(
        store_layout: &StoreLayout,
    ) -> Result<ProjectId> {
        let project_id =
            store_layout
                .identity
                .project_id
                .as_ref()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "registered code runtime requires an authoritative project identity"
                        .to_owned(),
                })?;
        ProjectId::new(project_id.clone()).map_err(|error| TraceDecayError::Config {
            message: format!("invalid registered project identity: {error}"),
        })
    }

    pub(super) async fn resolve_store_layout_for_project(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_authority(project_root, open_options, None, true).await
    }

    pub(crate) async fn resolve_registered_configuration_layout(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: &RegisteredGlobalDb,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_authority(
            project_root,
            open_options,
            Some(registry_database),
            false,
        )
        .await
    }

    /// Resolves the store layout for a project that has never been enrolled,
    /// minting a fresh path-derived profile-sharded identity so first-touch
    /// `init` can bootstrap it under the daemon's authority.
    ///
    /// This differs from [`Self::resolve_registered_configuration_layout`] only
    /// in that a project with no enrollment marker or registry match falls
    /// through to a default identity instead of failing closed.
    pub(crate) async fn resolve_first_touch_configuration_layout(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: &RegisteredGlobalDb,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_authority(
            project_root,
            open_options,
            Some(registry_database),
            true,
        )
        .await
    }

    /// Candidate enrollment roots a registered project claims: its canonical
    /// and display roots plus every registered alias.
    pub(crate) fn registry_context_candidate_roots(
        context: &crate::global_db::ProjectRegistryContext,
    ) -> Vec<PathBuf> {
        let mut candidates = vec![
            PathBuf::from(&context.project.canonical_root),
            PathBuf::from(&context.project.display_root),
        ];
        candidates.extend(
            context
                .aliases
                .iter()
                .map(|alias| PathBuf::from(&alias.alias_path)),
        );
        candidates
    }

    /// Filters candidate roots down to the ones that already carry a
    /// profile-sharded enrollment marker naming exactly `project_id`.
    ///
    /// This never creates or repairs a marker, so a caller that must not mount
    /// a store the profile has not enrolled — a cross-project memory reader,
    /// for one — can tell "not enrolled here" apart from "enrolled".
    pub(crate) fn enrolled_project_roots(
        candidates: impl IntoIterator<Item = PathBuf>,
        project_id: &ProjectId,
    ) -> Result<Vec<PathBuf>> {
        let mut candidates = candidates.into_iter().collect::<Vec<_>>();
        candidates.sort();
        candidates.dedup();

        let mut roots = Vec::new();
        for candidate in candidates {
            let candidate =
                crate::worktree::repository_identity_root(&candidate).unwrap_or(candidate);
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };
            if roots.contains(&canonical) {
                continue;
            }
            let Some(marker) = storage::read_enrollment_marker(&canonical)? else {
                continue;
            };
            if marker.storage_mode == storage::StorageMode::ProfileSharded
                && marker.project_id == project_id.as_str()
            {
                roots.push(canonical);
            }
        }
        Ok(roots)
    }

    pub(crate) async fn registered_enrollment_roots(
        project_root: &Path,
        store_layout: &StoreLayout,
        project_id: &ProjectId,
        registry_database: &RegisteredGlobalDb,
    ) -> Result<Vec<PathBuf>> {
        let mut candidates = vec![
            project_root.to_path_buf(),
            store_layout.project_root.clone(),
        ];
        if let Some(context) = registry_database
            .project_registry_context_by_id(project_id.as_str())
            .await?
        {
            candidates.extend(Self::registry_context_candidate_roots(&context));
        }

        let mut roots = Self::enrolled_project_roots(candidates, project_id)?;
        if roots.is_empty() {
            let enrollment_root = crate::worktree::repository_identity_root(project_root)
                .unwrap_or_else(|| project_root.to_path_buf());
            let canonical =
                enrollment_root
                    .canonicalize()
                    .map_err(|error| TraceDecayError::Config {
                        message: format!(
                            "could not canonicalize project enrollment root '{}': {error}",
                            enrollment_root.display()
                        ),
                    })?;
            storage::write_enrollment_marker(
                &canonical,
                &storage::EnrollmentMarker {
                    project_id: project_id.as_str().to_owned(),
                    storage_mode: storage::StorageMode::ProfileSharded,
                },
            )?;
            roots.push(canonical);
        }
        Ok(roots)
    }

    async fn resolve_store_layout_for_authority(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: Option<&RegisteredGlobalDb>,
        allow_default_identity: bool,
    ) -> Result<StoreLayout> {
        let profile_root = open_options.resolved_profile_root()?;
        if storage::read_enrollment_marker(project_root)?.is_some() {
            return storage::resolve_persisted_layout(project_root, &profile_root)?.ok_or_else(
                || TraceDecayError::Config {
                    message: "enrollment marker did not resolve a profile store".to_string(),
                },
            );
        }

        let mut selected = storage::resolve_persisted_layout(project_root, &profile_root)?;
        // Every linked worktree resolves through its repository, attached or
        // not; suppressing this for detached worktrees dropped them onto the
        // path-hashed identity fallback and minted a duplicate store.
        let git_common_dir = crate::worktree::git_common_dir(project_root);
        if selected.is_none()
            && let Some(registry_database) = registry_database
            && let Some(resolution) = registry_database
                .resolve_project_store_by_identity(project_root, git_common_dir.as_deref())
                .await?
        {
            selected = Some(storage::profile_sharded_layout(
                project_root,
                &profile_root,
                &storage::EnrollmentMarker {
                    project_id: resolution.project.project_id,
                    storage_mode: storage::StorageMode::ProfileSharded,
                },
            )?);
        }

        match selected {
            Some(layout) => Ok(layout),
            None if allow_default_identity => {
                storage::default_profile_sharded_layout(project_root, &profile_root)
            }
            None => Err(TraceDecayError::Config {
                message:
                    "registered configuration layout requires an enrolled or registry-resolved project identity"
                        .to_owned(),
            }),
        }
    }

    /// Returns `true` if a `TraceDecay` project has been initialized at the given root.
    pub fn is_initialized(project_root: &Path) -> bool {
        Self::is_initialized_with_options(project_root, &TraceDecayOpenOptions::default())
    }

    pub fn is_initialized_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> bool {
        let option_resolved_store_exists = open_options
            .resolved_profile_root()
            .and_then(|profile_root| crate::storage::resolve_layout(project_root, &profile_root))
            .is_ok_and(|layout| {
                layout.storage_mode == crate::storage::StorageMode::ProfileSharded
                    && layout.graph_db_path.exists()
            });
        if open_options.profile_root.is_some() || open_options.global_db_path.is_some() {
            return option_resolved_store_exists;
        }
        option_resolved_store_exists
            || crate::config::has_project_database(project_root)
            || crate::storage::has_enrollment_marker(project_root)
    }

    pub async fn has_initialized_store(project_root: &Path) -> bool {
        Self::has_initialized_store_with_options(project_root, &TraceDecayOpenOptions::default())
            .await
    }

    pub async fn has_initialized_store_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> bool {
        Self::initialized_store_layout_with_options(project_root, open_options)
            .await
            .is_some()
    }

    /// Resolves the store layout for a project using the same registry/alias
    /// aware path as [`Self::has_initialized_store`], returning it only when
    /// the resolved store's graph database actually exists.
    pub async fn initialized_store_layout_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Option<StoreLayout> {
        Self::try_initialized_store_layout_with_options(project_root, open_options)
            .await
            .ok()
            .flatten()
    }

    /// Resolves an initialized store without discarding identity conflicts or
    /// other storage errors. User-facing diagnostics must use this variant so
    /// a preserved split store is never mislabeled as uninitialized.
    pub async fn try_initialized_store_layout_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<Option<StoreLayout>> {
        let layout =
            Self::resolve_store_layout_for_local_identity(project_root, open_options).await?;
        Ok(layout.graph_db_path.is_file().then_some(layout))
    }

    /// Resolves the profile store layout for a local path using enrollment
    /// markers first, then the global registry aliases for the git identity.
    pub async fn resolve_store_layout_for_identity(project_root: &Path) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_identity_with_options(
            project_root,
            &TraceDecayOpenOptions::default(),
        )
        .await
    }

    pub async fn resolve_store_layout_for_identity_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_local_identity(project_root, open_options).await
    }

    async fn resolve_store_layout_for_local_identity(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_authority(project_root, open_options, None, true).await
    }
}
