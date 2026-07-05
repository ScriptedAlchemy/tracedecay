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

#[test]
fn unique_stem_keeps_free_name() {
    let meta = crate::branch_meta::BranchMeta::new("main");
    let dir = Path::new("/nonexistent-branches-dir-for-test");
    assert_eq!(
        unique_branch_db_stem(&meta, dir, "feature/new").unwrap(),
        "feature_new"
    );
}

#[test]
fn unique_stem_disambiguates_sanitization_collision() {
    // "feature/foo" sanitizes to the same stem as the literal "feature_foo".
    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature/foo", "branches/feature_foo.db", "main");
    let dir = Path::new("/nonexistent-branches-dir-for-test");
    let stem = unique_branch_db_stem(&meta, dir, "feature_foo").unwrap();
    assert_ne!(
        stem, "feature_foo",
        "second branch must not reuse the first branch's DB file"
    );
    assert!(stem.starts_with("feature_foo-"), "got: {stem}");
}

#[test]
fn unique_stem_is_idempotent_for_same_branch() {
    // Recomputing for a branch already in meta must not treat its own entry
    // as a conflict.
    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature/foo", "branches/feature_foo.db", "main");
    let dir = Path::new("/nonexistent-branches-dir-for-test");
    assert_eq!(
        unique_branch_db_stem(&meta, dir, "feature/foo").unwrap(),
        "feature_foo"
    );
}

#[test]
fn unique_stem_rejects_empty_sanitization() {
    let meta = crate::branch_meta::BranchMeta::new("main");
    let dir = Path::new("/nonexistent-branches-dir-for-test");
    assert!(unique_branch_db_stem(&meta, dir, "..").is_none());
    assert!(unique_branch_db_stem(&meta, dir, "///").is_none());
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
fn clone_or_copy_db_produces_identical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.db");
    let dst = dir.path().join("dst.db");
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &payload).unwrap();

    clone_or_copy_db(&src, &dst).unwrap();

    let copied = std::fs::read(&dst).unwrap();
    assert_eq!(copied, payload, "reflink-or-copy must be byte-identical");
}

#[test]
fn gc_removes_dead_stale_branch() {
    let (_base, project_root, td) = setup_repo_with_meta();
    // Ref-gone (never created) and last synced long ago (> 14d).
    let stale = now_unix_secs() - 20 * 86_400;
    let db = add_tracked_branch(&project_root, &td, "gone", stale, false);
    assert!(db.exists());

    let report = gc_dead_branch_stores(&project_root, &td, 14, 7);

    assert_eq!(report.removed_tracked, vec!["gone".to_string()]);
    assert!(!db.exists(), "dead branch DB should be deleted");
    let meta = crate::branch_meta::load_branch_meta(&td).unwrap();
    assert!(!meta.is_tracked("gone"));
}

#[test]
fn gc_keeps_default_branch() {
    let (_base, project_root, td) = setup_repo_with_meta();
    // Delete the git ref for main so only the never-remove-default guard
    // protects it; also backdate would require touching main's entry.
    run_git(&project_root, &["checkout", "--detach"]);
    // Force main's ref away is unnecessary — GC skips default by name.
    let report = gc_dead_branch_stores(&project_root, &td, 14, 7);
    assert!(report.removed_tracked.is_empty());
    let meta = crate::branch_meta::load_branch_meta(&td).unwrap();
    assert!(meta.is_tracked("main"));
    assert!(td.join("tracedecay.db").exists());
}

#[test]
fn gc_keeps_fresh_dead_branch() {
    let (_base, project_root, td) = setup_repo_with_meta();
    // Ref gone but synced just now: within grace, keep it.
    let db = add_tracked_branch(&project_root, &td, "recent", now_unix_secs(), false);
    let report = gc_dead_branch_stores(&project_root, &td, 14, 7);
    assert!(report.removed_tracked.is_empty());
    assert!(db.exists());
}

#[test]
fn gc_keeps_branch_with_live_ref() {
    let (_base, project_root, td) = setup_repo_with_meta();
    // Ref exists AND stale: still keep it, ref presence wins.
    let stale = now_unix_secs() - 100 * 86_400;
    let db = add_tracked_branch(&project_root, &td, "live", stale, true);
    assert!(is_branch_ref_present(&project_root, "live"));
    let report = gc_dead_branch_stores(&project_root, &td, 14, 7);
    assert!(report.removed_tracked.is_empty());
    assert!(db.exists());
}

#[test]
fn gc_deletes_stale_orphan_db_keeps_fresh() {
    let (_base, project_root, td) = setup_repo_with_meta();
    let branches_dir = crate::branch_meta::ensure_branches_dir(&td).unwrap();

    // Stale orphan: not in meta, mtime backdated > 7d.
    let stale_orphan = branches_dir.join("orphan_stale.db");
    std::fs::write(&stale_orphan, b"junk").unwrap();
    let stale_wal = branches_dir.join("orphan_stale.db-wal");
    std::fs::write(&stale_wal, b"wal").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_hours(720);
    set_mtime(&stale_orphan, old);

    // Fresh orphan: just created, must survive.
    let fresh_orphan = branches_dir.join("orphan_fresh.db");
    std::fs::write(&fresh_orphan, b"junk").unwrap();

    let report = gc_dead_branch_stores(&project_root, &td, 14, 7);

    assert!(!stale_orphan.exists(), "stale orphan should be deleted");
    assert!(!stale_wal.exists(), "orphan sidecar should be deleted");
    assert!(fresh_orphan.exists(), "fresh orphan should be kept");
    assert!(report.removed_orphan_dbs.contains(&stale_orphan));
    assert!(!report.removed_orphan_dbs.contains(&fresh_orphan));
}

fn set_mtime(path: &Path, when: std::time::SystemTime) {
    // Best-effort mtime backdate via filetime-free approach: re-open and use
    // the standard library's set_modified (stable since 1.75).
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(when).unwrap();
}
