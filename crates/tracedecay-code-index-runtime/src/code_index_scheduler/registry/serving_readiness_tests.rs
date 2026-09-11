//! Serving notifications cover installation and renewed source admission.

use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_contracts::ResolvedScope;
use tracedecay_domain::ProjectId;

use super::super::graph_activation::install_injected_activation_gate;
use super::{CodeIndexCadenceOutcomeV1, CodeIndexSchedulerRegistryV1};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serving_waiter_tracks_installation_freshness_and_retirement() {
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
    let resolved_scope = ResolvedScope::new(
        generation.manifest().project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        generation.snapshot().reference.clone(),
    )
    .expect("resolved serving scope");
    assert!(
        generation
            .symbols()
            .symbols
            .iter()
            .any(|symbol| symbol.simple_name == "branch_probe")
    );
    let canonical_project = project.canonicalize().expect("canonical project");
    let freshness = {
        let mounted = registry.mounted.lock().await;
        mounted
            .get(&canonical_project)
            .expect("mounted worktree")
            .source_freshness
            .clone()
    };
    {
        let mut state = freshness.state.lock().expect("freshness state");
        state.last_reconciled_at = std::time::Instant::now()
            .checked_sub(state.staleness_threshold + Duration::from_secs(1))
            .expect("age the readiness proof");
    }
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold expired-proof verification in the background");
    changes.borrow_and_update();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(250),
            registry.latest_complete_ready(&project)
        )
        .await
        .expect("expired readiness returns without walking source")
        .is_none(),
        "an expired proof cannot be promoted current before the worker renews it"
    );
    assert!(
        registry
            .latest_complete_serving_for_scope(&resolved_scope)
            .await
            .is_some(),
        "the retained immutable generation remains available to stale reads"
    );
    assert!(
        registry
            .pending_wake_micros_for_scope(&resolved_scope)
            .await
            .is_some_and(|pending| pending != 0),
        "the read coalesces one verification wake on the retained worker"
    );
    assert!(
        !changes.has_changed().expect("live serving subscription"),
        "admission cannot fabricate a renewed source proof"
    );
    drop(admission);
    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            changes.changed().await.expect("source proof renewal");
            if let Some(ready) = registry.latest_complete_ready(&project).await {
                break ready;
            }
        }
    })
    .await
    .expect("the worker renews the unchanged source proof");
    assert_eq!(
        ready.generation().manifest().generation_id,
        published.generation_id
    );

    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold unchanged-source revalidation");
    changes.borrow_and_update();
    let seat_epoch = {
        let mounted = registry.mounted.lock().await;
        mounted
            .get(&canonical_project)
            .expect("mounted worktree")
            .serving_generation_epoch
            .clone()
    };
    let installed_epoch = seat_epoch.load(Ordering::Acquire);
    let scheduler = registry
        .scheduler_handle(&project)
        .await
        .expect("mounted scheduler");
    {
        let mut scheduler = scheduler.lock().expect("scheduler lock");
        scheduler.notify_path(project.join("src/lib.rs"));
        assert!(
            !scheduler.request_fresh_for_query_background(),
            "the hint's own pending pass is the remedy; the query ladder must not \
             escalate it into an observed-change rescan"
        );
    }
    assert!(
        registry.latest_complete_ready(&project).await.is_none(),
        "a true source hint invalidates the seated proof even inside the fresh clock window"
    );
    assert!(!changes.has_changed().expect("live serving subscription"));
    drop(admission);
    let revalidated = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            changes
                .changed()
                .await
                .expect("source revalidation notification");
            if let Some(ready) = registry.latest_complete_ready(&project).await {
                break ready;
            }
        }
    })
    .await
    .expect("unchanged-source reconciliation must wake the retained waiter");
    assert_eq!(
        revalidated.generation().manifest().generation_id,
        published.generation_id,
        "source revalidation must retain the exact unchanged generation"
    );
    assert_eq!(
        seat_epoch.load(Ordering::Acquire),
        installed_epoch,
        "source revalidation must not replace the existing serving slot"
    );
    assert!(
        registry
            .event_to_ready_receipts()
            .last()
            .is_some_and(|receipt| {
                matches!(receipt.outcome, CodeIndexCadenceOutcomeV1::Noop { .. })
            }),
        "the wake must follow the real unchanged-source reconcile"
    );
    assert!(
        matches!(
            publications.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "the unchanged source must wake admission without another publication"
    );

    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .expect("hold source rebuild while checking drift admission");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn changed_branch_probe() {}\n",
    )
    .expect("drift source after the retained generation");
    {
        let mut state = freshness.state.lock().expect("freshness state");
        state.last_reconciled_at = std::time::Instant::now()
            .checked_sub(state.staleness_threshold + Duration::from_secs(1))
            .expect("age the readiness proof");
    }
    assert!(
        registry.latest_complete_ready(&project).await.is_none(),
        "expired proof must not admit a source that changed"
    );
    assert_eq!(
        registry
            .scheduler_handle(&project)
            .await
            .expect("scheduler")
            .lock()
            .expect("scheduler lock")
            .pending_hint_count(),
        Some(0),
        "the read must leave source verification to the canonical worker without fabricating an overflow"
    );
    drop(admission);
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
