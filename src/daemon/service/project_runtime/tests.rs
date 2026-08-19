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

fn reservation_for<C>() -> ProjectRuntimeReservation
where
    C: ProjectRuntimeComponent,
{
    let mut reservation = ProjectRuntimeReservation::default();
    reservation.reserve::<C>();
    reservation
}

fn two_component_reservation() -> ProjectRuntimeReservation {
    let mut reservation = reservation_for::<TestFirst>();
    reservation.reserve::<TestSecond>();
    reservation
}

#[derive(Debug, PartialEq, Eq)]
enum TestReconcileError {
    RegistryClosed,
    Provider(&'static str),
}

impl From<ProjectRuntimeRegistryError> for TestReconcileError {
    fn from(_: ProjectRuntimeRegistryError) -> Self {
        Self::RegistryClosed
    }
}

#[test]
fn a_publication_cannot_stage_a_component_outside_its_reservation() {
    let mut publication = ProjectRuntimePublication::new(reservation_for::<TestFirst>());

    assert_eq!(
        publication.stage(TestSecond(2)),
        Err(ProjectRuntimeAlreadyRegistered),
        "the reservation is the structural source of preflight coverage"
    );
}

#[test]
fn omitted_publish_slot_panics_before_mutating_the_incumbent() {
    let mut reservation = reservation_for::<TestFirst>();
    reservation.reserve::<TestOmitted>();
    let mut publication = ProjectRuntimePublication::new(reservation);
    publication.stage(TestFirst(1)).unwrap();
    publication.stage(TestOmitted(1)).unwrap();
    let mut incumbent = ProjectRuntime::default();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        publication.commit_into(&mut incumbent)
    }));

    assert!(result.is_err(), "an omitted publish slot must panic");
    assert_eq!(
        incumbent.test_first, None,
        "validation must finish before any staged component becomes visible"
    );
}

#[tokio::test]
async fn a_second_registration_is_refused_and_leaves_the_incumbent_live() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");

    registry
        .register(project.clone(), component(1))
        .await
        .expect("the first registration owns the empty slot");
    let refused = registry.register(project.clone(), component(2)).await;

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
async fn publishing_replaces_the_incumbent() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");

    registry
        .publish(project.clone(), component(1))
        .await
        .unwrap();
    registry
        .publish(project.clone(), component(2))
        .await
        .unwrap();

    assert_eq!(
        registry
            .get::<Component>(&project)
            .await
            .as_ref()
            .and_then(mark),
        Some(2)
    );
}

#[tokio::test]
async fn atomic_publication_rejects_a_conflicting_bundle_without_a_partial_commit() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");

    let published: Result<(), ProjectRuntimeRegistryError> = registry
        .publish_atomically_after_preflight(
            project.clone(),
            reservation_for::<TestFirst>(),
            |mut publication| async move {
                publication.stage(TestFirst(1))?;
                Ok((publication, ()))
            },
        )
        .await;
    published.unwrap();

    let conflicting: Result<(), ProjectRuntimeRegistryError> = registry
        .publish_atomically_after_preflight(
            project.clone(),
            two_component_reservation(),
            |mut publication| async move {
                publication.stage(TestFirst(2))?;
                publication.stage(TestSecond(2))?;
                Ok((publication, ()))
            },
        )
        .await;
    assert_eq!(
        conflicting,
        Err(ProjectRuntimeRegistryError::AlreadyRegistered),
        "a collision in one staged component must reject the whole publication"
    );
    assert_eq!(
        registry.get::<TestFirst>(&project).await,
        Some(TestFirst(1)),
        "the incumbent component must remain live"
    );
    assert!(
        registry.get::<TestSecond>(&project).await.is_none(),
        "a non-conflicting component must not leak from a rejected bundle"
    );
}

#[tokio::test]
async fn racing_atomic_publications_leave_one_complete_bundle() {
    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let project = root("alpha");
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut attempts = tokio::task::JoinSet::new();

    for mark in [1, 2] {
        let registry = Arc::clone(&registry);
        let project = project.clone();
        let barrier = Arc::clone(&barrier);
        attempts.spawn(async move {
            barrier.wait().await;
            let result: Result<(), ProjectRuntimeRegistryError> = registry
                .publish_atomically_after_preflight(
                    project,
                    two_component_reservation(),
                    move |mut publication| async move {
                        publication.stage(TestFirst(mark))?;
                        publication.stage(TestSecond(mark))?;
                        Ok((publication, ()))
                    },
                )
                .await;
            result
        });
    }

    barrier.wait().await;
    let mut successful = 0;
    while let Some(result) = attempts.join_next().await {
        if result.unwrap().is_ok() {
            successful += 1;
        }
    }

    assert_eq!(successful, 1, "exactly one racing bundle may publish");
    let first = registry.get::<TestFirst>(&project).await.unwrap();
    let second = registry.get::<TestSecond>(&project).await.unwrap();
    assert_eq!(
        first.0, second.0,
        "readers must see components from one publication, never a mixed runtime"
    );
}

#[tokio::test]
async fn synchronized_reader_never_observes_a_partial_publication() {
    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let project = root("observed");
    let samples = Arc::new(std::sync::Mutex::new(Vec::new()));
    let publisher_registry = Arc::clone(&registry);
    let publisher_project = project.clone();
    let (build_started, build_is_started) = tokio::sync::oneshot::channel();
    let (continue_build, build_may_continue) = tokio::sync::oneshot::channel();
    let publisher = tokio::spawn(async move {
        let result: Result<(), ProjectRuntimeRegistryError> = publisher_registry
            .publish_atomically_after_preflight(
                publisher_project,
                two_component_reservation(),
                move |mut publication| async move {
                    build_started.send(()).expect("build-start receiver");
                    build_may_continue.await.expect("continue-build sender");
                    publication.stage(TestFirst(7))?;
                    publication.stage(TestSecond(7))?;
                    Ok((publication, ()))
                },
            )
            .await;
        result
    });
    build_is_started.await.expect("publication build started");

    tokio::time::timeout(
        std::time::Duration::from_millis(250),
        registry.request_runtimes(Some(&project), None),
    )
    .await
    .expect("request dispatch must not stall while publication builds");

    let observer_registry = Arc::clone(&registry);
    let observer_project = project.clone();
    let observer_samples = Arc::clone(&samples);
    let (observer_sampled, observer_has_sampled) = tokio::sync::oneshot::channel();
    let observer = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(2), async move {
            let mut observer_sampled = Some(observer_sampled);
            loop {
                let sample = {
                    let runtimes = observer_registry.lock_runtimes();
                    let runtime = runtimes.get(&observer_project);
                    (
                        runtime.and_then(|runtime| runtime.test_first.map(|value| value.0)),
                        runtime.and_then(|runtime| runtime.test_second.map(|value| value.0)),
                    )
                };
                observer_samples
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(sample);
                if let Some(sampled) = observer_sampled.take() {
                    sampled.send(()).expect("observer sample receiver");
                }
                if sample.0.is_some() && sample.1.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("observer must reach the complete publication");
    });
    observer_has_sampled
        .await
        .expect("observer sampled during build");
    continue_build.send(()).expect("publication build receiver");

    tokio::time::timeout(std::time::Duration::from_secs(2), publisher)
        .await
        .expect("publisher must not deadlock at commit")
        .expect("publisher task")
        .expect("atomic publication");
    tokio::time::timeout(std::time::Duration::from_secs(2), observer)
        .await
        .expect("observer must finish after publication")
        .expect("observer task");

    let samples = samples
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        samples.len() >= 2,
        "the synchronized observer must sample before and after publication"
    );
    assert_eq!(samples.first(), Some(&(None, None)));
    assert_eq!(samples.last(), Some(&(Some(7), Some(7))));
    assert!(
        samples
            .iter()
            .all(|sample| matches!(sample, (None, None) | (Some(7), Some(7)))),
        "a reader observed an intermediate runtime state: {samples:?}"
    );
}

#[tokio::test]
async fn conflicting_preflight_never_starts_the_publication_builder() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");
    registry
        .publish(project.clone(), TestFirst(1))
        .await
        .unwrap();
    let builds = AtomicUsize::new(0);

    let result: Result<(), ProjectRuntimeRegistryError> = registry
        .publish_atomically_after_preflight(
            project.clone(),
            two_component_reservation(),
            |mut publication| async {
                builds.fetch_add(1, Ordering::SeqCst);
                publication.stage(TestSecond(2))?;
                Ok((publication, ()))
            },
        )
        .await;

    assert_eq!(result, Err(ProjectRuntimeRegistryError::AlreadyRegistered));
    assert_eq!(
        builds.load(Ordering::SeqCst),
        0,
        "a rejected publication must not start side-effectful construction"
    );
    assert!(
        registry.get::<TestSecond>(&project).await.is_none(),
        "a rejected builder must not publish any staged component"
    );
}

#[tokio::test]
async fn failed_commit_drops_every_prepared_component_without_publication() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("rollback");
    let spy = Arc::new(TestLifecycleSpy::default());
    let mut reservation = two_component_reservation();
    reservation.reserve::<RecordingComponent<1>>();
    reservation.reserve::<RecordingComponent<2>>();

    let result: Result<(), ProjectRuntimeRegistryError> = registry
        .publish_atomically_after_preflight(project.clone(), reservation, |mut publication| {
            let spy = Arc::clone(&spy);
            async move {
                publication.record_lifecycle_with(Arc::clone(&spy));
                publication.stage(TestFirst(2))?;

                let first = RecordingComponent::<1>::new(2, Arc::clone(&spy));
                publication.stage(first)?;
                spy.record(TestLifecycleEvent::Stage(1));

                let second = RecordingComponent::<2>::new(2, Arc::clone(&spy));
                publication.stage(second)?;
                spy.record(TestLifecycleEvent::Stage(2));
                Ok((publication, ()))
            }
        })
        .await;

    assert_eq!(result, Err(ProjectRuntimeRegistryError::AlreadyRegistered));
    assert!(
        registry.get::<TestFirst>(&project).await.is_none(),
        "an incomplete prepared bundle must not publish its staged component"
    );
    assert!(
        !registry.holds::<RecordingComponent<1>>(&project).await,
        "the first prepared component must never become visible"
    );
    assert!(
        !registry.holds::<RecordingComponent<2>>(&project).await,
        "the second prepared component must never become visible"
    );
    assert_eq!(
        spy.live.load(Ordering::SeqCst),
        0,
        "every prepared resource must be dropped after rollback"
    );
    let events = spy.events();
    assert_eq!(
        &events[..5],
        &[
            TestLifecycleEvent::Construct { slot: 1, mark: 2 },
            TestLifecycleEvent::Stage(1),
            TestLifecycleEvent::Construct { slot: 2, mark: 2 },
            TestLifecycleEvent::Stage(2),
            TestLifecycleEvent::Publish,
        ],
        "construction and staging must complete before the real commit attempt"
    );
    assert_eq!(events.len(), 7, "both staged resources must be dropped");
    assert!(events[5..].contains(&TestLifecycleEvent::Drop { slot: 1, mark: 2 }));
    assert!(events[5..].contains(&TestLifecycleEvent::Drop { slot: 2, mark: 2 }));
    assert!(
        !registry.lock_runtimes().contains_key(&project),
        "rollback must remove the reservation-only project entry"
    );
}

#[tokio::test]
async fn failed_publication_builder_leaves_no_empty_project_entry() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("failed");

    let result: Result<(), ProjectRuntimeRegistryError> = registry
        .publish_atomically_after_preflight(
            project.clone(),
            reservation_for::<TestFirst>(),
            |_| async { Err(ProjectRuntimeRegistryError::AlreadyRegistered) },
        )
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
            .publish_atomically_after_preflight(
                publisher_project,
                reservation_for::<TestFirst>(),
                move |_| async move {
                    build_started.send(()).expect("build-start receiver");
                    std::future::pending().await
                },
            )
            .await;
        result
    });
    build_is_started.await.expect("publication build started");

    let writer_registry = Arc::clone(&registry);
    let writer_project = project.clone();
    let (writer_started, writer_is_started) = tokio::sync::oneshot::channel();
    let mut writer = tokio::spawn(async move {
        writer_started.send(()).expect("writer-start receiver");
        writer_registry.register(writer_project, TestFirst(1)).await
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
        .expect("the released slot must accept the next registration");
    assert_eq!(
        registry.get::<TestFirst>(&project).await,
        Some(TestFirst(1))
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
            .publish_atomically_after_preflight(
                publisher_project,
                reservation_for::<TestFirst>(),
                move |_| async move {
                    build_started.send(()).expect("build-start receiver");
                    build_must_fail.await.expect("fail-build sender");
                    Err(ProjectRuntimeRegistryError::AlreadyRegistered)
                },
            )
            .await;
        result
    });
    build_is_started.await.expect("publication build started");

    let writer_registry = Arc::clone(&registry);
    let writer_project = project.clone();
    let (writer_started, writer_is_started) = tokio::sync::oneshot::channel();
    let mut writer = tokio::spawn(async move {
        writer_started.send(()).expect("writer-start receiver");
        writer_registry.register(writer_project, TestFirst(1)).await
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
        .expect("the released slot must accept the next registration");
    assert_eq!(
        registry.get::<TestFirst>(&project).await,
        Some(TestFirst(1))
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
async fn reconfiguration_withdraw_unblocks_a_fresh_advisory_publication() {
    struct StubCyclePort;
    impl tracedecay_lsp::FeedbackCycleRuntimePort for StubCyclePort {
        fn execute(
            &self,
            _request: tracedecay_lsp::FeedbackCycleRequest,
        ) -> tracedecay_lsp::LspRuntimeFuture<Result<(), tracedecay_lsp::LspRuntimeFailure>> {
            Box::pin(async { Err(tracedecay_lsp::LspRuntimeFailure::new("stub-cycle")) })
        }
    }
    struct StubAdvisoryPort;
    impl super::super::invocation::DaemonAdvisoryCycleInvocationPort for StubAdvisoryPort {
        fn invoke(
            &self,
            _request: super::super::invocation::DaemonAdvisoryCycleInvocationRequest,
        ) -> super::super::invocation::DaemonAdvisoryCycleInvocationFuture<'_> {
            Box::pin(async {
                Err(tracedecay_application::ApplicationProblem::cancelled_before_admission())
            })
        }
    }
    let advisory = || {
        (
            RegisteredAdvisoryRuntimeV1::new(Arc::new(()) as Arc<dyn Any + Send + Sync>),
            DaemonAdvisoryCycleInvocationOwner::new(
                tracedecay_domain::ProjectId::new("project.advisory.reconfiguration").unwrap(),
                Arc::new(StubAdvisoryPort),
            ),
            Arc::new(StubCyclePort) as Arc<dyn tracedecay_lsp::FeedbackCycleRuntimePort>,
        )
    };
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");
    registry
        .publish(
            project.clone(),
            Arc::new(SwitchableFeedbackCycleRuntimeV1::new(Arc::new(
                StubCyclePort,
            ))),
        )
        .await
        .unwrap();
    let (runtime, cycle, input) = advisory();
    registry
        .publish_advisory_atomically(&project, runtime, cycle, input)
        .await
        .expect("first advisory publication");
    let (runtime, cycle, input) = advisory();
    assert!(
        registry
            .publish_advisory_atomically(&project, runtime, cycle, input)
            .await
            .is_err(),
        "a live advisory owner keeps its slots"
    );

    registry
        .withdraw_feedback_and_advisory_for_reconfiguration(&project)
        .await
        .expect("withdraw for reconfiguration");

    let (runtime, cycle, input) = advisory();
    registry
        .publish_advisory_atomically(&project, runtime, cycle, input)
        .await
        .expect("the remount republishes over the withdrawn slots");
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
    assert!(registry.sole::<Component>().await.is_none(), "none held");

    registry.publish(root("alpha"), component(1)).await.unwrap();
    assert_eq!(
        registry.sole::<Component>().await.as_ref().and_then(mark),
        Some(1)
    );

    registry.publish(root("beta"), component(2)).await.unwrap();
    assert!(
        registry.sole::<Component>().await.is_none(),
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
        .register_or_reconcile::<Component, TestReconcileError, _, _>(
            project.clone(),
            |_| Ok(()),
            || {
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
        .register_or_reconcile::<Component, TestReconcileError, _, _>(
            project.clone(),
            |_| Ok(()),
            || {
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
        .register_or_reconcile::<Component, TestReconcileError, _, _>(
            project.clone(),
            |_| Ok(()),
            || Ok(component(1)),
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

#[tokio::test]
async fn a_refusing_reconcile_keeps_the_incumbent() {
    let registry = ProjectRuntimeRegistryV1::default();
    let project = root("alpha");
    registry
        .publish(project.clone(), component(1))
        .await
        .unwrap();

    let refused = registry
        .register_or_reconcile::<Component, TestReconcileError, _, _>(
            project.clone(),
            |_| {
                Err(TestReconcileError::Provider(
                    "a different authority is already registered",
                ))
            },
            || Ok(component(2)),
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

#[tokio::test]
async fn request_resolution_answers_nothing_for_an_absent_or_unnamed_project() {
    let registry = ProjectRuntimeRegistryV1::default();
    registry.publish(root("alpha"), component(1)).await.unwrap();

    for project_root in [None, Some(root("beta").as_path())] {
        let resolved = registry.request_runtimes(project_root, None).await;
        assert!(resolved.feedback.is_none());
        assert!(resolved.feedback_owner.is_none());
        assert!(resolved.configuration.is_none());
        assert!(resolved.work.is_none());
        assert!(resolved.lsp_owner.is_none());
    }
}

#[tokio::test]
async fn canonical_fallback_finds_a_component_without_an_alias_runtime() {
    let registry = ProjectRuntimeRegistryV1::default();
    let alias = root("alias");
    let canonical = root("canonical");
    registry
        .publish(canonical.clone(), TestFirst(7))
        .await
        .unwrap();

    let runtimes = registry.lock_runtimes();
    assert_eq!(
        ProjectRuntimeRegistryV1::component_with_canonical_fallback::<TestFirst>(
            &runtimes,
            &alias,
            Some(&canonical),
        ),
        Some(TestFirst(7)),
        "a request through an unregistered alias must still reach the canonical component"
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
        let result: Result<(), ProjectRuntimeRegistryError> = publisher_registry
            .publish_atomically_after_preflight(
                publisher_project,
                reservation_for::<TestFirst>(),
                move |mut publication| async move {
                    build_started.send(()).expect("build-start receiver");
                    build_may_continue.await.expect("continue-build sender");
                    publication.stage(TestFirst(1))?;
                    Ok((publication, ()))
                },
            )
            .await;
        result
    });
    build_is_started.await.expect("publication build started");

    let shutdown_registry = Arc::clone(&registry);
    let shutdown = tokio::spawn(async move {
        shutdown_registry.shut_down_all().await;
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
        Err(ProjectRuntimeRegistryError::Closed),
        "a publication reserved before shutdown must not commit afterward"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), registry.shut_down_all())
        .await
        .expect("a replacement shutdown caller must observe the completed background drain");
    assert!(registry.is_empty().await);
}

#[tokio::test]
async fn cancelled_publication_releases_while_shutdown_drain_is_waiting() {
    let registry = Arc::new(ProjectRuntimeRegistryV1::default());
    let project = root("shutdown-drain-split");
    let publisher_registry = Arc::clone(&registry);
    let (build_started, build_is_started) = tokio::sync::oneshot::channel();
    let (_continue_build, build_may_continue) = tokio::sync::oneshot::channel::<()>();
    let (drain_waiting, drain_is_waiting) = tokio::sync::oneshot::channel();
    registry.arm_shutdown_drain_waiting(drain_waiting);

    let publisher = tokio::spawn(async move {
        let result: Result<(), ProjectRuntimeRegistryError> = publisher_registry
            .publish_atomically_after_preflight(
                project,
                reservation_for::<TestFirst>(),
                move |mut publication| async move {
                    build_started.send(()).expect("build-start receiver");
                    build_may_continue.await.expect("continue-build sender");
                    publication.stage(TestFirst(1))?;
                    Ok((publication, ()))
                },
            )
            .await;
        result
    });
    build_is_started.await.expect("publication build started");

    let shutdown_registry = Arc::clone(&registry);
    let shutdown = tokio::spawn(async move {
        shutdown_registry.shut_down_all().await;
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
    assert!(registry.is_empty().await);
}

#[tokio::test]
async fn shutting_down_empties_the_registry() {
    let registry = ProjectRuntimeRegistryV1::default();
    registry.publish(root("alpha"), component(1)).await.unwrap();
    registry.publish(root("beta"), component(2)).await.unwrap();

    registry.shut_down_all().await;

    assert!(registry.is_empty().await);
    assert!(!registry.holds::<Component>(&root("alpha")).await);
    assert_eq!(
        registry.register(root("late"), component(3)).await,
        Err(ProjectRuntimeRegistryError::Closed),
        "registration must fail after shutdown closes the registry"
    );
    assert_eq!(
        registry.publish(root("late"), component(4)).await,
        Err(ProjectRuntimeRegistryError::Closed),
        "publishing must report closed admission"
    );
    assert!(
        registry.is_empty().await,
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
        registry.register(retired.clone(), component(3)).await,
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
