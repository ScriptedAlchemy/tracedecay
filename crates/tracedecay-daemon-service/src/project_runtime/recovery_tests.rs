use std::any::Any;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::{ProjectRuntimeRegistryError, ProjectRuntimeRegistryV1, RecoveryCancelProbe};

type Component = Arc<dyn Any + Send + Sync>;

fn component(mark: u32) -> Component {
    Arc::new(mark)
}

fn root(name: &str) -> PathBuf {
    PathBuf::from("/projects").join(name)
}

#[tokio::test]
async fn quiescing_exact_roots_reopens_publication_after_the_guard_drops() {
    let registry = ProjectRuntimeRegistryV1::default();
    let recovered = root("recovered");
    let retained = root("retained-during-recovery");
    registry
        .publish(recovered.clone(), component(1))
        .await
        .unwrap();
    registry
        .publish(retained.clone(), component(2))
        .await
        .unwrap();

    let quiescence = registry
        .quiesce_roots(&BTreeSet::from([recovered.clone()]))
        .await
        .expect("recovery quiescence drains the exact runtime");
    assert!(!registry.holds::<Component>(&recovered).await);
    assert_eq!(
        registry.publish(recovered.clone(), component(3)).await,
        Err(ProjectRuntimeRegistryError::Closed),
        "the recovered root remains fenced while database replacement is active"
    );
    assert!(registry.holds::<Component>(&retained).await);

    drop(quiescence);
    registry
        .publish(recovered, component(4))
        .await
        .expect("recovery quiescence must not poison permanent retirement");
}

#[tokio::test]
async fn permanent_retirement_outlives_an_existing_recovery_quiescence() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("deleted-during-recovery");
    registry
        .publish(project.clone(), component(1))
        .await
        .unwrap();
    let roots = BTreeSet::from([project.clone()]);
    let quiescence = registry
        .quiesce_roots(&roots)
        .await
        .expect("recovery quiescence");

    assert!(registry.retire_roots(&roots).await);
    drop(quiescence);
    assert_eq!(
        registry.publish(project, component(2)).await,
        Err(ProjectRuntimeRegistryError::Closed),
        "dropping a temporary fence must not undo permanent retirement"
    );
}

#[tokio::test]
async fn register_or_reconcile_cannot_republish_under_a_quiesced_root() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("reconciling-during-recovery");
    let retained = root("retained-during-reconcile");
    registry
        .publish(retained.clone(), component(1))
        .await
        .unwrap();
    let quiescence = registry
        .quiesce_roots(&BTreeSet::from([project.clone()]))
        .await
        .expect("recovery quiescence");

    let rejected = registry
        .register_or_reconcile::<Component, ProjectRuntimeRegistryError, _, _, _>(
            project.clone(),
            |_| Ok(()),
            || async { Ok(component(2)) },
        )
        .await;

    assert_eq!(rejected, Err(ProjectRuntimeRegistryError::Closed));
    assert!(!registry.holds::<Component>(&project).await);
    assert!(registry.holds::<Component>(&retained).await);
    drop(quiescence);
    registry
        .register_or_reconcile::<Component, ProjectRuntimeRegistryError, _, _, _>(
            project.clone(),
            |_| Ok(()),
            || async { Ok(component(3)) },
        )
        .await
        .expect("publication resumes only after recovery releases the root");
    assert!(registry.holds::<Component>(&project).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_request_snapshot_must_settle_before_project_quiescence_drains() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("cached-route-during-recovery");
    registry
        .publish(project.clone(), component(1))
        .await
        .unwrap();

    let snapshot = registry.request_runtimes(Some(&project), None).await;
    assert!(snapshot.is_admitted());
    let quiescing_registry = registry.clone();
    let quiescing_root = project.clone();
    let quiescence = tokio::spawn(async move {
        quiescing_registry
            .quiesce_roots(&BTreeSet::from([quiescing_root]))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !registry.lock_root_fences().contains(&project) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("quiescence installs its admission fence");

    assert!(
        !quiescence.is_finished(),
        "a cached request snapshot must retain its project runtime until settlement"
    );
    assert_eq!(
        registry.publish(project.clone(), component(2)).await,
        Err(ProjectRuntimeRegistryError::Closed),
        "quiescence fences a replacement while the admitted request settles"
    );

    drop(snapshot);
    let guard = quiescence
        .await
        .expect("quiescence task")
        .expect("runtime drains after request settlement");
    assert!(!registry.holds::<Component>(&project).await);
    drop(guard);
}

#[tokio::test]
async fn project_quiescence_rejects_new_cached_route_snapshots() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("route-admission-during-recovery");
    registry
        .publish(project.clone(), component(1))
        .await
        .unwrap();
    let guard = registry
        .quiesce_roots(&BTreeSet::from([project.clone()]))
        .await
        .expect("project quiescence");

    let snapshot = registry.request_runtimes(Some(&project), None).await;
    assert!(!snapshot.is_admitted());
    assert!(snapshot.feedback.is_none());
    assert!(snapshot.feedback_owner.is_none());
    assert!(snapshot.configuration.is_none());
    assert!(snapshot.work.is_none());
    assert!(snapshot.retained.is_none());
    assert!(snapshot.lsp_owner.is_none());

    drop(guard);
}

#[tokio::test]
async fn request_admission_cannot_cross_project_runtime_registries() {
    let first = ProjectRuntimeRegistryV1::default();
    let second = ProjectRuntimeRegistryV1::default();
    let project = root("same-root-distinct-registry");
    first
        .publish(project.clone(), component(1))
        .await
        .expect("first registry publication");
    second
        .publish(project.clone(), component(2))
        .await
        .expect("second registry publication");
    let admission = first
        .admit_request(&project, None)
        .expect("first registry admission");

    let snapshot = second.request_runtimes_with_admission(&project, None, &admission);
    assert!(!snapshot.is_admitted());
    assert!(snapshot.feedback.is_none());
    assert!(snapshot.lsp_owner.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn captured_admission_continues_after_quiescence_installs_its_fence() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("captured-admission-continuation");
    registry
        .publish(project.clone(), component(1))
        .await
        .expect("project runtime publication");
    let admission = registry
        .admit_request(&project, None)
        .expect("outer request admission");
    let quiescing_registry = registry.clone();
    let quiescing_root = project.clone();
    let quiescence = tokio::spawn(async move {
        quiescing_registry
            .quiesce_roots(&BTreeSet::from([quiescing_root]))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !registry.lock_root_fences().contains(&project) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("quiescence installs its fence");

    let continuation = registry.request_runtimes_with_admission(&project, None, &admission);
    assert!(
        continuation.is_admitted(),
        "a nested route must settle through the exact captured outer admission"
    );
    assert!(
        !quiescence.is_finished(),
        "quiescence must continue waiting for the outer admitted request"
    );

    drop(continuation);
    drop(admission);
    let guard = quiescence
        .await
        .expect("quiescence task")
        .expect("quiescence completes after outer settlement");
    drop(guard);
}

/// Recovery owners used to be cancelled deep inside the async `shut_down_all`
/// drain, so they kept running through every shutdown phase in front of it —
/// blocked-interval and workflow-census scans were still logging seconds after
/// the daemon began shutting down. `begin_shutdown` runs in the synchronous
/// `cancel_admissions` half of shutdown, so the cancellation must already have
/// happened by the time it returns.
#[tokio::test]
async fn begin_shutdown_cancels_project_recovery_owners_before_it_returns() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("recovery-owner-at-prepare-time");
    let probe = RecoveryCancelProbe::default();
    registry
        .publish(project.clone(), probe.clone())
        .await
        .expect("recovery cancellation registers at owner mount");
    assert!(
        !probe.is_cancelled(),
        "a mounted recovery owner runs until shutdown begins"
    );

    registry.begin_shutdown();

    // No await between the call and this assertion: the sweep is synchronous.
    assert!(
        probe.is_cancelled(),
        "begin_shutdown must cancel retained recovery owners before returning"
    );
}

/// The log-shaped invariant: once `begin_shutdown` has returned, no recovery
/// owner starts another cycle.
///
/// Time is paused on a current-thread runtime, so the worker cannot be polled
/// while the synchronous `begin_shutdown` runs — the cycle count read after it
/// returns is exactly the count at the moment it returned.
#[tokio::test(start_paused = true)]
async fn no_recovery_cycle_starts_after_begin_shutdown_returns() {
    const CYCLE: Duration = Duration::from_secs(5);

    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("recovery-cycles-during-shutdown");
    let probe = RecoveryCancelProbe::default();
    registry
        .publish(project.clone(), probe.clone())
        .await
        .expect("recovery cancellation registers at owner mount");

    let cycles = Arc::new(AtomicUsize::new(0));
    let worker_cycles = Arc::clone(&cycles);
    let cancellation = probe.cancellation.clone();
    let worker = tokio::spawn(async move {
        loop {
            // The shape every recovery owner's loop uses: a biased
            // cancellation arm in front of the interval, so a cancelled owner
            // starts no further cycle.
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                () = tokio::time::sleep(CYCLE) => {}
            }
            worker_cycles.fetch_add(1, Ordering::SeqCst);
        }
    });

    for _ in 0..3 {
        tokio::time::advance(CYCLE).await;
    }
    let before_shutdown = cycles.load(Ordering::SeqCst);
    assert!(
        before_shutdown > 0,
        "the recovery owner must be cycling before shutdown begins"
    );

    registry.begin_shutdown();
    let at_return = cycles.load(Ordering::SeqCst);

    tokio::time::advance(CYCLE * 4).await;
    worker.await.expect("recovery worker");
    assert_eq!(
        cycles.load(Ordering::SeqCst),
        at_return,
        "no recovery cycle may start after begin_shutdown returns"
    );
}

/// Cancelling at prepare time is only safe because registry admission is
/// one-way. `shut_down_all` is retryable — a failed drain clears
/// `shutdown_started` — so a retry re-enters `begin_shutdown`; this pins the
/// property that makes that harmless: nothing can re-admit an owner for the
/// retry to run, and the sweep is idempotent.
#[tokio::test]
async fn a_retried_shutdown_cannot_readmit_a_cancelled_recovery_owner() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("retry-after-a-failed-shutdown");
    let probe = RecoveryCancelProbe::default();
    registry
        .publish(project.clone(), probe.clone())
        .await
        .expect("recovery cancellation registers at owner mount");

    registry.begin_shutdown();
    assert!(probe.is_cancelled());

    assert_eq!(
        registry
            .publish(project.clone(), RecoveryCancelProbe::default())
            .await,
        Err(ProjectRuntimeRegistryError::Closed),
        "a shutting-down registry can never admit another recovery owner"
    );

    // A retried attempt re-enters the sweep; it must stay a no-op.
    registry.begin_shutdown();
    assert!(probe.is_cancelled());
    assert!(registry.shut_down_all().await);
    assert_eq!(
        registry
            .publish(project, RecoveryCancelProbe::default())
            .await,
        Err(ProjectRuntimeRegistryError::Closed),
        "shutdown admission never reopens"
    );
}

/// Targeted single-project retirement must not reach through the whole
/// registry the way daemon shutdown does: a project being retired or
/// database-replaced leaves every other project's recovery owner running.
#[tokio::test]
async fn retiring_one_root_leaves_other_projects_recovery_owners_running() {
    let registry = ProjectRuntimeRegistryV1::default();
    let retired = root("retired-project");
    let retained = root("retained-project");
    let retired_probe = RecoveryCancelProbe::default();
    let retained_probe = RecoveryCancelProbe::default();
    registry
        .publish(retired.clone(), retired_probe.clone())
        .await
        .expect("retired project publication");
    registry
        .publish(retained.clone(), retained_probe.clone())
        .await
        .expect("retained project publication");

    assert!(registry.retire_roots(&BTreeSet::from([retired])).await);

    assert!(
        !retained_probe.is_cancelled(),
        "retiring one root must not cancel another project's recovery owner"
    );
    assert!(registry.holds::<RecoveryCancelProbe>(&retained).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_shutdown_drains_admitted_requests_before_removing_runtimes() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("admitted-request-during-shutdown");
    registry
        .publish(project.clone(), component(1))
        .await
        .expect("project runtime publication");
    let snapshot = registry.request_runtimes(Some(&project), None).await;
    assert!(snapshot.is_admitted());

    let shutdown_registry = registry.clone();
    let shutdown = tokio::spawn(async move {
        shutdown_registry.shut_down_all().await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !registry.closed.load(std::sync::atomic::Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown closes request admission");
    assert!(
        !shutdown.is_finished(),
        "shutdown cannot remove a runtime retained by an admitted request"
    );

    drop(snapshot);
    tokio::time::timeout(std::time::Duration::from_secs(1), shutdown)
        .await
        .expect("request settlement unblocks shutdown")
        .expect("shutdown task");
    assert!(!registry.holds::<Component>(&project).await);
}
