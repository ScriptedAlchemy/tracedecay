use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracedecay_domain::{
    AccessPolicyDigest, CapabilityId, ComponentVersion, LocatorDigest, ManifestDigest,
    PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProviderId,
    ResolutionAuthorizationV1, RetrievalAnchorId, SanitizationReceiptId, SanitizationReceiptRefV1,
    ScopeResolutionId, SourceAcquisitionCapabilitiesV1, SourceAcquisitionContractV1,
    SourceBindingIdentityV1, SourceBindingOwnerV1, SourceBindingV1, SourceCaptureModeV1,
    SourceContentStateV1, SourceCoverageV1, SourceDefinitionV1, SourceDeletionSemanticsV1,
    SourceEnvelopeKindV1, SourceEventAdmissionDispositionV1, SourceEventV1, SourceInstanceId,
    SourceNativeObjectIdV1, SourceObjectObservationV1, SourceObjectRevisionV1, SourcePartitionIdV1,
    SourceProviderEnvelopeV1, SourceRefetchStrategyV1, SourceSnapshotIdV1, UtcMicros,
};
use tracedecay_store::{
    SourceObjectMutationV1, SourceObjectTransitionV1, SourceObservationEvidenceV1,
};

use super::*;

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
}

fn source() -> (SourceDefinitionV1, SourceBindingV1) {
    let capabilities = SourceAcquisitionCapabilitiesV1::new(
        BTreeSet::from([SourceCaptureModeV1::Event]),
        BTreeSet::from([SourceRefetchStrategyV1::WholeRoot]),
        BTreeSet::from([SourceDeletionSemanticsV1::ExplicitOnly]),
    )
    .unwrap();
    let definition = SourceDefinitionV1::new(
        SourceInstanceId::new("source.scheduler-fixture").unwrap(),
        1,
        SourceAcquisitionContractV1::new(
            ProviderId::new("fixture-provider").unwrap(),
            capabilities,
        )
        .unwrap(),
        SourceCaptureModeV1::Event,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::ExplicitOnly,
        1,
    )
    .unwrap();
    let binding = SourceBindingV1::new(
        &definition,
        SourceBindingOwnerV1::Project(ProjectId::new("project.scheduler-fixture").unwrap()),
        PrivacyDomainId::new("privacy.scheduler-fixture").unwrap(),
        LocatorDigest::new(digest('a').as_str()).unwrap(),
        1,
    )
    .unwrap();
    (definition, binding)
}

#[derive(Default)]
struct MemoryStatePort {
    states: Mutex<BTreeMap<SourceBindingIdentityV1, SourceAcquisitionQueueStateV1>>,
}

impl SourceAcquisitionStatePortV1 for MemoryStatePort {
    fn load<'a>(
        &'a self,
        binding: &'a SourceBindingIdentityV1,
    ) -> SourceAcquisitionFuture<
        'a,
        Result<Option<SourceAcquisitionQueueStateV1>, SourceAcquisitionStateErrorV1>,
    > {
        Box::pin(async move { Ok(self.states.lock().unwrap().get(binding).cloned()) })
    }

    fn compare_and_swap<'a>(
        &'a self,
        binding: &'a SourceBindingIdentityV1,
        expected: Option<&'a ManifestDigest>,
        next: SourceAcquisitionQueueStateV1,
    ) -> SourceAcquisitionFuture<
        'a,
        Result<SourceAcquisitionCasOutcomeV1, SourceAcquisitionStateErrorV1>,
    > {
        Box::pin(async move {
            let mut states = self.states.lock().unwrap();
            let current = states.get(binding);
            if current.map(SourceAcquisitionQueueStateV1::state_digest) != expected {
                return Ok(SourceAcquisitionCasOutcomeV1::Conflict);
            }
            states.insert(binding.clone(), next);
            Ok(SourceAcquisitionCasOutcomeV1::Committed)
        })
    }

    fn next_ready<'a>(
        &'a self,
        now: UtcMicros,
    ) -> SourceAcquisitionFuture<
        'a,
        Result<Option<SourceAcquisitionQueueStateV1>, SourceAcquisitionStateErrorV1>,
    > {
        Box::pin(async move {
            Ok(self
                .states
                .lock()
                .unwrap()
                .values()
                .find(|state| state.is_ready(now))
                .cloned())
        })
    }

    fn pending_count(
        &self,
    ) -> SourceAcquisitionFuture<'_, Result<usize, SourceAcquisitionStateErrorV1>> {
        Box::pin(async move {
            Ok(self
                .states
                .lock()
                .unwrap()
                .values()
                .filter(|state| state.active().is_some())
                .count())
        })
    }
}

struct NeverPort;

impl SourceAcquisitionAuthorizationPortV1 for NeverPort {
    fn recheck<'a>(
        &'a self,
        _task: &'a SourceScheduledRefetchV1,
        _phase: SourceAcquisitionAuthorizationPhaseV1,
    ) -> SourceAcquisitionFuture<'a, SourceAcquisitionAuthorizationOutcomeV1> {
        Box::pin(async { panic!("admission must not invoke authorization") })
    }
}

impl SourceCanonicalRefetchPortV1 for NeverPort {
    fn refetch<'a>(
        &'a self,
        _task: &'a SourceScheduledRefetchV1,
        _grant: &'a SourceAcquisitionGrantV1,
        _cancellation: &'a crate::observation::ObservationCancellation,
    ) -> SourceAcquisitionFuture<'a, SourceCanonicalRefetchOutcomeV1> {
        Box::pin(async { panic!("admission must not invoke refetch") })
    }
}

impl SourceCanonicalCommitPortV1 for NeverPort {
    fn commit<'a>(
        &'a self,
        _task: &'a SourceScheduledRefetchV1,
        _grant: &'a SourceAcquisitionGrantV1,
        _page: SourceCanonicalRefetchPageV1,
        _authority: &'a tracedecay_application::SourceCanonicalRefetchAuthorityV1,
        _cancellation: &'a crate::observation::ObservationCancellation,
    ) -> SourceAcquisitionFuture<'a, SourceCanonicalCommitOutcomeV1> {
        Box::pin(async { panic!("admission must not invoke commit") })
    }
}

struct ScriptedPort {
    authorizations: Mutex<VecDeque<SourceAcquisitionAuthorizationOutcomeV1>>,
    fetches: Mutex<VecDeque<SourceCanonicalRefetchOutcomeV1>>,
    fetch_count: AtomicUsize,
    commit_count: AtomicUsize,
}

impl ScriptedPort {
    fn new(
        authorizations: impl IntoIterator<Item = SourceAcquisitionAuthorizationOutcomeV1>,
        fetches: impl IntoIterator<Item = SourceCanonicalRefetchOutcomeV1>,
    ) -> Self {
        Self {
            authorizations: Mutex::new(authorizations.into_iter().collect()),
            fetches: Mutex::new(fetches.into_iter().collect()),
            fetch_count: AtomicUsize::new(0),
            commit_count: AtomicUsize::new(0),
        }
    }
}

impl SourceAcquisitionAuthorizationPortV1 for ScriptedPort {
    fn recheck<'a>(
        &'a self,
        _task: &'a SourceScheduledRefetchV1,
        _phase: SourceAcquisitionAuthorizationPhaseV1,
    ) -> SourceAcquisitionFuture<'a, SourceAcquisitionAuthorizationOutcomeV1> {
        Box::pin(async move {
            self.authorizations
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted authorization outcome")
        })
    }
}

impl SourceCanonicalRefetchPortV1 for ScriptedPort {
    fn refetch<'a>(
        &'a self,
        _task: &'a SourceScheduledRefetchV1,
        _grant: &'a SourceAcquisitionGrantV1,
        _cancellation: &'a crate::observation::ObservationCancellation,
    ) -> SourceAcquisitionFuture<'a, SourceCanonicalRefetchOutcomeV1> {
        Box::pin(async move {
            self.fetch_count.fetch_add(1, Ordering::Relaxed);
            self.fetches
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted refetch outcome")
        })
    }
}

impl SourceCanonicalCommitPortV1 for ScriptedPort {
    fn commit<'a>(
        &'a self,
        task: &'a SourceScheduledRefetchV1,
        _grant: &'a SourceAcquisitionGrantV1,
        page: SourceCanonicalRefetchPageV1,
        authority: &'a tracedecay_application::SourceCanonicalRefetchAuthorityV1,
        _cancellation: &'a crate::observation::ObservationCancellation,
    ) -> SourceAcquisitionFuture<'a, SourceCanonicalCommitOutcomeV1> {
        Box::pin(async move {
            assert!(
                authority.authorizes(task.refresh()),
                "commit must receive the resumed canonical-refetch authority"
            );
            self.commit_count.fetch_add(1, Ordering::Relaxed);
            let whole_root_stage = matches!(
                page.envelope.kind(),
                SourceEnvelopeKindV1::WholeRoot | SourceEnvelopeKindV1::WholeRootFallback
            )
            .then(|| {
                SourceWholeRootStageV1::advance(
                    task.whole_root_stage(),
                    &page.envelope,
                    page.present_objects,
                )
                .unwrap()
            });
            SourceCanonicalCommitOutcomeV1::Committed {
                coverage: page.envelope.coverage(),
                whole_root_stage,
            }
        })
    }
}

fn grant(seed: char) -> SourceAcquisitionGrantV1 {
    SourceAcquisitionGrantV1::new(1, digest(seed), 1, digest('8'), digest('9')).unwrap()
}

fn page(
    task: &SourceScheduledRefetchV1,
    grant: &SourceAcquisitionGrantV1,
    refresh_id: ManifestDigest,
    coverage: SourceCoverageV1,
) -> SourceCanonicalRefetchPageV1 {
    let identity = task.binding().immutable_identity().unwrap();
    let partition = SourcePartitionIdV1::new(digest('c'));
    let snapshot = SourceSnapshotIdV1::new(digest('d'));
    let envelope = SourceProviderEnvelopeV1::new(
        identity.clone(),
        task.definition().provider.clone(),
        refresh_id,
        SourceRefreshCauseV1::Event,
        task.definition().capture_mode,
        task.definition().refetch_strategy,
        SourceEnvelopeKindV1::WholeRoot,
        partition.clone(),
        1,
        None,
        (coverage == SourceCoverageV1::Partial)
            .then(|| tracedecay_domain::SourceCursorV1::new(digest('4'))),
        Some(snapshot),
        coverage,
        digest('e'),
    )
    .unwrap();
    let observation = SourceObjectObservationV1::new(
        SourceNativeObjectIdV1::new(digest('f')),
        SourceObjectRevisionV1::new(digest('0')),
        digest('1'),
        SourceContentStateV1::Live,
    )
    .unwrap();
    let evidence = SourceObservationEvidenceV1::new(
        identity.clone(),
        partition,
        &observation,
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.scheduler-fixture").unwrap(),
            ComponentVersion::new("sanitizer.scheduler-fixture.v1").unwrap(),
        )
        .unwrap(),
        RetrievalAnchorId::new("retrieval.scheduler-fixture").unwrap(),
        ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.scheduler-fixture").unwrap(),
            privacy_domain_id: identity.privacy_domain,
            access_policy_digest: AccessPolicyDigest::new(digest('2').as_str()).unwrap(),
            capability_id: CapabilityId::new("capability.scheduler-fixture").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(digest('3').as_str())
                .unwrap(),
        },
        grant.source_authorization_digest.clone(),
    )
    .unwrap();
    SourceCanonicalRefetchPageV1 {
        envelope,
        present_objects: [observation.native_object().clone()].into_iter().collect(),
        mutations: vec![
            SourceObjectMutationV1::new(
                observation,
                None,
                SourceObjectTransitionV1::Initial,
                evidence,
            )
            .unwrap(),
        ],
    }
}

async fn pending_task(
    state: &MemoryStatePort,
    binding: &SourceBindingV1,
) -> SourceScheduledRefetchV1 {
    state
        .load(&binding.immutable_identity().unwrap())
        .await
        .unwrap()
        .unwrap()
        .active()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn event_wake_is_content_free_and_duplicate_delivery_schedules_once() {
    let state = Arc::new(MemoryStatePort::default());
    let owner = Arc::new(
        ExternalSourceAcquisitionOwnerV1::new(
            state,
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
        .unwrap(),
    );
    let (definition, binding) = source();
    let event = SourceEventV1::new(binding.immutable_identity().unwrap(), digest('b')).unwrap();
    let wake = {
        let owner = owner.clone();
        tokio::spawn(async move {
            owner.wait_for_wake().await;
        })
    };

    let first = owner
        .admit_event(&definition, &binding, event.clone(), UtcMicros(10))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), wake)
        .await
        .expect("event admission must wake the acquisition owner")
        .unwrap();
    let duplicate = owner
        .admit_event(&definition, &binding, event, UtcMicros(11))
        .await
        .unwrap();

    assert_eq!(
        first.disposition(),
        SourceEventAdmissionDispositionV1::Enqueued
    );
    assert_eq!(
        duplicate.disposition(),
        SourceEventAdmissionDispositionV1::Duplicate
    );
    assert_eq!(duplicate.original_refresh(), first.original_refresh());
    assert_eq!(
        owner.pending_count().await.unwrap(),
        1,
        "duplicate delivery must not schedule another canonical refetch"
    );
    let encoded = serde_json::to_string(&first).unwrap();
    for forbidden in ["title", "body", "excerpt", "path", "url", "payload"] {
        assert!(
            !encoded.contains(forbidden),
            "event receipt exposed forbidden provider content field {forbidden}"
        );
    }
}

#[tokio::test]
async fn restart_replays_exact_refresh_and_commits_sanitized_provenance() {
    let state = Arc::new(MemoryStatePort::default());
    let (definition, binding) = source();
    let event = SourceEventV1::new(binding.immutable_identity().unwrap(), digest('b')).unwrap();
    let admission_owner = ExternalSourceAcquisitionOwnerV1::new(
        state.clone(),
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
    admission_owner
        .admit_event(&definition, &binding, event, UtcMicros(10))
        .await
        .unwrap();
    drop(admission_owner);

    let task = pending_task(&state, &binding).await;
    let grant = grant('7');
    let scripted = Arc::new(ScriptedPort::new(
        [
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant.clone()),
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant.clone()),
        ],
        [SourceCanonicalRefetchOutcomeV1::Fetched(page(
            &task,
            &grant,
            task.refresh().refresh_id().clone(),
            SourceCoverageV1::Complete,
        ))],
    ));
    let restarted = ExternalSourceAcquisitionOwnerV1::new(
        state,
        scripted.clone(),
        scripted.clone(),
        scripted.clone(),
        SourceAcquisitionPolicyV1::new(
            3,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(40),
        )
        .unwrap(),
    )
    .unwrap();

    let outcome = restarted
        .run_one(
            UtcMicros(10),
            &crate::observation::ObservationCancellation::default(),
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        SourceAcquisitionRunOutcomeV1::Committed {
            coverage: SourceCoverageV1::Complete,
            exact_duplicate: false,
        }
    );
    assert_eq!(scripted.fetch_count.load(Ordering::Relaxed), 1);
    assert_eq!(scripted.commit_count.load(Ordering::Relaxed), 1);
    assert_eq!(restarted.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn coalesced_event_runs_once_after_the_active_refresh_completes() {
    let state = Arc::new(MemoryStatePort::default());
    let (definition, binding) = source();
    let seed = ExternalSourceAcquisitionOwnerV1::new(
        state.clone(),
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
        SourceEventV1::new(binding.immutable_identity().unwrap(), digest('b')).unwrap(),
        UtcMicros(10),
    )
    .await
    .unwrap();
    let coalesced = seed
        .admit_event(
            &definition,
            &binding,
            SourceEventV1::new(binding.immutable_identity().unwrap(), digest('c')).unwrap(),
            UtcMicros(11),
        )
        .await
        .unwrap();
    assert_eq!(
        coalesced.disposition(),
        SourceEventAdmissionDispositionV1::Coalesced
    );

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
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant.clone()),
        ],
        [
            SourceCanonicalRefetchOutcomeV1::Fetched(complete.clone()),
            SourceCanonicalRefetchOutcomeV1::Fetched(complete),
        ],
    ));
    let owner = ExternalSourceAcquisitionOwnerV1::new(
        state,
        scripted.clone(),
        scripted.clone(),
        scripted.clone(),
        SourceAcquisitionPolicyV1::new(
            3,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(40),
        )
        .unwrap(),
    )
    .unwrap();
    let cancellation = crate::observation::ObservationCancellation::default();

    assert!(matches!(
        owner.run_one(UtcMicros(11), &cancellation).await.unwrap(),
        SourceAcquisitionRunOutcomeV1::Committed { .. }
    ));
    assert_eq!(
        owner.pending_count().await.unwrap(),
        1,
        "the coalesced successor must become active after the first commit"
    );
    assert!(matches!(
        owner.run_one(UtcMicros(11), &cancellation).await.unwrap(),
        SourceAcquisitionRunOutcomeV1::Committed { .. }
    ));
    assert_eq!(scripted.fetch_count.load(Ordering::Relaxed), 2);
    assert_eq!(owner.pending_count().await.unwrap(), 0);
}

#[tokio::test]
async fn revoked_authorization_blocks_fetch_and_commit_independently() {
    let state = Arc::new(MemoryStatePort::default());
    let (definition, binding) = source();
    let event = SourceEventV1::new(binding.immutable_identity().unwrap(), digest('b')).unwrap();
    let denied = Arc::new(ScriptedPort::new(
        [SourceAcquisitionAuthorizationOutcomeV1::Unauthorized],
        [],
    ));
    let owner = ExternalSourceAcquisitionOwnerV1::new(
        state.clone(),
        denied.clone(),
        denied.clone(),
        denied.clone(),
        SourceAcquisitionPolicyV1::new(
            3,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(40),
        )
        .unwrap(),
    )
    .unwrap();
    owner
        .admit_event(&definition, &binding, event, UtcMicros(10))
        .await
        .unwrap();
    assert_eq!(
        owner
            .run_one(
                UtcMicros(10),
                &crate::observation::ObservationCancellation::default(),
            )
            .await
            .unwrap(),
        SourceAcquisitionRunOutcomeV1::Unauthorized
    );
    assert_eq!(denied.fetch_count.load(Ordering::Relaxed), 0);
    assert_eq!(denied.commit_count.load(Ordering::Relaxed), 0);

    let event = SourceEventV1::new(binding.immutable_identity().unwrap(), digest('5')).unwrap();
    let task_seed = ExternalSourceAcquisitionOwnerV1::new(
        state.clone(),
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
    task_seed
        .admit_event(&definition, &binding, event, UtcMicros(20))
        .await
        .unwrap();
    let task = pending_task(&state, &binding).await;
    let grant = grant('7');
    let revoked = Arc::new(ScriptedPort::new(
        [
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant.clone()),
            SourceAcquisitionAuthorizationOutcomeV1::Unauthorized,
        ],
        [SourceCanonicalRefetchOutcomeV1::Fetched(page(
            &task,
            &grant,
            task.refresh().refresh_id().clone(),
            SourceCoverageV1::Complete,
        ))],
    ));
    let owner = ExternalSourceAcquisitionOwnerV1::new(
        state,
        revoked.clone(),
        revoked.clone(),
        revoked.clone(),
        SourceAcquisitionPolicyV1::new(
            3,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(40),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        owner
            .run_one(
                UtcMicros(20),
                &crate::observation::ObservationCancellation::default(),
            )
            .await
            .unwrap(),
        SourceAcquisitionRunOutcomeV1::Unauthorized
    );
    assert_eq!(revoked.fetch_count.load(Ordering::Relaxed), 1);
    assert_eq!(revoked.commit_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn remote_refresh_change_is_blocked_before_commit() {
    let state = Arc::new(MemoryStatePort::default());
    let (definition, binding) = source();
    let event = SourceEventV1::new(binding.immutable_identity().unwrap(), digest('b')).unwrap();
    let seed = ExternalSourceAcquisitionOwnerV1::new(
        state.clone(),
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
    seed.admit_event(&definition, &binding, event, UtcMicros(10))
        .await
        .unwrap();
    let task = pending_task(&state, &binding).await;
    let grant = grant('7');
    let changed = Arc::new(ScriptedPort::new(
        [SourceAcquisitionAuthorizationOutcomeV1::Authorized(
            grant.clone(),
        )],
        [SourceCanonicalRefetchOutcomeV1::Fetched(page(
            &task,
            &grant,
            digest('6'),
            SourceCoverageV1::Complete,
        ))],
    ));
    let owner = ExternalSourceAcquisitionOwnerV1::new(
        state,
        changed.clone(),
        changed.clone(),
        changed.clone(),
        SourceAcquisitionPolicyV1::new(
            3,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(40),
        )
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        owner
            .run_one(
                UtcMicros(10),
                &crate::observation::ObservationCancellation::default(),
            )
            .await
            .unwrap(),
        SourceAcquisitionRunOutcomeV1::BlockedRemoteChange
    );
    assert_eq!(changed.commit_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn unavailable_refetch_uses_bounded_backoff_and_cancellation_preserves_replay() {
    let state = Arc::new(MemoryStatePort::default());
    let (definition, binding) = source();
    let event = SourceEventV1::new(binding.immutable_identity().unwrap(), digest('b')).unwrap();
    let unavailable = Arc::new(ScriptedPort::new(
        [
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant('7')),
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant('7')),
            SourceAcquisitionAuthorizationOutcomeV1::Authorized(grant('7')),
        ],
        [
            SourceCanonicalRefetchOutcomeV1::Unavailable,
            SourceCanonicalRefetchOutcomeV1::Unavailable,
            SourceCanonicalRefetchOutcomeV1::Unavailable,
        ],
    ));
    let owner = ExternalSourceAcquisitionOwnerV1::new(
        state.clone(),
        unavailable.clone(),
        unavailable.clone(),
        unavailable,
        SourceAcquisitionPolicyV1::new(
            3,
            Duration::from_secs(1),
            Duration::from_millis(10),
            Duration::from_millis(40),
        )
        .unwrap(),
    )
    .unwrap();
    owner
        .admit_event(&definition, &binding, event, UtcMicros(1_000))
        .await
        .unwrap();
    assert_eq!(
        owner
            .run_one(
                UtcMicros(1_000),
                &crate::observation::ObservationCancellation::default(),
            )
            .await
            .unwrap(),
        SourceAcquisitionRunOutcomeV1::Unavailable {
            attempt: 1,
            retry_at: UtcMicros(11_000),
        }
    );
    assert_eq!(
        owner
            .run_one(
                UtcMicros(10_999),
                &crate::observation::ObservationCancellation::default(),
            )
            .await
            .unwrap(),
        SourceAcquisitionRunOutcomeV1::Idle
    );
    let cancelled = crate::observation::ObservationCancellation::default();
    cancelled.cancel();
    assert_eq!(
        owner.run_one(UtcMicros(11_000), &cancelled).await.unwrap(),
        SourceAcquisitionRunOutcomeV1::Cancelled
    );
    assert_eq!(owner.pending_count().await.unwrap(), 1);
    assert!(matches!(
        owner
            .run_one(
                UtcMicros(11_000),
                &crate::observation::ObservationCancellation::default(),
            )
            .await
            .unwrap(),
        SourceAcquisitionRunOutcomeV1::Unavailable { attempt: 2, .. }
    ));
    assert_eq!(
        owner
            .run_one(
                UtcMicros(31_000),
                &crate::observation::ObservationCancellation::default(),
            )
            .await
            .unwrap(),
        SourceAcquisitionRunOutcomeV1::Exhausted
    );
    assert_eq!(owner.pending_count().await.unwrap(), 0);
}
