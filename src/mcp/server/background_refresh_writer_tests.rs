use super::{
    BackgroundRefreshRequest, BackgroundRefreshWriter, McpServer, direct_hook_branch_writer,
};
use crate::config::PinnedUserDataDir;
use crate::tracedecay::TraceDecay;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) {
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

async fn init_indexed_repo() -> (TraceDecay, TempDir, PinnedUserDataDir) {
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
    let cg = TraceDecay::init(root).await.expect("init");
    cg.index_all().await.expect("index");
    let mut config = crate::config::load_config(root).expect("load config");
    config.sync.session_start_sync = false;
    crate::config::save_config(root, &config).expect("disable startup sync");
    (cg, dir, pin)
}

#[tokio::test]
async fn read_refresh_uses_injected_writer_without_direct_fallback() {
    let (cg, dir, _pin) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    let source_path = root.join("src/a.rs");
    std::fs::write(&source_path, "pub fn a() { println!(\"changed\"); }\n").expect("modify source");
    std::fs::File::options()
        .write(true)
        .open(&source_path)
        .expect("open modified source")
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
        .expect("advance source mtime");
    assert!(
        cg.find_stale_files()
            .await
            .iter()
            .any(|path| path == "src/a.rs"),
        "fixture must be stale before refresh"
    );

    let observed = Arc::new(Mutex::new(Vec::<(PathBuf, usize)>::new()));
    let refresh_writer: BackgroundRefreshWriter = {
        let observed = Arc::clone(&observed);
        Arc::new(move |request: BackgroundRefreshRequest| {
            let observed = Arc::clone(&observed);
            Box::pin(async move {
                observed
                    .lock()
                    .expect("recording lock")
                    .push((request.project_root, request.full_sync_escalation_files));
                Ok(Some(HashMap::from([("injected.rs".to_string(), 41)])))
            })
        })
    };
    let server = McpServer::new_with_dbs_and_reconcilers_and_writers(
        cg,
        None,
        None,
        None,
        None,
        None,
        true,
        None,
        None,
        crate::dashboard::direct_dashboard_automation_writer(),
        direct_hook_branch_writer(),
        refresh_writer,
    )
    .await;
    let snapshot = server.cg_snapshot().await;
    server
        .background_refresh_running
        .store(true, Ordering::Release);

    server.spawn_read_refresh_task(&snapshot, 17);

    tokio::time::timeout(Duration::from_secs(5), async {
        while server.background_refresh_running.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("injected refresh settles");

    assert_eq!(
        observed.lock().expect("recording lock").as_slice(),
        &[(root, 17)]
    );
    assert_eq!(
        server.file_token_map_snapshot(),
        HashMap::from([("injected.rs".to_string(), 41)])
    );
    assert!(
        snapshot
            .find_stale_files()
            .await
            .iter()
            .any(|path| path == "src/a.rs"),
        "injected refresh must not execute the direct open/sync fallback"
    );
    assert_ne!(
        server
            .last_background_refresh_done_at
            .load(Ordering::Acquire),
        0,
        "completion timestamp must be preserved"
    );
    server.shutdown().await;
}
