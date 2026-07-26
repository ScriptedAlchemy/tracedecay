use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;

use crate::config::PinnedUserDataDir;
use crate::tracedecay::TraceDecay;

pub(super) fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new(crate::git::git_program())
        .current_dir(root)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) struct WriterTestFixtureAuthority {
    _pin: PinnedUserDataDir,
    _runtime: Arc<crate::application::host_admission::HostAdmissionTestRuntimeV1>,
}

pub(super) async fn init_indexed_repo() -> (TraceDecay, TempDir, WriterTestFixtureAuthority) {
    let pin = PinnedUserDataDir::new();
    let dir = TempDir::new().expect("temp repo");
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "t@t.com"]);
    git(root, &["config", "user.name", "T"]);
    std::fs::write(root.join(".gitignore"), ".tracedecay/\n").expect("write gitignore");
    std::fs::create_dir_all(root.join("src")).expect("create src");
    std::fs::write(root.join("src/a.rs"), "pub fn a() {}\n").expect("write source");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);
    let (cg, runtime) =
        TraceDecay::init_test_fixture_with_registered_runtime(root, "project.mcp-writer")
            .await
            .expect("init");
    cg.index_all().await.expect("index");
    let mut config = crate::config::load_config(root).expect("load config");
    config.sync.session_start_sync = false;
    crate::config::save_config(root, &config).expect("disable startup sync");
    (
        cg,
        dir,
        WriterTestFixtureAuthority {
            _pin: pin,
            _runtime: runtime,
        },
    )
}
