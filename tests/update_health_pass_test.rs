//! Integration tests for the post-update health pass that runs at the end of
//! `tracedecay update` / `tracedecay post-update` (skippable via `--no-heal`).
//!
//! All tests spawn the real binary against an isolated home directory via
//! `apply_tracedecay_home_env`, so they never touch the real `~/.tracedecay`.

mod common;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::apply_tracedecay_home_env;
use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::storage::BRANCH_META_QUARANTINE_PREFIX;

fn canonical_temp_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        path.to_path_buf()
    }
    #[cfg(not(windows))]
    {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }
}

fn cli_timeout() -> Duration {
    if cfg!(windows) {
        Duration::from_secs(90)
    } else {
        Duration::from_secs(30)
    }
}

/// `post-update` requires `tracedecay` on PATH (for the plugin refresh), so
/// link the test binary into an isolated bin dir and prepend it to PATH.
fn add_tracedecay_path_shim(command: &mut Command, home: &Path) {
    let bin_dir = home.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let shim = bin_dir.join(if cfg!(windows) {
        "tracedecay.exe"
    } else {
        "tracedecay"
    });
    if std::fs::hard_link(env!("CARGO_BIN_EXE_tracedecay"), &shim).is_err() {
        std::fs::copy(env!("CARGO_BIN_EXE_tracedecay"), &shim).unwrap();
    }
    #[cfg(unix)]
    {
        let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim, permissions).unwrap();
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let joined =
        std::env::join_paths(std::iter::once(bin_dir).chain(std::env::split_paths(&path))).unwrap();
    command.env("PATH", joined);
}

fn post_update_command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    apply_tracedecay_home_env(&mut command, home);
    add_tracedecay_path_shim(&mut command, home);
    command
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> Output {
    let mut child = command
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn tracedecay: {e}"));
    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .unwrap_or_else(|e| panic!("failed to poll child: {e}"))
            .is_some()
        {
            return child
                .wait_with_output()
                .unwrap_or_else(|e| panic!("failed to collect output: {e}"));
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .unwrap_or_else(|e| panic!("failed to collect timed-out output: {e}"));
            panic!(
                "tracedecay post-update hung after {:?}\nstdout:\n{}\nstderr:\n{}",
                started.elapsed(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_branch_meta(profile_root: &Path, project_id: &str, content: &str) -> PathBuf {
    let shard = profile_root.join("projects").join(project_id);
    std::fs::create_dir_all(&shard).unwrap();
    let path = shard.join("branch-meta.json");
    std::fs::write(&path, content).unwrap();
    path
}

fn quarantined_branch_meta_files(shard: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(shard)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with(BRANCH_META_QUARANTINE_PREFIX)
            })
        })
        .collect()
}

#[test]
fn post_update_quarantines_corrupt_branch_meta() {
    let home = TempDir::new().unwrap();
    let home_root = canonical_temp_path(home.path());
    let profile_root = home_root.join(".tracedecay");
    let corrupt = write_branch_meta(&profile_root, "proj_corrupt", "{not valid json");
    let valid = write_branch_meta(
        &profile_root,
        "proj_valid",
        r#"{"default_branch":"main","branches":{}}"#,
    );

    let mut command = post_update_command(&home_root);
    command.arg("post-update");
    let output = run_with_timeout(command, cli_timeout());

    assert_success(&output, "post-update");
    assert!(
        !corrupt.exists(),
        "corrupt branch-meta.json should be quarantined away"
    );
    let quarantined = quarantined_branch_meta_files(corrupt.parent().unwrap());
    assert_eq!(
        quarantined.len(),
        1,
        "exactly one quarantine file expected, got {quarantined:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&quarantined[0]).unwrap(),
        "{not valid json",
        "quarantine must preserve the corrupt content as evidence"
    );
    assert_eq!(
        std::fs::read_to_string(&valid).unwrap(),
        r#"{"default_branch":"main","branches":{}}"#,
        "valid branch-meta.json must be left untouched"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Quarantined 1 corrupt branch metadata file(s)"),
        "stderr should report the quarantine\nstderr:\n{stderr}"
    );
}

#[test]
fn post_update_quarantines_schema_corrupt_branch_meta() {
    let home = TempDir::new().unwrap();
    let home_root = canonical_temp_path(home.path());
    let profile_root = home_root.join(".tracedecay");
    // Valid JSON, but not a valid BranchMeta — the runtime treats any schema
    // mismatch as corrupt, so the health pass must quarantine it too.
    let schema_corrupt =
        write_branch_meta(&profile_root, "proj_schema", r#"{"default_branch": 5}"#);

    let mut command = post_update_command(&home_root);
    command.arg("post-update");
    let output = run_with_timeout(command, cli_timeout());

    assert_success(&output, "post-update");
    assert!(
        !schema_corrupt.exists(),
        "schema-corrupt branch-meta.json should be quarantined away"
    );
    let quarantined = quarantined_branch_meta_files(schema_corrupt.parent().unwrap());
    assert_eq!(
        quarantined.len(),
        1,
        "exactly one quarantine file expected, got {quarantined:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&quarantined[0]).unwrap(),
        r#"{"default_branch": 5}"#,
        "quarantine must preserve the corrupt content as evidence"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Quarantined 1 corrupt branch metadata file(s)"),
        "stderr should report the quarantine\nstderr:\n{stderr}"
    );
}

#[test]
fn post_update_no_heal_skips_health_pass() {
    let home = TempDir::new().unwrap();
    let home_root = canonical_temp_path(home.path());
    let profile_root = home_root.join(".tracedecay");
    let corrupt = write_branch_meta(&profile_root, "proj_corrupt", "{not valid json");

    let mut command = post_update_command(&home_root);
    command.args(["post-update", "--no-heal"]);
    let output = run_with_timeout(command, cli_timeout());

    assert_success(&output, "post-update --no-heal");
    assert!(
        corrupt.exists(),
        "--no-heal must leave the corrupt branch-meta.json in place"
    );
    assert_eq!(
        std::fs::read_to_string(&corrupt).unwrap(),
        "{not valid json"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Skipping post-update health pass (--no-heal)"),
        "stderr should say the health pass was skipped\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("Post-update health pass"),
        "the health pass must not run with --no-heal\nstderr:\n{stderr}"
    );
}

#[tokio::test]
async fn post_update_gcs_stale_registry_rows_under_temp_dir_only() {
    let home = TempDir::new().unwrap();
    let home_root = canonical_temp_path(home.path());
    let profile_root = home_root.join(".tracedecay");
    std::fs::create_dir_all(&profile_root).unwrap();

    // The child's system temp directory is pinned to a dir inside the test
    // home, so the GC scope is fully controlled by the fixture.
    let fake_tmp = home_root.join("fake-tmp");
    std::fs::create_dir_all(&fake_tmp).unwrap();
    let fake_tmp = canonical_temp_path(&fake_tmp);
    let live_root = fake_tmp.join("live-project");
    std::fs::create_dir_all(&live_root).unwrap();

    {
        let db = GlobalDb::open_at(&profile_root.join("global.db"))
            .await
            .expect("global db should open");
        db.upsert_code_project(
            "proj_tmp_gone",
            &fake_tmp.join("gone-project"),
            None,
            None,
            Some("main"),
        )
        .await
        .expect("stale temp project should upsert");
        db.upsert_code_project(
            "proj_elsewhere_gone",
            &home_root.join("gone-elsewhere"),
            None,
            None,
            Some("main"),
        )
        .await
        .expect("stale non-temp project should upsert");
        db.upsert_code_project("proj_tmp_live", &live_root, None, None, Some("main"))
            .await
            .expect("live temp project should upsert");
    }

    let mut command = post_update_command(&home_root);
    command
        .arg("post-update")
        .env("TMPDIR", &fake_tmp)
        .env("TMP", &fake_tmp)
        .env("TEMP", &fake_tmp);
    let output = run_with_timeout(command, cli_timeout());

    assert_success(&output, "post-update");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Purged 1 stale temp-root registry row(s)"),
        "stderr should report the temp-root registry GC\nstderr:\n{stderr}"
    );

    let db = GlobalDb::open_at(&profile_root.join("global.db"))
        .await
        .expect("global db should reopen");
    let remaining: Vec<String> = db
        .list_code_projects(usize::MAX)
        .await
        .into_iter()
        .map(|project| project.project_id)
        .collect();
    assert!(
        !remaining.contains(&"proj_tmp_gone".to_string()),
        "stale temp-root row must be purged, remaining: {remaining:?}"
    );
    assert!(
        remaining.contains(&"proj_elsewhere_gone".to_string()),
        "stale row outside the temp dir must be surfaced, not auto-purged: {remaining:?}"
    );
    assert!(
        remaining.contains(&"proj_tmp_live".to_string()),
        "temp-root row with an existing project root must survive: {remaining:?}"
    );
    assert!(
        stderr.contains("stale code project registry row(s) outside the temp directory"),
        "remaining stale rows should be surfaced in the findings summary\nstderr:\n{stderr}"
    );
}
