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
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        unique_branch_db_stem(&meta, dir.path(), "feature/new")
            .unwrap()
            .unwrap(),
        "feature_new"
    );
}

#[test]
fn unique_stem_disambiguates_sanitization_collision() {
    // "feature/foo" sanitizes to the same stem as the literal "feature_foo".
    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature/foo", "branches/feature_foo.db", "main");
    let dir = tempfile::tempdir().unwrap();
    let stem = unique_branch_db_stem(&meta, dir.path(), "feature_foo")
        .unwrap()
        .unwrap();
    assert_ne!(
        stem, "feature_foo",
        "second branch must not reuse the first branch's DB file"
    );
    assert!(stem.starts_with("feature_foo-"), "got: {stem}");
}

#[test]
fn unique_stem_preserves_hashed_orphan_recovery_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature/foo", "branches/feature_foo.db", "main");
    let hashed = format!("feature_foo-{}", short_branch_hash("feature_foo"));
    std::fs::write(dir.path().join(format!("{hashed}.db")), b"recovery").unwrap();

    assert_eq!(
        unique_branch_db_stem(&meta, dir.path(), "feature_foo")
            .unwrap()
            .unwrap(),
        format!("{hashed}-1")
    );
}

#[test]
fn unique_stem_is_idempotent_for_same_branch() {
    // Recomputing for a branch already in meta must not treat its own entry
    // as a conflict.
    let mut meta = crate::branch_meta::BranchMeta::new("main");
    meta.add_branch("feature/foo", "branches/feature_foo.db", "main");
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        unique_branch_db_stem(&meta, dir.path(), "feature/foo")
            .unwrap()
            .unwrap(),
        "feature_foo"
    );
}

#[test]
fn unique_stem_rejects_empty_sanitization() {
    let meta = crate::branch_meta::BranchMeta::new("main");
    let dir = Path::new("/nonexistent-branches-dir-for-test");
    assert!(unique_branch_db_stem(&meta, dir, "..").unwrap().is_none());
    assert!(unique_branch_db_stem(&meta, dir, "///").unwrap().is_none());
}

#[test]
fn unique_stem_reuses_an_unpublished_missing_database_path() {
    let temp = tempfile::tempdir().unwrap();
    let branches_dir = temp.path().join("branches");
    std::fs::create_dir_all(&branches_dir).unwrap();

    let meta = crate::branch_meta::BranchMeta::new("main");
    let stem = unique_branch_db_stem(&meta, &branches_dir, "feature")
        .unwrap()
        .unwrap();

    assert_eq!(stem, "feature");
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

#[tokio::test]
async fn writer_owned_sqlite_snapshot_includes_committed_wal_data_and_readers_stay_read_only() {
    // `publish_test_runtime` materialises a sidecar *profile* shard next to the
    // fixture database, and the kernel initialises profile-scoped shards through
    // a fail-closed port whose installer lives in `tracedecay-global-db`. Only
    // the root crate can supply it; production reaches this through
    // `DaemonSessionRuntimeRegistryV1::open`. Idempotent.
    crate::daemon::store_runtime::register_registered_schema_installer();
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.db");
    let dst = dir.path().join("dst.db");
    let authority =
        crate::db::DatabaseAuthority::acquire_test(&src, "branch snapshot test").unwrap();
    let (writer, _) = crate::db::Database::publish_test_runtime(
        &src,
        &authority,
        crate::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    writer
        .execute_write_batch(
            "seed branch snapshot fixture",
            "CREATE TABLE snapshot_probe(value TEXT NOT NULL);
             INSERT INTO snapshot_probe(value) VALUES ('committed-in-wal');",
        )
        .await
        .unwrap();
    writer.snapshot_to(&dst).await.unwrap();
    writer.close();

    let (source, _) = crate::db::Database::publish_test_runtime(
        &src,
        &authority,
        crate::db::TestDatabaseRuntimeMode::ReadOnly,
    )
    .await
    .unwrap();
    assert!(
        source
            .conn()
            .execute("CREATE TABLE forbidden_snapshot_write (id INTEGER)", ())
            .await
            .is_err()
    );

    let snapshot_authority =
        crate::db::DatabaseAuthority::acquire_test(&dst, "branch snapshot verification").unwrap();
    let (snapshot, _) = crate::db::Database::publish_test_runtime(
        &dst,
        &snapshot_authority,
        crate::db::TestDatabaseRuntimeMode::ReadOnly,
    )
    .await
    .unwrap();
    let mut rows = snapshot
        .conn()
        .query("SELECT value FROM snapshot_probe", ())
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "committed-in-wal");
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
