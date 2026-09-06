//! Fixture setup shared by every provider module in this suite.
//!
//! Note: nextest runs each test in its own process, so per-process caches
//! (like `common::write_empty_global_db_schema`'s DB template) do not pay
//! off here — each test opens exactly one project session DB anyway.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

pub fn normalize_path_text(raw: &str) -> String {
    let raw = raw.strip_prefix("\\\\?\\").unwrap_or(raw);
    let unc_path;
    let raw = if let Some(rest) = raw.strip_prefix("UNC\\") {
        unc_path = format!("\\\\{rest}");
        unc_path.as_str()
    } else {
        raw
    };
    let path = Path::new(raw);
    let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let text = normalized.to_string_lossy();
    text.strip_prefix("\\\\?\\").unwrap_or(&text).to_string()
}

pub fn assert_path_text_eq(actual: &str, expected: &Path) {
    assert_eq!(
        normalize_path_text(actual),
        normalize_path_text(&expected.to_string_lossy())
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
