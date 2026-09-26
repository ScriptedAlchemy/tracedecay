use std::path::Path;

use crate::{commands::daemon_tool_json, current_unix_timestamp};

pub(crate) use tracedecay_runtime_core::storage::{ProjectStorageLocation, ProjectStorageStatus};

pub(crate) fn classify_project_storage(project_root: &Path) -> ProjectStorageLocation {
    tracedecay_runtime_core::storage::classify_project_storage(project_root)
}

pub(crate) async fn classify_project_storage_with_registry(
    project_root: &Path,
    registry: Option<
        &tracedecay_global_db::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime,
    >,
    profile_root: Option<&Path>,
) -> tracedecay_domain::errors::Result<ProjectStorageLocation> {
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
    store: &tracedecay_global_db::StoreInstanceRecord,
) -> Option<ProjectStorageLocation> {
    store.classify_storage(project_root, profile_root)
}

pub(crate) fn classify_registry_storage_value(
    project_root: &Path,
    profile_root: &Path,
    store: &serde_json::Value,
) -> Option<ProjectStorageLocation> {
    tracedecay_runtime_core::storage::classify_registry_storage_value(
        project_root,
        profile_root,
        store,
    )
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
    config: &mut tracedecay_session_memory::user_config::UserConfig,
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
    if let Some(worldwide_total) = crate::cloud::flush_pending(config.pending_upload) {
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
    config: &mut tracedecay_session_memory::user_config::UserConfig,
    skip_cache: bool,
    skip_suppression: bool,
) {
    let current_version = env!("CARGO_PKG_VERSION");
    let now = current_unix_timestamp();

    let latest = if !skip_cache && elapsed_since(now, config.last_version_check_at) < 300 {
        if config.cached_latest_version.is_empty() {
            return;
        }
        config.cached_latest_version.clone()
    } else if let Some(v) = crate::cloud::fetch_latest_version() {
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
        crate::cloud::is_newer_version(current_version, &latest)
    } else {
        crate::cloud::is_newer_minor_version(current_version, &latest)
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
) -> tracedecay_domain::errors::Result<Vec<std::path::PathBuf>> {
    if all {
        let payload = daemon_tool_json(
            None,
            "tracedecay_admin_cli",
            serde_json::json!({
                "action": "registry_list",
                "limit": 100_000,
                "query": null,
            }),
        )
        .await?;
        registry_project_roots(&payload)
    } else {
        Ok(gather_local_projects())
    }
}

fn registry_project_roots(
    payload: &serde_json::Value,
) -> tracedecay_domain::errors::Result<Vec<std::path::PathBuf>> {
    let projects = payload
        .get("projects")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
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
                .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon registry list response has no project_root for project at index {index}"
                    ),
                })
        })
        .collect()
}

/// Returns initialized project roots at cwd, an ancestor, or a descendant.
pub(crate) fn gather_local_projects() -> Vec<std::path::PathBuf> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    gather_local_projects_from(&cwd)
}

/// Same as [`gather_local_projects`] but takes the starting directory explicitly.
///
/// Ancestors count when they host a profile-sharded store or, at a worktree
/// root, the repository identity marker; ambient roots (filesystem root, the
/// user's home) never do. Descendants count by repository identity marker.
pub(crate) fn gather_local_projects_from(cwd: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for dir in cwd.ancestors() {
        if tracedecay_runtime_core::config::is_initialized_project_root(dir)
            && !tracedecay_runtime_core::config::is_ambient_project_root(dir)
            && seen.insert(dir.to_path_buf())
        {
            out.push(dir.to_path_buf());
        }
    }

    find_descendant_tracedecay(cwd, &mut seen, &mut out);

    out
}

/// Iteratively walks `start` looking for repository roots carrying the
/// repository identity marker.
///
/// Skips common heavy directories (node_modules, target, .git, etc.) and
/// `.tracedecay` data dirs. Tracks canonicalized directories to break
/// symlink/junction cycles, and uses an explicit worklist instead of
/// recursion so deep trees can't overflow the stack.
pub(crate) fn find_descendant_tracedecay(
    start: &Path,
    seen: &mut std::collections::HashSet<std::path::PathBuf>,
    out: &mut Vec<std::path::PathBuf>,
) {
    use std::collections::HashSet;

    let mut visited: HashSet<std::path::PathBuf> = HashSet::new();
    let mut work: Vec<std::path::PathBuf> = vec![start.to_path_buf()];

    while let Some(dir) = work.pop() {
        // Cycle guard, best-effort. If canonicalize fails (permission, broken
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
            if name_str == tracedecay_project::config::TRACEDECAY_DIR {
                continue;
            }
            if name_str == ".git" {
                if repository_identity_root_exists(&dir) && seen.insert(dir.clone()) {
                    out.push(dir.clone());
                }
                continue;
            }
            if matches!(
                name_str.as_ref(),
                "node_modules"
                    | "target"
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

/// A repository checkout root whose `.git/` carries the repository identity
/// marker, the only identity a current install writes.
fn repository_identity_root_exists(dir: &Path) -> bool {
    dir.join(".git").exists()
        && tracedecay_runtime_core::storage::has_repository_identity_marker(dir)
}

/// Prints the big flashing warning shown before a wipe.
pub(crate) fn print_flash_warning(all: bool, targets: &[ProjectStorageLocation]) {
    // Banner is `INNER_WIDTH` display columns wide. The colored title row is
    // padded with red-background spaces so the highlight reaches the same
    // width as the `═` rules above and below, a fixed-width visual block
    // rather than a short red strip floating between long horizontal lines.
    const INNER_WIDTH: usize = 64;
    let title = "⚠  DESTRUCTIVE ACTION. TRACEDECAY WIPE  ⚠";
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
            "\x1b[1;31mThis will wipe \x1b[5mALL\x1b[25;1;31m profile-scoped database state:\x1b[0m"
        );
        eprintln!("  \x1b[31m✗\x1b[0m global registry, user memory and sessions");
        eprintln!("  \x1b[31m✗\x1b[0m project, legacy, and remote stores");
        eprintln!("  \x1b[31m✗\x1b[0m Grafeo WAL and host-admission state");
        eprintln!("  \x1b[31m✗\x1b[0m agent-managed skills and their usage records");
        eprintln!("Profile identity, configuration, and agent integrations are preserved.");
    } else {
        eprintln!(
            "\x1b[1;31mThis will wipe local tracedecay DBs in the current folder \
             (parents and children).\x1b[0m"
        );
    }
    eprintln!();
    if !all && targets.is_empty() {
        eprintln!("  \x1b[33m(no project .tracedecay directories found)\x1b[0m");
    } else if !targets.is_empty() {
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

    fn make_enrolled_project(root: &Path, project_id: &str) {
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(root, project_id)
            .unwrap();
    }

    #[test]
    fn finds_project_at_cwd() {
        let _profile = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        make_enrolled_project(&cwd, "proj_cwd");

        let out = gather_local_projects_from(&cwd);
        assert_eq!(out, vec![cwd]);
    }

    #[test]
    fn finds_profile_sharded_store_at_cwd() {
        let _profile = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let store = tracedecay_runtime_core::storage::default_profile_sharded_layout(
            &cwd,
            &tracedecay_runtime_core::config::user_data_dir().unwrap(),
        )
        .unwrap();
        fs::create_dir_all(&store.data_root).unwrap();
        fs::write(&store.graph_db_path, b"").unwrap();

        assert_eq!(gather_local_projects_from(&cwd), vec![cwd]);
    }

    #[test]
    fn ignores_repo_local_graph_database_directories() {
        let _profile = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let child = cwd.join("child");
        for root in [&cwd, &child] {
            fs::create_dir_all(root.join(".tracedecay")).unwrap();
            fs::write(root.join(".tracedecay/tracedecay.db"), b"").unwrap();
        }

        let out = gather_local_projects_from(&cwd);
        assert!(
            out.is_empty(),
            "repo-local data dirs are not projects: {out:?}"
        );
    }

    #[test]
    fn finds_both_ancestor_and_descendant_dedup() {
        let _profile = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cwd = root.join("mid");
        fs::create_dir_all(&cwd).unwrap();
        let child = cwd.join("child");
        fs::create_dir_all(&child).unwrap();
        make_enrolled_project(&child, "proj_child");
        make_enrolled_project(&root, "proj_root");

        let out = gather_local_projects_from(&cwd);
        assert!(out.contains(&root));
        assert!(out.contains(&child));
        let unique: std::collections::HashSet<_> = out.iter().collect();
        assert_eq!(unique.len(), out.len(), "duplicates: {out:?}");
    }

    #[test]
    fn finds_profile_enrolled_projects_without_graph_db() {
        let _profile = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cwd = root.join("mid");
        let child = cwd.join("child");
        let unenrolled = cwd.join("unenrolled");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&unenrolled).unwrap();
        // Nested repositories first: pinning the outer root first would make
        // the children resolve to its `.git/` instead of their own.
        make_enrolled_project(&child, "proj_child");
        make_enrolled_project(&root, "proj_root");
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&unenrolled)
            .status()
            .unwrap();
        assert!(status.success());
        fs::create_dir_all(unenrolled.join(".tracedecay")).unwrap();
        fs::write(
            unenrolled.join(".tracedecay/enrollment.json"),
            r#"{"project_id":"proj_legacy","storage_mode":"profile_sharded"}"#,
        )
        .unwrap();

        let out = gather_local_projects_from(&cwd);

        assert!(
            out.contains(&root),
            "ancestor repository identity marker must be detected, got {out:?}"
        );
        assert!(
            out.contains(&child),
            "descendant repository identity marker must be detected, got {out:?}"
        );
        assert!(
            !out.contains(&unenrolled),
            "a retired enrollment file is not an identity, got {out:?}"
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
            data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
            b"{}",
        )
        .unwrap();
        let store = tracedecay_global_db::StoreInstanceRecord {
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
        let _profile = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let buried = cwd.join("node_modules").join("pkg");
        fs::create_dir_all(&buried).unwrap();
        make_enrolled_project(&buried, "proj_buried");

        let out = gather_local_projects_from(&cwd);
        assert!(
            !out.contains(&buried),
            "projects inside node_modules must be skipped, got {out:?}"
        );
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
        let mut config = tracedecay_session_memory::user_config::UserConfig {
            upload_enabled: true,
            pending_upload: 42,
            ..tracedecay_session_memory::user_config::UserConfig::default()
        };

        try_flush(&mut config, true, false);

        assert_eq!(config.pending_upload, 42);
        assert_eq!(config.last_flush_attempt_at, 0);
    }
}
