//! Fixture setup shared by every provider module in this suite.
//!
//! Note: nextest runs each test in its own process, so per-process caches
//! (like `common::write_empty_global_db_schema`'s DB template) do not pay
//! off here — each test opens exactly one project session DB anyway.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// Initializes `project` as a tracedecay project the ingest resolvers accept
/// (a local `.tracedecay/tracedecay.db` marker).
pub fn init_project_at(project: &Path) {
    std::fs::create_dir_all(project).unwrap();
    std::fs::create_dir_all(project.join(".tracedecay")).unwrap();
    std::fs::write(project.join(".tracedecay/tracedecay.db"), "").unwrap();
}

pub fn init_git_repo(project: &Path) {
    let output = std::process::Command::new("git")
        .arg("init")
        .current_dir(project)
        .output()
        .expect("git init should run");
    assert!(
        output.status.success(),
        "git init should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
