use super::*;
use tracedecay_domain::{
    LocatorDigest, PrivacyDomainId, ProjectId, ProviderId, SourceAcquisitionCapabilitiesV1,
    SourceAcquisitionContractV1, SourceBindingOwnerV1, SourceCoverageV1, SourceDeletionSemanticsV1,
    SourceEnvelopeKindV1, SourceInstanceId, SourceNativeObjectIdV1, SourceObjectRevisionV1,
    SourcePartitionIdV1, SourceSnapshotIdV1,
};

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
}

fn acquisition_contract(
    capture_mode: SourceCaptureModeV1,
    refetch_strategy: SourceRefetchStrategyV1,
    deletion_semantics: SourceDeletionSemanticsV1,
) -> SourceAcquisitionContractV1 {
    SourceAcquisitionContractV1::new(
        ProviderId::new("fixture-provider").unwrap(),
        SourceAcquisitionCapabilitiesV1::new(
            BTreeSet::from([capture_mode]),
            BTreeSet::from([refetch_strategy]),
            BTreeSet::from([deletion_semantics]),
        )
        .unwrap(),
    )
    .unwrap()
}

struct EventFixture {
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    event: SourceEventV1,
    refresh: SourceRefreshReceiptV1,
}

struct CaptureFixture {
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    refresh: SourceRefreshReceiptV1,
    envelope: SourceProviderEnvelopeV1,
    next_partition: SourcePartitionFrontierV1,
    observations: Vec<SourceObjectObservationV1>,
}

impl CaptureFixture {
    fn new() -> Self {
        let definition = SourceDefinitionV1::new(
            SourceInstanceId::new("source.capture-fixture").unwrap(),
            1,
            acquisition_contract(
                SourceCaptureModeV1::Poll,
                SourceRefetchStrategyV1::WholeRoot,
                SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
            ),
            SourceCaptureModeV1::Poll,
            SourceRefetchStrategyV1::WholeRoot,
            SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
            1,
        )
        .unwrap();
        let binding = SourceBindingV1::new(
            &definition,
            SourceBindingOwnerV1::Project(ProjectId::new("project.capture-fixture").unwrap()),
            PrivacyDomainId::new("privacy.capture-fixture").unwrap(),
            LocatorDigest::new(digest('a').as_str()).unwrap(),
            1,
        )
        .unwrap();
        let binding_identity = binding.immutable_identity().unwrap();
        let refresh_id = digest('b');
        let refresh = SourceRefreshReceiptV1::new(
            binding_identity.clone(),
            definition.provider.clone(),
            refresh_id.clone(),
            SourceRefreshCauseV1::Poll,
            definition.capture_mode,
            definition.refetch_strategy,
        )
        .unwrap();
        let partition = SourcePartitionIdV1::new(digest('c'));
        let snapshot = SourceSnapshotIdV1::new(digest('d'));
        let envelope = SourceProviderEnvelopeV1::new(
            binding_identity.clone(),
            definition.provider.clone(),
            refresh_id,
            SourceRefreshCauseV1::Poll,
            definition.capture_mode,
            definition.refetch_strategy,
            SourceEnvelopeKindV1::WholeRoot,
            partition.clone(),
            1,
            None,
            None,
            Some(snapshot.clone()),
            SourceCoverageV1::Complete,
            digest('e'),
        )
        .unwrap();
        let next_partition = SourcePartitionFrontierV1::new(
            binding_identity,
            partition,
            None,
            Some(snapshot),
            None,
            SourceCoverageV1::Complete,
            1,
            None,
            envelope.envelope_digest().clone(),
        )
        .unwrap();
        let observations = vec![
            SourceObjectObservationV1::new(
                SourceNativeObjectIdV1::new(digest('f')),
                SourceObjectRevisionV1::new(digest('0')),
                digest('1'),
                SourceContentStateV1::Live,
            )
            .unwrap(),
        ];
        Self {
            definition,
            binding,
            refresh,
            envelope,
            next_partition,
            observations,
        }
    }

    fn authority_context(&self, configuration_digest: ManifestDigest) -> SourceAuthorityContextV1 {
        SourceAuthorityContextV1::new(
            &self.definition,
            &self.binding,
            1,
            configuration_digest,
            1,
            digest('3'),
            &self.refresh,
            &self.envelope,
        )
        .unwrap()
    }

    fn capture_with_authorities(
        &self,
        admission_authority: &SourceAdmissionAuthorityV1,
        sanitization_authority: &SourceSanitizationAuthorityV1,
        observations: Vec<SourceObjectObservationV1>,
    ) -> Result<SourceCaptureAdmissionV1, SourceCaptureAdmissionErrorV1> {
        SourceCaptureAdmissionV1::from_authorities(
            self.definition.clone(),
            self.binding.clone(),
            self.refresh.clone(),
            self.envelope.clone(),
            admission_authority,
            sanitization_authority,
            None,
            None,
            self.next_partition.clone(),
            None,
            observations,
            digest('4'),
            digest('5'),
        )
    }
}

impl EventFixture {
    fn new() -> Self {
        let definition = SourceDefinitionV1::new(
            SourceInstanceId::new("source.event-fixture").unwrap(),
            1,
            acquisition_contract(
                SourceCaptureModeV1::Event,
                SourceRefetchStrategyV1::WholeRoot,
                SourceDeletionSemanticsV1::ExplicitOnly,
            ),
            SourceCaptureModeV1::Event,
            SourceRefetchStrategyV1::WholeRoot,
            SourceDeletionSemanticsV1::ExplicitOnly,
            1,
        )
        .unwrap();
        let binding = SourceBindingV1::new(
            &definition,
            SourceBindingOwnerV1::Project(ProjectId::new("project.event-fixture").unwrap()),
            PrivacyDomainId::new("privacy.event-fixture").unwrap(),
            LocatorDigest::new(digest('a').as_str()).unwrap(),
            1,
        )
        .unwrap();
        let identity = binding.immutable_identity().unwrap();
        let event = SourceEventV1::new(identity.clone(), digest('b')).unwrap();
        let refresh = SourceRefreshReceiptV1::new(
            identity,
            definition.provider.clone(),
            digest('c'),
            SourceRefreshCauseV1::Event,
            definition.capture_mode,
            definition.refetch_strategy,
        )
        .unwrap();
        Self {
            definition,
            binding,
            event,
            refresh,
        }
    }
}

#[test]
fn event_duplicate_reuses_original_refresh_without_scheduling() {
    let fixture = EventFixture::new();
    let enqueued = SourceEventAdmissionV1::admit(
        &fixture.definition,
        &fixture.binding,
        fixture.event.clone(),
        SourceEventAdmissionContextV1::Enqueue(fixture.refresh.clone()),
    )
    .unwrap();
    let duplicate = SourceEventAdmissionV1::admit(
        &fixture.definition,
        &fixture.binding,
        fixture.event,
        SourceEventAdmissionContextV1::Duplicate(enqueued.receipt().clone()),
    )
    .unwrap();

    assert_eq!(
        duplicate.receipt().disposition(),
        SourceEventAdmissionDispositionV1::Duplicate
    );
    assert_eq!(
        duplicate.receipt().original_refresh(),
        enqueued.receipt().original_refresh()
    );
    assert!(!duplicate.schedules_refresh());
}

#[test]
fn restart_reissues_only_the_persisted_event_refresh_authority() {
    let fixture = EventFixture::new();
    let enqueued = SourceEventAdmissionV1::admit(
        &fixture.definition,
        &fixture.binding,
        fixture.event,
        SourceEventAdmissionContextV1::Enqueue(fixture.refresh.clone()),
    )
    .unwrap();

    let resumed = SourceEventAdmissionV1::resume(
        &fixture.definition,
        &fixture.binding,
        enqueued.receipt().clone(),
    )
    .unwrap();

    assert!(!resumed.schedules_refresh());
    assert!(
        resumed.canonical_refetch().matches(&fixture.refresh),
        "restart authority must remain bound to the persisted original refresh"
    );
}

#[test]
fn poll_only_definition_rejects_event_before_refresh_scheduling() {
    let definition = SourceDefinitionV1::new(
        SourceInstanceId::new("source.poll-fixture").unwrap(),
        1,
        acquisition_contract(
            SourceCaptureModeV1::Poll,
            SourceRefetchStrategyV1::WholeRoot,
            SourceDeletionSemanticsV1::ExplicitOnly,
        ),
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::ExplicitOnly,
        1,
    )
    .unwrap();
    let binding = SourceBindingV1::new(
        &definition,
        SourceBindingOwnerV1::Project(ProjectId::new("project.poll-fixture").unwrap()),
        PrivacyDomainId::new("privacy.poll-fixture").unwrap(),
        LocatorDigest::new(digest('d').as_str()).unwrap(),
        1,
    )
    .unwrap();
    let identity = binding.immutable_identity().unwrap();
    let event = SourceEventV1::new(identity.clone(), digest('e')).unwrap();
    let refresh = SourceRefreshReceiptV1::new(
        identity,
        definition.provider.clone(),
        digest('f'),
        SourceRefreshCauseV1::Poll,
        definition.capture_mode,
        definition.refetch_strategy,
    )
    .unwrap();

    assert!(matches!(
        SourceEventAdmissionV1::admit(
            &definition,
            &binding,
            event,
            SourceEventAdmissionContextV1::Enqueue(refresh),
        ),
        Err(SourceCaptureAdmissionErrorV1::EventModeMismatch)
    ));
}

#[test]
fn application_owner_issues_authorities_for_sanitized_capture() {
    let fixture = CaptureFixture::new();
    let owner = SourceCaptureApplicationV1::authorize(
        &fixture.definition,
        &fixture.binding,
        1,
        digest('2'),
        1,
        digest('3'),
        &fixture.refresh,
        &fixture.envelope,
    )
    .unwrap();

    let admission = owner
        .capture_sanitized(
            fixture.definition.clone(),
            fixture.binding.clone(),
            fixture.refresh.clone(),
            fixture.envelope.clone(),
            None,
            None,
            fixture.next_partition.clone(),
            None,
            fixture.observations.clone(),
            digest('4'),
            digest('5'),
        )
        .unwrap();

    assert!(admission.snapshot_completion().is_some());
}

#[test]
fn admission_and_sanitization_authorities_are_separately_bound() {
    let fixture = CaptureFixture::new();
    let context = fixture.authority_context(digest('2'));
    let admission_authority = SourceAdmissionAuthorityV1::issue(context.clone());
    let stale_sanitization_authority = SourceSanitizationAuthorityV1::issue(
        fixture.authority_context(digest('6')),
        &fixture.observations,
    )
    .unwrap();
    assert!(matches!(
        fixture.capture_with_authorities(
            &admission_authority,
            &stale_sanitization_authority,
            fixture.observations.clone(),
        ),
        Err(SourceCaptureAdmissionErrorV1::SanitizationAuthority)
    ));

    let sanitization_authority =
        SourceSanitizationAuthorityV1::issue(context, &fixture.observations).unwrap();
    let changed_observations = vec![
        SourceObjectObservationV1::new(
            SourceNativeObjectIdV1::new(digest('f')),
            SourceObjectRevisionV1::new(digest('0')),
            digest('6'),
            SourceContentStateV1::Live,
        )
        .unwrap(),
    ];
    assert!(matches!(
        fixture.capture_with_authorities(
            &admission_authority,
            &sanitization_authority,
            changed_observations,
        ),
        Err(SourceCaptureAdmissionErrorV1::SanitizationAuthority)
    ));
}
