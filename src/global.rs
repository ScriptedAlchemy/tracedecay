use std::path::Path;

use crate::current_unix_timestamp;
use tracedecay::tracedecay::TraceDecay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectStorageStatus {
    RepoLocal,
    ProfileSharded,
    ManifestReconstructable,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectStorageLocation {
    pub project_root: std::path::PathBuf,
    pub data_root: std::path::PathBuf,
    pub marker_root: Option<std::path::PathBuf>,
    pub status: ProjectStorageStatus,
}

impl ProjectStorageStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RepoLocal => "repo-local",
            Self::ProfileSharded => "profile-sharded",
            Self::ManifestReconstructable => "manifest-reconstructable",
            Self::Stale => "stale",
        }
    }

    pub(crate) fn is_live(self) -> bool {
        matches!(self, Self::RepoLocal | Self::ProfileSharded)
    }
}

pub(crate) fn classify_project_storage(project_root: &Path) -> ProjectStorageLocation {
    match tracedecay::storage::resolve_layout_for_current_profile(project_root) {
        Ok(layout) => classify_layout_storage(project_root, layout),
        Err(_) => ProjectStorageLocation {
            project_root: project_root.to_path_buf(),
            data_root: tracedecay::config::get_tracedecay_dir(project_root),
            marker_root: None,
            status: ProjectStorageStatus::Stale,
        },
    }
}

pub(crate) async fn classify_project_storage_with_registry(
    project_root: &Path,
    global_db: Option<&tracedecay::global_db::GlobalDb>,
    profile_root: Option<&Path>,
) -> ProjectStorageLocation {
    let location = classify_project_storage(project_root);
    if location.status != ProjectStorageStatus::Stale {
        return location;
    }
    let Some(db) = global_db else {
        return location;
    };
    let Some(profile_root) = profile_root else {
        return location;
    };
    let Some(resolution) = db.resolve_project_store_by_alias(project_root).await else {
        return location;
    };
    classify_registry_storage(project_root, profile_root, &resolution.store).unwrap_or(location)
}

fn classify_layout_storage(
    project_root: &Path,
    layout: tracedecay::storage::StoreLayout,
) -> ProjectStorageLocation {
    let graph_exists = layout.graph_db_path.exists();
    let manifest_exists = layout
        .manifest_path
        .as_ref()
        .is_some_and(|path| path.is_file());
    let status = match layout.storage_mode {
        tracedecay::storage::StorageMode::ProjectLocal if graph_exists => {
            ProjectStorageStatus::RepoLocal
        }
        tracedecay::storage::StorageMode::ProfileSharded if graph_exists => {
            ProjectStorageStatus::ProfileSharded
        }
        tracedecay::storage::StorageMode::ProfileSharded if manifest_exists => {
            ProjectStorageStatus::ManifestReconstructable
        }
        _ => ProjectStorageStatus::Stale,
    };
    let marker_root = (layout.storage_mode == tracedecay::storage::StorageMode::ProfileSharded)
        .then(|| project_root.join(tracedecay::config::TRACEDECAY_DIR));
    ProjectStorageLocation {
        project_root: project_root.to_path_buf(),
        data_root: layout.data_root,
        marker_root,
        status,
    }
}

fn classify_registry_storage(
    project_root: &Path,
    profile_root: &Path,
    store: &tracedecay::global_db::StoreInstanceRecord,
) -> Option<ProjectStorageLocation> {
    if store.storage_mode != "profile_sharded" {
        return None;
    }
    let store_relpath = registry_relpath(&store.store_relpath);
    let manifest_relpath = store
        .manifest_relpath
        .as_ref()
        .map(|relpath| registry_relpath(relpath));
    let mut stale_location = None;
    let mut manifest_location = None;
    for profile_root in registry_profile_roots(profile_root) {
        let Ok(data_root) =
            tracedecay::storage::StoreArtifactPath::resolve(&profile_root, &store_relpath)
        else {
            continue;
        };
        let data_root = data_root.absolute_path();
        let manifest_exists = manifest_relpath.as_ref().map_or_else(
            || {
                data_root
                    .join(tracedecay::storage::STORE_MANIFEST_FILENAME)
                    .is_file()
            },
            |relpath| {
                [&profile_root, &data_root].iter().any(|root| {
                    tracedecay::storage::StoreArtifactPath::resolve(root, relpath)
                        .ok()
                        .is_some_and(|path| path.absolute_path().is_file())
                })
            },
        );
        let status = if data_root
            .join(tracedecay::config::db_filename(&data_root))
            .exists()
        {
            ProjectStorageStatus::ProfileSharded
        } else if manifest_exists {
            ProjectStorageStatus::ManifestReconstructable
        } else {
            ProjectStorageStatus::Stale
        };
        let location = ProjectStorageLocation {
            project_root: project_root.to_path_buf(),
            data_root,
            marker_root: Some(project_root.join(tracedecay::config::TRACEDECAY_DIR)),
            status,
        };
        match location.status {
            ProjectStorageStatus::ProfileSharded => return Some(location),
            ProjectStorageStatus::ManifestReconstructable if manifest_location.is_none() => {
                manifest_location = Some(location);
            }
            ProjectStorageStatus::Stale if stale_location.is_none() => {
                stale_location = Some(location);
            }
            _ => {}
        }
    }
    manifest_location.or(stale_location)
}

fn registry_relpath(value: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return path.to_path_buf();
    }
    value
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect()
}

fn registry_profile_roots(profile_root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut roots = vec![profile_root.to_path_buf()];
    if let Ok(canonical) = profile_root.canonicalize() {
        if !roots.iter().any(|root| root == &canonical) {
            roots.push(canonical);
        }
    }
    roots
}

/// Returns how many seconds have elapsed since a persisted timestamp.
///
/// User config timestamps can land in the future because of clock skew,
/// manual edits, or state copied across machines. Clamp those cases to zero so
/// cooldown/version-cache logic degrades gracefully instead of getting stuck on
/// a negative delta.
fn elapsed_since(now: i64, recorded_at: i64) -> i64 {
    if recorded_at >= now {
        0
    } else {
        now - recorded_at
    }
}

/// Best-effort: register this project in the user-level global DB and
/// accumulate the token savings delta into the pending upload counter.
pub(crate) async fn update_global_db(cg: &TraceDecay) {
    if !tracedecay::user_config::UserConfig::exists() {
        return;
    }
    let tokens = match cg.get_tokens_saved().await {
        Ok(tokens) => tokens,
        Err(err) => {
            eprintln!(
                "[tracedecay] failed to read tokens-saved counter for {}: {err}",
                cg.project_root().display()
            );
            return;
        }
    };
    if let Some(gdb) = tracedecay::global_db::GlobalDb::open().await {
        let previous = gdb.get_project_tokens(cg.project_root()).await;
        gdb.upsert(cg.project_root(), tokens).await;

        // Accumulate delta into pending upload
        if tokens > previous {
            let mut config = tracedecay::user_config::UserConfig::load();
            config.pending_upload += tokens - previous;
            config.save_if_exists();
        }
    }
}

/// Best-effort: try to flush pending tokens to the worldwide counter.
/// `force` = true on status/sync commands (always attempt), false on others
/// (only flush if stale > 30s).
pub(crate) fn try_flush(config: &mut tracedecay::user_config::UserConfig, force: bool) {
    if config.pending_upload == 0 || !config.upload_enabled {
        return;
    }
    let now = current_unix_timestamp();

    // Cooldown: skip if last flush attempt failed less than 60s ago
    if config.last_flush_attempt_at > config.last_upload_at
        && elapsed_since(now, config.last_flush_attempt_at) < 60
    {
        return;
    }

    // Staleness check for non-force commands
    if !force && elapsed_since(now, config.last_upload_at) < 30 {
        return;
    }

    config.last_flush_attempt_at = now;
    if let Some(worldwide_total) = tracedecay::cloud::flush_pending(config.pending_upload) {
        config.pending_upload = 0;
        config.last_upload_at = now;
        config.last_worldwide_total = worldwide_total;
        config.last_worldwide_fetch_at = now;
    }
}

/// Best-effort version check with 5-minute network cache. If `skip_cache` is
/// true, always fetches from GitHub (used during sync where the call runs in
/// parallel). If `skip_suppression` is false, the warning is suppressed for 15
/// minutes after it was last shown; if true it is always shown (used for status).
pub(crate) fn check_for_update(
    config: &mut tracedecay::user_config::UserConfig,
    skip_cache: bool,
    skip_suppression: bool,
) {
    let current_version = env!("CARGO_PKG_VERSION");
    let now = current_unix_timestamp();

    let latest = if !skip_cache && elapsed_since(now, config.last_version_check_at) < 300 {
        // Use cached value
        if config.cached_latest_version.is_empty() {
            return;
        }
        config.cached_latest_version.clone()
    } else if let Some(v) = tracedecay::cloud::fetch_latest_version() {
        config.cached_latest_version = v.clone();
        config.last_version_check_at = now;
        config.save_if_exists();
        v
    } else {
        return;
    };

    // The status page (skip_suppression=true) warns on any newer version;
    // the CLI only warns on minor+ bumps to avoid nagging on patch releases.
    let dominated = if skip_suppression {
        tracedecay::cloud::is_newer_version(current_version, &latest)
    } else {
        tracedecay::cloud::is_newer_minor_version(current_version, &latest)
    };

    if dominated && (skip_suppression || elapsed_since(now, config.last_version_warning_at) >= 900)
    {
        eprintln!(
            "\n\x1b[33mUpdate available: v{} → v{}\x1b[0m\n  Run: \x1b[1mtracedecay upgrade\x1b[0m",
            current_version, latest
        );
        if !skip_suppression {
            config.last_version_warning_at = now;
            config.save_if_exists();
        }
    }
}

/// Returns the total size in bytes of every file under `dir`. Best-effort.
pub(crate) fn tracedecay_dir_size(dir: &Path) -> u64 {
    fn walk(p: &Path, acc: &mut u64) {
        let Ok(entries) = std::fs::read_dir(p) else {
            return;
        };
        for entry in entries.flatten() {
            // One stat per entry instead of file_type() + metadata():
            // `metadata()` already carries the file-type bits, so calling
            // both means a redundant syscall on filesystems that don't
            // cache the dirent stat.
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walk(&entry.path(), acc);
            } else if meta.is_file() {
                *acc = acc.saturating_add(meta.len());
            }
        }
    }
    let mut total = 0u64;
    walk(dir, &mut total);
    total
}

/// Returns the project paths the `wipe` / `list` commands should act on.
///
/// `--all` returns every path tracked in the global DB (including stale rows).
/// Otherwise returns the local discovery from cwd / ancestors / descendants.
pub(crate) async fn gather_target_projects(
    all: bool,
    home_tracedecay: &Option<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    if all {
        let Some(gdb) = tracedecay::global_db::GlobalDb::open().await else {
            return Vec::new();
        };
        gdb.list_project_paths()
            .await
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect()
    } else {
        gather_local_projects(home_tracedecay)
    }
}

/// Returns project roots whose `.tracedecay` data dir lives in cwd, an
/// ancestor, or a descendant.
pub(crate) fn gather_local_projects(
    home_tracedecay: &Option<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    gather_local_projects_from(&cwd, home_tracedecay)
}

/// Same as [`gather_local_projects`] but takes the starting directory explicitly.
///
/// Pure (apart from filesystem reads) — easier to test than the cwd-driven wrapper.
pub(crate) fn gather_local_projects_from(
    cwd: &Path,
    home_tracedecay: &Option<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    use std::collections::HashSet;
    use std::path::PathBuf;

    // Canonicalize the home data dir once so symlinked HOME paths still
    // get correctly skipped during the ancestor + descendant walks. A user
    // whose `$HOME` is `/Users/x` but whose canonical home is
    // `/private/var/...` would otherwise leak the global DB into the wipe set.
    let canon_home_ts: Option<PathBuf> =
        home_tracedecay.as_ref().and_then(|p| p.canonicalize().ok());

    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    let is_home_tracedecay = |ts: &Path| -> bool {
        if let Some(ref canon) = canon_home_ts {
            if ts.canonicalize().ok().as_ref() == Some(canon) {
                return true;
            }
        }
        false
    };

    let is_project_dir = |project_root: &Path, ts: &Path| -> bool {
        !is_home_tracedecay(ts) && local_project_marker_exists(project_root, ts)
    };

    let mut cursor: Option<&Path> = Some(cwd);
    while let Some(dir) = cursor {
        let ts = dir.join(tracedecay::config::TRACEDECAY_DIR);
        if is_project_dir(dir, &ts) && seen.insert(dir.to_path_buf()) {
            out.push(dir.to_path_buf());
        }
        cursor = dir.parent();
    }

    find_descendant_tracedecay(cwd, &canon_home_ts, &mut seen, &mut out);

    out
}

/// Iteratively walks `start` looking for `.tracedecay/tracedecay.db` project
/// data dirs.
///
/// Skips common heavy directories (node_modules, target, .git, etc.) and never
/// descends into a data dir once found. Tracks canonicalized directories
/// to break symlink/junction cycles, and uses an explicit worklist instead of
/// recursion so deep trees can't overflow the stack.
pub(crate) fn find_descendant_tracedecay(
    start: &Path,
    canon_home_ts: &Option<std::path::PathBuf>,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
    out: &mut Vec<std::path::PathBuf>,
) {
    use std::collections::HashSet;

    let mut visited: HashSet<std::path::PathBuf> = HashSet::new();
    let mut work: Vec<std::path::PathBuf> = vec![start.to_path_buf()];

    while let Some(dir) = work.pop() {
        // Cycle guard — best-effort. If canonicalize fails (permission, broken
        // symlink) we fall back to the raw path, which still dedupes most cases.
        let canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if !visited.insert(canon) {
            continue;
        }

        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            // `file_type()` does not traverse symlinks, so symlinks-to-dirs
            // report `is_symlink()` and are skipped here. That's the primary
            // cycle defense; the `visited` set above is belt-and-suspenders.
            if !ft.is_dir() {
                continue;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str == tracedecay::config::TRACEDECAY_DIR {
                // Only canonicalize when the entry could match the home skip;
                // doing it for every dir entry would mean one syscall per
                // entry on tree walks of arbitrary size.
                if let Some(canon) = canon_home_ts {
                    if path.canonicalize().ok().as_ref() == Some(canon) {
                        continue;
                    }
                }
                if let Some(parent) = path.parent() {
                    if local_project_marker_exists(parent, &path) {
                        let pb = parent.to_path_buf();
                        if seen.insert(pb.clone()) {
                            out.push(pb);
                        }
                    }
                }
                continue;
            }
            if matches!(
                name_str.as_ref(),
                "node_modules"
                    | "target"
                    | ".git"
                    | "vendor"
                    | "dist"
                    | "build"
                    | ".next"
                    | ".venv"
                    | "__pycache__"
            ) {
                continue;
            }
            work.push(path);
        }
    }
}

fn local_project_marker_exists(project_root: &Path, data_dir: &Path) -> bool {
    if !data_dir.is_dir() {
        return false;
    }
    if data_dir
        .join(tracedecay::config::db_filename(data_dir))
        .exists()
    {
        return true;
    }
    data_dir.file_name().is_some_and(|name| {
        name == tracedecay::config::TRACEDECAY_DIR
            && tracedecay::storage::has_enrollment_marker(project_root)
    })
}

/// Prints the big flashing warning shown before a wipe.
pub(crate) fn print_flash_warning(all: bool, targets: &[ProjectStorageLocation]) {
    // Banner is `INNER_WIDTH` display columns wide. The colored title row is
    // padded with red-background spaces so the highlight reaches the same
    // width as the `═` rules above and below — a fixed-width visual block
    // rather than a short red strip floating between long horizontal lines.
    const INNER_WIDTH: usize = 64;
    let title = "⚠  DESTRUCTIVE ACTION — TRACEDECAY WIPE  ⚠";
    // Visible columns: ⚠(2) + "  "(2) + 36 + "  "(2) + ⚠(2) = 44.
    // Modern terminals render U+26A0 as a 2-col emoji glyph; older terminals
    // that pick the text presentation will leave a 2-col gap, which is mild.
    const TITLE_COLS: usize = 44;
    let pad_total = INNER_WIDTH.saturating_sub(TITLE_COLS);
    let pad_left = " ".repeat(pad_total / 2);
    let pad_right = " ".repeat(pad_total - pad_total / 2);
    let banner = "═".repeat(INNER_WIDTH);
    let blank_red = " ".repeat(INNER_WIDTH);

    eprintln!();
    eprintln!("\x1b[1;31m{banner}\x1b[0m");
    eprintln!("\x1b[1;5;37;41m{blank_red}\x1b[0m");
    eprintln!("\x1b[1;5;37;41m{pad_left}{title}{pad_right}\x1b[0m");
    eprintln!("\x1b[1;5;37;41m{blank_red}\x1b[0m");
    eprintln!("\x1b[1;31m{banner}\x1b[0m");
    eprintln!();
    if all {
        eprintln!(
            "\x1b[1;31mThis will wipe \x1b[5mALL\x1b[25;1;31m tracked tracedecay projects \
             AND empty the global DB.\x1b[0m"
        );
    } else {
        eprintln!(
            "\x1b[1;31mThis will wipe local tracedecay DBs in the current folder \
             (parents and children).\x1b[0m"
        );
    }
    eprintln!();
    if targets.is_empty() {
        eprintln!("  \x1b[33m(no project .tracedecay directories found)\x1b[0m");
    } else {
        eprintln!("Targets:");
        for t in targets {
            eprintln!(
                "  \x1b[31m✗\x1b[0m {} [{}]",
                t.data_root.display(),
                t.status.label()
            );
            if let Some(marker_root) = &t.marker_root {
                eprintln!("    marker: {}", marker_root.display());
            }
        }
    }
    if all {
        if let Some(p) = tracedecay::global_db::global_db_path() {
            eprintln!("  \x1b[31m✗\x1b[0m {} (global DB)", p.display());
        }
    }
    eprintln!();
    eprintln!("\x1b[1;5;33mThis cannot be undone.\x1b[0m");
    eprintln!();
}

// Test-only fixtures intentionally use unwrap/expect so setup failures abort the
// test immediately instead of smearing the failure across later assertions.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod gather_tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    /// Plant a `.tracedecay/tracedecay.db` marker so `is_project_dir` returns true.
    fn make_project(root: &Path) {
        let ts = root.join(".tracedecay");
        fs::create_dir_all(&ts).unwrap();
        fs::write(ts.join("tracedecay.db"), b"").unwrap();
    }

    fn make_enrolled_project(root: &Path, project_id: &str) {
        let ts = root.join(".tracedecay");
        fs::create_dir_all(&ts).unwrap();
        fs::write(
            ts.join(tracedecay::storage::ENROLLMENT_FILENAME),
            format!(
                r#"{{
  "project_id": "{project_id}",
  "storage_mode": "profile_sharded"
}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn finds_project_at_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        make_project(&cwd);

        let out = gather_local_projects_from(&cwd, &None);
        assert_eq!(out, vec![cwd]);
    }

    #[test]
    fn finds_project_at_ancestor_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let nested = root.join("a").join("b").join("c");
        fs::create_dir_all(&nested).unwrap();
        make_project(&root);

        let out = gather_local_projects_from(&nested, &None);
        assert!(
            out.contains(&root),
            "ancestor project must be detected, got {out:?}"
        );
    }

    #[test]
    fn finds_project_at_descendant_only() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let child = cwd.join("sub").join("proj");
        fs::create_dir_all(&child).unwrap();
        make_project(&child);

        let out = gather_local_projects_from(&cwd, &None);
        assert!(
            out.contains(&child),
            "descendant project must be detected, got {out:?}"
        );
    }

    #[test]
    fn finds_both_ancestor_and_descendant_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cwd = root.join("mid");
        fs::create_dir_all(&cwd).unwrap();
        let child = cwd.join("child");
        fs::create_dir_all(&child).unwrap();
        make_project(&root);
        make_project(&child);

        let out = gather_local_projects_from(&cwd, &None);
        assert!(out.contains(&root));
        assert!(out.contains(&child));
        let unique: std::collections::HashSet<_> = out.iter().collect();
        assert_eq!(unique.len(), out.len(), "duplicates: {out:?}");
    }

    #[test]
    fn finds_profile_enrolled_projects_without_graph_db() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cwd = root.join("mid");
        let child = cwd.join("child");
        fs::create_dir_all(&child).unwrap();
        make_enrolled_project(&root, "proj_root");
        make_enrolled_project(&child, "proj_child");

        let out = gather_local_projects_from(&cwd, &None);

        assert!(
            out.contains(&root),
            "ancestor enrollment marker must be detected, got {out:?}"
        );
        assert!(
            out.contains(&child),
            "descendant enrollment marker must be detected, got {out:?}"
        );
    }

    #[test]
    fn registry_manifest_relpath_resolves_from_profile_root() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("repo");
        let profile_root = dir.path().join("profile");
        let data_root = profile_root.join("projects").join("proj_123");
        fs::create_dir_all(&project_root).unwrap();
        fs::create_dir_all(&data_root).unwrap();
        fs::write(
            data_root.join(tracedecay::storage::STORE_MANIFEST_FILENAME),
            b"{}",
        )
        .unwrap();
        let store = tracedecay::global_db::StoreInstanceRecord {
            store_id: "store_123".to_string(),
            project_id: "proj_123".to_string(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_123".to_string(),
            manifest_relpath: Some("projects/proj_123/store_manifest.json".to_string()),
            created_at: 1_800_000_000,
            last_verified_at: None,
            last_write_at: None,
        };

        let location = classify_registry_storage(&project_root, &profile_root, &store).unwrap();

        assert_eq!(
            location.status,
            ProjectStorageStatus::ManifestReconstructable
        );
        let actual_data_root = location
            .data_root
            .canonicalize()
            .unwrap_or_else(|_| location.data_root.clone());
        let expected_data_root = data_root
            .canonicalize()
            .unwrap_or_else(|_| data_root.clone());
        assert_eq!(actual_data_root, expected_data_root);
        #[cfg(unix)]
        {
            let symlinked_profile_root = dir.path().join("profile-link");
            symlink(&profile_root, &symlinked_profile_root).unwrap();
            let location =
                classify_registry_storage(&project_root, &symlinked_profile_root, &store).unwrap();
            assert_eq!(
                location.status,
                ProjectStorageStatus::ManifestReconstructable
            );
        }
    }

    #[test]
    fn skips_projects_inside_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let buried = cwd.join("node_modules").join("pkg");
        fs::create_dir_all(&buried).unwrap();
        make_project(&buried);

        let out = gather_local_projects_from(&cwd, &None);
        assert!(
            !out.contains(&buried),
            "projects inside node_modules must be skipped, got {out:?}"
        );
    }

    #[test]
    fn skips_home_data_dir_via_canonical_path() {
        // Simulate a symlinked HOME: `home_alias` → `home_real`. The user
        // passes `home_alias/.tracedecay` as the skip path, but the descendant
        // walk encounters the directory through `home_real/.tracedecay`.
        // Canonicalization must resolve them as equal so the global DB
        // directory is not picked up as a wipe target.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        let home_real = root.join("home_real");
        fs::create_dir_all(&home_real).unwrap();
        make_project(&home_real); // pretend `~/.tracedecay` is a project (it shouldn't be wiped)

        // Try to symlink: home_alias -> home_real. If the platform doesn't
        // allow symlinks (e.g. Windows without dev mode) we just skip the
        // canonical-equivalence check and verify the direct-path skip works.
        let home_alias = root.join("home_alias");
        let symlink_ok = symlink_dir(&home_real, &home_alias).is_ok();

        let cwd = root.clone();
        let alias_ts: PathBuf = if symlink_ok {
            home_alias.join(".tracedecay")
        } else {
            home_real.join(".tracedecay")
        };

        let out = gather_local_projects_from(&cwd, &Some(alias_ts));
        assert!(
            !out.contains(&home_real),
            "home `.tracedecay` (canonical) must be skipped, got {out:?}"
        );
        if symlink_ok {
            assert!(
                !out.contains(&home_alias),
                "home `.tracedecay` (alias) must be skipped, got {out:?}"
            );
        }
    }

    #[cfg(unix)]
    fn symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(src, dst)
    }

    #[cfg(windows)]
    fn symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(src, dst)
    }

    #[test]
    fn empty_dir_yields_empty_result() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let out = gather_local_projects_from(&cwd, &None);
        assert!(out.is_empty(), "got {out:?}");
    }

    #[test]
    fn elapsed_since_clamps_future_timestamps() {
        assert_eq!(elapsed_since(100, 40), 60);
        assert_eq!(elapsed_since(100, 100), 0);
        assert_eq!(elapsed_since(100, 140), 0);
    }
}
