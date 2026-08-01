//! `types` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

#[tokio::test]
async fn feedback_cycle_router_upgrades_existing_lsp_sessions_to_advisory_runtime() {
    let proximity_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let advisory_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observations = Arc::new(RecordingFeedbackCycleObservations::default());
    let router = SwitchableFeedbackCycleRuntimeV1::new(Arc::new(unavailable_feedback_cycle(
        Arc::clone(&observations),
    )));
    let request = FeedbackCycleRequest {
        root_uri: "file:///project".to_owned(),
        document_uri: "file:///project/src/lib.rs".to_owned(),
        trigger: DiagnosticTrigger::DocumentSave,
    };

    assert!(router.execute(request.clone()).await.is_err());
    assert!(matches!(
        observations.0.lock().expect("observations").as_slice(),
        [Plan26FeedbackSourceEventV1::Delivery {
            operation: Plan26FeedbackOperationV1::FeedbackCycle,
            route: Plan26DeliveryRouteV1::Lsp,
            outcome: Plan26FeedbackOutcomeV1::Unavailable,
            item_count: 0,
            ..
        }]
    ));
    router
        .replace(Arc::new(CountingFeedbackCycle(Arc::clone(
            &proximity_calls,
        ))))
        .unwrap();
    router.execute(request.clone()).await.unwrap();
    router
        .replace(Arc::new(CountingFeedbackCycle(Arc::clone(&advisory_calls))))
        .unwrap();
    router.execute(request).await.unwrap();

    assert_eq!(proximity_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(advisory_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn pr13_hook_orchestration_admits_only_saved_edit_stop_and_explicit() {
    let saved = Pr13HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
    assert_eq!(saved.trigger, Pr13HookOrchestrationTriggerV1::SavedEdit);

    let stop = Pr13HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::TurnComplete,
        }),
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
    assert_eq!(stop.trigger, Pr13HookOrchestrationTriggerV1::Stop);

    let without_scout_lifecycle = Pr13HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        None,
        1,
        false,
    )
    .unwrap();
    assert_eq!(
        without_scout_lifecycle.trigger,
        Pr13HookOrchestrationTriggerV1::SavedEdit
    );
    assert!(without_scout_lifecycle.lifecycle.is_none());

    assert!(
        Pr13HookOrchestrationRequestV1::from_envelope(
            hook_envelope(HookEventV2::TestLifecycle {
                test_run_id: [8; 16],
                test_count: 1,
                phase: tracedecay_hooks::HookLifecyclePhaseV1::Completed,
                receipt_id: Some([9; 16]),
            }),
            &hook_binding(),
            Some(hook_lifecycle()),
            1,
            false,
        )
        .is_none()
    );
    assert_eq!(
        Pr13HookOrchestrationRequestV1::from_envelope(
            hook_envelope(HookEventV2::SessionBoundary {
                boundary: HookBoundaryV1::Start,
            }),
            &hook_binding(),
            Some(hook_lifecycle()),
            1,
            true,
        )
        .unwrap()
        .trigger,
        Pr13HookOrchestrationTriggerV1::Explicit
    );
}

#[tokio::test]
async fn pr13_hook_orchestration_backpressures_without_waiting() {
    let release = Arc::new(tokio::sync::Notify::new());
    let work_release = Arc::clone(&release);
    let work = move |_| {
        let release = Arc::clone(&work_release);
        async move { release.notified().await }
    };
    let runtime = BoundedPr13HookOrchestratorV1::new(1, work).unwrap();
    let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_completions = Arc::clone(&completions);
    let completed = Arc::new(tokio::sync::Notify::new());
    let completion_notification = Arc::clone(&completed);
    let mut request = Pr13HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
    request.completion = Some(Arc::new(move || {
        observed_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        completion_notification.notify_one();
    }));

    assert_eq!(
        runtime.admit(request.clone()),
        Pr13HookOrchestrationAdmissionV1::Enqueued
    );
    assert_eq!(
        runtime.admit(request),
        Pr13HookOrchestrationAdmissionV1::Backpressured
    );
    assert_eq!(completions.load(std::sync::atomic::Ordering::Relaxed), 0);
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(1), completed.notified())
        .await
        .expect("producer work completion");
    assert_eq!(
        completions.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "only completed producer work may clear the durable outbox"
    );
}

#[tokio::test]
async fn pr13_hook_orchestration_runs_feedback_work_without_scout_lifecycle() {
    let ran = Arc::new(tokio::sync::Notify::new());
    let work_ran = Arc::clone(&ran);
    let runtime = BoundedPr13HookOrchestratorV1::new(1, move |_| {
        let ran = Arc::clone(&work_ran);
        async move { ran.notify_one() }
    })
    .unwrap();
    let runtime: Arc<dyn Pr13HookOrchestrationPortV1> = runtime;
    pr13_hook_orchestration_registry()
        .lock()
        .unwrap()
        .insert(([3; 16], [5; 16]), Arc::downgrade(&runtime));

    assert_eq!(
        admit_registered_pr13_hook_orchestration(
            hook_envelope(HookEventV2::SavedEdit {
                file_id: [7; 16],
                changed_range_count: 1,
            }),
            hook_binding(),
            None,
            1,
            false,
            None,
        ),
        Pr13HookOrchestrationAdmissionV1::Enqueued
    );
    ran.notified().await;
    pr13_hook_orchestration_registry()
        .lock()
        .unwrap()
        .remove(&([3; 16], [5; 16]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_admission_conflicts_construct_zero_losing_producers() {
    #[derive(Clone, Copy, Debug)]
    enum ConflictSlot {
        CallableCode,
        Feedback,
        FeedbackCycleInput,
    }

    let _pin = crate::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.feedback.atomic-publication").expect("project id");
    let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
        crate::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered project runtime");
    let graph = host
        .initialize_project_graph_for_test(
            project.path(),
            crate::tracedecay::TraceDecayOpenOptions::default(),
        )
        .await
        .expect("initialized project graph");
    let database = graph.dashboard_database_guard().as_ref().clone();
    let scope = ResolvedScope::new(
        project_id.clone(),
        tracedecay_domain::RepositoryId::new("repository.feedback.atomic-publication")
            .expect("repository id"),
        tracedecay_domain::WorktreeId::new("worktree.feedback.atomic-publication")
            .expect("worktree id"),
        None,
    )
    .expect("resolved scope");
    let access = ProjectSourceAccessSnapshot {
        scope: scope.clone(),
        requester: ActorId::new("actor.feedback.atomic-publication").expect("actor"),
        binding: tracedecay_domain::configuration::ScopeSourceBinding::new(
            tracedecay_domain::SourceBindingId::new("binding.feedback.atomic-publication")
                .expect("binding"),
            tracedecay_domain::configuration::SourceKindV1::Cursor,
            tracedecay_domain::LocatorDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("locator"),
            tracedecay_domain::configuration::AuthorityRef::Project(project_id.clone()),
        )
        .expect("source binding"),
        configuration_revision: ConfigurationRevisionId::new(
            "revision.feedback.atomic-publication",
        )
        .expect("configuration revision"),
        configuration_digest: canonical_sha256(&"feedback-atomic-configuration")
            .expect("configuration digest"),
        configuration_provenance_digest: canonical_sha256(
            &"feedback-atomic-configuration-provenance",
        )
        .expect("configuration provenance"),
        effective_capabilities: std::collections::BTreeSet::default(),
        grant_expires_at: UtcMicros(i64::MAX),
    };
    let incumbent_runtime = Arc::new(
        open_pr12_feedback_runtime(
            database.clone(),
            project.path(),
            scope.clone(),
            access.clone(),
        )
        .await
        .expect("incumbent feedback runtime"),
    );
    let publications = incumbent_runtime.publication_store();
    let incumbent_boot = publications
        .observation_read_model()
        .await
        .expect("incumbent observation model")
        .watermark
        .producer_boot_id
        .expect("incumbent producer boot");
    for conflict in [
        ConflictSlot::CallableCode,
        ConflictSlot::Feedback,
        ConflictSlot::FeedbackCycleInput,
    ] {
        let service = DaemonInvocationService::default();
        match conflict {
            ConflictSlot::CallableCode => {
                service
                    .project_runtimes
                    .publish(
                        project.path().to_path_buf(),
                        RegisteredCallableCodeRuntime {
                            authorization: DaemonCallableCodeAuthorizationSource::production(
                                project.path().to_path_buf(),
                                scope.clone(),
                                Arc::clone(graph.configuration_runtime()),
                            ),
                            scope: scope.clone(),
                        },
                    )
                    .await
                    .unwrap();
            }
            ConflictSlot::Feedback => {
                service
                    .project_runtimes
                    .publish(
                        project.path().to_path_buf(),
                        RegisteredFeedbackRuntime {
                            project_id: project_id.clone(),
                            runtime: Arc::clone(&incumbent_runtime),
                        },
                    )
                    .await
                    .unwrap();
            }
            ConflictSlot::FeedbackCycleInput => {
                let unavailable = Arc::new(UnavailableFeedbackCycleRuntimeV1::new(
                    project_id.clone(),
                    incumbent_runtime.source_observation_port(),
                ));
                service
                    .project_runtimes
                    .publish(
                        project.path().to_path_buf(),
                        Arc::new(SwitchableFeedbackCycleRuntimeV1::new(unavailable)),
                    )
                    .await
                    .unwrap();
            }
        }

        let registrar = DaemonFeedbackRuntimeRegistrar::new(&service);
        let result = registrar
            .open_and_register(
                database.clone(),
                project.path().to_path_buf(),
                scope.clone(),
                access.clone(),
                Arc::clone(graph.configuration_runtime()),
            )
            .await;
        assert!(
            matches!(
                result,
                Err(DaemonFeedbackRuntimeRegistrationError::AlreadyRegistered)
            ),
            "{conflict:?} must reject the whole feedback publication"
        );
        assert_eq!(
            registrar.producer_constructions.load(Ordering::SeqCst),
            0,
            "{conflict:?} must reject before constructing any producer"
        );
        let watermark = publications
            .observation_read_model()
            .await
            .expect("observation model after rejected publication")
            .watermark;
        assert_eq!(
            watermark.producer_boot_id,
            Some(incumbent_boot.clone()),
            "{conflict:?} must preserve the incumbent durable producer boot"
        );
        assert_eq!(
            service
                .project_runtimes
                .holds::<RegisteredCallableCodeRuntime>(project.path())
                .await,
            matches!(conflict, ConflictSlot::CallableCode)
        );
        assert_eq!(
            service
                .project_runtimes
                .holds::<RegisteredFeedbackRuntime>(project.path())
                .await,
            matches!(conflict, ConflictSlot::Feedback)
        );
        assert_eq!(
            service
                .project_runtimes
                .holds::<Arc<SwitchableFeedbackCycleRuntimeV1>>(project.path())
                .await,
            matches!(conflict, ConflictSlot::FeedbackCycleInput)
        );
    }

    drop(publications);
    drop(incumbent_runtime);
    let service = DaemonInvocationService::default();
    service
        .project_runtimes
        .publish(
            project.path().to_path_buf(),
            Arc::new(()) as Arc<dyn Any + Send + Sync>,
        )
        .await
        .unwrap();
    let (publication_ready, publication_is_ready) = tokio::sync::oneshot::channel();
    let (continue_publication, publication_may_continue) = tokio::sync::oneshot::channel();
    let (commit_starting, commit_is_starting) = tokio::sync::oneshot::channel();
    let gate = Arc::new(DaemonFeedbackPublicationTestGate {
        publication_ready: StdMutex::new(Some(publication_ready)),
        continue_publication: Mutex::new(Some(publication_may_continue)),
    });
    service
        .project_runtimes
        .arm_commit_starting(commit_starting);
    let registrar = DaemonFeedbackRuntimeRegistrar::new(&service).with_publication_gate(gate);
    let publisher_registrar = registrar.clone();
    let publisher_database = database.clone();
    let publisher_root = project.path().to_path_buf();
    let publisher_scope = scope.clone();
    let publisher_access = access.clone();
    let publisher_configuration = Arc::clone(graph.configuration_runtime());
    let publisher = tokio::spawn(async move {
        publisher_registrar
            .open_and_register(
                publisher_database,
                publisher_root,
                publisher_scope,
                publisher_access,
                publisher_configuration,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), publication_is_ready)
        .await
        .expect("feedback publication must reach its commit gate")
        .expect("publication-ready sender");

    let observer_registry = service.project_runtimes.clone();
    let observer_root = project.path().to_path_buf();
    let (observer_holding, observer_is_holding) = std::sync::mpsc::channel();
    let (release_observer, observer_may_release) = std::sync::mpsc::channel();
    let observer = tokio::spawn(async move {
        let mut samples = vec![
            observer_registry
                .feedback_publication_state(&observer_root)
                .await,
        ];
        observer_registry
            .read::<Arc<dyn Any + Send + Sync>, _, _>(&observer_root, move |_| {
                observer_holding
                    .send(())
                    .expect("observer-holding receiver");
                observer_may_release
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("release-observer sender");
            })
            .await
            .expect("observer marker");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let sample = observer_registry
                    .feedback_publication_state(&observer_root)
                    .await;
                samples.push(sample);
                if sample == (true, true, true) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("observer must reach the complete feedback publication");
        samples
    });
    tokio::task::spawn_blocking(move || {
        observer_is_holding
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("observer must hold the registry lock");
    })
    .await
    .expect("observer-holding task");
    continue_publication
        .send(())
        .expect("continue-publication receiver");
    tokio::time::timeout(std::time::Duration::from_secs(2), commit_is_starting)
        .await
        .expect("feedback publication must start committing")
        .expect("commit-starting sender");
    release_observer
        .send(())
        .expect("release-observer receiver");

    tokio::time::timeout(std::time::Duration::from_secs(2), publisher)
        .await
        .expect("feedback publisher must not deadlock at commit")
        .expect("feedback publisher task")
        .expect("feedback publication");
    assert_eq!(
        registrar.producer_constructions.load(Ordering::SeqCst),
        1,
        "the successful registrar path must construct exactly one producer"
    );
    let samples = tokio::time::timeout(std::time::Duration::from_secs(2), observer)
        .await
        .expect("feedback observer must finish after publication")
        .expect("feedback observer task");
    assert_eq!(samples.last(), Some(&(true, true, true)));
    assert!(
        samples
            .iter()
            .all(|sample| matches!(sample, (false, false, false) | (true, true, true))),
        "the real registrar exposed a partial feedback runtime: {samples:?}"
    );
}
