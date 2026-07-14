use super::{
    HookBranchWriteRequest, HookBranchWriteResult, HookBranchWriter, McpServer,
    direct_background_refresh_writer,
};
use crate::config::PinnedUserDataDir;
use crate::daemon::HookAgent;
use crate::mcp::hook_events::HookEventPlan;
use crate::tracedecay::TraceDecay;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedWrite {
    root: PathBuf,
    branch: String,
    incremental_sync_agent: Option<HookAgent>,
}

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

fn recording_writer(
    observed: Arc<Mutex<Vec<ObservedWrite>>>,
    result: HookBranchWriteResult,
) -> HookBranchWriter {
    Arc::new(move |request: HookBranchWriteRequest| {
        let observed = Arc::clone(&observed);
        let result = result.clone();
        Box::pin(async move {
            observed
                .lock()
                .expect("recording lock")
                .push(ObservedWrite {
                    root: request.root,
                    branch: request.branch,
                    incremental_sync_agent: request.incremental_sync_agent,
                });
            Ok(result)
        })
    })
}

fn assert_branch_not_tracked(cg: &TraceDecay, branch: &str) {
    let tracked = crate::branch_meta::load_branch_meta(&cg.store_layout().data_root)
        .is_some_and(|meta| meta.is_tracked(branch));
    assert!(
        !tracked,
        "injected hook branch writer must not fall back to direct branch tracking"
    );
}

#[tokio::test]
async fn add_branch_plan_uses_injected_writer_without_direct_fallback() {
    let (cg, dir, _pin) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    let branch = "injected-branch";
    git(&root, &["branch", branch]);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let writer = recording_writer(
        Arc::clone(&observed),
        HookBranchWriteResult {
            branch_outcome: crate::branch::BranchAddOutcome::NotIndexed,
            refresh_file_token_map: false,
        },
    );
    let server = McpServer::new_with_dbs_and_reconcilers_and_writers(
        cg,
        None,
        None,
        None,
        true,
        None,
        None,
        writer,
        direct_background_refresh_writer(),
    )
    .await;
    let snapshot = server.cg_snapshot().await;

    server
        .run_hook_event_plan(
            snapshot.clone(),
            &root,
            HookEventPlan::AddBranch(branch.into()),
        )
        .await;

    assert_eq!(
        observed.lock().expect("recording lock").as_slice(),
        &[ObservedWrite {
            root: root.clone(),
            branch: branch.to_string(),
            incremental_sync_agent: None,
        }]
    );
    assert_branch_not_tracked(&snapshot, branch);
    server.shutdown().await;
}

#[tokio::test]
async fn add_branch_at_plan_delegates_open_and_sync_without_direct_fallback() {
    let (cg, dir, _pin) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    let branch = "injected-worktree-branch";
    git(&root, &["branch", branch]);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let writer = recording_writer(
        Arc::clone(&observed),
        HookBranchWriteResult {
            branch_outcome: crate::branch::BranchAddOutcome::AlreadyTracked,
            refresh_file_token_map: false,
        },
    );
    let server = McpServer::new_with_dbs_and_reconcilers_and_writers(
        cg,
        None,
        None,
        None,
        true,
        None,
        None,
        writer,
        direct_background_refresh_writer(),
    )
    .await;
    let snapshot = server.cg_snapshot().await;

    server
        .run_hook_event_plan(
            snapshot.clone(),
            &root,
            HookEventPlan::AddBranchAt {
                root: root.clone(),
                branch: branch.to_string(),
                agent: HookAgent::Codex,
            },
        )
        .await;

    assert_eq!(
        observed.lock().expect("recording lock").as_slice(),
        &[ObservedWrite {
            root: root.clone(),
            branch: branch.to_string(),
            incremental_sync_agent: Some(HookAgent::Codex),
        }]
    );
    assert_branch_not_tracked(&snapshot, branch);
    server.shutdown().await;
}
