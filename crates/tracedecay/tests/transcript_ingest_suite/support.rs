//! Fixture setup shared by every provider module in this suite.
//!
//! Note: nextest runs each test in its own process, so per-process caches
//! (like `common::write_empty_global_db_schema`'s DB template) do not pay
//! off here — each test opens exactly one project session DB anyway.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay_runtime_core::path_safety::{canonicalize_path_or_existing_parent, plain_host_path};

/// One filesystem identity, one spelling: resolves aliases (`/var` firmlinks,
/// symlinked family roots) and drops the Windows verbatim prefix
/// `canonicalize` adds.
///
/// macOS `canonicalize` expands `/var` to `/private/var`. Stored session keys
/// keep the public `/var/...` spelling (same policy as
/// [`tracedecay_sessions::runtime::git_correlation::normalize_worktree`]), so
/// search selectors built from this helper must fold that expansion back.
pub fn normalize_path_text(raw: &str) -> String {
    let plain = plain_host_path(Path::new(raw));
    let canonical = plain_host_path(&canonicalize_path_or_existing_parent(&plain));
    let text = canonical.to_string_lossy();
    match text.strip_prefix("/private/var/") {
        Some(rest) => format!("/var/{rest}"),
        None => text.into_owned(),
    }
}

pub fn assert_path_text_eq(actual: &str, expected: &Path) {
    assert_eq!(
        normalize_path_text(actual),
        normalize_path_text(&expected.to_string_lossy()),
        "stored path {actual:?} does not name {}",
        expected.display()
    );
}

pub fn assert_metadata_path_eq(actual: &serde_json::Value, expected: &Path) {
    let actual = actual.as_str().expect("metadata path should be a string");
    assert_path_text_eq(actual, expected);
}

/// Initializes `project` as a tracedecay project the ingest resolvers accept
/// (a local `.tracedecay/tracedecay.db` marker).
pub fn init_project_at(project: &Path) {
    std::fs::create_dir_all(project).unwrap();
    std::fs::create_dir_all(project.join(".tracedecay")).unwrap();
    std::fs::write(project.join(".tracedecay/tracedecay.db"), "").unwrap();
}

pub fn run_git(project: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {:?} should succeed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn init_git_repo(project: &Path) {
    run_git(project, &["init"]);
}

pub fn create_git_repo_with_linked_worktree(project: &Path, linked_worktree: &Path) {
    run_git(project, &["-c", "init.defaultBranch=main", "init"]);
    std::fs::write(project.join("README.md"), "transcript location fixture\n").unwrap();
    run_git(project, &["add", "README.md"]);
    run_git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Tests",
            "-c",
            "user.email=tests@example.invalid",
            "commit",
            "-m",
            "init fixture repo",
        ],
    );
    run_git(
        project,
        &[
            "worktree",
            "add",
            "-b",
            "linked-worktree",
            linked_worktree.to_str().unwrap(),
        ],
    );
    init_project_at(linked_worktree);
}

/// Builds an initialized project dir under `tmp` and returns it.
pub fn init_project(tmp: &TempDir) -> PathBuf {
    let project = tmp.path().join("project");
    init_project_at(&project);
    project
}

/// Builds an initialized project dir and returns `(home, project)`.
pub fn setup(tmp: &TempDir) -> (PathBuf, PathBuf) {
    (tmp.path().join("home"), init_project(tmp))
}
