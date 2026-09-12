//! Store-layout identity resolution: mapping a project root to its
//! authoritative store layout.

use std::path::Path;

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::storage::{self, StoreLayout};

use super::{MovedStoreAdoption, TraceDecay, TraceDecayOpenOptions};

impl TraceDecay {
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
                // The registry refuses to mint a durable authority for a root
                // under the OS temp directory, but by the time it is asked the
                // shard directory, hook configs, and databases have already
                // been materialized from the default layout — which is how a
                // fixture reaching a daemon under another profile left 111
                // /tmp-rooted stores in that profile. Refuse here, before any
                // layout exists to write into.
                if let Some(message) =
                    tracedecay_global_db::ephemeral_root_rejection(project_root, &profile_root)
                {
                    return Err(TraceDecayError::Config { message });
                }
                if let Some(registry_database) = registry_database
                    && let Some(layout) =
                        tracedecay_application::project_adoption::adopt_moved_nongit_project(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A durable profile root: one that is not under the OS temp directory.
    /// Cargo's target directory qualifies, and the test binary already lives
    /// there, so derive it from `current_exe` rather than hard-coding a path.
    fn durable_profile_root(name: &str) -> std::path::PathBuf {
        let exe = std::env::current_exe().expect("test binary path");
        let base = exe
            .parent()
            .and_then(Path::parent)
            .expect("test binary sits under a cargo target profile directory")
            .join("first-touch-layout-fixtures")
            .join(name);
        std::fs::create_dir_all(&base).expect("create durable profile fixture");
        base
    }

    /// First touch of a root under the OS temp directory against a durable
    /// profile is refused before any layout — and therefore any shard
    /// directory — exists. A hermetic (temp) profile still admits temp roots.
    #[tokio::test]
    async fn first_touch_refuses_an_ephemeral_root_before_minting_a_layout() {
        let ephemeral_project = tempfile::TempDir::new().expect("ephemeral project");
        let durable_profile = durable_profile_root("refuses-ephemeral-root");
        let options = TraceDecayOpenOptions {
            profile_root: Some(durable_profile.clone()),
            global_db_path: Some(durable_profile.join("registry.db")),
        };
        let error = TraceDecay::resolve_store_layout_for_authority(
            ephemeral_project.path(),
            &options,
            None,
            true,
            &MovedStoreAdoption::Never,
        )
        .await
        .expect_err("an ephemeral root must not receive a default layout in a durable profile");
        assert!(
            matches!(error, TraceDecayError::Config { .. }),
            "the refusal is a typed configuration failure: {error:?}"
        );
        assert!(
            !durable_profile.join("projects").exists(),
            "refusal must leave no shard directory behind"
        );

        let hermetic = tempfile::TempDir::new().expect("hermetic profile");
        let options = TraceDecayOpenOptions {
            profile_root: Some(hermetic.path().join("profile")),
            global_db_path: Some(hermetic.path().join("profile/registry.db")),
        };
        let layout = TraceDecay::resolve_store_layout_for_authority(
            ephemeral_project.path(),
            &options,
            None,
            true,
            &MovedStoreAdoption::Never,
        )
        .await
        .expect("a throwaway profile still admits throwaway roots");
        assert!(layout.data_root.starts_with(hermetic.path()));
    }
}
