use std::any::Any;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use super::{ProjectRuntimeRegistryError, ProjectRuntimeRegistryV1};

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
        .register_or_reconcile::<Component, ProjectRuntimeRegistryError, _, _>(
            project.clone(),
            |_| Ok(()),
            || Ok(component(2)),
        )
        .await;

    assert_eq!(rejected, Err(ProjectRuntimeRegistryError::Closed));
    assert!(!registry.holds::<Component>(&project).await);
    assert!(registry.holds::<Component>(&retained).await);
    drop(quiescence);
    registry
        .register_or_reconcile::<Component, ProjectRuntimeRegistryError, _, _>(
            project.clone(),
            |_| Ok(()),
            || Ok(component(3)),
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
