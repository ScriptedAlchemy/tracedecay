use std::path::Path;

use crate::current_unix_timestamp;

pub(crate) use tracedecay::storage::{ProjectStorageLocation, ProjectStorageStatus};

pub(crate) fn classify_project_storage(project_root: &Path) -> ProjectStorageLocation {
    tracedecay::storage::classify_project_storage(project_root)
}

pub(crate) async fn classify_project_storage_with_registry(
    project_root: &Path,
    registry: Option<&tracedecay::migrate::registry::MigrationRegistryRuntime>,
    profile_root: Option<&Path>,
) -> tracedecay::errors::Result<ProjectStorageLocation> {
    let location = classify_project_storage(project_root);
    let (Some(registry), Some(profile_root)) = (registry, profile_root) else {
        return Ok(location);
    };
    registry
        .classify_project_storage(project_root, profile_root)
        .await
}

#[cfg(test)]
fn classify_registry_storage(
    project_root: &Path,
    profile_root: &Path,
    store: &tracedecay::global_db::StoreInstanceRecord,
) -> Option<ProjectStorageLocation> {
    tracedecay::storage::classify_registry_storage(project_root, profile_root, store)
}

pub(crate) fn classify_registry_storage_value(
    project_root: &Path,
    profile_root: &Path,
    store: &serde_json::Value,
) -> Option<ProjectStorageLocation> {
    tracedecay::storage::classify_registry_storage_value(project_root, profile_root, store)
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

/// Best-effort: try to flush pending tokens to the worldwide counter.
///
/// `upload_enabled` is the already-resolved canonical user-profile setting.
/// Legacy user metadata carries the pending counter and cooldown timestamps,
/// but it is never an authorization fallback for upload.
/// `force` = true on status/sync commands (always attempt), false on others
/// (only flush if stale > 30s).
pub(crate) fn try_flush(
    config: &mut tracedecay::user_config::UserConfig,
    force: bool,
    upload_enabled: bool,
) {
    if config.pending_upload == 0 || !upload_enabled {
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
        if let Err(err) = config.save_if_exists() {
            eprintln!("warning: could not save tracedecay config: {err}");
        }
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
            if let Err(err) = config.save_if_exists() {
                eprintln!("warning: could not save tracedecay config: {err}");
            }
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
///
/// Global discovery is deliberately fail-closed: destructive callers must not
/// interpret an unavailable daemon or malformed registry response as an empty
/// registry.
pub(crate) async fn gather_target_projects(
    all: bool,
    home_tracedecay: &Option<std::path::PathBuf>,
) -> tracedecay::errors::Result<Vec<std::path::PathBuf>> {
    if all {
        let payload = call_admin_cli(
            None,
            serde_json::json!({
                "action": "registry_list",
                "limit": 100_000,
                "query": null,
            }),
        )
        .await?;
        registry_project_roots(&payload)
    } else {
        Ok(gather_local_projects(home_tracedecay))
    }
}

fn registry_project_roots(
    payload: &serde_json::Value,
) -> tracedecay::errors::Result<Vec<std::path::PathBuf>> {
    let projects = payload
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: "daemon registry list response omitted projects array".to_string(),
        })?;

    projects
        .iter()
        .enumerate()
        .map(|(index, project)| {
            project
                .get("project_root")
                .and_then(serde_json::Value::as_str)
                .map(std::path::PathBuf::from)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon registry list response has no project_root for project at index {index}"
                    ),
                })
        })
        .collect()
}

async fn call_admin_cli(
    project_root: Option<&Path>,
    arguments: serde_json::Value,
) -> tracedecay::errors::Result<serde_json::Value> {
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        project_root.map(Path::to_path_buf),
        None,
        false,
        false,
    )?;
    let result =
        tracedecay::daemon::call_default_tool(&handshake, "tracedecay_admin_cli", arguments)
            .await?;
    tracedecay::daemon::tool_json_payload(&result, "tracedecay_admin_cli")
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
        if let Some(ref canon) = canon_home_ts
            && ts.canonicalize().ok().as_ref() == Some(canon)
        {
            return true;
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
                if let Some(canon) = canon_home_ts
                    && path.canonicalize().ok().as_ref() == Some(canon)
                {
                    continue;
                }
                if let Some(parent) = path.parent()
                    && local_project_marker_exists(parent, &path)
                {
                    let pb = parent.to_path_buf();
                    if seen.insert(pb.clone()) {
                        out.push(pb);
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
    if all && let Some(p) = tracedecay::global_db::global_db_path() {
        eprintln!("  \x1b[31m✗\x1b[0m {} (global DB)", p.display());
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
    fn registry_target_parser_rejects_malformed_rows() {
        let error = registry_project_roots(&serde_json::json!({
            "projects": [{ "project_id": "missing-root" }]
        }))
        .expect_err("malformed registry data must not become an empty target list");

        assert!(error.to_string().contains("project_root"));
    }

    #[test]
    fn registry_target_parser_preserves_an_explicitly_empty_registry() {
        let paths = registry_project_roots(&serde_json::json!({ "projects": [] }))
            .expect("an explicit empty registry is valid");

        assert!(paths.is_empty());
    }

    #[test]
    fn elapsed_since_clamps_future_timestamps() {
        assert_eq!(elapsed_since(100, 40), 60);
        assert_eq!(elapsed_since(100, 100), 0);
        assert_eq!(elapsed_since(100, 140), 0);
    }

    #[test]
    fn canonical_upload_denial_overrides_stale_legacy_metadata() {
        let mut config = tracedecay::user_config::UserConfig {
            upload_enabled: true,
            pending_upload: 42,
            ..tracedecay::user_config::UserConfig::default()
        };

        try_flush(&mut config, true, false);

        assert_eq!(config.pending_upload, 42);
        assert_eq!(config.last_flush_attempt_at, 0);
    }
}
