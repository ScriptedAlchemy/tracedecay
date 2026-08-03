//! Store-layout identity resolution: mapping a project root to its
//! authoritative store layout, plus the legacy-shard inventory and
//! identity-cutover conflict helpers `choose_identity_layout` relies on.

use std::path::{Path, PathBuf};

use crate::branch_meta;
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
        Self::resolve_store_layout_with_identity_migration(
            project_root,
            open_options,
            true,
            None,
            true,
        )
        .await
    }

    pub(crate) async fn resolve_registered_configuration_layout(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: &RegisteredGlobalDb,
        allow_repair: bool,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_with_identity_migration(
            project_root,
            open_options,
            allow_repair,
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
    /// in that a project with no enrollment marker, registry match, or legacy
    /// shard falls through to a default identity instead of failing closed.
    /// Ambiguous or conflicting *existing* stores still surface their own
    /// identity-cutover errors from [`Self::choose_identity_layout`] and never
    /// reach the default-identity branch, so this never masks a real conflict.
    pub(crate) async fn resolve_first_touch_configuration_layout(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        registry_database: &RegisteredGlobalDb,
        allow_repair: bool,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_with_identity_migration(
            project_root,
            open_options,
            allow_repair,
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

    async fn resolve_store_layout_with_identity_migration(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        allow_repair: bool,
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

        let selected_id = selected
            .as_ref()
            .and_then(|layout| layout.identity.project_id.as_deref());
        // Store inventory opens graph and session databases, so keep it behind
        // the rare paths that compare actual stores rather than every resolve.
        let (candidates, selected_is_sole_exact_root) =
            storage::matching_legacy_profile_layouts(project_root, &profile_root, selected_id)?;
        let selected = Self::choose_identity_layout(
            project_root,
            selected,
            candidates,
            selected_is_sole_exact_root,
            allow_repair,
        )
        .await?;
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

    async fn choose_identity_layout(
        project_root: &Path,
        selected: Option<StoreLayout>,
        candidates: Vec<StoreLayout>,
        selected_is_sole_exact_root: bool,
        allow_repair: bool,
    ) -> Result<Option<StoreLayout>> {
        // With no competing candidate the selected layout wins without an
        // inventory read.
        if selected_is_sole_exact_root
            && !candidates.is_empty()
            && let Some(selected) = selected.as_ref()
        {
            let selected_inventory = store_identity_inventory(selected).await;
            if selected_inventory.is_healthy() && !selected_inventory.is_pristine() {
                return Ok(Some(selected.clone()));
            }
        }
        if candidates.len() > 1 {
            let mut details = Vec::new();
            for candidate in &candidates {
                details.push(store_identity_inventory(candidate).await.to_string());
            }
            return Err(TraceDecayError::Config {
                message: format!(
                    "ambiguous legacy profile stores for '{}': {}; no files changed",
                    project_root.display(),
                    details.join("; ")
                ),
            });
        }
        let Some(candidate) = candidates.into_iter().next() else {
            return Ok(selected);
        };
        let Some(selected) = selected else {
            return Ok(Some(candidate));
        };

        let selected_inventory = store_identity_inventory(&selected).await;
        let candidate_inventory = store_identity_inventory(&candidate).await;
        let manifest_matches_project_root = |layout: &StoreLayout| {
            let manifest_path = layout.manifest_path.as_deref()?;
            let manifest = storage::read_store_manifest(manifest_path).ok()?;
            Some(
                manifest.project_root == project_root
                    || match (
                        manifest.project_root.canonicalize(),
                        project_root.canonicalize(),
                    ) {
                        (Ok(manifest_root), Ok(project_root)) => manifest_root == project_root,
                        _ => false,
                    },
            )
        };
        if manifest_matches_project_root(&candidate) == Some(true)
            && manifest_matches_project_root(&selected) == Some(false)
            && candidate_inventory.is_healthy()
            && !candidate_inventory.is_pristine()
            && selected_inventory.is_healthy()
            && !selected_inventory.is_pristine()
        {
            return Ok(Some(candidate));
        }
        if selected_inventory.is_pristine() && candidate_inventory.is_healthy() {
            if !allow_repair {
                return Err(identity_cutover_conflict(
                    project_root,
                    &selected_inventory,
                    &candidate_inventory,
                    "safe empty-store repair is available during a writable open",
                ));
            }
            let candidate_id = candidate.identity.project_id.as_deref().ok_or_else(|| {
                TraceDecayError::Config {
                    message: "legacy candidate has no project id".to_string(),
                }
            })?;
            storage::write_repository_identity_marker(project_root, candidate_id)?;
            storage::retire_identity_cutover_manifest(&selected)?;
            return Ok(Some(candidate));
        }
        if candidate_inventory.is_pristine() && selected_inventory.is_healthy() {
            if allow_repair {
                let selected_id = selected.identity.project_id.as_deref().ok_or_else(|| {
                    TraceDecayError::Config {
                        message: "selected store has no project id".to_string(),
                    }
                })?;
                storage::write_repository_identity_marker(project_root, selected_id)?;
                storage::retire_identity_cutover_manifest(&candidate)?;
            }
            return Ok(Some(selected));
        }
        Err(identity_cutover_conflict(
            project_root,
            &selected_inventory,
            &candidate_inventory,
            "choose one shard and retire the other before changing the marker",
        ))
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
        Self::resolve_store_layout_with_identity_migration(
            project_root,
            open_options,
            false,
            None,
            true,
        )
        .await
    }
}

#[derive(Debug)]
struct StoreIdentityInventory {
    project_id: String,
    data_root: PathBuf,
    graph_health: &'static str,
    nodes: u64,
    files: u64,
    facts: u64,
    sessions: u64,
    messages: u64,
    lcm_rows: u64,
    branches: usize,
    automation_files: u64,
    payload_files: u64,
    response_files: u64,
}

impl StoreIdentityInventory {
    fn is_healthy(&self) -> bool {
        self.graph_health == "healthy"
    }

    fn is_pristine(&self) -> bool {
        self.is_healthy()
            && self.nodes == 0
            && self.files == 0
            && self.facts == 0
            && self.sessions == 0
            && self.messages == 0
            && self.lcm_rows == 0
            && self.branches <= 1
            && self.automation_files == 0
            && self.payload_files == 0
            && self.response_files == 0
    }
}

impl std::fmt::Display for StoreIdentityInventory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "project_id={} path='{}' graph_health={} nodes={} files={} facts={} sessions={} messages={} lcm={} branches={} automation_files={} payload_files={} response_files={}",
            self.project_id,
            self.data_root.display(),
            self.graph_health,
            self.nodes,
            self.files,
            self.facts,
            self.sessions,
            self.messages,
            self.lcm_rows,
            self.branches,
            self.automation_files,
            self.payload_files,
            self.response_files,
        )
    }
}

async fn store_identity_inventory(layout: &StoreLayout) -> StoreIdentityInventory {
    let scratch_root = layout.data_root.join("scratch").join("sqlite-read");
    let open_result = match storage::PrivateStoreIo::create_dir_all(&scratch_root) {
        Ok(()) => crate::sqlite_read_snapshot::open_in(&layout.graph_db_path, &scratch_root).await,
        Err(error) => Err(error),
    };
    let (graph_health, nodes, files, facts) = match open_result {
        Ok(snapshot) => {
            let connection = snapshot.connection();
            let healthy = quick_check_ok(connection).await && snapshot.validate_source().is_ok();
            if healthy {
                (
                    "healthy",
                    count_rows(connection, "nodes").await,
                    count_rows(connection, "files").await,
                    count_rows(connection, "memory_facts").await,
                )
            } else {
                ("corrupt", 0, 0, 0)
            }
        }
        Err(_) if layout.graph_db_path.exists() => ("corrupt", 0, 0, 0),
        Err(_) => ("missing", 0, 0, 0),
    };

    let (sessions, messages, lcm_rows) =
        match storage::PrivateStoreIo::create_dir_all(&scratch_root) {
            Ok(()) => {
                match crate::sqlite_read_snapshot::open_in(&layout.sessions_db_path, &scratch_root)
                    .await
                {
                    Ok(snapshot) => {
                        let connection = snapshot.connection();
                        let counts = (
                            count_rows(connection, "sessions").await,
                            count_rows(connection, "session_messages").await,
                            count_rows(connection, "lcm_raw_messages").await
                                + count_rows(connection, "lcm_summary_nodes").await,
                        );
                        if snapshot.validate_source().is_ok() {
                            counts
                        } else {
                            (0, 0, 0)
                        }
                    }
                    Err(_) => (0, 0, 0),
                }
            }
            Err(_) => (0, 0, 0),
        };

    StoreIdentityInventory {
        project_id: layout
            .identity
            .project_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        data_root: layout.data_root.clone(),
        graph_health,
        nodes,
        files,
        facts,
        sessions,
        messages,
        lcm_rows,
        branches: branch_meta::load_branch_meta(&layout.data_root)
            .map_or(0, |meta| meta.branches.len()),
        automation_files: count_tree_files(&layout.dashboard_root),
        payload_files: count_tree_files(&layout.lcm_payload_root),
        response_files: count_tree_files(&layout.response_handle_root),
    }
}

async fn quick_check_ok(connection: &(impl crate::db::engine::QueryExecutor + ?Sized)) -> bool {
    let Ok(mut rows) = connection.query("PRAGMA quick_check", ()).await else {
        return false;
    };
    rows.next()
        .await
        .ok()
        .flatten()
        .and_then(|row| row.get::<String>(0).ok())
        .is_some_and(|result| result == "ok")
}

async fn count_rows(
    connection: &(impl crate::db::engine::QueryExecutor + ?Sized),
    table: &str,
) -> u64 {
    let Ok(mut rows) = connection
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
    else {
        return 0;
    };
    rows.next()
        .await
        .ok()
        .flatten()
        .and_then(|row| row.get::<i64>(0).ok())
        .and_then(|count| u64::try_from(count).ok())
        .unwrap_or(0)
}

fn count_tree_files(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .map(|path| match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => 1,
            Ok(metadata) if metadata.is_dir() => count_tree_files(&path),
            _ => 0,
        })
        .sum()
}

fn identity_cutover_conflict(
    project_root: &Path,
    selected: &StoreIdentityInventory,
    legacy: &StoreIdentityInventory,
    action: &str,
) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "identity cutover conflict for '{}': selected [{}]; legacy [{}]; {action}; both shards were preserved and no files changed",
            project_root.display(),
            selected,
            legacy
        ),
    }
}
