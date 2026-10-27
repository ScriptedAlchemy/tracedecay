use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// Any component exercises the registry the same way, so this test-only marker
/// keeps generic registry checks independent of a production capability slot.
type Component = Arc<dyn Any + Send + Sync>;

fn component(mark: u32) -> Component {
    Arc::new(mark)
}

fn mark(component: &Component) -> Option<u32> {
    component.downcast_ref::<u32>().copied()
}

fn root(name: &str) -> PathBuf {
    PathBuf::from("/projects").join(name)
}

async fn wait_for_registration_build_waiters<C>(
    registry: &ProjectRuntimeRegistryV1,
    project: &Path,
    expected: usize,
) where
    C: ProjectRuntimeComponent,
{
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiters = registry
                .lock_runtimes()
                .get(project)
                .and_then(|runtime| runtime.registration_builds.get(&TypeId::of::<C>()))
                .map_or(0, |reservation| reservation.outcome.receiver_count());
            if waiters >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("concurrent registrations must park on the build reservation");
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum TestReconcileError {
    #[error("registry closed")]
    RegistryClosed,
    #[error("{0}")]
    Provider(&'static str),
    #[error("concurrent build failed: {detail}")]
    ConcurrentBuildFailed { detail: String },
}

impl From<ProjectRuntimeRegistryError> for TestReconcileError {
    fn from(error: ProjectRuntimeRegistryError) -> Self {
        match error {
            ProjectRuntimeRegistryError::AlreadyRegistered
            | ProjectRuntimeRegistryError::Closed => Self::RegistryClosed,
            ProjectRuntimeRegistryError::ConcurrentBuildFailed { detail } => {
                Self::ConcurrentBuildFailed { detail }
            }
        }
    }
}

#[tokio::test]
async fn a_second_registration_is_refused_and_leaves_the_incumbent_live() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");

    registry
        .publish(project.clone(), component(1))
        .await
        .expect("the first registration owns the empty slot");
    let refused = registry
        .register_or_reconcile_with::<Component, ProjectRuntimeRegistryError, _, _, _, _, _>(
            project.clone(),
            |_| Err(ProjectRuntimeRegistryError::AlreadyRegistered),
            || async { panic!("a rejected incumbent must not build a replacement") },
            mark,
        )
        .await;

    assert_eq!(refused, Err(ProjectRuntimeRegistryError::AlreadyRegistered));
    assert_eq!(
        registry
            .get::<Component>(&project)
            .await
            .as_ref()
            .and_then(mark),
        Some(1),
        "a refused registration must not detach the live component"
    );
}

#[tokio::test]
async fn failed_publication_builder_leaves_no_empty_project_entry() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("failed");

    let result: Result<(), ProjectRuntimeRegistryError> = registry
        .publish_feedback_atomically(project.clone(), || async {
            Err(ProjectRuntimeRegistryError::AlreadyRegistered)
        })
        .await;

    assert_eq!(result, Err(ProjectRuntimeRegistryError::AlreadyRegistered));
    assert!(
        !registry.lock_runtimes().contains_key(&project),
        "failed construction must not leave an empty project runtime behind"
    );
}

#[tokio::test]
async fn cancelled_publication_releases_its_slot_reservation() {
    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let project = root("cancelled");
    let publisher_registry = Arc::clone(&registry);
    let publisher_project = project.clone();
    let (build_started, build_is_started) = tokio::sync::oneshot::channel();

    let publisher = tokio::spawn(async move {
        let result: Result<(), ProjectRuntimeRegistryError> = publisher_registry
            .publish_feedback_atomically(publisher_project, move || async move {
                build_started.send(()).expect("build-start receiver");
                std::future::pending().await
            })
            .await;
        result
    });
    build_is_started.await.expect("publication build started");

    let writer_registry = Arc::clone(&registry);
    let writer_project = project.clone();
    let (writer_started, writer_is_started) = tokio::sync::oneshot::channel();
    let mut writer = tokio::spawn(async move {
        writer_started.send(()).expect("writer-start receiver");
        writer_registry
            .register_or_reconcile::<RegisteredCallableCodeRuntime, ProjectRuntimeRegistryError, _, _, _>(
                writer_project,
                |_| Ok(()),
                || async { Err(ProjectRuntimeRegistryError::AlreadyRegistered) },
            )
            .await
    });
    writer_is_started.await.expect("blocked writer started");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut writer)
            .await
            .is_err(),
        "the writer must be waiting on the live reservation before cancellation"
    );

    publisher.abort();
    let _ = publisher.await;

    tokio::time::timeout(std::time::Duration::from_secs(2), &mut writer)
        .await
        .expect("cancelled reservation cleanup must wake a blocked writer")
        .expect("writer task")
        .expect_err("the released slot must reach its refusing builder");
    assert!(
        registry.is_empty(),
        "failed builders must release every reservation"
    );
}

#[tokio::test]
async fn failed_builder_releases_and_wakes_a_blocked_writer() {
    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let project = root("failed-wakeup");
    let publisher_registry = Arc::clone(&registry);
    let publisher_project = project.clone();
    let (build_started, build_is_started) = tokio::sync::oneshot::channel();
    let (fail_build, build_must_fail) = tokio::sync::oneshot::channel();

    let publisher = tokio::spawn(async move {
        let result: Result<(), ProjectRuntimeRegistryError> = publisher_registry
            .publish_feedback_atomically(publisher_project, move || async move {
                build_started.send(()).expect("build-start receiver");
                build_must_fail.await.expect("fail-build sender");
                Err(ProjectRuntimeRegistryError::AlreadyRegistered)
            })
            .await;
        result
    });
    build_is_started.await.expect("publication build started");

    let writer_registry = Arc::clone(&registry);
    let writer_project = project.clone();
    let (writer_started, writer_is_started) = tokio::sync::oneshot::channel();
    let mut writer = tokio::spawn(async move {
        writer_started.send(()).expect("writer-start receiver");
        writer_registry
            .register_or_reconcile::<RegisteredCallableCodeRuntime, ProjectRuntimeRegistryError, _, _, _>(
                writer_project,
                |_| Ok(()),
                || async { Err(ProjectRuntimeRegistryError::AlreadyRegistered) },
            )
            .await
    });
    writer_is_started.await.expect("blocked writer started");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut writer)
            .await
            .is_err(),
        "the writer must be waiting on the live reservation before builder failure"
    );

    fail_build.send(()).expect("publication builder receiver");
    assert_eq!(
        publisher.await.expect("publisher task"),
        Err(ProjectRuntimeRegistryError::AlreadyRegistered)
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), &mut writer)
        .await
        .expect("failed builder cleanup must wake a blocked writer")
        .expect("writer task")
        .expect_err("the released slot must reach its refusing builder");
    assert!(
        registry.is_empty(),
        "failed builders must release every reservation"
    );
}

#[tokio::test]
async fn withdrawing_hands_the_component_back_exactly_once() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");
    registry
        .publish(project.clone(), component(1))
        .await
        .unwrap();

    let withdrawn = registry.withdraw::<Component>(&project).await;

    assert_eq!(withdrawn.as_ref().and_then(mark), Some(1));
    assert!(!registry.holds::<Component>(&project).await);
    assert!(registry.withdraw::<Component>(&project).await.is_none());
}

#[tokio::test]
async fn one_project_s_components_are_not_another_s() {
    let registry = ProjectRuntimeRegistryV1::default();
    registry.publish(root("alpha"), component(1)).await.unwrap();

    assert!(!registry.holds::<Component>(&root("beta")).await);
    assert!(registry.get::<Component>(&root("beta")).await.is_none());
}

#[tokio::test]
async fn a_sole_component_is_only_answered_while_exactly_one_project_holds_it() {
    let registry = ProjectRuntimeRegistryV1::default();
    assert!(registry.sole::<Component>().is_none(), "none held");

    registry.publish(root("alpha"), component(1)).await.unwrap();
    assert_eq!(
        registry.sole::<Component>().as_ref().and_then(mark),
        Some(1)
    );

    registry.publish(root("beta"), component(2)).await.unwrap();
    assert!(
        registry.sole::<Component>().is_none(),
        "answering while two projects hold one would attach a request to \
         whichever project sorted first"
    );
}

#[tokio::test]
async fn reconciling_an_occupied_slot_never_builds_a_replacement() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");
    registry
        .publish(project.clone(), component(1))
        .await
        .unwrap();
    let builds = AtomicUsize::new(0);

    let accepted = registry
        .register_or_reconcile::<Component, TestReconcileError, _, _, _>(
            project.clone(),
            |_| Ok(()),
            || async {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(component(2))
            },
        )
        .await;

    assert_eq!(accepted, Ok(()));
    assert_eq!(
        builds.load(Ordering::SeqCst),
        0,
        "a component that owns processes must not be constructed just to \
         be dropped on the reconcile path"
    );
    assert_eq!(
        registry
            .get::<Component>(&project)
            .await
            .as_ref()
            .and_then(mark),
        Some(1)
    );
}

#[tokio::test]
async fn reconciling_an_empty_slot_builds_once_and_keeps_the_build_error() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");

    let failed = registry
        .register_or_reconcile::<Component, TestReconcileError, _, _, _>(
            project.clone(),
            |_| Ok(()),
            || async {
                Err(TestReconcileError::Provider(
                    "the provider runtime would not open",
                ))
            },
        )
        .await;
    assert_eq!(
        failed,
        Err(TestReconcileError::Provider(
            "the provider runtime would not open"
        ))
    );
    assert!(
        !registry.holds::<Component>(&project).await,
        "a failed build must not leave a slot claimed"
    );

    let built = registry
        .register_or_reconcile::<Component, TestReconcileError, _, _, _>(
            project.clone(),
            |_| Ok(()),
            || async { Ok(component(1)) },
        )
        .await;
    assert_eq!(built, Ok(()));
    assert_eq!(
        registry
            .get::<Component>(&project)
            .await
            .as_ref()
            .and_then(mark),
        Some(1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_registration_build_does_not_block_an_established_project() {
    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let building = root("building");
    let established = root("established");
    registry
        .publish(established.clone(), TestFirst(7))
        .await
        .expect("established project publication");
    let (build_started, build_is_started) = tokio::sync::oneshot::channel();
    let (continue_build, build_may_continue) = tokio::sync::oneshot::channel();
    let building_registry = Arc::clone(&registry);
    let registration = tokio::spawn(async move {
        building_registry
            .register_or_reconcile::<TestFirst, ProjectRuntimeRegistryError, _, _, _>(
                building,
                |_| Ok(()),
                move || async move {
                    build_started.send(()).expect("build-start receiver");
                    build_may_continue.await.expect("continue-build sender");
                    Ok(TestFirst(1))
                },
            )
            .await
    });
    build_is_started
        .await
        .expect("registration builder started");

    let reading_registry = Arc::clone(&registry);
    let mut read = tokio::spawn(async move {
        reading_registry.read_now::<TestFirst, _, _>(&established, |value| value.0)
    });
    let completed_while_building =
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut read)
            .await
            .ok();

    continue_build
        .send(())
        .expect("registration builder receiver");
    registration
        .await
        .expect("registration task")
        .expect("registration result");
    let read_completed_while_building = completed_while_building.is_some();
    let read = match completed_while_building {
        Some(read) => read,
        None => read.await,
    }
    .expect("established-project read task");
    assert_eq!(read, Some(7));
    assert!(
        read_completed_while_building,
        "an unrelated established-project read must complete while another project's builder is stalled"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_failed_registrations_build_only_once() {
    const REGISTRATIONS: usize = 8;

    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let project = root("failed-wave");
    let builds = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(REGISTRATIONS + 1));
    let release_build = Arc::new(tokio::sync::Notify::new());
    let (build_started, build_is_started) = tokio::sync::oneshot::channel();
    let build_started = Arc::new(std::sync::Mutex::new(Some(build_started)));
    let mut registrations = tokio::task::JoinSet::new();
    for _ in 0..REGISTRATIONS {
        let registry = Arc::clone(&registry);
        let project = project.clone();
        let builds = Arc::clone(&builds);
        let barrier = Arc::clone(&barrier);
        let release_build = Arc::clone(&release_build);
        let build_started = Arc::clone(&build_started);
        registrations.spawn(async move {
            barrier.wait().await;
            registry
                .register_or_reconcile::<Component, TestReconcileError, _, _, _>(
                    project,
                    |_| Ok(()),
                    || async move {
                        builds.fetch_add(1, Ordering::SeqCst);
                        if let Some(started) = build_started
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        {
                            started.send(()).expect("build-start receiver");
                        }
                        release_build.notified().await;
                        Err(TestReconcileError::Provider("fixture build failed"))
                    },
                )
                .await
        });
    }
    barrier.wait().await;
    build_is_started.await.expect("one builder started");
    wait_for_registration_build_waiters::<Component>(
        registry.as_ref(),
        &project,
        REGISTRATIONS - 1,
    )
    .await;
    release_build.notify_one();

    let mut original_failures = 0;
    let mut concurrent_failures = 0;
    while let Some(result) = registrations.join_next().await {
        match result.expect("registration task") {
            Err(TestReconcileError::Provider("fixture build failed")) => {
                original_failures += 1;
            }
            Err(TestReconcileError::ConcurrentBuildFailed { detail }) => {
                assert_eq!(detail, "fixture build failed");
                concurrent_failures += 1;
            }
            result => panic!("unexpected registration outcome: {result:?}"),
        }
    }
    assert_eq!(original_failures, 1);
    assert_eq!(concurrent_failures, REGISTRATIONS - 1);
    assert_eq!(
        builds.load(Ordering::SeqCst),
        1,
        "one failed build outcome must be handed to the whole concurrent wave"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_successful_registrations_build_only_once() {
    const REGISTRATIONS: usize = 8;

    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let project = root("successful-wave");
    let builds = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(REGISTRATIONS + 1));
    let release_build = Arc::new(tokio::sync::Notify::new());
    let mut registrations = tokio::task::JoinSet::new();
    for _ in 0..REGISTRATIONS {
        let registry = Arc::clone(&registry);
        let project = project.clone();
        let builds = Arc::clone(&builds);
        let barrier = Arc::clone(&barrier);
        let release_build = Arc::clone(&release_build);
        registrations.spawn(async move {
            barrier.wait().await;
            registry
                .register_or_reconcile_with::<Component, TestReconcileError, _, _, _, _, _>(
                    project,
                    |incumbent| {
                        assert_eq!(mark(incumbent), Some(11));
                        Ok(())
                    },
                    || async move {
                        builds.fetch_add(1, Ordering::SeqCst);
                        release_build.notified().await;
                        Ok(component(11))
                    },
                    Arc::clone,
                )
                .await
        });
    }
    barrier.wait().await;
    wait_for_registration_build_waiters::<Component>(
        registry.as_ref(),
        &project,
        REGISTRATIONS - 1,
    )
    .await;
    release_build.notify_one();

    let mut successes = 0;
    while let Some(result) = registrations.join_next().await {
        let returned = result
            .expect("registration task")
            .expect("joined registration succeeds");
        let installed = registry.get::<Component>(&project).await.unwrap();
        assert!(Arc::ptr_eq(&installed, &returned));
        successes += 1;
    }
    assert_eq!(successes, REGISTRATIONS);
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    assert_eq!(
        registry
            .get::<Component>(&project)
            .await
            .as_ref()
            .and_then(mark),
        Some(11)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_registration_preserves_other_components_and_wakes_waiters() {
    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let project = root("cancelled-registration");
    registry
        .publish(project.clone(), TestFirst(9))
        .await
        .expect("existing component publication");
    let (build_started, build_is_started) = tokio::sync::oneshot::channel();
    let building_registry = Arc::clone(&registry);
    let building_project = project.clone();
    let builder = tokio::spawn(async move {
        building_registry
            .register_or_reconcile::<Component, TestReconcileError, _, _, _>(
                building_project,
                |_| Ok(()),
                || async move {
                    build_started.send(()).expect("build-start receiver");
                    std::future::pending().await
                },
            )
            .await
    });
    build_is_started
        .await
        .expect("registration builder started");

    let waiting_registry = Arc::clone(&registry);
    let waiting_project = project.clone();
    let waiter = tokio::spawn(async move {
        waiting_registry
            .register_or_reconcile::<Component, TestReconcileError, _, _, _>(
                waiting_project,
                |_| Ok(()),
                || async { Ok(component(3)) },
            )
            .await
    });
    wait_for_registration_build_waiters::<Component>(registry.as_ref(), &project, 1).await;
    builder.abort();
    assert!(
        builder
            .await
            .expect_err("builder task must be cancelled")
            .is_cancelled()
    );
    assert_eq!(
        waiter.await.expect("waiter task"),
        Err(TestReconcileError::ConcurrentBuildFailed {
            detail: "project runtime component build was cancelled".to_owned(),
        })
    );
    assert_eq!(
        registry.get::<TestFirst>(&project).await,
        Some(TestFirst(9))
    );
    assert!(!registry.holds::<Component>(&project).await);

    registry
        .register_or_reconcile::<Component, TestReconcileError, _, _, _>(
            project.clone(),
            |_| Ok(()),
            || async { Ok(component(4)) },
        )
        .await
        .expect("cancelled reservation releases the exact slot");
    assert_eq!(
        registry
            .get::<Component>(&project)
            .await
            .as_ref()
            .and_then(mark),
        Some(4)
    );
}

#[tokio::test]
async fn a_refusing_reconcile_keeps_the_incumbent() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");
    registry
        .publish(project.clone(), component(1))
        .await
        .unwrap();

    let refused = registry
        .register_or_reconcile::<Component, TestReconcileError, _, _, _>(
            project.clone(),
            |_| {
                Err(TestReconcileError::Provider(
                    "a different authority is already registered",
                ))
            },
            || async { Ok(component(2)) },
        )
        .await;

    assert_eq!(
        refused,
        Err(TestReconcileError::Provider(
            "a different authority is already registered"
        ))
    );
    assert_eq!(
        registry
            .get::<Component>(&project)
            .await
            .as_ref()
            .and_then(mark),
        Some(1)
    );
}

#[tokio::test]
async fn reading_answers_only_for_a_project_that_holds_the_component() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");
    registry
        .publish(project.clone(), component(7))
        .await
        .unwrap();

    assert_eq!(
        registry.read::<Component, _, _>(&project, mark).await,
        Some(Some(7))
    );
    assert!(
        registry
            .read::<Component, _, _>(&root("beta"), |_| ())
            .await
            .is_none()
    );
}

/// Project-open registers owners under `Path::canonicalize()` (`\\?\C:\...` on
/// Windows). Later storage-status / primitive lookups arrive with the ordinary
/// handshake spelling. Admission already treats those as one project; `get`
/// and `read` must resolve the same registered owner instead of answering
/// "still mounting" forever.
#[tokio::test]
async fn get_and_read_resolve_an_owner_registered_under_a_windows_verbatim_spelling() {
    let registry = ProjectRuntimeRegistryV1::default();
    let registered = PathBuf::from(r"\\?\C:\Users\test\project");
    let request = PathBuf::from(r"C:\Users\test\project");
    registry.publish(registered, TestFirst(7)).await.unwrap();

    assert_eq!(
        registry.get::<TestFirst>(&request).await,
        Some(TestFirst(7)),
        "an ordinary Windows request spelling must reach the verbatim registered owner"
    );
    assert_eq!(
        registry
            .read::<TestFirst, _, _>(&request, |owner| owner.0)
            .await,
        Some(7),
        "primitive-style read must use the same admitted-root resolution as get"
    );
    assert_eq!(
        registry.publication_state(&request),
        Some(ProjectRuntimePublicationStateV1::Warming),
        "publication stage must resolve through the same admitted root as get"
    );
}

#[tokio::test]
async fn ambiguous_registered_spellings_refuse_instead_of_selecting_an_owner() {
    let registry = ProjectRuntimeRegistryV1::default();
    let ordinary = PathBuf::from(r"C:\Users\test\ambiguous");
    let verbatim = PathBuf::from(r"\\?\C:\Users\test\ambiguous");
    registry
        .publish(ordinary.clone(), TestFirst(1))
        .await
        .unwrap();
    registry.publish(verbatim, TestFirst(2)).await.unwrap();

    assert!(
        registry.get::<TestFirst>(&ordinary).await.is_none(),
        "two registered spellings that could name different authorities must not select the first map entry"
    );
    assert!(
        !registry
            .request_runtimes(Some(&ordinary), None)
            .await
            .is_admitted(),
        "an ambiguous root is a lookup miss, not a warming admitted runtime"
    );
}

#[tokio::test]
async fn stale_publication_failure_cannot_poison_a_newer_ready_attempt() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("publication-attempt-fence");
    registry
        .publish(project.clone(), TestFirst(1))
        .await
        .unwrap();
    let stale = registry
        .begin_publication(&project)
        .expect("first publication attempt");
    let quiescence = registry
        .quiesce_roots(&BTreeSet::from([project.clone()]))
        .await
        .expect("retire the first runtime under a replacement fence");
    drop(quiescence);
    registry
        .publish(project.clone(), TestFirst(2))
        .await
        .expect("replacement runtime");
    let current = registry
        .begin_publication(&project)
        .expect("replacement publication attempt");

    assert!(!registry.mark_publication_failed(&stale));
    assert!(registry.mark_publication_ready(&current));
    assert_eq!(
        registry.publication_state(&project),
        Some(ProjectRuntimePublicationStateV1::Ready)
    );
}

#[tokio::test]
async fn failed_publication_can_reopen_and_reach_ready() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("publication-reopen");
    registry
        .publish(project.clone(), TestFirst(1))
        .await
        .unwrap();
    let failed = registry
        .begin_publication(&project)
        .expect("failed publication attempt");

    assert!(registry.mark_publication_failed(&failed));
    assert_eq!(
        registry.publication_state(&project),
        Some(ProjectRuntimePublicationStateV1::Failed)
    );

    let reopened = registry
        .begin_publication(&project)
        .expect("fresh publication attempt");
    assert_eq!(
        registry.publication_state(&project),
        Some(ProjectRuntimePublicationStateV1::Warming)
    );
    assert!(registry.mark_publication_ready(&reopened));
    assert_eq!(
        registry.publication_state(&project),
        Some(ProjectRuntimePublicationStateV1::Ready)
    );
}

#[tokio::test]
async fn get_does_not_treat_a_different_project_as_an_equivalent_root() {
    let registry = ProjectRuntimeRegistryV1::default();
    registry.publish(root("alpha"), TestFirst(1)).await.unwrap();

    assert_eq!(
        registry.get::<TestFirst>(&root("alpha")).await,
        Some(TestFirst(1))
    );
    assert!(
        registry.get::<TestFirst>(&root("beta")).await.is_none(),
        "linked or foreign roots must not collapse onto another project's owner"
    );
}

#[tokio::test]
async fn cancelled_shutdown_caller_does_not_abandon_the_registry_drain() {
    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let project = root("shutdown-race");
    let publisher_registry = Arc::clone(&registry);
    let publisher_project = project.clone();
    let (build_started, build_is_started) = tokio::sync::oneshot::channel();
    let (continue_build, build_may_continue) = tokio::sync::oneshot::channel();

    let publisher = tokio::spawn(async move {
        let lease = publisher_registry.reserve_feedback(publisher_project)?;
        build_started.send(()).expect("build-start receiver");
        build_may_continue.await.expect("continue-build sender");
        drop(lease);
        Ok::<_, ProjectRuntimeRegistryError>(())
    });
    build_is_started.await.expect("publication build started");

    let shutdown_registry = Arc::clone(&registry);
    let shutdown = tokio::spawn(async move {
        shutdown_registry.shut_down_all(None).await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !registry.closed.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown must close admission before waiting");
    shutdown.abort();
    assert!(
        shutdown
            .await
            .expect_err("shutdown caller must be cancelled")
            .is_cancelled(),
        "the first shutdown caller must not finish while a reservation is live"
    );
    continue_build.send(()).expect("publication build receiver");

    assert_eq!(
        publisher.await.expect("publisher task"),
        Ok(()),
        "the reservation owner must be able to release during shutdown"
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        registry.shut_down_all(None),
    )
    .await
    .expect("a replacement shutdown caller must observe the completed background drain");
    assert!(registry.is_empty());
}

#[tokio::test]
async fn cancelled_publication_releases_while_shutdown_drain_is_waiting() {
    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let project = root("shutdown-drain-split");
    let publisher_registry = Arc::clone(&registry);
    let (build_started, build_is_started) = tokio::sync::oneshot::channel();
    let (drain_waiting, drain_is_waiting) = tokio::sync::oneshot::channel();
    registry.arm_shutdown_drain_waiting(drain_waiting);

    let publisher = tokio::spawn(async move {
        let result: Result<(), ProjectRuntimeRegistryError> = publisher_registry
            .publish_feedback_atomically(project, move || async move {
                build_started.send(()).expect("build-start receiver");
                std::future::pending().await
            })
            .await;
        result
    });
    build_is_started.await.expect("publication build started");

    let shutdown_registry = Arc::clone(&registry);
    let shutdown = tokio::spawn(async move {
        shutdown_registry.shut_down_all(None).await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), drain_is_waiting)
        .await
        .expect("shutdown must reach its reservation wait")
        .expect("drain-waiting sender");

    publisher.abort();
    assert!(
        publisher
            .await
            .expect_err("publisher must be cancelled")
            .is_cancelled()
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
        .await
        .expect("separate reservation cleanup must unblock shutdown")
        .expect("shutdown task");
    assert!(registry.is_empty());
}

/// The drain task runs on the native runtime, so a caller with no Tokio
/// runtime still shuts every project runtime down.
#[test]
fn shutting_down_off_tokio_empties_the_registry() {
    let registry = ProjectRuntimeRegistryV1::default();
    let writer_waits = tracedecay_rusqlite_runtime::exact_sql::native_thread_writer_waits();
    let shut_down = tracedecay_runtime_core::native_runtime::block_on(async {
        assert!(tokio::runtime::Handle::try_current().is_err());
        registry.publish(root("alpha"), component(1)).await.unwrap();
        registry.shut_down_all(None).await
    })
    .expect("native runtime");

    assert_eq!(
        shut_down,
        super::ProjectRuntimesStop::Stopped,
        "the off-Tokio drain completes"
    );
    assert!(registry.is_empty());
    assert_eq!(
        tracedecay_rusqlite_runtime::exact_sql::native_thread_writer_waits(),
        writer_waits
    );
}

#[tokio::test]
async fn shutting_down_empties_the_registry() {
    let registry = ProjectRuntimeRegistryV1::default();
    registry.publish(root("alpha"), component(1)).await.unwrap();
    registry.publish(root("beta"), component(2)).await.unwrap();

    registry.shut_down_all(None).await;

    assert!(registry.is_empty());
    assert!(!registry.holds::<Component>(&root("alpha")).await);
    assert_eq!(
        registry
            .register_or_reconcile_with::<Component, ProjectRuntimeRegistryError, _, _, _, _, _>(
                root("late"),
                |_| Ok(()),
                || async { Ok(component(3)) },
                mark,
            )
            .await,
        Err(ProjectRuntimeRegistryError::Closed),
        "registration must fail after shutdown closes the registry"
    );
    assert_eq!(
        registry.publish(root("late"), component(4)).await,
        Err(ProjectRuntimeRegistryError::Closed),
        "publishing must report closed admission"
    );
    assert!(
        registry.is_empty(),
        "a delayed publisher must not resurrect the drained registry"
    );
}

#[tokio::test]
async fn retiring_exact_roots_fences_republication_without_closing_other_projects() {
    let registry = ProjectRuntimeRegistryV1::default();
    let retired = root("retired");
    let retained = root("retained");
    registry
        .publish(retired.clone(), component(1))
        .await
        .unwrap();
    registry
        .publish(retained.clone(), component(2))
        .await
        .unwrap();

    assert!(
        registry
            .retire_roots(&[retired.clone()].into_iter().collect())
            .await
    );
    assert!(!registry.holds::<Component>(&retired).await);
    assert_eq!(
        registry
            .register_or_reconcile_with::<Component, ProjectRuntimeRegistryError, _, _, _, _, _>(
                retired.clone(),
                |_| Ok(()),
                || async { Ok(component(3)) },
                mark,
            )
            .await,
        Err(ProjectRuntimeRegistryError::Closed)
    );
    assert_eq!(
        registry.publish(retired.clone(), component(4)).await,
        Err(ProjectRuntimeRegistryError::Closed)
    );
    assert_eq!(
        registry
            .get::<Component>(&retained)
            .await
            .as_ref()
            .and_then(mark),
        Some(2)
    );
    registry
        .publish(retained.clone(), component(5))
        .await
        .expect("unrelated project remains open");
}

/// Test-only component whose drop parks until the test releases it: a stand-in
/// for a generation-sized owner whose deallocation takes seconds.
struct BlockingDropComponent {
    release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    dropped: std::sync::mpsc::SyncSender<()>,
}

impl Drop for BlockingDropComponent {
    fn drop(&mut self) {
        if let Some(release) = self.release.lock().unwrap().take() {
            let _ = release.recv();
        }
        let _ = self.dropped.send(());
    }
}

#[tokio::test]
async fn daemon_shutdown_does_not_wait_for_drained_runtime_deallocation() {
    let registry = ProjectRuntimeRegistryV1::default();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let (dropped_tx, dropped) = std::sync::mpsc::sync_channel::<()>(1);
    let component: Component = Arc::new(BlockingDropComponent {
        release: std::sync::Mutex::new(Some(release_rx)),
        dropped: dropped_tx,
    });
    registry
        .publish(root("slow-drop"), component)
        .await
        .unwrap();

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        registry.shut_down_all(None),
    )
    .await
    .expect("shutdown must not park behind a drained runtime's deallocation");
    assert!(registry.is_empty());
    assert!(
        dropped.try_recv().is_err(),
        "the drained runtime is still being released off the shutdown path"
    );

    release.send(()).expect("blocked drop receiver");
    dropped
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the detached release must still free the drained runtime");
}

#[tokio::test]
async fn registration_projection_refuses_shutdown_during_build() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("projection-shutdown");
    let building_registry = registry.clone();
    let building_project = project.clone();
    let (started, building) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    let registration = tokio::spawn(async move {
        building_registry
            .register_or_reconcile_with::<Component, ProjectRuntimeRegistryError, _, _, _, _, _>(
                building_project,
                |_| Ok(()),
                || async {
                    started.send(()).unwrap();
                    resume.await.unwrap();
                    Ok(component(42))
                },
                mark,
            )
            .await
    });
    building.await.unwrap();
    registry.begin_shutdown();
    release.send(()).unwrap();
    assert_eq!(
        registration.await.unwrap(),
        Err(ProjectRuntimeRegistryError::Closed)
    );
    assert!(!registry.holds::<Component>(&project).await);
}
