use std::path::Path;
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_contracts::retrieval::{
    AdminCliRegistryListV1, AdminCliResultV1, AdminCliSurfaceRequestV1,
};

use crate::{
    commands::{admin_cli_result, admin_cli_result_mismatch},
    current_unix_timestamp,
};

pub(crate) use tracedecay_runtime_core::storage::{ProjectStorageLocation, ProjectStorageStatus};

pub(crate) fn classify_project_storage(
    profile: &ProfileRoot,
    project_root: &Path,
) -> tracedecay_domain::errors::Result<ProjectStorageLocation> {
    tracedecay_runtime_core::storage::classify_project_storage(profile.data_dir(), project_root)
}

pub(crate) async fn classify_project_storage_with_registry(
    profile: &ProfileRoot,
    project_root: &Path,
    registry: Option<
        &tracedecay_global_db::profile_registry_maintenance::ProfileRegistryMaintenanceRuntime,
    >,
    profile_root: Option<&Path>,
) -> tracedecay_domain::errors::Result<ProjectStorageLocation> {
    let (Some(registry), Some(profile_root)) = (registry, profile_root) else {
        return classify_project_storage(profile, project_root);
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
/// `upload_enabled` is the already-resolved canonical user-profile setting;
/// `config` only carries the pending counter and cooldown timestamps.
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
    profile: &ProfileRoot,
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
    } else {
        match crate::cloud::fetch_latest_version() {
            Ok(v) => {
                config.cached_latest_version = v.clone();
                config.last_version_check_at = now;
                if let Err(err) = config.save_if_exists(profile.data_dir()) {
                    eprintln!("warning: could not save tracedecay config: {err}");
                }
                v
            }
            Err(error) => {
                tracing::debug!(%error, "version-update check could not read releases");
                return;
            }
        }
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
            if let Err(err) = config.save_if_exists(profile.data_dir()) {
                eprintln!("warning: could not save tracedecay config: {err}");
            }
        }
    }
}

/// Returns the project paths the `wipe` command should act on.
///
/// `--all` returns every path tracked in the global DB (including stale rows).
/// Otherwise returns the pruned local walk from cwd / ancestors / descendants.
/// Wipe runs inside the profile-offline window and cannot ask the daemon, so
/// this path must not call [`admin_cli_result`]. `list` uses
/// [`gather_list_projects`] instead.
///
/// Global discovery is deliberately fail-closed: destructive callers must not
/// interpret an unavailable daemon or malformed registry response as an empty
/// registry.
pub(crate) async fn gather_target_projects(
    profile: &ProfileRoot,
    all: bool,
) -> tracedecay_domain::errors::Result<Vec<std::path::PathBuf>> {
    if all {
        let request = AdminCliSurfaceRequestV1::RegistryList {
            limit: 100_000,
            query: None,
            project_arg: None,
        };
        match admin_cli_result(profile, None, request).await? {
            AdminCliResultV1::RegistryList(listing) => Ok(registry_project_roots(listing)),
            _ => Err(admin_cli_result_mismatch("registry_list")),
        }
    } else {
        Ok(gather_local_projects(profile))
    }
}

/// Returns the project paths `list` (without `--all`) should show.
///
/// The registry is the authority: filter registered roots to cwd, its
/// ancestors, and its descendants. Enrolled cwd/ancestor roots that the
/// registry does not yet name are still included. Descendants are never
/// discovered by walking the worktree.
pub(crate) async fn gather_list_projects(
    profile: &ProfileRoot,
) -> tracedecay_domain::errors::Result<Vec<std::path::PathBuf>> {
    let request = AdminCliSurfaceRequestV1::RegistryList {
        limit: 100_000,
        query: None,
        project_arg: None,
    };
    let roots = match admin_cli_result(profile, None, request).await? {
        AdminCliResultV1::RegistryList(listing) => registry_project_roots(listing),
        _ => return Err(admin_cli_result_mismatch("registry_list")),
    };
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(Vec::new());
    };
    Ok(merge_local_list_projects(profile, &cwd, roots))
}

fn merge_local_list_projects(
    profile: &ProfileRoot,
    cwd: &Path,
    registry_roots: Vec<std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    let mut out = filter_local_registry_roots(cwd, registry_roots);
    for dir in cwd.ancestors() {
        if profile.is_initialized_project_root(dir)
            && !profile.is_ambient_project_root(dir)
            && !out.iter().any(|existing| paths_equivalent(existing, dir))
        {
            out.push(dir.to_path_buf());
        }
    }
    out
}

fn filter_local_registry_roots(
    cwd: &Path,
    roots: impl IntoIterator<Item = std::path::PathBuf>,
) -> Vec<std::path::PathBuf> {
    roots
        .into_iter()
        .filter(|root| project_root_is_local(cwd, root))
        .collect()
}

fn project_root_is_local(cwd: &Path, project_root: &Path) -> bool {
    let cwd = path_key(cwd);
    let root = path_key(project_root);
    if !(cwd.starts_with(&root) || root.starts_with(&cwd)) {
        return false;
    }
    if root.starts_with(&cwd)
        && let Ok(relative) = root.strip_prefix(&cwd)
        && relative
            .components()
            .any(|component| is_skipped_descendant_dir(&component.as_os_str().to_string_lossy()))
    {
        return false;
    }
    true
}

fn path_key(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}

fn registry_project_roots(listing: AdminCliRegistryListV1) -> Vec<std::path::PathBuf> {
    let AdminCliRegistryListV1::Ok { projects, .. } = listing;
    projects
        .into_iter()
        .map(|project| std::path::PathBuf::from(project.project_root))
        .collect()
}

/// Returns initialized project roots at cwd, an ancestor, or a descendant.
pub(crate) fn gather_local_projects(profile: &ProfileRoot) -> Vec<std::path::PathBuf> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    gather_local_projects_from(profile, &cwd)
}

/// Same as [`gather_local_projects`] but takes the starting directory explicitly.
///
/// A directory counts when [`ProfileRoot::is_initialized_project_root`] says
/// this profile holds its store; ambient ancestors (filesystem root, the
/// user's home) never do. Descendants are checked at each repository root.
pub(crate) fn gather_local_projects_from(
    profile: &ProfileRoot,
    cwd: &Path,
) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for dir in cwd.ancestors() {
        if profile.is_initialized_project_root(dir)
            && !profile.is_ambient_project_root(dir)
            && seen.insert(dir.to_path_buf())
        {
            out.push(dir.to_path_buf());
        }
    }

    find_descendant_tracedecay(profile, cwd, &mut seen, &mut out);

    out
}

/// Iteratively walks `start` looking for repository roots this profile holds
/// a store for.
///
/// Skips `.git`, `.tracedecay`, and the canonical generated-directory
/// segments (`node_modules`, `target`, `.worktrees`, …). Tracks
/// canonicalized directories to break symlink/junction cycles, and uses an
/// explicit worklist instead of recursion so deep trees can't overflow the
/// stack.
pub(crate) fn find_descendant_tracedecay(
    profile: &ProfileRoot,
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
            if name_str == ".git" {
                if profile.is_initialized_project_root(&dir) && seen.insert(dir.clone()) {
                    out.push(dir.clone());
                }
                continue;
            }
            if is_skipped_descendant_dir(&name_str) {
                continue;
            }
            work.push(path);
        }
    }
}

fn is_skipped_descendant_dir(name: &str) -> bool {
    name == tracedecay_runtime_core::config::TRACEDECAY_DIR
        || name == ".git"
        || tracedecay_runtime_core::config::is_generated_dir_segment(name)
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
        eprintln!("  \x1b[31m✗\x1b[0m hook analytics and maintenance inventories");
        eprintln!("Profile identity, configuration, and agent integrations are preserved.");
    } else {
        eprintln!(
            "\x1b[1;31mThis will wipe local tracedecay DBs in the current folder \
             (parents and children).\x1b[0m"
        );
    }
    eprintln!();
    if !all && targets.is_empty() {
        eprintln!("  \x1b[33m(no project stores found)\x1b[0m");
    } else if !targets.is_empty() {
        eprintln!("Targets:");
        for t in targets {
            eprintln!(
                "  \x1b[31m✗\x1b[0m {} [{}]",
                t.data_root.display(),
                t.status.label()
            );
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
    use tracedecay_runtime_core::path_safety::{plain_git_args, plain_host_path};

    fn make_enrolled_project(profile: &ProfileRoot, root: &Path, project_id: &str) {
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(root, project_id)
            .unwrap();
        fs::create_dir_all(tracedecay_runtime_core::storage::profile_sharded_data_root(
            profile.data_dir(),
            project_id,
        ))
        .unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(plain_git_args(args))
            .current_dir(plain_host_path(dir))
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn ignores_repository_marker_enrolled_by_another_profile() {
        let owner_dir = tempfile::tempdir().unwrap();
        let owner = &ProfileRoot::new(owner_dir.path());
        let other_dir = tempfile::tempdir().unwrap();
        let other = &ProfileRoot::new(other_dir.path());
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().canonicalize().unwrap();
        let repo = parent.join("repo");
        let worktree = parent.join("wt");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        git(
            &repo,
            &["worktree", "add", "--quiet", worktree.to_str().unwrap()],
        );
        make_enrolled_project(owner, &repo, "proj_foreign");

        assert!(gather_local_projects_from(other, &worktree).is_empty());
        assert!(gather_local_projects_from(other, &parent).is_empty());
        assert_eq!(gather_local_projects_from(owner, &worktree), vec![worktree]);
    }

    #[test]
    fn finds_project_at_cwd() {
        let profile_dir = tempfile::tempdir().unwrap();
        let profile = &ProfileRoot::new(profile_dir.path());
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        make_enrolled_project(profile, &cwd, "proj_cwd");

        let out = gather_local_projects_from(profile, &cwd);
        assert_eq!(out, vec![cwd]);
    }

    #[test]
    fn finds_profile_sharded_store_at_cwd() {
        let profile_dir = tempfile::tempdir().unwrap();
        let profile = &ProfileRoot::new(profile_dir.path());
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let store = tracedecay_runtime_core::storage::default_profile_sharded_layout(
            &cwd,
            profile.data_dir(),
        )
        .unwrap();
        fs::create_dir_all(&store.data_root).unwrap();
        fs::write(&store.graph_db_path, b"").unwrap();

        assert_eq!(gather_local_projects_from(profile, &cwd), vec![cwd]);
    }

    #[test]
    fn ignores_repo_local_graph_database_directories() {
        let profile_dir = tempfile::tempdir().unwrap();
        let profile = &ProfileRoot::new(profile_dir.path());
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let child = cwd.join("child");
        for root in [&cwd, &child] {
            fs::create_dir_all(root.join(".tracedecay")).unwrap();
            fs::write(root.join(".tracedecay/tracedecay.db"), b"").unwrap();
        }

        let out = gather_local_projects_from(profile, &cwd);
        assert!(
            out.is_empty(),
            "repo-local data dirs are not projects: {out:?}"
        );
    }

    #[test]
    fn finds_both_ancestor_and_descendant_dedup() {
        let profile_dir = tempfile::tempdir().unwrap();
        let profile = &ProfileRoot::new(profile_dir.path());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cwd = root.join("mid");
        fs::create_dir_all(&cwd).unwrap();
        let child = cwd.join("child");
        fs::create_dir_all(&child).unwrap();
        make_enrolled_project(profile, &child, "proj_child");
        make_enrolled_project(profile, &root, "proj_root");

        let out = gather_local_projects_from(profile, &cwd);
        assert!(out.contains(&root));
        assert!(out.contains(&child));
        let unique: std::collections::HashSet<_> = out.iter().collect();
        assert_eq!(unique.len(), out.len(), "duplicates: {out:?}");
    }

    #[test]
    fn finds_profile_enrolled_projects_without_graph_db() {
        let profile_dir = tempfile::tempdir().unwrap();
        let profile = &ProfileRoot::new(profile_dir.path());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cwd = root.join("mid");
        let child = cwd.join("child");
        let unenrolled = cwd.join("unenrolled");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&unenrolled).unwrap();
        // Nested repositories first: pinning the outer root first would make
        // the children resolve to its `.git/` instead of their own.
        make_enrolled_project(profile, &child, "proj_child");
        make_enrolled_project(profile, &root, "proj_root");
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

        let out = gather_local_projects_from(profile, &cwd);

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
        let profile_dir = tempfile::tempdir().unwrap();
        let profile = &ProfileRoot::new(profile_dir.path());
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let buried = cwd.join("node_modules").join("pkg");
        fs::create_dir_all(&buried).unwrap();
        make_enrolled_project(profile, &buried, "proj_buried");

        let out = gather_local_projects_from(profile, &cwd);
        assert!(
            !out.contains(&buried),
            "projects inside node_modules must be skipped, got {out:?}"
        );
    }

    #[test]
    fn skips_projects_inside_generated_directories() {
        let profile_dir = tempfile::tempdir().unwrap();
        let profile = &ProfileRoot::new(profile_dir.path());
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        // `.worktrees` is in the canonical generated-dir list but was missing
        // from the hardcoded descendant skip set.
        let buried = cwd.join(".worktrees").join("lane");
        fs::create_dir_all(&buried).unwrap();
        make_enrolled_project(profile, &buried, "proj_worktree");

        let out = gather_local_projects_from(profile, &cwd);
        assert!(
            !out.contains(&buried),
            "projects inside generated directories must be skipped, got {out:?}"
        );
    }

    #[test]
    fn registry_local_targets_keep_cwd_ancestors_and_descendants() {
        let cwd = std::path::Path::new("/repos/mono/crates/cli");
        let out = filter_local_registry_roots(
            cwd,
            vec![
                std::path::PathBuf::from("/repos/mono"),
                std::path::PathBuf::from("/repos/mono/crates/cli"),
                std::path::PathBuf::from("/repos/mono/crates/cli/nested"),
                std::path::PathBuf::from("/elsewhere"),
            ],
        );
        assert_eq!(
            out,
            vec![
                std::path::PathBuf::from("/repos/mono"),
                std::path::PathBuf::from("/repos/mono/crates/cli"),
                std::path::PathBuf::from("/repos/mono/crates/cli/nested"),
            ]
        );
    }

    #[test]
    fn registry_local_targets_skip_generated_descendant_roots() {
        let cwd = std::path::Path::new("/repos/mono");
        let out = filter_local_registry_roots(
            cwd,
            vec![
                std::path::PathBuf::from("/repos/mono"),
                std::path::PathBuf::from("/repos/mono/node_modules/pkg"),
                std::path::PathBuf::from("/repos/mono/.worktrees/lane"),
            ],
        );
        assert_eq!(out, vec![std::path::PathBuf::from("/repos/mono")]);
    }

    #[test]
    fn merge_local_list_keeps_enrolled_cwd_absent_from_the_registry() {
        let profile_dir = tempfile::tempdir().unwrap();
        let profile = &ProfileRoot::new(profile_dir.path());
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        make_enrolled_project(profile, &cwd, "proj_cwd");

        let out = merge_local_list_projects(profile, &cwd, Vec::new());
        assert_eq!(out, vec![cwd]);
    }

    fn listing(projects: serde_json::Value) -> serde_json::Result<AdminCliRegistryListV1> {
        serde_json::from_value(serde_json::json!({
            "status": "ok",
            "limit": 100_000,
            "query": null,
            "truncated": false,
            "summary": {"project_count": 0, "repo_count": 0, "truncated": false},
            "project_tree": [],
            "projects": projects,
        }))
    }

    /// `wipe --all` acts on every registry row: a malformed row is a refused
    /// answer, never a shorter or empty target list.
    #[test]
    fn registry_targets_refuse_malformed_rows_and_keep_an_explicit_empty_registry() {
        assert_eq!(
            listing(serde_json::json!([{ "project_id": "missing-root" }]))
                .unwrap_err()
                .to_string(),
            "missing field `label`"
        );
        assert_eq!(
            registry_project_roots(listing(serde_json::json!([])).unwrap()),
            Vec::<std::path::PathBuf>::new()
        );
        assert_eq!(
            registry_project_roots(
                listing(serde_json::json!([{
                    "project_id": "project.a",
                    "label": "a",
                    "project_root": "/repos/a",
                    "display_root": "/repos/a",
                    "canonical_root": "/repos/a",
                    "git_common_dir": null,
                    "default_branch": null,
                    "head_branch": null,
                    "created_at": 1,
                    "last_seen_at": 2,
                }]))
                .unwrap()
            ),
            vec![std::path::PathBuf::from("/repos/a")]
        );
    }

    #[test]
    fn elapsed_since_clamps_future_timestamps() {
        assert_eq!(elapsed_since(100, 40), 60);
        assert_eq!(elapsed_since(100, 100), 0);
        assert_eq!(elapsed_since(100, 140), 0);
    }

    #[test]
    fn canonical_upload_denial_leaves_pending_tokens_unflushed() {
        let mut config = tracedecay_session_memory::user_config::UserConfig {
            pending_upload: 42,
            ..tracedecay_session_memory::user_config::UserConfig::default()
        };

        try_flush(&mut config, true, false);

        assert_eq!(config.pending_upload, 42);
        assert_eq!(config.last_flush_attempt_at, 0);
    }
}
