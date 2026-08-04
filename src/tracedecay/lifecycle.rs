//! Lifecycle: init/open/branch-tracking entry points plus the profile-store
//! registration helpers they rely on.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::branch;
use crate::branch_meta::{self, BranchMeta};
use crate::config::{TraceDecayConfig, db_filename, load_config_from_path, save_config_to_path};
use crate::db::migrations::{FULL_REINDEX_REQUIRED_KEY, FULL_REINDEX_REQUIRED_VALUE};
use crate::db::{Database, DatabaseAuthority};
use crate::errors::{Result, TraceDecayError};
use crate::extraction::LanguageRegistry;
use crate::global_db::{GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert};
use crate::storage::{self, StoreLayout};

use super::locking::{
    clear_dirty_sentinel_at, has_dirty_sentinel_at, try_acquire_graph_sync_locks,
};
use super::{TraceDecay, TraceDecayOpenOptions, current_timestamp};

impl TraceDecay {
    /// Initializes a new `TraceDecay` project at the given root.
    ///
    /// Writes a default configuration to the resolved project store and
    /// initializes a fresh `SQLite` database.
    pub async fn init(project_root: &Path) -> Result<Self> {
        Self::init_with_options(project_root, TraceDecayOpenOptions::default()).await
    }

    pub async fn init_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        let store_layout =
            Self::resolve_store_layout_for_project(project_root, &open_options).await?;
        let authority = DatabaseAuthority::for_runtime(&store_layout.graph_db_path, "init")?;
        let config = TraceDecayConfig {
            root_dir: project_root.to_string_lossy().to_string(),
            ..TraceDecayConfig::default()
        };
        save_config_to_path(&store_layout.config_path, &config)?;

        let (db, _migrated) = Database::initialize(&store_layout.graph_db_path, &authority).await?;
        let active_graph_layout = active_graph_layout(&store_layout.graph_db_path);
        if store_layout.storage_mode == storage::StorageMode::ProfileSharded {
            storage::write_store_manifest(&store_layout)?;
        }

        // Bootstrap branch metadata if we can detect a default branch
        let active_branch = branch::current_branch(project_root);
        let default_branch = active_branch.as_ref().and_then(|_| {
            branch::detect_default_branch(project_root).or_else(|| active_branch.clone())
        });
        if let Some(ref default) = default_branch {
            let meta = BranchMeta::new_for_dir(&store_layout.data_root, default);
            let _ = branch_meta::save_branch_meta(&store_layout.data_root, &meta);
        }

        let ts = Self {
            db,
            config,
            project_root: project_root.to_path_buf(),
            store_layout,
            active_graph_layout,
            open_options,
            registry: LanguageRegistry::new(),
            active_branch,
            serving_branch: None,
            fallback_warning: None,
            read_only: false,
        };
        ts.register_project_store_in_global_registry().await;
        Ok(ts)
    }

    pub async fn init_and_index_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        let cg = Self::init_with_options(project_root, open_options).await?;
        cg.index_all().await?;
        Ok(cg)
    }

    /// Returns a reference to the underlying database.
    pub fn db(&self) -> &Database {
        &self.db
    }

    async fn schema_version(db: &Database, operation: &str) -> Result<u32> {
        let mut rows = db
            .conn()
            .query("PRAGMA user_version", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("{operation}: failed to read user_version: {e}"),
                operation: operation.to_string(),
            })?;
        let row = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("{operation}: failed to read user_version row: {e}"),
            operation: operation.to_string(),
        })?;
        match row {
            Some(row) => {
                let version: i64 = row.get(0).map_err(|e| TraceDecayError::Database {
                    message: format!("{operation}: failed to read user_version value: {e}"),
                    operation: operation.to_string(),
                })?;
                Ok(version as u32)
            }
            None => Ok(0),
        }
    }

    async fn latest_schema_version() -> Result<u32> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let db_path = std::env::temp_dir().join(format!(
            "tracedecay-current-schema-{}-{stamp}.db",
            std::process::id()
        ));
        let authority = DatabaseAuthority::acquire_test(&db_path, "latest schema version")?;
        let (db, _) = Database::initialize(&db_path, &authority).await?;
        let version = Self::schema_version(&db, "latest_schema_version").await;
        db.close();
        delete_db_files(&db_path);
        version
    }

    pub async fn ensure_schema_current(&self) -> Result<()> {
        let current = Self::schema_version(&self.db, "ensure_schema_current").await?;
        let latest = Self::latest_schema_version().await?;
        if current < latest {
            return Err(TraceDecayError::Config {
                message: format!(
                    "read-only TraceDecay database schema is v{current}, but this binary requires \
                     v{latest}; open the project with write access to run migrations before serving \
                     it read-only"
                ),
            });
        }
        if current > latest {
            return Err(TraceDecayError::Config {
                message: format!(
                    "TraceDecay database schema v{current} is newer than this binary supports \
                     (v{latest}); upgrade tracedecay before serving this store"
                ),
            });
        }
        Ok(())
    }

    async fn resolve_store_layout_for_project(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_with_identity_migration(project_root, open_options, true).await
    }

    async fn resolve_store_layout_for_project_read_only(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        Self::resolve_store_layout_with_identity_migration(project_root, open_options, false).await
    }

    async fn resolve_store_layout_with_identity_migration(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
        allow_repair: bool,
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
        let git_common_dir = (!crate::worktree::is_detached_linked_worktree(project_root))
            .then(|| crate::worktree::git_common_dir(project_root))
            .flatten();
        if selected.is_none()
            && let Some(global_db) = open_options.open_global_db().await
        {
            if let Some(resolution) = global_db
                .resolve_project_store_by_identity(project_root, git_common_dir.as_deref())
                .await
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
        }

        let selected_id = selected
            .as_ref()
            .and_then(|layout| layout.identity.project_id.as_deref());
        // Store inventory opens the graph and sessions databases, so it must
        // stay behind the rare paths that actually compare stores. Resolving a
        // layout is on every open, including fail-closed clients that must not
        // touch the store at all.
        let (candidates, selected_is_sole_exact_root) =
            storage::matching_legacy_profile_layouts(project_root, &profile_root, selected_id)?;
        Self::choose_identity_layout(
            project_root,
            selected,
            candidates,
            selected_is_sole_exact_root,
            allow_repair,
        )
        .await?
        .map_or_else(
            || storage::default_profile_sharded_layout(project_root, &profile_root),
            Ok,
        )
    }

    async fn choose_identity_layout(
        project_root: &Path,
        selected: Option<StoreLayout>,
        candidates: Vec<StoreLayout>,
        selected_is_sole_exact_root: bool,
        allow_repair: bool,
    ) -> Result<Option<StoreLayout>> {
        // With no competing candidate the selected layout wins regardless, so
        // skip the inventory rather than opening the store to confirm it.
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
        let command =
            consolidation_dry_run_command(project_root, &candidate_inventory, &selected_inventory);
        Err(identity_cutover_conflict(
            project_root,
            &selected_inventory,
            &candidate_inventory,
            &format!("run the offline dry-run `{command}` before changing the marker"),
        ))
    }

    /// Opens an existing `TraceDecay` project at the given root.
    ///
    /// If branch metadata exists, resolves the current git branch, auto-adds
    /// it to branch tracking when needed, and opens the corresponding DB.
    /// Falls back to the nearest tracked ancestor DB with a warning only when
    /// the live branch cannot be auto-tracked, such as detached HEAD.
    /// If the previous operation was interrupted (dirty sentinel exists),
    /// the database is integrity-checked before any writable open.
    pub async fn open(project_root: &Path) -> Result<Self> {
        Self::open_with_options(project_root, TraceDecayOpenOptions::default()).await
    }

    pub async fn open_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        Self::open_with_options_inner(project_root, open_options, true).await
    }

    async fn open_with_options_inner(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        allow_corrupt_branch_repair: bool,
    ) -> Result<Self> {
        let store_layout =
            Self::resolve_store_layout_for_project(project_root, &open_options).await?;
        let config = load_config_from_path(project_root, &store_layout.config_path)?;
        let active_branch = branch::current_branch(project_root);
        Self::auto_track_active_branch(
            project_root,
            &store_layout.data_root,
            active_branch.as_deref(),
            open_options.clone(),
        )
        .await?;

        let (db_path, serving_branch, fallback_warning) = Self::resolve_db_for_branch(
            project_root,
            &store_layout.data_root,
            active_branch.as_deref(),
        );

        // Sync state belongs to the concrete graph DB, not the repository-wide
        // store root. Different tracked branches have independent databases
        // and must never clear or inherit one another's dirty marker or lock.
        let active_graph_layout = active_graph_layout(&db_path);
        let repair_corrupt_branch = allow_corrupt_branch_repair
            && active_branch.is_some()
            && active_branch == serving_branch
            && db_path != store_layout.graph_db_path
            && db_path.parent() == Some(store_layout.data_root.join("branches").as_path());

        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "no TraceDecay database found at '{}'; run 'tracedecay init' first",
                    db_path.display()
                ),
            });
        }

        // If the dirty sentinel exists, a previous sync/index was interrupted.
        // Check integrity and rebuild if necessary.
        let crashed = has_dirty_sentinel_at(&active_graph_layout.dirty_path)
            || has_dirty_sentinel_at(&store_layout.dirty_path);
        if crashed {
            eprintln!(
                "[tracedecay] previous operation was interrupted — checking database integrity…"
            );
        }

        // A dirty marker can also describe a sync that is still active in a
        // peer process. Recovery must own both graph-local and legacy locks so
        // it cannot race that writer or clear its sentinel. Preflight through
        // the read-only connection before Database::open applies writable
        // pragmas or migrations to a potentially damaged recovery set.
        let mut recovery_lock = if crashed {
            Some(try_acquire_graph_sync_locks(
                &active_graph_layout.sync_lock_path,
                &store_layout.sync_lock_path,
            )?)
        } else {
            None
        };
        if crashed {
            let authority = DatabaseAuthority::for_runtime(&db_path, "crash verification")?;
            let verification = match Database::open_read_only(&db_path, &authority).await {
                Ok((db, _)) => db,
                Err(error) => {
                    drop(recovery_lock);
                    return Self::recover_corrupt_branch_or_fail(
                        project_root,
                        open_options,
                        &store_layout,
                        &db_path,
                        error,
                        repair_corrupt_branch,
                    )
                    .await;
                }
            };
            let integrity = verification.quick_check().await;
            verification.close();
            match integrity {
                Ok(true) => {}
                Ok(false) => {
                    drop(recovery_lock);
                    return Self::recover_corrupt_branch_or_fail(
                        project_root,
                        open_options,
                        &store_layout,
                        &db_path,
                        "read-only SQLite quick_check did not return ok",
                        repair_corrupt_branch,
                    )
                    .await;
                }
                Err(error) => {
                    drop(recovery_lock);
                    return Self::recover_corrupt_branch_or_fail(
                        project_root,
                        open_options,
                        &store_layout,
                        &db_path,
                        error,
                        repair_corrupt_branch,
                    )
                    .await;
                }
            }
        }

        // Ordinary opens never replace database files. A daemon or another MCP
        // process may still hold the current DB/WAL/SHM inodes, and deleting
        // them here would split readers and writers across different stores.
        let authority = DatabaseAuthority::for_runtime(&db_path, "open project store")?;
        let open_result = Database::open(&db_path, &authority).await;
        let (db, migrated) = match open_result {
            Ok(pair) => pair,
            Err(e) if Database::is_corruption_error(&e) || crashed => {
                drop(recovery_lock);
                return Self::recover_corrupt_branch_or_fail(
                    project_root,
                    open_options,
                    &store_layout,
                    &db_path,
                    e,
                    repair_corrupt_branch,
                )
                .await;
            }
            Err(e) => return Err(e),
        };
        let reindex_pending = db.get_metadata(FULL_REINDEX_REQUIRED_KEY).await?.as_deref()
            == Some(FULL_REINDEX_REQUIRED_VALUE);
        let needs_reindex = migrated || reindex_pending;

        // If the sentinel was set but the database opened successfully, run a
        // quick integrity check.
        if crashed {
            match db.quick_check().await {
                Ok(true) => {
                    if !needs_reindex {
                        clear_dirty_sentinel_at(&active_graph_layout.dirty_path);
                        clear_dirty_sentinel_at(&store_layout.dirty_path);
                    }
                }
                Ok(false) => {
                    db.close();
                    drop(recovery_lock);
                    return Self::recover_corrupt_branch_or_fail(
                        project_root,
                        open_options,
                        &store_layout,
                        &db_path,
                        "SQLite quick_check did not return ok",
                        repair_corrupt_branch,
                    )
                    .await;
                }
                Err(e) => {
                    db.close();
                    drop(recovery_lock);
                    return Self::recover_corrupt_branch_or_fail(
                        project_root,
                        open_options,
                        &store_layout,
                        &db_path,
                        e,
                        repair_corrupt_branch,
                    )
                    .await;
                }
            }
        }

        let ts = Self {
            db,
            config,
            project_root: project_root.to_path_buf(),
            store_layout,
            active_graph_layout,
            open_options,
            registry: LanguageRegistry::new(),
            active_branch,
            serving_branch,
            fallback_warning,
            read_only: false,
        };

        if needs_reindex {
            eprintln!("[tracedecay] schema re-index required — performing full re-index…");
            let on_file = |current, total, file: &str| {
                eprintln!("[tracedecay] re-indexing [{current}/{total}] {file}");
            };
            match recovery_lock.take() {
                Some(lock) => {
                    ts.index_all_with_progress_holding_lock(on_file, lock)
                        .await?
                }
                None => ts.index_all_with_progress(on_file).await?,
            };
            ts.db.set_metadata(FULL_REINDEX_REQUIRED_KEY, "0").await?;
            eprintln!("[tracedecay] re-index complete.");
        }
        drop(recovery_lock);

        ts.register_project_store_in_global_registry().await;
        Ok(ts)
    }

    async fn recover_corrupt_branch_or_fail(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
        store_layout: &StoreLayout,
        db_path: &Path,
        detail: impl std::fmt::Display,
        repair_corrupt_branch: bool,
    ) -> Result<Self> {
        let detail = detail.to_string();
        if repair_corrupt_branch {
            let active_graph_layout = active_graph_layout(db_path);
            let repair_result = (|| {
                let _sync_locks = try_acquire_graph_sync_locks(
                    &active_graph_layout.sync_lock_path,
                    &store_layout.sync_lock_path,
                )?;
                let _authority =
                    DatabaseAuthority::for_runtime(db_path, "preserve corrupt branch store")?;
                preserve_corrupt_branch_store(store_layout, db_path)
            })();

            match repair_result {
                Ok(recovery_dir) => {
                    eprintln!(
                        "[tracedecay] corrupt derived branch index preserved at '{}' — rebuilding from a healthy tracked ancestor",
                        recovery_dir.display()
                    );
                    return Box::pin(Self::open_with_options_inner(
                        project_root,
                        open_options,
                        false,
                    ))
                    .await;
                }
                Err(repair_error) => {
                    print_corruption_warning(db_path);
                    return Err(recovery_required_error(
                        db_path,
                        format!("{detail}; automatic derived-branch repair failed: {repair_error}"),
                    ));
                }
            }
        }

        print_corruption_warning(db_path);
        Err(recovery_required_error(db_path, detail))
    }

    /// Opens an existing project for read-only inspection.
    ///
    /// Unlike [`Self::open`], this does not run migrations, repair dirty
    /// sentinels, clear markers, or rewrite corrupted DBs. It is intended for
    /// status/verification commands that must be able to inspect read-only
    /// stores without mutating them.
    pub async fn open_read_only(project_root: &Path) -> Result<Self> {
        Self::open_read_only_with_options(project_root, TraceDecayOpenOptions::default()).await
    }

    pub async fn open_read_only_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        let store_layout =
            Self::resolve_store_layout_for_project_read_only(project_root, &open_options).await?;
        let config = load_config_from_path(project_root, &store_layout.config_path)?;
        let active_branch = branch::current_branch(project_root);

        let (db_path, serving_branch, fallback_warning) = Self::resolve_db_for_branch(
            project_root,
            &store_layout.data_root,
            active_branch.as_deref(),
        );
        let active_graph_layout = active_graph_layout(&db_path);

        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "no TraceDecay database found at '{}'; run 'tracedecay init' first",
                    db_path.display()
                ),
            });
        }

        let authority = DatabaseAuthority::for_runtime(&db_path, "open project store read-only")?;
        let (db, _) = Database::open_read_only(&db_path, &authority).await?;
        Ok(Self {
            db,
            config,
            project_root: project_root.to_path_buf(),
            store_layout,
            active_graph_layout,
            open_options,
            registry: LanguageRegistry::new(),
            active_branch,
            serving_branch,
            fallback_warning,
            read_only: true,
        })
    }

    async fn auto_track_active_branch(
        project_root: &Path,
        tracedecay_dir: &Path,
        active_branch: Option<&str>,
        open_options: TraceDecayOpenOptions,
    ) -> Result<()> {
        let Some(branch_name) = active_branch else {
            return Ok(());
        };
        let _ = Self::add_branch_tracking_in_layout(
            project_root,
            branch_name,
            tracedecay_dir,
            open_options,
        )
        .await?;
        Ok(())
    }

    /// Silently bootstraps/maintains tracedecay branch tracking for `branch_name`.
    ///
    /// This is the library-level core shared with the `tracedecay branch add`
    /// CLI command and hook integrations. It loads or bootstraps branch
    /// metadata, no-ops when the branch is already tracked, otherwise copies
    /// the nearest tracked ancestor's DB and runs an incremental sync against
    /// the new branch DB.
    pub async fn add_branch_tracking(
        project_root: &Path,
        branch_name: &str,
    ) -> Result<branch::BranchAddOutcome> {
        Self::add_branch_tracking_with_options(
            project_root,
            branch_name,
            TraceDecayOpenOptions::default(),
        )
        .await
    }

    pub async fn add_branch_tracking_with_options(
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
    ) -> Result<branch::BranchAddOutcome> {
        let store_layout = match Self::resolve_store_layout_for_project(project_root, &open_options)
            .await
        {
            Ok(layout) => layout,
            Err(TraceDecayError::Config { .. }) => return Ok(branch::BranchAddOutcome::NotIndexed),
            Err(err) => return Err(err),
        };

        if !store_layout.graph_db_path.is_file() {
            return Ok(branch::BranchAddOutcome::NotIndexed);
        }

        // Branch preparation copies a live SQLite store and rewrites metadata;
        // reject non-daemon callers before either filesystem mutation occurs.
        let _authority =
            DatabaseAuthority::for_runtime(&store_layout.graph_db_path, "add branch tracking")?;
        Self::add_branch_tracking_in_layout(
            project_root,
            branch_name,
            &store_layout.data_root,
            open_options,
        )
        .await
    }

    async fn add_branch_tracking_in_layout(
        project_root: &Path,
        branch_name: &str,
        tracedecay_dir: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<branch::BranchAddOutcome> {
        let prepared =
            branch::prepare_branch_tracking_in_layout(project_root, branch_name, tracedecay_dir)
                .await?;
        let branch::BranchTrackingPreparation::Added(prepared) = prepared else {
            return Ok(match prepared {
                branch::BranchTrackingPreparation::AlreadyTracked => {
                    branch::BranchAddOutcome::AlreadyTracked
                }
                branch::BranchTrackingPreparation::Deferred => branch::BranchAddOutcome::Deferred,
                branch::BranchTrackingPreparation::Added(_) => unreachable!(),
            });
        };

        let sync_result =
            Self::sync_new_branch_with_retries(project_root, branch_name, open_options).await;
        if let Err(TraceDecayError::SyncLock { .. }) = sync_result {
            return Ok(branch::BranchAddOutcome::Deferred);
        } else if let Err(e) = sync_result {
            branch::rollback_prepared_branch_tracking(tracedecay_dir, &prepared);
            return Err(e);
        }

        branch::finalize_prepared_branch_tracking(tracedecay_dir, &prepared);
        Ok(branch::BranchAddOutcome::Added)
    }

    async fn sync_new_branch_with_retries(
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
    ) -> Result<()> {
        let mut attempts = 0;
        loop {
            let cg =
                Self::open_branch_with_options(project_root, branch_name, open_options.clone())
                    .await?;
            match cg.sync().await {
                Ok(_) => return Ok(()),
                Err(TraceDecayError::SyncLock { .. }) if attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Resolves which DB file to open for a given branch.
    ///
    /// Returns `(db_path, serving_branch, fallback_warning)`.
    /// `serving_branch` is the branch whose DB is actually opened.
    /// The warning is `Some` when falling back to an ancestor branch's DB.
    pub(crate) fn resolve_db_for_branch(
        project_root: &Path,
        tracedecay_dir: &Path,
        branch: Option<&str>,
    ) -> (PathBuf, Option<String>, Option<String>) {
        let default_db = tracedecay_dir.join(db_filename(tracedecay_dir));

        let Some(meta) = branch_meta::load_branch_meta(tracedecay_dir) else {
            // No branch metadata — single-DB mode (backward compat)
            return (default_db, None, None);
        };

        let Some(branch) = branch else {
            // Detached HEAD — use default branch DB
            return (
                default_db,
                Some(meta.default_branch.clone()),
                Some("detached HEAD — using default branch index".to_string()),
            );
        };

        // Exact match: branch is tracked
        if let Some(path) = branch::resolve_branch_db_path(tracedecay_dir, branch, &meta) {
            if path.exists() {
                return (path, Some(branch.to_string()), None);
            }
        }

        // Fallback: find nearest tracked ancestor
        if let Some(ancestor) = branch::find_nearest_tracked_ancestor(project_root, branch, &meta) {
            if let Some(path) = branch::resolve_branch_db_path(tracedecay_dir, &ancestor, &meta) {
                if path.exists() {
                    return (
                        path,
                        Some(ancestor.clone()),
                        Some(format!(
                            "branch '{branch}' is not tracked — serving from '{ancestor}'. \
                             Run `tracedecay branch add {branch}` to track it."
                        )),
                    );
                }
            }
        }

        // Last resort: default branch DB
        let serving = meta.default_branch.clone();
        (
            default_db,
            Some(serving),
            Some(format!(
                "branch '{branch}' is not tracked — serving from '{}'. \
                 Run `tracedecay branch add {branch}` to track it.",
                meta.default_branch
            )),
        )
    }

    /// Opens a specific branch's DB.
    ///
    /// Returns an error if the branch is not tracked or the DB doesn't exist.
    pub async fn open_branch(project_root: &Path, branch_name: &str) -> Result<Self> {
        Self::open_branch_with_options(project_root, branch_name, TraceDecayOpenOptions::default())
            .await
    }

    pub async fn open_branch_with_options(
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
    ) -> Result<Self> {
        let store_layout =
            Self::resolve_store_layout_for_project(project_root, &open_options).await?;
        let config = load_config_from_path(project_root, &store_layout.config_path)?;

        let meta = branch_meta::load_branch_meta(&store_layout.data_root).ok_or_else(|| {
            TraceDecayError::Config {
                message: "no branch tracking configured — run `tracedecay branch add` first"
                    .to_string(),
            }
        })?;

        let db_path = branch::resolve_branch_db_path(&store_layout.data_root, branch_name, &meta)
            .ok_or_else(|| TraceDecayError::Config {
            message: format!("branch '{branch_name}' is not tracked"),
        })?;
        let active_graph_layout = active_graph_layout(&db_path);

        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "DB for branch '{branch_name}' not found at '{}'",
                    db_path.display()
                ),
            });
        }

        let authority = DatabaseAuthority::for_runtime(&db_path, "open branch store")?;
        let (db, _) = Database::open(&db_path, &authority).await?;
        Ok(Self {
            db,
            config,
            project_root: project_root.to_path_buf(),
            store_layout,
            active_graph_layout,
            open_options,
            registry: LanguageRegistry::new(),
            active_branch: Some(branch_name.to_string()),
            serving_branch: Some(branch_name.to_string()),
            fallback_warning: None,
            read_only: false,
        })
    }

    /// Lists tracked branches from metadata. Returns `None` if no branch tracking.
    pub fn list_tracked_branches(project_root: &Path) -> Option<Vec<String>> {
        let store_layout = storage::resolve_layout_for_current_profile(project_root).ok()?;
        let meta = branch_meta::load_branch_meta(&store_layout.data_root)?;
        Some(meta.branches.keys().cloned().collect())
    }

    pub(crate) async fn register_project_store_in_global_registry(&self) {
        static REGISTRY_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

        if self.store_layout.storage_mode != storage::StorageMode::ProfileSharded {
            return;
        }

        let Some(project_id) = self.store_layout.identity.project_id.as_deref() else {
            return;
        };
        let Some(profile_root) = profile_root_for_layout(&self.store_layout) else {
            return;
        };
        let Some(store_relpath) = profile_relative(&profile_root, &self.store_layout.data_root)
        else {
            return;
        };

        let _registry_write = REGISTRY_WRITE_LOCK.lock().await;

        let Some(global_db) = self.open_options.open_global_db().await else {
            return;
        };

        let meta = branch_meta::load_branch_meta(&self.store_layout.data_root);
        let default_branch = meta.as_ref().map(|meta| meta.default_branch.as_str());
        let isolated_detached = crate::worktree::is_detached_linked_worktree(&self.project_root);
        let git_common_dir = (!isolated_detached)
            .then(|| crate::worktree::git_common_dir(&self.project_root))
            .flatten();
        let git_remote_url = (!isolated_detached)
            .then(|| git_remote_url(&self.project_root))
            .flatten();

        // A shared project id can be reached from any linked worktree (see
        // the git-common-dir alias registered below), so registering
        // straight from `self.project_root` would let whichever worktree
        // happens to touch the project last pin its canonical_root /
        // display_root to a transient worktree path. Redirect registration
        // to the primary checkout when one is detected and still exists.
        let primary_root = (!isolated_detached)
            .then(|| {
                crate::project_registry::primary_checkout_root(
                    &self.project_root,
                    git_common_dir.as_deref(),
                )
            })
            .flatten();
        let previous_canonical_root = if primary_root.is_some() {
            global_db
                .get_code_project(project_id)
                .await
                .map(|record| record.canonical_root)
        } else {
            None
        };
        let registration_root = primary_root.as_deref().unwrap_or(&self.project_root);

        let Some(project) = global_db
            .upsert_code_project(
                project_id,
                registration_root,
                git_common_dir.as_deref(),
                git_remote_url.as_deref(),
                default_branch,
            )
            .await
        else {
            return;
        };

        if let Err(error) =
            storage::write_repository_identity_marker(&self.project_root, &project.project_id)
        {
            eprintln!(
                "warning: could not persist TraceDecay repository identity for '{}': {error}",
                self.project_root.display()
            );
        }

        if let Some(primary_root) = primary_root.as_deref() {
            // The registry now points canonical_root/display_root at the
            // primary checkout; keep this worktree itself resolvable for
            // future lookups by registering its own path as an alias.
            let _ = global_db
                .upsert_project_alias(&self.project_root, &project.project_id)
                .await;

            let repaired_stale_worktree_root = previous_canonical_root.is_some_and(|previous| {
                previous != crate::global_db::GlobalDb::canonical_project_key(primary_root)
            });
            if repaired_stale_worktree_root {
                eprintln!(
                    "warning: repaired tracedecay project '{project_id}' canonical_root — \
                     it was pinned to a linked worktree ({}); restored to the primary checkout ({})",
                    self.project_root.display(),
                    primary_root.display()
                );
            }
        }

        let store_id = profile_store_id(&project.project_id);
        let manifest_relpath = self
            .store_layout
            .manifest_path
            .as_ref()
            .and_then(|path| profile_relative(&profile_root, path));
        let now = current_timestamp();
        let Some(store) = global_db
            .upsert_store_instance(StoreInstanceUpsert {
                store_id,
                project_id: project.project_id,
                store_kind: "code_project".to_string(),
                storage_mode: "profile_sharded".to_string(),
                store_relpath,
                manifest_relpath,
                last_verified_at: Some(now),
                last_write_at: Some(now),
            })
            .await
        else {
            return;
        };

        if let Some(meta) = meta {
            for (branch_name, entry) in meta.branches {
                let db_path = self.store_layout.data_root.join(&entry.db_file);
                let Some(db_relpath) = profile_relative(&profile_root, &db_path) else {
                    continue;
                };
                let _ = global_db
                    .upsert_graph_scope(GraphScopeUpsert {
                        graph_scope_id: profile_graph_scope_id(&store.store_id, &branch_name),
                        project_id: store.project_id.clone(),
                        store_id: store.store_id.clone(),
                        branch_name: branch_name.clone(),
                        db_relpath,
                        parent_scope_id: entry
                            .parent
                            .as_deref()
                            .map(|parent| profile_graph_scope_id(&store.store_id, parent)),
                        last_synced_at: entry.last_synced_at.parse::<i64>().ok(),
                        writable: true,
                    })
                    .await;
            }
        }

        let mut artifacts = Vec::new();
        push_existing_store_artifact(
            &mut artifacts,
            &store.store_id,
            "graph_db",
            &profile_root,
            &self.store_layout.graph_db_path,
            None,
            now,
        );
        push_existing_store_artifact(
            &mut artifacts,
            &store.store_id,
            "sessions_db",
            &profile_root,
            &self.store_layout.sessions_db_path,
            None,
            now,
        );
        push_existing_store_artifact(
            &mut artifacts,
            &store.store_id,
            "branch_meta",
            &profile_root,
            &self.store_layout.branch_meta_path,
            None,
            now,
        );
        if let Some(manifest_path) = &self.store_layout.manifest_path {
            push_existing_store_artifact(
                &mut artifacts,
                &store.store_id,
                "store_manifest",
                &profile_root,
                manifest_path,
                Some(storage::STORE_MANIFEST_SCHEMA_VERSION.to_string()),
                now,
            );
        }
        for artifact in artifacts {
            let _ = global_db.upsert_store_artifact(artifact).await;
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
        Self::resolve_store_layout_with_identity_migration(project_root, open_options, false).await
    }
}

fn graph_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = db_path.file_name().unwrap_or_default().to_os_string();
    file_name.push(suffix);
    db_path.with_file_name(file_name)
}

fn preserve_corrupt_branch_store(store_layout: &StoreLayout, db_path: &Path) -> Result<PathBuf> {
    let db_name = db_path.file_name().ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "cannot preserve corrupt branch store with no filename: '{}'",
            db_path.display()
        ),
    })?;
    let recovery_root = store_layout.data_root.join("recovery");
    std::fs::create_dir_all(&recovery_root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create branch recovery directory '{}': {error}",
            recovery_root.display()
        ),
    })?;
    let recovery_dir = recovery_root.join(format!(
        "{}-{}-{}",
        db_name.to_string_lossy(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id()
    ));
    std::fs::create_dir(&recovery_dir).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create branch recovery set '{}': {error}",
            recovery_dir.display()
        ),
    })?;

    let db_wal = graph_sidecar_path(db_path, "-wal");
    let db_shm = graph_sidecar_path(db_path, "-shm");
    let db_dirty = graph_sidecar_path(db_path, ".dirty");
    let sources = [&db_wal, &db_shm, &db_dirty, db_path];
    let mut preserved_db = false;
    let mut preserved = Vec::new();
    for source in sources {
        let metadata = match std::fs::symlink_metadata(source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "failed to inspect recovery-set member '{}': {error}",
                        source.display()
                    ),
                });
            }
        };
        if !metadata.file_type().is_file() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "refusing to preserve non-regular recovery-set member '{}'",
                    source.display()
                ),
            });
        }
        let target = recovery_dir.join(source.file_name().unwrap_or_default());
        let copied = std::fs::copy(source, &target).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to preserve recovery-set member '{}' at '{}': {error}",
                source.display(),
                target.display()
            ),
        })?;
        if copied != metadata.len() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "incomplete recovery-set copy for '{}': copied {copied} of {} bytes",
                    source.display(),
                    metadata.len()
                ),
            });
        }
        // Windows `FlushFileBuffers` requires a write handle, so a read-only
        // open would fail the durability sync with "access is denied".
        std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .and_then(|file| file.sync_all())
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to sync preserved recovery-set member '{}': {error}",
                    target.display()
                ),
            })?;
        preserved_db |= source == db_path;
        preserved.push(source.to_path_buf());
    }
    if !preserved_db {
        return Err(TraceDecayError::Config {
            message: format!(
                "corrupt branch database '{}' disappeared",
                db_path.display()
            ),
        });
    }
    #[cfg(unix)]
    for directory in [&recovery_dir, &recovery_root, &store_layout.data_root] {
        std::fs::File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to sync preserved recovery-set directory '{}': {error}",
                    directory.display()
                ),
            })?;
    }

    // Delete the source database last. If any sidecar deletion fails, the
    // corrupt DB remains in place and the complete copied recovery set is
    // still available for a later retry or offline salvage.
    for source in preserved {
        std::fs::remove_file(&source).map_err(|error| TraceDecayError::Config {
            message: format!(
                "preserved recovery set at '{}', but failed to retire '{}': {error}",
                recovery_dir.display(),
                source.display()
            ),
        })?;
    }
    Ok(recovery_dir)
}

fn active_graph_layout(db_path: &Path) -> super::ActiveGraphLayout {
    super::ActiveGraphLayout {
        dirty_path: graph_sidecar_path(db_path, ".dirty"),
        sync_lock_path: graph_sidecar_path(db_path, ".sync.lock"),
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
    let authority = DatabaseAuthority::for_runtime(&layout.graph_db_path, "store inventory");
    let open_result = match authority {
        Ok(authority) => Database::open_read_only(&layout.graph_db_path, &authority).await,
        Err(error) => Err(error),
    };
    let (graph_health, nodes, files, facts) = match open_result {
        Ok((db, _)) => {
            if let Ok(stats) = db.get_stats().await {
                let facts = count_rows(db.conn(), "memory_facts").await;
                db.close();
                ("healthy", stats.node_count, stats.file_count, facts)
            } else {
                db.close();
                ("corrupt", 0, 0, 0)
            }
        }
        Err(_) if layout.graph_db_path.exists() => ("corrupt", 0, 0, 0),
        Err(_) => ("missing", 0, 0, 0),
    };

    let (sessions, messages, lcm_rows) = if let Some(db) =
        crate::global_db::GlobalDb::open_read_only_at(&layout.sessions_db_path).await
    {
        let conn = db.dashboard_connection();
        let counts = (
            count_rows(&conn, "sessions").await,
            count_rows(&conn, "session_messages").await,
            count_rows(&conn, "lcm_raw_messages").await
                + count_rows(&conn, "lcm_summary_nodes").await,
        );
        db.close();
        counts
    } else {
        (0, 0, 0)
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

async fn count_rows(connection: &libsql::Connection, table: &str) -> u64 {
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

fn consolidation_dry_run_command(
    project_root: &Path,
    source: &StoreIdentityInventory,
    target: &StoreIdentityInventory,
) -> String {
    format!(
        "tracedecay migrate consolidate --project {} --source-project-id {} --target-project-id {}",
        shell_quote(&project_root.to_string_lossy()),
        source.project_id,
        target.project_id,
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn profile_relative(profile_root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(profile_root)
        .ok()
        .map(|rel| rel.to_string_lossy().to_string())
}

fn profile_root_for_layout(layout: &StoreLayout) -> Option<PathBuf> {
    layout.data_root.parent()?.parent().map(Path::to_path_buf)
}

fn profile_store_id(project_id: &str) -> String {
    format!("store:{project_id}:profile_sharded")
}

pub(crate) fn git_remote_url(project_root: &Path) -> Option<String> {
    // gix reads the same config `git config --get` would (repo-local +
    // global) without a subprocess spawn.
    if let Ok(repo) = gix::discover(project_root) {
        let url = repo
            .config_snapshot()
            .string("remote.origin.url")?
            .to_string();
        let url = url.trim();
        return (!url.is_empty()).then(|| url.to_string());
    }
    if !crate::worktree::git_may_resolve_repo(project_root) {
        return None;
    }
    crate::git::git_capture(project_root, &["config", "--get", "remote.origin.url"])
}

fn profile_graph_scope_id(store_id: &str, branch_name: &str) -> String {
    format!("{store_id}:branch:{branch_name}")
}

fn push_existing_store_artifact(
    artifacts: &mut Vec<StoreArtifactUpsert>,
    store_id: &str,
    artifact_kind: &str,
    profile_root: &Path,
    path: &Path,
    schema_version: Option<String>,
    updated_at: i64,
) {
    let Some(relpath) = profile_relative(profile_root, path) else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    artifacts.push(StoreArtifactUpsert {
        store_id: store_id.to_string(),
        artifact_kind: artifact_kind.to_string(),
        relpath,
        size_bytes: i64::try_from(metadata.len()).ok(),
        schema_version,
        updated_at: Some(updated_at),
    });
}

/// Deletes the database and its WAL/SHM sidecars.
fn delete_db_files(db_path: &std::path::Path) {
    let _ = std::fs::remove_file(db_path);
    // WAL and SHM files use the same base name with different extensions
    let mut wal = db_path.to_path_buf();
    wal.set_extension("db-wal");
    let _ = std::fs::remove_file(&wal);
    wal.set_extension("db-shm");
    let _ = std::fs::remove_file(&wal);
}

/// Build an actionable error without replacing any member of the `SQLite`
/// recovery set.
fn recovery_required_error(
    db_path: &std::path::Path,
    detail: impl std::fmt::Display,
) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!(
            "database recovery required at '{}'; DB/WAL/SHM and dirty sentinel were preserved: {detail}",
            db_path.display()
        ),
        operation: "open_recovery_required".to_string(),
    }
}

fn print_corruption_warning(db_path: &std::path::Path) {
    let version = env!("CARGO_PKG_VERSION");
    eprintln!("[tracedecay] \x1b[33m⚠ database recovery required — store preserved\x1b[0m");
    eprintln!("[tracedecay]");
    eprintln!("[tracedecay] Store: {}", db_path.display());
    eprintln!("[tracedecay] Stop TraceDecay daemon/MCP processes before explicit repair.");
    eprintln!("[tracedecay] Preserve the DB, WAL, SHM, and dirty sentinel as one recovery set.");
    eprintln!("[tracedecay] Run `tracedecay doctor` from the project root for exact paths.");
    eprintln!("[tracedecay] Please report this at:");
    eprintln!("[tracedecay]   https://github.com/ScriptedAlchemy/tracedecay/issues");
    eprintln!(
        "[tracedecay]   Include: tracedecay version (v{version}), OS, and what happened before the crash."
    );
    eprintln!("[tracedecay]");
}
