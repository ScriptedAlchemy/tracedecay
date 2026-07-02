//! Lifecycle: init/open/branch-tracking entry points plus the profile-store
//! registration helpers they rely on.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::branch;
use crate::branch_meta::{self, BranchMeta};
use crate::config::{db_filename, load_config_from_path, save_config_to_path, TraceDecayConfig};
use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::extraction::LanguageRegistry;
use crate::global_db::{GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert};
use crate::storage::{self, StoreLayout};

use super::locking::{clear_dirty_sentinel_at, has_dirty_sentinel_at};
use super::{current_timestamp, TraceDecay, TraceDecayOpenOptions};

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
        let config = TraceDecayConfig {
            root_dir: project_root.to_string_lossy().to_string(),
            ..TraceDecayConfig::default()
        };
        save_config_to_path(&store_layout.config_path, &config)?;

        let (db, _migrated) = Database::initialize(&store_layout.graph_db_path).await?;
        if store_layout.storage_mode == storage::StorageMode::ProfileSharded {
            storage::write_store_manifest(&store_layout)?;
        }

        // Bootstrap branch metadata if we can detect a default branch
        let active_branch = branch::current_branch(project_root);
        let default_branch =
            branch::detect_default_branch(project_root).or_else(|| active_branch.clone());
        if let Some(ref default) = default_branch {
            let meta = BranchMeta::new_for_dir(&store_layout.data_root, default);
            let _ = branch_meta::save_branch_meta(&store_layout.data_root, &meta);
        }

        let ts = Self {
            db,
            config,
            project_root: project_root.to_path_buf(),
            store_layout,
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
        let (db, _) = Database::initialize(&db_path).await?;
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
        let profile_root = open_options.resolved_profile_root()?;
        if storage::read_enrollment_marker(project_root)?.is_some() {
            return storage::resolve_layout(project_root, &profile_root);
        }

        let git_common_dir = crate::worktree::git_common_dir(project_root);
        let git_remote_url = git_remote_url(project_root);
        if let Some(global_db) = open_options.open_global_db().await {
            let resolution = match global_db
                .resolve_project_store_by_identity(project_root, git_common_dir.as_deref())
                .await
            {
                Some(resolution) => Some(resolution),
                None => match git_remote_url.as_deref() {
                    Some(remote) => {
                        global_db
                            .resolve_unique_project_store_by_git_remote(remote)
                            .await
                    }
                    None => None,
                },
            };

            if let Some(resolution) = resolution {
                return storage::profile_sharded_layout(
                    project_root,
                    &profile_root,
                    &storage::EnrollmentMarker {
                        project_id: resolution.project.project_id,
                        storage_mode: storage::StorageMode::ProfileSharded,
                    },
                );
            }
        }

        storage::default_profile_sharded_layout(project_root, &profile_root)
    }

    /// Opens an existing `TraceDecay` project at the given root.
    ///
    /// If branch metadata exists, resolves the current git branch, auto-adds
    /// it to branch tracking when needed, and opens the corresponding DB.
    /// Falls back to the nearest tracked ancestor DB with a warning only when
    /// the live branch cannot be auto-tracked, such as detached HEAD.
    /// If the previous operation was interrupted (dirty sentinel exists),
    /// the database is integrity-checked and rebuilt if corrupted.
    pub async fn open(project_root: &Path) -> Result<Self> {
        Self::open_with_options(project_root, TraceDecayOpenOptions::default()).await
    }

    pub async fn open_with_options(
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
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
        let crashed = has_dirty_sentinel_at(&store_layout.dirty_path);
        if crashed {
            eprintln!(
                "[tracedecay] previous operation was interrupted — checking database integrity…"
            );
        }

        // Try to open; if the database is completely unreadable, delete and
        // re-initialize rather than failing permanently.
        let open_result = Database::open(&db_path).await;
        let (db, migrated) = match open_result {
            Ok(pair) => pair,
            Err(ref e) if Database::is_corruption_error(e) || crashed => {
                print_corruption_warning();
                delete_db_files(&db_path);
                clear_dirty_sentinel_at(&store_layout.dirty_path);
                let (db, _) = Database::initialize(&db_path).await?;
                let ts = Self {
                    db,
                    config,
                    project_root: project_root.to_path_buf(),
                    store_layout: store_layout.clone(),
                    open_options: open_options.clone(),
                    registry: LanguageRegistry::new(),
                    active_branch: active_branch.clone(),
                    serving_branch: serving_branch.clone(),
                    fallback_warning: fallback_warning.clone(),
                    read_only: false,
                };
                ts.index_all_with_progress(|c, t, f| {
                    eprintln!("[tracedecay] re-indexing [{c}/{t}] {f}");
                })
                .await?;
                eprintln!("[tracedecay] re-index complete.");
                ts.register_project_store_in_global_registry().await;
                return Ok(ts);
            }
            Err(e) => return Err(e),
        };

        // If the sentinel was set but the database opened successfully, run a
        // quick integrity check.
        if crashed {
            let intact = db.quick_check().await.unwrap_or(false);
            if !intact {
                print_corruption_warning();
                drop(db);
                delete_db_files(&db_path);
                clear_dirty_sentinel_at(&store_layout.dirty_path);
                let (new_db, _) = Database::initialize(&db_path).await?;
                let ts = Self {
                    db: new_db,
                    config,
                    project_root: project_root.to_path_buf(),
                    store_layout: store_layout.clone(),
                    open_options: open_options.clone(),
                    registry: LanguageRegistry::new(),
                    active_branch: active_branch.clone(),
                    serving_branch: serving_branch.clone(),
                    fallback_warning: fallback_warning.clone(),
                    read_only: false,
                };
                ts.index_all_with_progress(|c, t, f| {
                    eprintln!("[tracedecay] re-indexing [{c}/{t}] {f}");
                })
                .await?;
                eprintln!("[tracedecay] re-index complete.");
                ts.register_project_store_in_global_registry().await;
                return Ok(ts);
            }
            // DB is fine — clean up the stale sentinel.
            clear_dirty_sentinel_at(&store_layout.dirty_path);
        }

        let ts = Self {
            db,
            config,
            project_root: project_root.to_path_buf(),
            store_layout,
            open_options,
            registry: LanguageRegistry::new(),
            active_branch,
            serving_branch,
            fallback_warning,
            read_only: false,
        };

        if migrated {
            eprintln!("[tracedecay] schema changed — performing full re-index…");
            ts.index_all_with_progress(|current, total, file| {
                eprintln!("[tracedecay] re-indexing [{current}/{total}] {file}");
            })
            .await?;
            eprintln!("[tracedecay] re-index complete.");
        }

        ts.register_project_store_in_global_registry().await;
        Ok(ts)
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
            Self::resolve_store_layout_for_project(project_root, &open_options).await?;
        let config = load_config_from_path(project_root, &store_layout.config_path)?;
        let active_branch = branch::current_branch(project_root);

        let (db_path, serving_branch, fallback_warning) = Self::resolve_db_for_branch(
            project_root,
            &store_layout.data_root,
            active_branch.as_deref(),
        );

        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "no TraceDecay database found at '{}'; run 'tracedecay init' first",
                    db_path.display()
                ),
            });
        }

        let (db, _) = Database::open_read_only(&db_path).await?;
        Ok(Self {
            db,
            config,
            project_root: project_root.to_path_buf(),
            store_layout,
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
    pub(super) fn resolve_db_for_branch(
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

        if !db_path.exists() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "DB for branch '{branch_name}' not found at '{}'",
                    db_path.display()
                ),
            });
        }

        let (db, _) = Database::open(&db_path).await?;
        Ok(Self {
            db,
            config,
            project_root: project_root.to_path_buf(),
            store_layout,
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

    async fn register_project_store_in_global_registry(&self) {
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
        let git_common_dir = crate::worktree::git_common_dir(&self.project_root);
        let git_remote_url = git_remote_url(&self.project_root);
        let Some(project) = global_db
            .upsert_code_project(
                project_id,
                &self.project_root,
                git_common_dir.as_deref(),
                git_remote_url.as_deref(),
                default_branch,
            )
            .await
        else {
            return;
        };

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
        Self::resolve_store_layout_for_local_identity(project_root, open_options)
            .await
            .ok()
            .filter(|layout| layout.graph_db_path.is_file())
    }

    async fn resolve_store_layout_for_local_identity(
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<StoreLayout> {
        let profile_root = open_options.resolved_profile_root()?;
        if storage::read_enrollment_marker(project_root)?.is_some() {
            return storage::resolve_layout(project_root, &profile_root);
        }

        let git_common_dir = crate::worktree::git_common_dir(project_root);
        if let Some(global_db) = open_options.open_global_db().await {
            if let Some(resolution) = global_db
                .resolve_project_store_by_identity(project_root, git_common_dir.as_deref())
                .await
            {
                return storage::profile_sharded_layout(
                    project_root,
                    &profile_root,
                    &storage::EnrollmentMarker {
                        project_id: resolution.project.project_id,
                        storage_mode: storage::StorageMode::ProfileSharded,
                    },
                );
            }
        }

        storage::default_profile_sharded_layout(project_root, &profile_root)
    }
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

fn git_remote_url(project_root: &Path) -> Option<String> {
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
    git_output(project_root, &["config", "--get", "remote.origin.url"])
}

fn git_output(project_root: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
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

/// Prints a user-facing warning about database corruption with a request to
/// report the issue.
fn print_corruption_warning() {
    let version = env!("CARGO_PKG_VERSION");
    eprintln!("[tracedecay] \x1b[33m⚠ database corruption detected — rebuilding index\x1b[0m");
    eprintln!("[tracedecay]");
    eprintln!("[tracedecay] This was likely caused by a crash or kill during indexing.");
    eprintln!("[tracedecay] Please report this at:");
    eprintln!("[tracedecay]   https://github.com/ScriptedAlchemy/tracedecay/issues");
    eprintln!(
        "[tracedecay]   Include: tracedecay version (v{version}), OS, and what happened before the crash."
    );
    eprintln!("[tracedecay]");
}
