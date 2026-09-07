//! Store-layout identity resolution: mapping a project root to its
//! authoritative store layout.

use std::path::{Path, PathBuf};

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::storage::{self, StoreLayout};
use tracedecay_store::ProjectId;

use super::{MovedStoreAdoption, TraceDecay, TraceDecayOpenOptions};

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

    #[hotpath::measure(label = "lifecycle.resolve_registered_layout", future = true)]
    pub(crate) async fn resolve_registered_configuration_layout(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: &RegisteredGlobalDb,
    ) -> Result<StoreLayout> {
        let layout = Self::resolve_store_layout_for_authority(
            project_root,
            open_options,
            Some(registry_database),
            false,
            &MovedStoreAdoption::Never,
        )
        .await?;
        Self::reject_split_identity_cutover(project_root, open_options, &layout)?;
        Ok(layout)
    }

    /// Resolves the store layout for a project that has never been enrolled,
    /// minting a fresh path-derived profile-sharded identity so first-touch
    /// `init` can bootstrap it under the daemon's authority.
    ///
    /// This differs from [`Self::resolve_registered_configuration_layout`] only
    /// in that a project with no enrollment marker or registry match falls
    /// through to a default identity instead of failing closed.
    #[hotpath::skip]
    pub(crate) async fn resolve_first_touch_configuration_layout(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: &RegisteredGlobalDb,
    ) -> Result<StoreLayout> {
        Self::resolve_first_touch_configuration_layout_with_adoption(
            project_root,
            open_options,
            registry_database,
            &MovedStoreAdoption::Never,
        )
        .await
    }

    /// First-touch resolution that can remap a moved non-git project whose
    /// store evidence still names the previous registry root — only under an
    /// explicit operator adoption decision; ambient first-touch passes
    /// [`MovedStoreAdoption::Never`] and always mints fresh.
    #[hotpath::measure(label = "lifecycle.resolve_first_touch_layout", future = true)]
    pub(crate) async fn resolve_first_touch_configuration_layout_with_adoption(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: &RegisteredGlobalDb,
        adoption: &MovedStoreAdoption,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_authority(
            project_root,
            open_options,
            Some(registry_database),
            true,
            adoption,
        )
        .await
    }

    /// Candidate enrollment roots a registered project claims: its canonical
    /// and display roots plus every registered alias.
    pub(crate) fn registry_context_candidate_roots(
        context: &tracedecay_global_db::ProjectRegistryContext,
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

    /// Filters candidate roots down to the ones whose root-side evidence
    /// names exactly `project_id`: a `.git/` repository identity marker with
    /// that id, or (for roots without one) a deterministic path-derived
    /// identity equal to it.
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
            let candidate = tracedecay_runtime_core::worktree::repository_identity_root(&candidate)
                .unwrap_or(candidate);
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };
            if roots.contains(&canonical) {
                continue;
            }
            let named_id = match storage::read_repository_identity_marker(&canonical)? {
                Some(marker) => marker.project_id,
                None => storage::default_profile_project_id(&canonical),
            };
            if named_id == project_id.as_str() {
                roots.push(canonical);
            }
        }
        Ok(roots)
    }

    #[hotpath::measure(label = "lifecycle.enrollment_roots", future = true)]
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
        // Self-heal the sanctioned `.git/`-side anchor: a session mount for a
        // registered project rewrites a missing repository identity marker in
        // place (re-adoption after loss, first mount, or a moved checkout).
        // A non-git root persists nothing — its identity is deterministic
        // from the canonical path with the registry as the durable home.
        // Nothing is ever written into the working tree.
        let enrollment_root =
            tracedecay_runtime_core::worktree::repository_identity_root(project_root)
                .unwrap_or_else(|| project_root.to_path_buf());
        match enrollment_root.canonicalize() {
            Ok(canonical) => {
                if storage::read_repository_identity_marker(&canonical)?.is_none() {
                    storage::write_repository_identity_marker(&canonical, project_id.as_str())?;
                }
                if roots.is_empty() {
                    roots.push(canonical);
                }
            }
            Err(error) if roots.is_empty() => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "could not canonicalize project enrollment root '{}': {error}",
                        enrollment_root.display()
                    ),
                });
            }
            Err(_) => {}
        }
        Ok(roots)
    }

    #[hotpath::measure(label = "lifecycle.resolve_store_layout", future = true)]
    async fn resolve_store_layout_for_authority(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: Option<&RegisteredGlobalDb>,
        allow_default_identity: bool,
        adoption: &MovedStoreAdoption,
    ) -> Result<StoreLayout> {
        let profile_root = open_options.resolved_profile_root()?;
        let mut selected = storage::resolve_persisted_layout(project_root, &profile_root)?;
        // Every linked worktree resolves through its repository, attached or
        // not; suppressing this for detached worktrees dropped them onto the
        // path-hashed identity fallback and minted a duplicate store.
        let git_common_dir = tracedecay_runtime_core::worktree::git_common_dir(project_root);
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

        // One-time legacy adoption: a project enrolled before the working-tree
        // cutover may carry a retired `<repo>/.tracedecay/enrollment.json` and
        // no other resolvable identity. Adopt the identity it names so the
        // following open registers it durably (registry row plus `.git/`
        // marker); after that, the marker or registry resolves first and the
        // legacy file is never consulted again. The file itself is left
        // untouched — users may delete it.
        if selected.is_none() {
            let enrollment_root =
                tracedecay_runtime_core::worktree::repository_identity_root(project_root)
                    .unwrap_or_else(|| project_root.to_path_buf());
            if let Some(marker) = storage::read_legacy_enrollment_marker(&enrollment_root)?
                && marker.storage_mode == storage::StorageMode::ProfileSharded
            {
                selected = Some(storage::profile_sharded_layout(
                    project_root,
                    &profile_root,
                    &marker,
                )?);
            }
        }

        if allow_default_identity
            && let MovedStoreAdoption::AdoptNamed(requested) = adoption
            && let Some(layout) = selected.as_ref()
            && layout.identity.project_id.as_deref() != Some(requested.as_str())
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "cannot adopt project '{requested}' onto root '{}' that already \
                     resolves to registered project '{}'",
                    project_root.display(),
                    layout.identity.project_id.as_deref().unwrap_or("<unknown>")
                ),
            });
        }

        match selected {
            Some(layout) => Ok(layout),
            None if allow_default_identity => {
                if let Some(registry_database) = registry_database
                    && let Some(layout) = Self::adopt_moved_nongit_project(
                        project_root,
                        &profile_root,
                        registry_database,
                        adoption,
                    )
                    .await?
                {
                    return Ok(layout);
                }
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
            .and_then(|profile_root| {
                tracedecay_runtime_core::storage::resolve_layout(project_root, &profile_root)
            })
            .is_ok_and(|layout| {
                layout.storage_mode == tracedecay_runtime_core::storage::StorageMode::ProfileSharded
                    && layout.graph_db_path.exists()
            });
        if open_options.profile_root.is_some() || open_options.global_db_path.is_some() {
            return option_resolved_store_exists;
        }
        option_resolved_store_exists
            || crate::config::has_project_database(project_root)
            || tracedecay_runtime_core::storage::has_repository_identity_marker(project_root)
    }

    #[hotpath::skip]
    pub async fn has_initialized_store(project_root: &Path) -> bool {
        Self::has_initialized_store_with_options(project_root, &TraceDecayOpenOptions::default())
            .await
    }

    #[hotpath::skip]
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
    #[hotpath::skip]
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
    #[hotpath::measure(label = "lifecycle.try_initialized_layout", future = true)]
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
    #[hotpath::skip]
    pub async fn resolve_store_layout_for_identity(project_root: &Path) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_identity_with_options(
            project_root,
            &TraceDecayOpenOptions::default(),
        )
        .await
    }

    #[hotpath::skip]
    pub async fn resolve_store_layout_for_identity_with_options(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_for_local_identity(project_root, open_options).await
    }

    #[hotpath::skip]
    async fn resolve_store_layout_for_local_identity(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        let layout = Self::resolve_store_layout_for_authority(
            project_root,
            open_options,
            None,
            true,
            &MovedStoreAdoption::Never,
        )
        .await?;
        Self::reject_split_identity_cutover(project_root, open_options, &layout)?;
        Ok(layout)
    }

    fn reject_split_identity_cutover(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        selected: &StoreLayout,
    ) -> Result<()> {
        let profile_root = open_options.resolved_profile_root()?;
        let selected_id = selected.identity.project_id.as_deref();
        let (candidates, _, candidates_match_exact_root) =
            storage::matching_legacy_profile_layouts(project_root, &profile_root, selected_id)?;
        // Sibling worktree manifests share a git common dir but name a
        // different checkout path. They are not a second identity for this
        // exact root and must not fail a registered exact-root resolution.
        if !candidates_match_exact_root {
            return Ok(());
        }
        let Some(legacy) = candidates
            .into_iter()
            .find(|layout| layout.graph_db_path.is_file())
        else {
            return Ok(());
        };
        if !selected.graph_db_path.is_file() {
            return Ok(());
        }
        let selected_id = selected_id.unwrap_or("unknown");
        let legacy_id = legacy.identity.project_id.as_deref().unwrap_or("unknown");
        let command = format!(
            "tracedecay migrate consolidate --project {} --source-project-id {legacy_id} --target-project-id {selected_id}",
            shell_quote(&project_root.to_string_lossy()),
        );
        Err(TraceDecayError::Config {
            message: format!(
                "identity cutover conflict for '{}': selected [project_id={selected_id} path='{}']; legacy [project_id={legacy_id} path='{}']; choose one shard and retire the other; run the offline dry-run `{command}` before changing the marker; both shards were preserved and no files changed",
                project_root.display(),
                selected.data_root.display(),
                legacy.data_root.display(),
            ),
        })
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
