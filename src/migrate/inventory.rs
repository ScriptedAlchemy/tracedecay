use std::collections::HashSet;
use std::path::{Path, PathBuf};

use libsql::{Builder, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::config::{self, db_filename, TRACEDECAY_DIR};
use crate::errors::Result;
use crate::global_db;
use crate::storage::{BRANCH_META_FILENAME, SESSIONS_DB_FILENAME, STORE_MANIFEST_FILENAME};

#[derive(Debug, Clone, Default)]
pub struct MigrationInventoryOptions {
    pub roots: Vec<PathBuf>,
    pub global_db_path: Option<PathBuf>,
    pub follow_symlinks: bool,
    pub include_all_registered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationInventory {
    pub stores: Vec<StoreInventory>,
    pub skipped: Vec<SkippedPath>,
    pub global_db: Option<GlobalDbInventory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreBrand {
    TraceDecay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreRole {
    CodeProjectStore,
    GlobalDbStore,
    DiskOnlyOrphan,
    HermesProfileStore,
    HermesStateDbSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreStatus {
    Ok,
    MissingDb,
    Dirty,
    Locked,
    Corrupt,
    NeedsManualReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryStatus {
    Registered,
    Unregistered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreInventory {
    pub project_root: PathBuf,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub brand: StoreBrand,
    pub role: StoreRole,
    pub registry_status: RegistryStatus,
    pub size_bytes: u64,
    pub statuses: Vec<StoreStatus>,
    pub artifacts: Vec<StoreArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreArtifact {
    pub kind: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedPath {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalDbInventory {
    pub path: PathBuf,
    pub exists: bool,
    pub path_overridden: bool,
    pub accounting_mode: String,
    pub legacy_home_fallback: bool,
    pub project_count: u64,
    pub session_count: u64,
    pub lcm_raw_message_count: u64,
    pub token_cache_present: bool,
    pub registered_project_paths: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

pub async fn build_inventory(options: MigrationInventoryOptions) -> Result<MigrationInventory> {
    let mut stores = Vec::new();
    let mut skipped = Vec::new();
    let mut seen_data_dirs = HashSet::new();
    let explicit_global_db_path = options.global_db_path.is_some();
    let global_db_path = options.global_db_path.or_else(global_db::global_db_path);

    for root in &options.roots {
        scan_root(
            root,
            options.follow_symlinks,
            &mut seen_data_dirs,
            &mut stores,
            &mut skipped,
        )
        .await?;
    }
    let include_default_hermes_home = options.roots.is_empty() && !explicit_global_db_path;
    scan_hermes_sources(
        &options.roots,
        include_default_hermes_home,
        options.follow_symlinks,
        &mut seen_data_dirs,
        &mut stores,
        &mut skipped,
    )
    .await?;

    let global_db = match global_db_path {
        Some(path) => Some(
            inspect_global_db(
                &path,
                explicit_global_db_path || global_db::global_db_path_is_overridden(),
            )
            .await,
        ),
        None => None,
    };
    let registered_project_keys = global_db
        .as_ref()
        .map(|global| canonical_path_set(&global.registered_project_paths))
        .unwrap_or_default();

    let include_registered_roots = options.roots.is_empty() || options.include_all_registered;
    if include_registered_roots {
        if let Some(global) = &global_db {
            for root in &global.registered_project_paths {
                let before = stores.len();
                inspect_data_dir_candidate(
                    root,
                    TRACEDECAY_DIR,
                    options.follow_symlinks,
                    &mut seen_data_dirs,
                    &mut stores,
                    &mut skipped,
                    StoreRole::CodeProjectStore,
                )
                .await?;
                if stores.len() == before {
                    stores.push(missing_registered_store(root));
                }
            }
        }
    }

    mark_registry_status(&mut stores, &registered_project_keys);
    stores.sort_by(|a, b| a.project_root.cmp(&b.project_root));
    skipped.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(MigrationInventory {
        stores,
        skipped,
        global_db,
    })
}

async fn scan_root(
    root: &Path,
    follow_symlinks: bool,
    seen_data_dirs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    skipped: &mut Vec<SkippedPath>,
) -> Result<()> {
    let mut visited = HashSet::new();
    let mut work = vec![root.to_path_buf()];

    while let Some(dir) = work.pop() {
        let visit_key = if follow_symlinks {
            dir.canonicalize().unwrap_or_else(|_| dir.clone())
        } else {
            dir.clone()
        };
        if !visited.insert(visit_key) {
            continue;
        }

        inspect_data_dir_candidate(
            &dir,
            TRACEDECAY_DIR,
            follow_symlinks,
            seen_data_dirs,
            stores,
            skipped,
            StoreRole::CodeProjectStore,
        )
        .await?;

        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_symlink() && !follow_symlinks {
                skipped.push(SkippedPath {
                    path,
                    reason: "symlink".to_string(),
                });
                continue;
            }
            if file_type.is_symlink() {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if !meta.is_dir() {
                    continue;
                }
            } else if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == TRACEDECAY_DIR {
                continue;
            }
            if should_prune_dir(&name) {
                continue;
            }
            work.push(path);
        }
    }

    Ok(())
}

async fn inspect_data_dir_candidate(
    project_root: &Path,
    dir_name: &str,
    follow_symlinks: bool,
    seen_data_dirs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    skipped: &mut Vec<SkippedPath>,
    role: StoreRole,
) -> Result<()> {
    let mut data_dir = project_root.join(dir_name);
    let Ok(meta) = std::fs::symlink_metadata(&data_dir) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        if !follow_symlinks {
            skipped.push(SkippedPath {
                path: data_dir,
                reason: "symlink".to_string(),
            });
            return Ok(());
        }
        if !data_dir.is_dir() {
            return Ok(());
        }
        data_dir = data_dir.canonicalize().unwrap_or(data_dir);
    } else if !meta.is_dir() {
        return Ok(());
    }
    let key = data_dir.canonicalize().unwrap_or_else(|_| data_dir.clone());
    if !seen_data_dirs.insert(key) {
        return Ok(());
    }
    let brand = StoreBrand::TraceDecay;
    let db_path = data_dir.join(db_filename(&data_dir));
    let store = inspect_project_store(
        project_root,
        &data_dir,
        db_path,
        brand,
        role,
        follow_symlinks,
        skipped,
    )
    .await?;
    stores.push(store);
    Ok(())
}

async fn inspect_project_store(
    project_root: &Path,
    data_dir: &Path,
    db_path: PathBuf,
    brand: StoreBrand,
    role: StoreRole,
    follow_symlinks: bool,
    skipped: &mut Vec<SkippedPath>,
) -> Result<StoreInventory> {
    let mut statuses = Vec::new();
    let mut artifacts = Vec::new();

    if db_path.is_file() {
        artifacts.push(StoreArtifact {
            kind: "graph_db".to_string(),
            size_bytes: file_size(&db_path),
            path: db_path.clone(),
        });
        if !sqlite_quick_check(&db_path).await {
            statuses.push(StoreStatus::Corrupt);
        }
    } else {
        statuses.push(StoreStatus::MissingDb);
    }

    record_optional_artifact(
        data_dir,
        "sessions_db",
        SESSIONS_DB_FILENAME,
        &mut artifacts,
    );
    record_optional_artifact(
        data_dir,
        "branch_meta",
        BRANCH_META_FILENAME,
        &mut artifacts,
    );
    record_branch_db_artifacts(
        data_dir,
        follow_symlinks,
        skipped,
        &mut statuses,
        &mut artifacts,
    )
    .await;
    record_optional_artifact(data_dir, "config", "config.json", &mut artifacts);
    record_optional_artifact(
        data_dir,
        "store_manifest",
        STORE_MANIFEST_FILENAME,
        &mut artifacts,
    );
    record_optional_artifact(
        data_dir,
        "response_handles",
        "response-handles",
        &mut artifacts,
    );
    record_optional_artifact(data_dir, "lcm_payloads", "lcm-payloads", &mut artifacts);
    record_optional_artifact(data_dir, "dashboard", "dashboard", &mut artifacts);

    let dirty = data_dir.join("dirty");
    if dirty.exists() {
        statuses.push(StoreStatus::Dirty);
        artifacts.push(StoreArtifact {
            kind: "dirty_sentinel".to_string(),
            size_bytes: file_size(&dirty),
            path: dirty,
        });
    }

    let sync_lock = data_dir.join("sync.lock");
    if sync_lock.exists() {
        statuses.push(StoreStatus::Locked);
        artifacts.push(StoreArtifact {
            kind: "sync_lock".to_string(),
            size_bytes: file_size(&sync_lock),
            path: sync_lock,
        });
    }

    let config_tmp = data_dir.join("config.json.tmp");
    if config_tmp.exists() {
        statuses.push(StoreStatus::NeedsManualReview);
        artifacts.push(StoreArtifact {
            kind: "config_tmp".to_string(),
            size_bytes: file_size(&config_tmp),
            path: config_tmp,
        });
    }

    if statuses.is_empty() {
        statuses.push(StoreStatus::Ok);
    }

    Ok(StoreInventory {
        project_root: project_root.to_path_buf(),
        data_dir: data_dir.to_path_buf(),
        db_path,
        brand,
        role,
        registry_status: RegistryStatus::Unregistered,
        size_bytes: dir_size(data_dir),
        statuses,
        artifacts,
    })
}

async fn scan_hermes_sources(
    roots: &[PathBuf],
    include_default_home: bool,
    follow_symlinks: bool,
    seen_data_dirs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    skipped: &mut Vec<SkippedPath>,
) -> Result<()> {
    let mut seen_profiles = HashSet::new();
    let mut seen_state_dbs = HashSet::new();
    for hermes_home in hermes_home_candidates(roots, include_default_home) {
        inspect_hermes_profile_dir(
            &hermes_home,
            follow_symlinks,
            seen_data_dirs,
            &mut seen_profiles,
            &mut seen_state_dbs,
            stores,
            skipped,
        )
        .await?;

        let profiles_dir = hermes_home.join("profiles");
        let Ok(entries) = std::fs::read_dir(&profiles_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let mut profile_dir = entry.path();
            if file_type.is_symlink() {
                if !follow_symlinks {
                    skipped.push(SkippedPath {
                        path: profile_dir,
                        reason: "symlink".to_string(),
                    });
                    continue;
                }
                if !profile_dir.is_dir() {
                    continue;
                }
                profile_dir = profile_dir.canonicalize().unwrap_or(profile_dir);
            } else if !file_type.is_dir() {
                continue;
            }
            inspect_hermes_profile_dir(
                &profile_dir,
                follow_symlinks,
                seen_data_dirs,
                &mut seen_profiles,
                &mut seen_state_dbs,
                stores,
                skipped,
            )
            .await?;
        }
    }
    Ok(())
}

async fn inspect_hermes_profile_dir(
    profile_dir: &Path,
    follow_symlinks: bool,
    seen_data_dirs: &mut HashSet<PathBuf>,
    seen_profiles: &mut HashSet<PathBuf>,
    seen_state_dbs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
    skipped: &mut Vec<SkippedPath>,
) -> Result<()> {
    if !profile_dir.is_dir() {
        return Ok(());
    }
    let profile_key = canonicalize_lossy(profile_dir);
    if !seen_profiles.insert(profile_key) {
        return Ok(());
    }

    inspect_data_dir_candidate(
        profile_dir,
        TRACEDECAY_DIR,
        follow_symlinks,
        seen_data_dirs,
        stores,
        skipped,
        StoreRole::HermesProfileStore,
    )
    .await?;
    inspect_hermes_state_db(profile_dir, seen_state_dbs, stores).await;

    if let Some(project_root) = read_hermes_project_pin(&profile_dir.join("config.yaml")) {
        inspect_data_dir_candidate(
            &project_root,
            TRACEDECAY_DIR,
            follow_symlinks,
            seen_data_dirs,
            stores,
            skipped,
            StoreRole::CodeProjectStore,
        )
        .await?;
    }

    Ok(())
}

async fn inspect_hermes_state_db(
    profile_dir: &Path,
    seen_state_dbs: &mut HashSet<PathBuf>,
    stores: &mut Vec<StoreInventory>,
) {
    let db_path = profile_dir.join("state.db");
    if !db_path.is_file() {
        return;
    }
    let key = canonicalize_lossy(&db_path);
    if !seen_state_dbs.insert(key) {
        return;
    }
    let mut statuses = Vec::new();
    if !sqlite_quick_check(&db_path).await {
        statuses.push(StoreStatus::Corrupt);
    }
    if statuses.is_empty() {
        statuses.push(StoreStatus::Ok);
    }
    stores.push(StoreInventory {
        project_root: profile_dir.to_path_buf(),
        data_dir: profile_dir.to_path_buf(),
        db_path: db_path.clone(),
        brand: StoreBrand::TraceDecay,
        role: StoreRole::HermesStateDbSource,
        registry_status: RegistryStatus::Unregistered,
        size_bytes: file_size(&db_path),
        statuses,
        artifacts: vec![StoreArtifact {
            kind: "hermes_state_db".to_string(),
            path: db_path.clone(),
            size_bytes: file_size(&db_path),
        }],
    });
}

async fn inspect_global_db(path: &Path, path_overridden: bool) -> GlobalDbInventory {
    let exists = path.is_file();
    let mut project_count = 0;
    let mut session_count = 0;
    let mut lcm_raw_message_count = 0;
    let mut token_cache_present = false;
    let mut registered_project_paths = Vec::new();
    let mut warnings = Vec::new();

    if exists {
        let db_result = Builder::new_local(path)
            .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
            .build()
            .await;
        match db_result {
            Ok(db) => match db.connect() {
                Ok(conn) => {
                    if !sqlite_quick_check(path).await {
                        warnings.push(format!("global DB '{}' failed quick_check", path.display()));
                    }
                    project_count = table_count(&conn, "projects").await;
                    session_count = table_count(&conn, "sessions").await;
                    lcm_raw_message_count = table_count(&conn, "lcm_raw_messages").await;
                    token_cache_present = table_exists(&conn, "dashboard_token_counts").await;
                    registered_project_paths = project_paths(&conn).await;
                }
                Err(err) => warnings.push(format!(
                    "could not inspect global DB '{}': {err}",
                    path.display()
                )),
            },
            Err(err) => warnings.push(format!(
                "could not inspect global DB '{}': {err}",
                path.display()
            )),
        }
    }

    GlobalDbInventory {
        path: path.to_path_buf(),
        exists,
        path_overridden,
        accounting_mode: global_db::global_accounting_mode().as_str().to_string(),
        legacy_home_fallback: false,
        project_count,
        session_count,
        lcm_raw_message_count,
        token_cache_present,
        registered_project_paths,
        warnings,
    }
}

async fn sqlite_quick_check(path: &Path) -> bool {
    let Ok(db) = Builder::new_local(path)
        .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
    else {
        return false;
    };
    let Ok(conn) = db.connect() else {
        return false;
    };
    let Ok(mut rows) = conn.query("PRAGMA quick_check", ()).await else {
        return false;
    };
    let Ok(Some(row)) = rows.next().await else {
        return false;
    };
    row.get::<String>(0).is_ok_and(|value| value == "ok")
}

async fn table_count(conn: &libsql::Connection, table: &str) -> u64 {
    if !table_exists(conn, table).await {
        return 0;
    }
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let Ok(mut rows) = conn.query(&sql, ()).await else {
        return 0;
    };
    let Ok(Some(row)) = rows.next().await else {
        return 0;
    };
    row.get::<i64>(0).unwrap_or(0).max(0) as u64
}

async fn table_exists(conn: &libsql::Connection, table: &str) -> bool {
    let Ok(mut rows) = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            libsql::params![table],
        )
        .await
    else {
        return false;
    };
    matches!(rows.next().await, Ok(Some(_)))
}

async fn project_paths(conn: &libsql::Connection) -> Vec<PathBuf> {
    if !table_exists(conn, "projects").await {
        return Vec::new();
    }
    let Ok(mut rows) = conn.query("SELECT path FROM projects", ()).await else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(path) = row.get::<String>(0) {
            paths.push(PathBuf::from(path));
        }
    }
    paths
}

fn missing_registered_store(project_root: &Path) -> StoreInventory {
    let data_dir = project_root.join(TRACEDECAY_DIR);
    StoreInventory {
        project_root: project_root.to_path_buf(),
        db_path: data_dir.join(config::DB_FILENAME),
        data_dir,
        brand: StoreBrand::TraceDecay,
        role: StoreRole::CodeProjectStore,
        registry_status: RegistryStatus::Registered,
        size_bytes: 0,
        statuses: vec![StoreStatus::MissingDb],
        artifacts: Vec::new(),
    }
}

fn record_optional_artifact(
    data_dir: &Path,
    kind: &str,
    relpath: &str,
    artifacts: &mut Vec<StoreArtifact>,
) {
    let path = data_dir.join(relpath);
    if path.is_file() {
        let size_bytes = file_size(&path);
        artifacts.push(StoreArtifact {
            kind: kind.to_string(),
            size_bytes,
            path,
        });
    } else if path.is_dir() {
        let size_bytes = dir_size(&path);
        artifacts.push(StoreArtifact {
            kind: kind.to_string(),
            size_bytes,
            path,
        });
    }
}

async fn record_branch_db_artifacts(
    data_dir: &Path,
    follow_symlinks: bool,
    skipped: &mut Vec<SkippedPath>,
    statuses: &mut Vec<StoreStatus>,
    artifacts: &mut Vec<StoreArtifact>,
) {
    let mut branches_dir = data_dir.join("branches");
    let Ok(meta) = std::fs::symlink_metadata(&branches_dir) else {
        return;
    };
    if meta.file_type().is_symlink() {
        if !follow_symlinks {
            skipped.push(SkippedPath {
                path: branches_dir,
                reason: "symlink".to_string(),
            });
            return;
        }
        if !branches_dir.is_dir() {
            return;
        }
        branches_dir = branches_dir.canonicalize().unwrap_or(branches_dir);
    } else if !meta.is_dir() {
        return;
    }

    let Ok(entries) = std::fs::read_dir(branches_dir) else {
        return;
    };
    let mut db_paths = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            entry
                .file_type()
                .is_ok_and(|file_type| file_type.is_file())
                .then_some(path)
        })
        .filter(|path| path.extension().is_some_and(|extension| extension == "db"))
        .collect::<Vec<_>>();
    db_paths.sort();

    for path in db_paths {
        artifacts.push(StoreArtifact {
            kind: "branch_graph_db".to_string(),
            size_bytes: file_size(&path),
            path: path.clone(),
        });
        record_sqlite_sidecar_artifact(&path, "-wal", "branch_graph_db_wal", artifacts);
        record_sqlite_sidecar_artifact(&path, "-shm", "branch_graph_db_shm", artifacts);
        if !sqlite_quick_check(&path).await && !statuses.contains(&StoreStatus::Corrupt) {
            statuses.push(StoreStatus::Corrupt);
        }
    }
}

fn record_sqlite_sidecar_artifact(
    db_path: &Path,
    suffix: &str,
    kind: &str,
    artifacts: &mut Vec<StoreArtifact>,
) {
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    let path = PathBuf::from(path);
    if path.is_file() {
        artifacts.push(StoreArtifact {
            kind: kind.to_string(),
            size_bytes: file_size(&path),
            path,
        });
    }
}

fn mark_registry_status(stores: &mut [StoreInventory], registered_project_keys: &HashSet<PathBuf>) {
    for store in stores {
        let key = canonicalize_lossy(&store.project_root);
        store.registry_status = if registered_project_keys.contains(&key) {
            RegistryStatus::Registered
        } else {
            RegistryStatus::Unregistered
        };
    }
}

fn canonical_path_set(paths: &[PathBuf]) -> HashSet<PathBuf> {
    paths.iter().map(|path| canonicalize_lossy(path)).collect()
}

fn canonicalize_lossy(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn hermes_home_candidates(roots: &[PathBuf], include_default_home: bool) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    let mut has_env_home = false;
    if roots.is_empty() {
        if let Some(env_home) = std::env::var_os("HERMES_HOME") {
            if !env_home.is_empty() {
                has_env_home = true;
                push_unique_path(&mut candidates, &mut seen, PathBuf::from(env_home));
            }
        }
    }
    if include_default_home && !has_env_home {
        let Some(home) = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
        else {
            return candidates;
        };
        push_unique_path(&mut candidates, &mut seen, home.join(".hermes"));
    }
    for root in roots {
        if root.file_name().is_some_and(|name| name == ".hermes") {
            push_unique_path(&mut candidates, &mut seen, root.clone());
        }
        push_unique_path(&mut candidates, &mut seen, root.join(".hermes"));
    }
    candidates
}

fn push_unique_path(candidates: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    let key = canonicalize_lossy(&path);
    if seen.insert(key) {
        candidates.push(path);
    }
}

fn read_hermes_project_pin(config_path: &Path) -> Option<PathBuf> {
    let config = std::fs::read_to_string(config_path).ok()?;
    let lines = config.lines().collect::<Vec<_>>();
    let (plugins_start, plugins_end) = find_top_level_section(&lines, "plugins")?;
    read_project_pin_from_plugin_block(&lines, plugins_start, plugins_end, "tracedecay")
        .map(PathBuf::from)
}

fn read_project_pin_from_plugin_block(
    lines: &[&str],
    plugins_start: usize,
    plugins_end: usize,
    plugin_key: &str,
) -> Option<String> {
    let (block_start, block_end) =
        find_indented_section(lines, plugins_start + 1, plugins_end, 2, plugin_key)?;
    lines
        .iter()
        .take(block_end)
        .skip(block_start + 1)
        .find_map(|line| line.trim().strip_prefix("project_root:"))
        .and_then(parse_yaml_scalar)
}

fn find_top_level_section(lines: &[&str], key: &str) -> Option<(usize, usize)> {
    let section_start = lines
        .iter()
        .position(|line| line.trim() == format!("{key}:"))?;
    let section_end = lines
        .iter()
        .enumerate()
        .skip(section_start + 1)
        .find_map(|(index, line)| {
            (!line.trim().is_empty() && leading_spaces(line) == 0).then_some(index)
        })
        .unwrap_or(lines.len());
    Some((section_start, section_end))
}

fn find_indented_section(
    lines: &[&str],
    start: usize,
    end: usize,
    indent: usize,
    key: &str,
) -> Option<(usize, usize)> {
    let marker = format!("{key}:");
    let section_start =
        lines
            .iter()
            .enumerate()
            .take(end)
            .skip(start)
            .find_map(|(index, line)| {
                (leading_spaces(line) == indent && line.trim() == marker).then_some(index)
            })?;
    let section_end = lines
        .iter()
        .enumerate()
        .take(end)
        .skip(section_start + 1)
        .find_map(|(index, line)| {
            (!line.trim().is_empty() && leading_spaces(line) <= indent).then_some(index)
        })
        .unwrap_or(end);
    Some((section_start, section_end))
}

fn parse_yaml_scalar(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with('"') {
        return serde_json::from_str::<String>(value).ok();
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return Some(value[1..value.len() - 1].replace("''", "'"));
    }
    Some(value.to_string())
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|byte| *byte == b' ').count()
}

fn should_prune_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules" | "target" | ".git" | "vendor" | "dist" | "build" | ".next" | ".venv"
    )
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |meta| meta.len())
}

fn dir_size(dir: &Path) -> u64 {
    fn walk(path: &Path, total: &mut u64, visited_dirs: &mut HashSet<PathBuf>) {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        if meta.file_type().is_symlink() {
            *total = total.saturating_add(meta.len());
            return;
        }
        if !meta.is_dir() {
            if meta.is_file() {
                *total = total.saturating_add(meta.len());
            }
            return;
        }
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !visited_dirs.insert(key) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            walk(&entry.path(), total, visited_dirs);
        }
    }

    let mut total = 0;
    let mut visited_dirs = HashSet::new();
    walk(dir, &mut total, &mut visited_dirs);
    total
}
