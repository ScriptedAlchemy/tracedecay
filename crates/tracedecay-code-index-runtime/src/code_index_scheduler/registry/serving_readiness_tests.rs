//! Serving notifications follow actual installation, independently of sealing.

use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_domain::ProjectId;

use super::super::graph_activation::install_injected_activation_gate;
use super::CodeIndexSchedulerRegistryV1;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serving_waiter_wakes_after_sealed_generation_finishes_activation() {
    let fixture = TempDir::new().expect("fixture root");
    let project = fixture.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("source directory");
    std::fs::write(project.join("src/lib.rs"), "pub fn branch_probe() {}\n")
        .expect("fixture source");
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        assert!(
            Command::new(tracedecay_runtime_core::git::try_git_program().expect("git executable"))
                .current_dir(&project)
                .args(args)
                .status()
                .expect("run git")
                .success()
        );
    }
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold worker before publication");
    registry
        .mount_worktree(
            ProjectId::new("project.serving-readiness").expect("project identity"),
            &project,
            fixture.path().join("store"),
            None,
        )
        .await
        .expect("mount worktree");
    let scope = registry
        .serving_code_scope(&project)
        .await
        .expect("mounted scope");
    let gate = install_injected_activation_gate(&scope.worktree_id);
    let mut publications = registry.subscribe_generation_publications();
    let mut changes = registry
        .subscribe_serving_generation_changes(&project)
        .await
        .expect("serving subscription");
    drop(admission);
    let published = tokio::time::timeout(Duration::from_secs(5), publications.recv())
        .await
        .expect("sealed publication deadline")
        .expect("sealed publication");
    tokio::time::timeout(Duration::from_secs(5), gate.wait_until_started())
        .await
        .expect("activation gate reached after sealing");
    assert!(
        registry
            .serving_code_scope(&project)
            .await
            .expect("mounted scope")
            .serving_generation
            .is_none(),
        "seal alone must not claim serving readiness"
    );
    assert!(!changes.has_changed().expect("live serving subscription"));
    gate.release();
    let generation = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            changes
                .changed()
                .await
                .expect("serving change notification");
            if let Some(generation) = registry
                .serving_code_scope(&project)
                .await
                .expect("mounted scope")
                .serving_generation
            {
                break generation;
            }
        }
    })
    .await
    .expect("serving installation must wake the waiter without another seal");
    assert_eq!(generation.manifest().generation_id, published.generation_id);
    assert!(
        generation
            .symbols()
            .symbols
            .iter()
            .any(|symbol| symbol.simple_name == "branch_probe")
    );
    changes.borrow_and_update();
    registry.cancel();
    tokio::time::timeout(Duration::from_secs(5), changes.changed())
        .await
        .expect("cancellation must wake serving waiters")
        .expect("mounted owner remains observable until shutdown drains");
    assert!(
        registry
            .serving_code_scope(&project)
            .await
            .expect("mounted scope")
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
    );
    registry.shutdown().await;
    assert!(registry.serving_code_scope(&project).await.is_none());
    changes.borrow_and_update();
    assert!(
        changes.changed().await.is_err(),
        "removed owner must close its subscription"
    );
}
