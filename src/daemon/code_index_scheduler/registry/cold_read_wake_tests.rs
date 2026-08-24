//! Cold-read wake behavior while the retained code-index owner is active.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_application::ResolvedScope;

use super::super::{DaemonCodeIndexControlV1, ReconcilePassGuard};
use super::CodeIndexSchedulerRegistryV1;
use crate::code_index::production::CodeIndexExecutionControlV1;

#[tokio::test]
async fn cold_read_wakes_do_not_cancel_an_in_flight_reconcile_snapshot() {
    let fixture = TempDir::new().expect("fixture root");
    let project = fixture.path().join("project");
    fs::create_dir_all(project.join("src")).expect("create source root");
    fs::write(project.join("src/main.rs"), "fn main() {}\n").expect("write source");
    run_git_in(&project, &["init", "-q", "-b", "main"]);
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-qm", "fixture"]);

    let project_id =
        tracedecay_domain::ProjectId::new("project.cold-read-wake").expect("project identity");
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold background worker at its dequeue point");
    registry
        .mount_worktree(
            project_id.clone(),
            &project,
            fixture.path().join("store"),
            None,
        )
        .await
        .expect("mount scheduler");
    let canonical_project = project.canonicalize().expect("canonical project");

    let (scope, scheduler, hints, epoch, shutting_down, reconcile_in_progress) = {
        let mounted = registry.mounted.lock().await;
        let worktree = mounted.get(&canonical_project).expect("mounted worktree");
        let reference = worktree
            .scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .identity()
            .head_ref()
            .cloned()
            .expect("branch reference");
        (
            ResolvedScope::new(
                project_id,
                worktree.repository_id.clone(),
                worktree.worktree_id.clone(),
                Some(reference),
            )
            .expect("resolved scope"),
            Arc::clone(&worktree.scheduler),
            Arc::clone(&worktree.hints),
            Arc::clone(&worktree.epoch),
            Arc::clone(&worktree.shutting_down),
            Arc::clone(&worktree.reconcile_in_progress),
        )
    };
    registry.clear_pending_wake_for_scope(&scope).await;
    let reconcile_pass = ReconcilePassGuard::enter(&reconcile_in_progress);

    let latest_control =
        DaemonCodeIndexControlV1::new(Arc::clone(&epoch), Arc::clone(&shutting_down));
    assert!(
        registry.latest_complete_fresh(&project).await.is_none(),
        "a cold read must stay unavailable until the retained owner publishes"
    );
    assert!(
        !latest_control.is_cancelled(),
        "a cold latest-generation read may wake the owner but must not supersede its snapshot"
    );
    assert_ne!(
        registry
            .pending_wake_micros_for_scope(&scope)
            .await
            .expect("mounted worktree"),
        0,
        "the non-invalidating read must retain one authoritative follow-up wake"
    );
    assert_eq!(
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending_hint_count(),
        None,
        "the cold latest-generation wake must require an authoritative overflow reconcile"
    );

    registry.clear_pending_wake_for_scope(&scope).await;
    hints
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let query_control =
        DaemonCodeIndexControlV1::new(Arc::clone(&epoch), Arc::clone(&shutting_down));
    assert!(
        registry.request_query_background_reconcile(&scope).await,
        "a cold query still records one follow-up wake"
    );
    assert!(
        !query_control.is_cancelled(),
        "a cold search may wake the owner but must not supersede its snapshot"
    );
    assert_eq!(
        scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending_hint_count(),
        None,
        "the cold query wake must require an authoritative overflow reconcile"
    );

    let invalidation_control = DaemonCodeIndexControlV1::new(epoch, shutting_down);
    assert!(
        registry
            .notify_hook_paths(&project, &["src/main.rs".to_owned()])
            .await,
        "a real source hint reaches the mounted scheduler"
    );
    assert!(
        invalidation_control.is_cancelled(),
        "source-change evidence must still supersede the in-flight snapshot"
    );

    drop(reconcile_pass);
    drop(admission);
    registry.shutdown().await;
}

fn run_git_in(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
