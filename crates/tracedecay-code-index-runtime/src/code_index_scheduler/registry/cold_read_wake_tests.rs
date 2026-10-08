//! Cold-read wake behavior while the retained code-index owner is active.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_contracts::ResolvedScope;

use super::super::{
    CodeIndexBuildProgressSlotStateV1, DaemonCodeIndexControlV1, ReconcilePassGuard,
};
use super::{CodeIndexReconcileAdmissionV1, CodeIndexSchedulerRegistryV1};
use crate::code_index::production::CodeIndexExecutionControlV1;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

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
        .mount_worktree(project_id.clone(), &project, fixture.path().join("store"))
        .await
        .expect("mount scheduler");
    let canonical_project = canonical_existing_identity(&project).expect("canonical project");

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
        matches!(
            registry.request_query_background_reconcile(&scope).await,
            CodeIndexReconcileAdmissionV1::Accepted
        ),
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
        matches!(
            registry
                .notify_hook_paths(&project, &["src/main.rs".to_owned()])
                .await,
            super::CodeIndexDemandAdmissionV1::Queued
        ),
        "a real source hint reaches the mounted scheduler"
    );
    assert!(
        invalidation_control.is_cancelled(),
        "source-change evidence must still supersede the in-flight snapshot"
    );

    assert!(
        registry
            .plant_terminal_publication_authority_park_for_test(
                &project,
                "publication authority corrupt before progress",
            )
            .await
    );
    {
        let mounted = registry.mounted.lock().await;
        let worktree = mounted.get(&canonical_project).expect("mounted worktree");
        *worktree.build_progress.write().unwrap() = CodeIndexBuildProgressSlotStateV1::default();
    }
    registry.clear_pending_wake_for_scope(&scope).await;
    let admission_result = registry.request_query_background_reconcile(&scope).await;
    assert!(
        matches!(
            admission_result,
            CodeIndexReconcileAdmissionV1::PublicationAuthorityCorrupt(ref parked)
                if parked.reason == "publication authority corrupt before progress"
        ),
        "query admission must use the terminal park without a progress snapshot: {admission_result:?}"
    );
    assert_eq!(
        registry.pending_wake_micros_for_scope(&scope).await,
        Some(0),
        "terminal query admission must not enqueue another reconcile"
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_hint_interrupts_attribution_and_owned_noop_retries_same_generation() {
    use super::AttributionPreparationControlV1;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracedecay_code_index::production::{CodeIndexInterruptionV1, CodeIndexProductionErrorV1};
    use tracedecay_domain::ProviderEvaluationStateV1;
    use tracedecay_runtime_core::cancellation::CancellationToken;

    struct PausedControl {
        inner: AttributionPreparationControlV1,
        checks: AtomicUsize,
        entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl CodeIndexExecutionControlV1 for PausedControl {
        fn is_cancelled(&self) -> bool {
            if self.checks.fetch_add(1, Ordering::Relaxed) == 2 {
                self.entered
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            self.inner.is_cancelled()
        }
        fn is_deadline_exceeded(&self) -> bool {
            self.inner.is_deadline_exceeded()
        }
    }

    let fixture = TempDir::new().unwrap();
    let project = fixture.path().join("project");
    fs::create_dir_all(project.join("tests")).unwrap();
    fs::write(
        project.join("tests/example.rs"),
        "fn helper() {}\n#[test] fn first() { helper(); }\n#[test] fn second() { first(); }\n",
    )
    .unwrap();
    run_git_in(&project, &["init", "-q", "-b", "main"]);
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-qm", "fixture"]);
    let registry = CodeIndexSchedulerRegistryV1::with_background_reconcile_permits(1, 1);
    let admission = registry
        .background_reconcile_admission()
        .acquire_owned()
        .await
        .unwrap();
    let project_id = tracedecay_domain::ProjectId::new("project.attribution-interruption").unwrap();
    registry
        .mount_worktree(project_id.clone(), &project, fixture.path().join("store"))
        .await
        .unwrap();
    let root = canonical_existing_identity(&project).unwrap();
    let (generation, scope, control) = {
        let mounted = registry.mounted.lock().await;
        let worktree = mounted.get(&root).unwrap();
        let mut scheduler = worktree.scheduler.lock().unwrap();
        scheduler.reconcile_now().unwrap();
        let latest = scheduler.latest_complete().unwrap();
        let scope = ResolvedScope::new(
            project_id,
            worktree.repository_id.clone(),
            worktree.worktree_id.clone(),
            latest.generation().snapshot().reference.clone(),
        )
        .unwrap();
        let control = AttributionPreparationControlV1 {
            generation: DaemonCodeIndexControlV1::new(
                Arc::clone(&worktree.serving_generation_epoch),
                Arc::clone(&worktree.shutting_down),
            ),
            source_epoch: Arc::clone(&worktree.epoch),
            expected_source_epoch: worktree.epoch.load(Ordering::Acquire),
        };
        (latest.generation_handle(), scope, control)
    };
    let generation_id = generation.manifest().generation_id.clone();
    let (entered, observed) = tokio::sync::oneshot::channel();
    let (release, receive) = std::sync::mpsc::channel();
    let control = Arc::new(PausedControl {
        inner: control,
        checks: AtomicUsize::new(0),
        entered: std::sync::Mutex::new(Some(entered)),
        release: std::sync::Mutex::new(receive),
    });
    let preparing = Arc::clone(&generation);
    let preparing_control = Arc::clone(&control);
    let task = tokio::task::spawn_blocking(move || {
        preparing.prepare_test_attribution(preparing_control.as_ref())
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), observed)
        .await
        .unwrap()
        .unwrap();
    registry.request_complete_generation(&root).await;
    assert!(
        !control.inner.is_cancelled(),
        "ordinary decode demand does not cancel optional work"
    );
    registry
        .notify_hook_paths(&root, &["tests/example.rs".to_owned()])
        .await;
    assert!(
        control.inner.is_cancelled(),
        "real source demand interrupts the running preparation"
    );
    release.send(()).unwrap();
    assert!(matches!(
        task.await.unwrap(),
        Err(CodeIndexProductionErrorV1::Interrupted(
            CodeIndexInterruptionV1::Cancelled
        ))
    ));
    assert_eq!(
        generation.test_attribution_read().provider_state,
        ProviderEvaluationStateV1::Cancelled
    );
    let mut signals =
        super::owner_signals::CodeIndexOwnerSignalsV1::subscribe(&registry, &root).await;
    drop(admission);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while registry.retained_text_owner_for_root(&root).await.is_none() {
            signals.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let ready = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        registry.await_test_attribution_for_scope(
            &root,
            &scope,
            &generation_id,
            &CancellationToken::new(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(ready, ProviderEvaluationStateV1::Partial);
    assert!(
        generation.test_attribution_read().evidence.is_some(),
        "the owned no-op retries the interrupted immutable generation memo"
    );
    assert_eq!(
        registry.latest_generation_id(&root).await,
        Some(generation_id)
    );
    registry.shutdown().await;
}
