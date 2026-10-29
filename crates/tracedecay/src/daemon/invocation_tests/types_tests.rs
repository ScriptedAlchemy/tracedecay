use super::*;
use std::any::Any;
use std::sync::atomic::Ordering;

use tracedecay_application::ProjectSourceAccessSnapshot;
use tracedecay_application::feedback::concrete::open_feedback_runtime;
use tracedecay_daemon_service::DaemonCallableCodeAuthorizationSource;
use tracedecay_daemon_service::{DaemonInvocationService, *};
use tracedecay_domain::{ActorId, ConfigurationRevisionId, canonical_sha256};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_admission_conflicts_construct_zero_losing_producers() {
    #[derive(Clone, Copy, Debug)]
    enum ConflictSlot {
        CallableCode,
        Feedback,
        FeedbackCycleInput,
    }

    let profile = tempfile::tempdir().expect("profile root");
    let project = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.feedback.atomic-publication").expect("project id");
    let host =
        tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
            profile.path(),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered project runtime");
    let graph = host
        .initialize_project_graph_for_test(
            project.path(),
            tracedecay_project::project::TraceDecayOpenOptions {
                profile_root: Some(profile.path().to_path_buf()),
                global_db_path: None,
            },
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
        open_feedback_runtime(
            database.clone(),
            project.path(),
            project.path().join("response-handles"),
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
                        RegisteredCallableCodeRuntime::new(
                            scope.clone(),
                            Arc::new(DaemonCallableCodeAuthorizationSource::production(
                                project.path().to_path_buf(),
                                scope.clone(),
                                Arc::clone(graph.configuration_runtime()),
                            )),
                        ),
                    )
                    .await
                    .unwrap();
            }
            ConflictSlot::Feedback => {
                service
                    .project_runtimes
                    .publish(
                        project.path().to_path_buf(),
                        RegisteredFeedbackRuntime::new(
                            project_id.clone(),
                            Arc::clone(&incumbent_runtime),
                        ),
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
                project.path().join("response-handles"),
                scope.clone(),
                access.clone(),
                Arc::new(DaemonCallableCodeAuthorizationSource::production(
                    project.path().to_path_buf(),
                    scope.clone(),
                    Arc::clone(graph.configuration_runtime()),
                )),
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

    // Recovery can fence this exact root while feedback construction runs
    // outside the lock. The prepared bundle must then remain unpublished.
    let recovering_service = DaemonInvocationService::default();
    let (publication_ready, publication_is_ready) = tokio::sync::oneshot::channel();
    let (continue_publication, publication_may_continue) = tokio::sync::oneshot::channel();
    let gate = Arc::new(DaemonFeedbackPublicationTestGate::new(
        publication_ready,
        publication_may_continue,
    ));
    let registrar =
        DaemonFeedbackRuntimeRegistrar::new(&recovering_service).with_publication_gate(gate);
    let publisher_database = database.clone();
    let publisher_root = project.path().to_path_buf();
    let publisher_scope = scope.clone();
    let publisher_access = access.clone();
    let publisher_configuration = Arc::clone(graph.configuration_runtime());
    let publisher = tokio::spawn(async move {
        registrar
            .open_and_register(
                publisher_database,
                publisher_root.clone(),
                publisher_root.join("response-handles"),
                publisher_scope.clone(),
                publisher_access,
                Arc::new(DaemonCallableCodeAuthorizationSource::production(
                    publisher_root,
                    publisher_scope,
                    publisher_configuration,
                )),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), publication_is_ready)
        .await
        .expect("feedback construction must reach its publication gate")
        .expect("publication-ready sender");
    assert_eq!(
        recovering_service
            .project_runtimes
            .feedback_publication_state(project.path()),
        (false, false, false)
    );
    let roots = std::collections::BTreeSet::from([project.path().to_path_buf()]);
    let mut recovery = Box::pin(recovering_service.project_runtimes.quiesce_roots(&roots));
    let first_poll = std::future::poll_fn(|context| {
        std::task::Poll::Ready(std::future::Future::poll(recovery.as_mut(), context))
    })
    .await;
    assert!(
        first_poll.is_pending(),
        "recovery must fence the root and wait for the live feedback reservation"
    );
    continue_publication
        .send(())
        .expect("continue-publication receiver");
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), publisher)
        .await
        .expect("fenced publication must settle")
        .expect("feedback publisher task");
    assert!(matches!(
        result,
        Err(DaemonFeedbackRuntimeRegistrationError::RegistryClosed)
    ));
    let quiescence = tokio::time::timeout(std::time::Duration::from_secs(2), recovery)
        .await
        .expect("rejected publication must release its reservation")
        .expect("root recovery fence");
    assert!(recovering_service.project_runtimes.is_empty());
    drop(quiescence);

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
    let gate = Arc::new(DaemonFeedbackPublicationTestGate::new(
        publication_ready,
        publication_may_continue,
    ));
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
                publisher_root.clone(),
                publisher_root.join("response-handles"),
                publisher_scope.clone(),
                publisher_access,
                Arc::new(DaemonCallableCodeAuthorizationSource::production(
                    publisher_root,
                    publisher_scope,
                    publisher_configuration,
                )),
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
        let mut samples = vec![observer_registry.feedback_publication_state(&observer_root)];
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
                let sample = observer_registry.feedback_publication_state(&observer_root);
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
