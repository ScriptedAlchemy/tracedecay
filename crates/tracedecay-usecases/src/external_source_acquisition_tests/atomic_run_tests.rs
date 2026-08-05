use super::*;

struct BarrierAuthorizationPort {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl SourceAcquisitionAuthorizationPortV1 for BarrierAuthorizationPort {
    fn recheck<'a>(
        &'a self,
        _task: &'a SourceScheduledRefetchV1,
        _phase: SourceAcquisitionAuthorizationPhaseV1,
    ) -> SourceAcquisitionFuture<'a, SourceAcquisitionAuthorizationOutcomeV1> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            SourceAcquisitionAuthorizationOutcomeV1::Unauthorized
        })
    }
}

impl SourceCanonicalRefetchPortV1 for BarrierAuthorizationPort {
    fn refetch<'a>(
        &'a self,
        _task: &'a SourceScheduledRefetchV1,
        _grant: &'a SourceAcquisitionGrantV1,
        _cancellation: &'a crate::observation::ObservationCancellation,
    ) -> SourceAcquisitionFuture<'a, SourceCanonicalRefetchOutcomeV1> {
        Box::pin(async { panic!("unauthorized run must not fetch") })
    }
}

impl SourceCanonicalCommitPortV1 for BarrierAuthorizationPort {
    fn commit<'a>(
        &'a self,
        _task: &'a SourceScheduledRefetchV1,
        _grant: &'a SourceAcquisitionGrantV1,
        _page: SourceCanonicalRefetchPageV1,
        _authority: &'a tracedecay_application::SourceCanonicalRefetchAuthorityV1,
        _cancellation: &'a crate::observation::ObservationCancellation,
    ) -> SourceAcquisitionFuture<'a, SourceCanonicalCommitOutcomeV1> {
        Box::pin(async { panic!("unauthorized run must not commit") })
    }
}

#[tokio::test]
async fn atomic_admission_holds_wake_and_successor_until_bound_run_finishes() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let barrier = Arc::new(BarrierAuthorizationPort {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let owner = Arc::new(
        ExternalSourceAcquisitionOwnerV1::new(
            Arc::new(MemoryStatePort::default()),
            Arc::clone(&barrier),
            Arc::clone(&barrier),
            barrier,
            SourceAcquisitionPolicyV1::new(
                3,
                Duration::from_secs(1),
                Duration::from_millis(10),
                Duration::from_millis(40),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let wake = {
        let owner = Arc::clone(&owner);
        tokio::spawn(async move { owner.wait_for_wake().await })
    };
    tokio::task::yield_now().await;
    let (definition, binding, request) = source();
    let event = SourceEventV1::new(binding.immutable_identity().unwrap(), digest('b')).unwrap();
    let expected_event_key = event.event_key().clone();
    let successor_definition = definition.clone();
    let successor_binding = binding.clone();
    let successor_request = request.clone();
    let run = {
        let owner = Arc::clone(&owner);
        tokio::spawn(async move {
            owner
                .admit_event_and_run_one(
                    &definition,
                    &binding,
                    &request,
                    event,
                    UtcMicros(10),
                    &crate::observation::ObservationCancellation::default(),
                )
                .await
        })
    };

    entered.notified().await;
    let successor = {
        let owner = Arc::clone(&owner);
        tokio::spawn(async move {
            owner
                .admit_event(
                    &successor_definition,
                    &successor_binding,
                    &successor_request,
                    SourceEventV1::new(
                        successor_binding.immutable_identity().unwrap(),
                        digest('c'),
                    )
                    .unwrap(),
                    UtcMicros(11),
                )
                .await
        })
    };
    tokio::task::yield_now().await;
    assert!(!wake.is_finished());
    assert!(
        !successor.is_finished(),
        "successor admission must not contend with the active run's finish CAS"
    );
    release.notify_one();
    let run = run.await.unwrap().unwrap();
    let successor = successor.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(1), wake)
        .await
        .expect("background wake must publish after the bound run")
        .unwrap();

    assert_eq!(run.admission().event_key(), &expected_event_key);
    assert_eq!(run.outcome(), &SourceAcquisitionRunOutcomeV1::Unauthorized);
    assert_eq!(
        successor.disposition(),
        SourceEventAdmissionDispositionV1::Enqueued
    );
    assert_eq!(owner.pending_count().await.unwrap(), 1);
}

#[tokio::test]
async fn coalesced_successor_runs_once_after_bound_active_refresh() {
    let state = Arc::new(MemoryStatePort::default());
    let (definition, binding, request) = source();
    let seed = ExternalSourceAcquisitionOwnerV1::new(
        Arc::clone(&state),
        Arc::new(NeverPort),
        Arc::new(NeverPort),
        Arc::new(NeverPort),
        SourceAcquisitionPolicyV1::new(
            3,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(40),
        )
        .unwrap(),
    )
    .unwrap();
    seed.admit_event(
        &definition,
        &binding,
        &request,
        SourceEventV1::new(binding.immutable_identity().unwrap(), digest('b')).unwrap(),
        UtcMicros(10),
    )
    .await
    .unwrap();
    let task = pending_task(&state, &binding).await;
    let grant = grant('7');
    let complete = page(
        &task,
        &grant,
        task.refresh().refresh_id().clone(),
        SourceCoverageV1::Complete,
    );
    let scripted = Arc::new(ScriptedPort::new(
        [
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant.clone()),
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant.clone()),
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant.clone()),
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant),
        ],
        [
            SourceCanonicalRefetchOutcomeV1::Fetched(complete.clone()),
            SourceCanonicalRefetchOutcomeV1::Fetched(complete),
        ],
    ));
    let owner = ExternalSourceAcquisitionOwnerV1::new(
        state,
        Arc::clone(&scripted),
        Arc::clone(&scripted),
        Arc::clone(&scripted),
        SourceAcquisitionPolicyV1::new(
            3,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(40),
        )
        .unwrap(),
    )
    .unwrap();
    let event = SourceEventV1::new(binding.immutable_identity().unwrap(), digest('c')).unwrap();
    let event_key = event.event_key().clone();

    let first = owner
        .admit_event_and_run_one(
            &definition,
            &binding,
            &request,
            event,
            UtcMicros(11),
            &crate::observation::ObservationCancellation::default(),
        )
        .await
        .unwrap();

    assert_eq!(first.admission().event_key(), &event_key);
    assert_eq!(
        first.admission().disposition(),
        SourceEventAdmissionDispositionV1::Coalesced
    );
    assert!(matches!(
        first.outcome(),
        SourceAcquisitionRunOutcomeV1::Committed { .. }
    ));
    assert_eq!(owner.pending_count().await.unwrap(), 1);
    assert!(matches!(
        owner
            .run_one(
                UtcMicros(11),
                &crate::observation::ObservationCancellation::default(),
            )
            .await
            .unwrap(),
        SourceAcquisitionRunOutcomeV1::Committed { .. }
    ));
    assert_eq!(scripted.fetch_count.load(Ordering::Relaxed), 2);
    assert_eq!(scripted.commit_count.load(Ordering::Relaxed), 2);
    assert_eq!(owner.pending_count().await.unwrap(), 0);
}
