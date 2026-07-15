use super::writer_test_support::{git, init_indexed_repo};
use super::{
    HookBranchWriteRequest, HookBranchWriteResult, HookBranchWriter, McpServer,
    McpServerConstructionContext, direct_background_refresh_writer,
};
use crate::daemon::HookAgent;
use crate::errors::TraceDecayError;
use crate::mcp::hook_events::{self, HookEventPlan};
use crate::tracedecay::TraceDecay;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedWrite {
    root: PathBuf,
    branch: String,
    incremental_sync_agent: Option<HookAgent>,
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

fn failing_writer(observed: Arc<Mutex<Vec<ObservedWrite>>>) -> HookBranchWriter {
    Arc::new(move |request: HookBranchWriteRequest| {
        let observed = Arc::clone(&observed);
        Box::pin(async move {
            observed
                .lock()
                .expect("recording lock")
                .push(ObservedWrite {
                    root: request.root,
                    branch: request.branch,
                    incremental_sync_agent: request.incremental_sync_agent,
                });
            Err(TraceDecayError::Config {
                message: "injected writer failure".to_string(),
            })
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
    let server =
        McpServer::new_with_dbs_and_reconcilers_and_writers(McpServerConstructionContext {
            cg,
            scope_prefix: None,
            global_db: None,
            registry_db: None,
            session_db: None,
            user_session_db: None,
            allow_default_registry_fallback: true,
            automation_scheduler_reconciler: None,
            database_owner_reconciler: None,
            dashboard_automation_writer: crate::dashboard::direct_dashboard_automation_writer(),
            hook_branch_writer: writer,
            background_refresh_writer: direct_background_refresh_writer(),
        })
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
    let server =
        McpServer::new_with_dbs_and_reconcilers_and_writers(McpServerConstructionContext {
            cg,
            scope_prefix: None,
            global_db: None,
            registry_db: None,
            session_db: None,
            user_session_db: None,
            allow_default_registry_fallback: true,
            automation_scheduler_reconciler: None,
            database_owner_reconciler: None,
            dashboard_automation_writer: crate::dashboard::direct_dashboard_automation_writer(),
            hook_branch_writer: writer,
            background_refresh_writer: direct_background_refresh_writer(),
        })
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

#[tokio::test]
async fn sync_current_branch_deferred_writer_does_not_fall_back_to_direct_sync() {
    let (cg, dir, _pin) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    let branch = "deferred-branch";
    let observed = Arc::new(Mutex::new(Vec::new()));
    let writer = recording_writer(
        Arc::clone(&observed),
        HookBranchWriteResult {
            branch_outcome: crate::branch::BranchAddOutcome::Deferred,
            refresh_file_token_map: false,
        },
    );
    let server =
        McpServer::new_with_dbs_and_reconcilers_and_writers(McpServerConstructionContext {
            cg,
            scope_prefix: None,
            global_db: None,
            registry_db: None,
            session_db: None,
            user_session_db: None,
            allow_default_registry_fallback: true,
            automation_scheduler_reconciler: None,
            database_owner_reconciler: None,
            dashboard_automation_writer: crate::dashboard::direct_dashboard_automation_writer(),
            hook_branch_writer: writer,
            background_refresh_writer: direct_background_refresh_writer(),
        })
        .await;
    let snapshot = server.cg_snapshot().await;
    let marker =
        hook_events::sync_marker_path(&snapshot.store_layout().data_root, HookAgent::Codex);

    server
        .run_hook_event_plan(
            snapshot,
            &root,
            HookEventPlan::SyncCurrentBranch {
                branch: branch.to_string(),
                agent: HookAgent::Codex,
            },
        )
        .await;

    assert_eq!(
        observed.lock().expect("recording lock").as_slice(),
        &[ObservedWrite {
            root,
            branch: branch.to_string(),
            incremental_sync_agent: Some(HookAgent::Codex),
        }]
    );
    assert!(
        !marker.exists(),
        "deferred writer must not trigger direct sync"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn sync_current_branch_writer_error_does_not_fall_back_to_direct_sync() {
    let (cg, dir, _pin) = init_indexed_repo().await;
    let root = dir.path().to_path_buf();
    let branch = "failed-branch";
    let observed = Arc::new(Mutex::new(Vec::new()));
    let server =
        McpServer::new_with_dbs_and_reconcilers_and_writers(McpServerConstructionContext {
            cg,
            scope_prefix: None,
            global_db: None,
            registry_db: None,
            session_db: None,
            user_session_db: None,
            allow_default_registry_fallback: true,
            automation_scheduler_reconciler: None,
            database_owner_reconciler: None,
            dashboard_automation_writer: crate::dashboard::direct_dashboard_automation_writer(),
            hook_branch_writer: failing_writer(Arc::clone(&observed)),
            background_refresh_writer: direct_background_refresh_writer(),
        })
        .await;
    let snapshot = server.cg_snapshot().await;
    let marker =
        hook_events::sync_marker_path(&snapshot.store_layout().data_root, HookAgent::Codex);

    server
        .run_hook_event_plan(
            snapshot,
            &root,
            HookEventPlan::SyncCurrentBranch {
                branch: branch.to_string(),
                agent: HookAgent::Codex,
            },
        )
        .await;

    assert_eq!(
        observed.lock().expect("recording lock").as_slice(),
        &[ObservedWrite {
            root,
            branch: branch.to_string(),
            incremental_sync_agent: Some(HookAgent::Codex),
        }]
    );
    assert!(
        !marker.exists(),
        "writer error must not trigger direct sync"
    );
    server.shutdown().await;
}
