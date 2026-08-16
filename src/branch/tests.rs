use super::*;

#[test]
fn sanitize_simple() {
    assert_eq!(sanitize_branch_name("main"), "main");
}

#[test]
fn sanitize_slashes() {
    assert_eq!(sanitize_branch_name("feature/foo/bar"), "feature_foo_bar");
}

#[test]
fn sanitize_special_chars() {
    assert_eq!(sanitize_branch_name("fix: bug <1>"), "fix_bug_1");
}

#[test]
fn sanitize_dots_prevented() {
    // ".." becomes all underscores, collapsed and trimmed to empty
    assert_eq!(sanitize_branch_name(".."), "");
    // dots and slashes become underscores, collapsed
    assert_eq!(sanitize_branch_name("foo/../bar"), "foo_bar");
}

#[tokio::test]
async fn tracking_a_new_branch_publishes_metadata_without_creating_a_database() {
    let (_base, project_root, td) = setup_repo_with_meta();
    run_git(&project_root, &["branch", "feature/topic"]);

    let prepared = prepare_branch_tracking_in_layout(&project_root, "feature/topic", &td)
        .await
        .unwrap();

    let BranchTrackingPreparation::Added(prepared) = prepared else {
        panic!("new branch must prepare as Added");
    };
    let meta = crate::branch_meta::load_branch_meta(&td).unwrap();
    let entry = meta.branches.get("feature/topic").unwrap();
    assert_eq!(
        entry.db_file,
        crate::config::db_filename(&td),
        "single-store tracking must reference the canonical main database"
    );
    assert!(entry.served_by_project_store());
    assert_eq!(entry.parent.as_deref(), Some("main"));
    assert!(
        !td.join("branches").exists(),
        "tracking must not create a per-branch database"
    );

    assert_eq!(
        rollback_prepared_branch_tracking(&td, &prepared).unwrap(),
        PreparedBranchRollbackOutcome::RolledBack
    );
    let meta = crate::branch_meta::load_branch_meta(&td).unwrap();
    assert!(!meta.is_tracked("feature/topic"));
    assert!(
        td.join("tracedecay.db").exists(),
        "rollback must never touch the project store"
    );
}

#[tokio::test]
async fn rollback_prepared_tracking_preserves_a_newer_metadata_entry() {
    let (_base, project_root, td) = setup_repo_with_meta();
    run_git(&project_root, &["branch", "feature/topic"]);

    let prepared = prepare_branch_tracking_in_layout(&project_root, "feature/topic", &td)
        .await
        .unwrap();
    let BranchTrackingPreparation::Added(prepared) = prepared else {
        panic!("new branch must prepare as Added");
    };
    let mut advanced = crate::branch_meta::load_branch_meta(&td).unwrap();
    advanced
        .branches
        .get_mut("feature/topic")
        .unwrap()
        .last_synced_at = "999".to_owned();
    crate::branch_meta::save_branch_meta(&td, &advanced).unwrap();

    assert_eq!(
        rollback_prepared_branch_tracking(&td, &prepared).unwrap(),
        PreparedBranchRollbackOutcome::NoMatch,
        "a failed older attempt must not retire newer branch metadata"
    );
    assert_eq!(
        crate::branch_meta::load_branch_meta(&td)
            .unwrap()
            .branches
            .get("feature/topic")
            .unwrap(),
        advanced.branches.get("feature/topic").unwrap(),
        "the exact newer entry must survive the stale rollback"
    );
}

// --- git test harness (mirrors src/mcp/hook_events.rs tests) ------------

fn git_program() -> std::ffi::OsString {
    use std::sync::OnceLock;
    static GIT: OnceLock<std::ffi::OsString> = OnceLock::new();
    GIT.get_or_init(|| {
        if let Some(explicit) = std::env::var_os("GIT") {
            return explicit;
        }
        let exe_name = if cfg!(windows) { "git.exe" } else { "git" };
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                let candidate = dir.join(exe_name);
                if candidate.is_file() {
                    return candidate.into_os_string();
                }
            }
        }
        std::ffi::OsString::from("git")
    })
    .clone()
}

fn run_git(cwd: &Path, args: &[&str]) {
    assert!(cwd.is_dir(), "git cwd {cwd:?} should exist");
    let git = git_program();
    let mut last_err: Option<std::io::Error> = None;
    let mut output = None;
    for attempt in 0..5 {
        match std::process::Command::new(&git)
            .args(args)
            .current_dir(cwd)
            .output()
        {
            Ok(out) => {
                output = Some(out);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && attempt < 4 => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
            }
            Err(e) => panic!("git {args:?} should run (program {git:?}): {e}"),
        }
    }
    let output =
        output.unwrap_or_else(|| panic!("git {args:?} should run after retries: {last_err:?}"));
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(windows)]
fn git_test_root(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(not(windows))]
fn git_test_root(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|e| panic!("tempdir should canonicalize: {e}"))
}

/// Creates a temp git repo on `main` with one commit, plus a tracedecay dir
/// holding a stub default-branch DB. Returns `(tempdir, project_root,
/// tracedecay_dir)`.
fn setup_repo_with_meta() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let project_root = git_test_root(base.path());
    std::fs::write(project_root.join("f.txt"), "x\n").unwrap();
    run_git(&project_root, &["init", "-b", "main"]);
    run_git(&project_root, &["config", "user.email", "t@t.com"]);
    run_git(&project_root, &["config", "user.name", "T"]);
    run_git(&project_root, &["add", "."]);
    run_git(&project_root, &["commit", "-m", "initial"]);

    let tracedecay_dir = project_root.join(".tracedecay");
    std::fs::create_dir_all(&tracedecay_dir).unwrap();
    std::fs::write(tracedecay_dir.join("tracedecay.db"), b"maindb").unwrap();
    let meta = crate::branch_meta::BranchMeta::new("main");
    crate::branch_meta::save_branch_meta(&tracedecay_dir, &meta).unwrap();
    (base, project_root, tracedecay_dir)
}

/// Writes a tracked branch entry with a stub DB and an explicit
/// `last_synced_at` (unix secs), creating the git ref when `create_ref`.
fn add_tracked_branch(
    project_root: &Path,
    tracedecay_dir: &Path,
    name: &str,
    last_synced: u64,
    create_ref: bool,
) -> PathBuf {
    if create_ref {
        run_git(project_root, &["branch", name]);
    }
    let stem = sanitize_branch_name(name);
    let branches_dir = crate::branch_meta::ensure_branches_dir(tracedecay_dir).unwrap();
    let db_path = branches_dir.join(format!("{stem}.db"));
    std::fs::write(&db_path, b"branchdb").unwrap();
    let mut meta = crate::branch_meta::load_branch_meta(tracedecay_dir).unwrap();
    meta.add_branch(name, &format!("branches/{stem}.db"), "main");
    meta.branches.get_mut(name).unwrap().last_synced_at = last_synced.to_string();
    crate::branch_meta::save_branch_meta(tracedecay_dir, &meta).unwrap();
    db_path
}

#[test]
fn legacy_gc_wrapper_fails_closed() {
    let (_base, project_root, td) = setup_repo_with_meta();
    let stale = now_unix_secs() - 20 * 86_400;
    let db = add_tracked_branch(&project_root, &td, "gone", stale, false);
    assert!(db.exists());

    let report = gc_dead_branch_stores(&project_root, &td, 14, 7);

    assert!(report.removed_tracked.is_empty());
    assert!(report.removed_orphan_dbs.is_empty());
    assert!(db.exists());
    let meta = crate::branch_meta::load_branch_meta(&td).unwrap();
    assert!(meta.is_tracked("gone"));
}
