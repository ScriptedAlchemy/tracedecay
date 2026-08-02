use std::collections::BTreeSet;

use tracedecay_domain::{
    AccessPolicyDigest, CapabilityId, ComponentVersion, LocatorDigest, ManifestDigest,
    PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProviderId,
    ResolutionAuthorizationV1, RetrievalAnchorId, SanitizationReceiptId, SanitizationReceiptRefV1,
    ScopeResolutionId, SourceAcquisitionCapabilitiesV1, SourceAcquisitionContractV1,
    SourceAggregateFrontierV1, SourceBindingOwnerV1, SourceBindingV1, SourceCaptureModeV1,
    SourceContentStateV1, SourceCoverageV1, SourceCursorV1, SourceDefinitionV1,
    SourceDeletionSemanticsV1, SourceInstanceId, SourceNativeObjectIdV1, SourceObjectObservationV1,
    SourceObjectRevisionV1, SourcePartitionFrontierV1, SourcePartitionIdV1,
    SourceRefetchStrategyV1, SourceSnapshotCompletionV1, SourceSnapshotIdV1,
};
use tracedecay_store::{
    SourceAuthorityPublicationV1, SourceCommitApplyOutcomeV1, SourceCommitV1,
    SourceObjectMutationV1, SourceObjectTransitionV1, SourceObservationEvidenceV1,
    SourceStoreErrorV1, SourceStoreStateV1, apply_source_authority_publication,
    apply_source_commit,
};

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
}

fn definition() -> SourceDefinitionV1 {
    definition_with_max(4)
}

fn definition_with_max(max_partitions: u16) -> SourceDefinitionV1 {
    let capabilities = SourceAcquisitionCapabilitiesV1::new(
        BTreeSet::from([SourceCaptureModeV1::Poll]),
        BTreeSet::from([SourceRefetchStrategyV1::WholeRoot]),
        BTreeSet::from([SourceDeletionSemanticsV1::CompleteSnapshotAbsence]),
    )
    .unwrap();
    SourceDefinitionV1::new(
        SourceInstanceId::new("source.github-review").unwrap(),
        1,
        SourceAcquisitionContractV1::new(ProviderId::new("github").unwrap(), capabilities).unwrap(),
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
        max_partitions,
    )
    .unwrap()
}

fn revised_definition(revision: u64, max_partitions: u16) -> SourceDefinitionV1 {
    let capabilities = SourceAcquisitionCapabilitiesV1::new(
        BTreeSet::from([SourceCaptureModeV1::Poll]),
        BTreeSet::from([SourceRefetchStrategyV1::WholeRoot]),
        BTreeSet::from([SourceDeletionSemanticsV1::CompleteSnapshotAbsence]),
    )
    .unwrap();
    SourceDefinitionV1::new(
        SourceInstanceId::new("source.github-review").unwrap(),
        revision,
        SourceAcquisitionContractV1::new(ProviderId::new("github").unwrap(), capabilities).unwrap(),
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
        max_partitions,
    )
    .unwrap()
}

fn binding(definition: &SourceDefinitionV1) -> SourceBindingV1 {
    SourceBindingV1::new(
        definition,
        SourceBindingOwnerV1::Project(ProjectId::new("project.source-commit").unwrap()),
        PrivacyDomainId::new("privacy.source-commit").unwrap(),
        LocatorDigest::new(digest('a').as_str()).unwrap(),
        1,
    )
    .unwrap()
}

fn partition() -> SourcePartitionIdV1 {
    SourcePartitionIdV1::new(digest('b'))
}

fn partition_with_seed(seed: char) -> SourcePartitionIdV1 {
    SourcePartitionIdV1::new(digest(seed))
}

fn object() -> SourceObjectObservationV1 {
    object_with('c', 'd', 'e', SourceContentStateV1::Live)
}

fn object_with(
    native_seed: char,
    revision_seed: char,
    content_seed: char,
    state: SourceContentStateV1,
) -> SourceObjectObservationV1 {
    SourceObjectObservationV1::new(
        SourceNativeObjectIdV1::new(digest(native_seed)),
        SourceObjectRevisionV1::new(digest(revision_seed)),
        digest(content_seed),
        state,
    )
    .unwrap()
}

fn evidence(
    binding: &SourceBindingV1,
    partition: &SourcePartitionIdV1,
    observation: &SourceObjectObservationV1,
    seed: char,
) -> SourceObservationEvidenceV1 {
    SourceObservationEvidenceV1::new(
        binding.immutable_identity().unwrap(),
        partition.clone(),
        observation,
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.external-source.{seed}")).unwrap(),
            ComponentVersion::new("sanitizer.external-source.v1").unwrap(),
        )
        .unwrap(),
        RetrievalAnchorId::new(format!("retrieval.external-source.{seed}")).unwrap(),
        ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new(format!("scope.external-source.{seed}"))
                .unwrap(),
            privacy_domain_id: binding.immutable_identity().unwrap().privacy_domain,
            access_policy_digest: AccessPolicyDigest::new(digest(seed).as_str()).unwrap(),
            capability_id: CapabilityId::new(format!("capability.external-source.{seed}")).unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(digest(seed).as_str())
                .unwrap(),
        },
        digest(seed),
    )
    .unwrap()
}

fn mutation(
    binding: &SourceBindingV1,
    partition: &SourcePartitionIdV1,
    observation: SourceObjectObservationV1,
    predecessor: Option<SourceObjectRevisionV1>,
    transition: SourceObjectTransitionV1,
    seed: char,
) -> SourceObjectMutationV1 {
    let evidence = evidence(binding, partition, &observation, seed);
    SourceObjectMutationV1::new(observation, predecessor, transition, evidence).unwrap()
}

fn commit(
    definition: &SourceDefinitionV1,
    binding: &SourceBindingV1,
    expected: Option<SourceAggregateFrontierV1>,
    coverage: SourceCoverageV1,
    mutations: Vec<SourceObjectMutationV1>,
    present_objects: Option<BTreeSet<SourceNativeObjectIdV1>>,
    idempotency_seed: char,
) -> SourceCommitV1 {
    commit_for_partition(
        definition,
        binding,
        partition(),
        expected,
        coverage,
        mutations,
        present_objects,
        idempotency_seed,
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn commit_for_partition(
    definition: &SourceDefinitionV1,
    binding: &SourceBindingV1,
    partition: SourcePartitionIdV1,
    expected: Option<SourceAggregateFrontierV1>,
    coverage: SourceCoverageV1,
    mutations: Vec<SourceObjectMutationV1>,
    present_objects: Option<BTreeSet<SourceNativeObjectIdV1>>,
    idempotency_seed: char,
) -> Result<SourceCommitV1, SourceStoreErrorV1> {
    let previous_partition = expected
        .as_ref()
        .and_then(|frontier| frontier.partition(&partition));
    let snapshot = (coverage == SourceCoverageV1::Complete)
        .then(|| SourceSnapshotIdV1::new(digest(idempotency_seed)));
    let continuation =
        (coverage == SourceCoverageV1::Partial).then(|| SourceCursorV1::new(digest('f')));
    let next_partition = SourcePartitionFrontierV1::new(
        binding.immutable_identity().unwrap(),
        partition.clone(),
        continuation.clone(),
        snapshot.clone(),
        continuation,
        coverage,
        previous_partition.map_or(0, SourcePartitionFrontierV1::sequence) + 1,
        previous_partition.and_then(SourcePartitionFrontierV1::last_complete_snapshot),
        digest('0'),
    )
    .unwrap();
    let next_frontier = SourceAggregateFrontierV1::with_updated_partition(
        binding.immutable_identity().unwrap(),
        expected.as_ref(),
        next_partition,
    )
    .unwrap();
    let snapshot_completion = snapshot.map(|snapshot| {
        SourceSnapshotCompletionV1::new(
            partition.clone(),
            snapshot,
            present_objects.expect("complete snapshots declare their staged object set"),
        )
        .unwrap()
    });
    SourceCommitV1::new(
        definition.clone(),
        binding.clone(),
        partition,
        ComponentVersion::new("github-review-source-projector-v1").unwrap(),
        digest(idempotency_seed),
        digest('1'),
        expected,
        next_frontier,
        mutations,
        snapshot_completion,
    )
}

fn committed(outcome: SourceCommitApplyOutcomeV1) -> SourceStoreStateV1 {
    match outcome {
        SourceCommitApplyOutcomeV1::Committed(state) => *state,
        other => panic!("expected a committed source state, got {other:?}"),
    }
}

#[test]
fn replay_partial_coverage_and_complete_snapshot_preserve_tombstone_rules() {
    let definition = definition();
    let binding = binding(&definition);
    let live = object();
    let initial = mutation(
        &binding,
        &partition(),
        live.clone(),
        None,
        SourceObjectTransitionV1::Initial,
        '2',
    );
    let first = commit(
        &definition,
        &binding,
        None,
        SourceCoverageV1::Complete,
        vec![initial],
        Some(BTreeSet::from([live.native_object().clone()])),
        '2',
    );
    let state = committed(apply_source_commit(None, first.clone()).unwrap());

    let restarted: SourceStoreStateV1 =
        serde_json::from_str(&serde_json::to_string(&state).unwrap())
            .expect("source state survives a durable restart encoding");
    assert!(matches!(
        apply_source_commit(Some(&restarted), first.clone()).unwrap(),
        SourceCommitApplyOutcomeV1::ExactDuplicate(_)
    ));

    let unchanged_complete = commit(
        &definition,
        &binding,
        Some(restarted.source_frontier().clone()),
        SourceCoverageV1::Complete,
        Vec::new(),
        Some(BTreeSet::from([live.native_object().clone()])),
        '3',
    );
    let state = committed(apply_source_commit(Some(&restarted), unchanged_complete).unwrap());
    assert_eq!(
        state.projected_objects()[live.native_object()].content_state(),
        SourceContentStateV1::Live
    );

    let partial = commit(
        &definition,
        &binding,
        Some(state.source_frontier().clone()),
        SourceCoverageV1::Partial,
        Vec::new(),
        None,
        '4',
    );
    let state = committed(apply_source_commit(Some(&state), partial).unwrap());
    assert_eq!(
        state
            .projected_objects()
            .get(live.native_object())
            .expect("partial coverage retains the prior object")
            .content_state(),
        SourceContentStateV1::Live
    );

    let complete = commit(
        &definition,
        &binding,
        Some(state.source_frontier().clone()),
        SourceCoverageV1::Complete,
        Vec::new(),
        Some(BTreeSet::new()),
        '5',
    );
    let state = committed(apply_source_commit(Some(&state), complete).unwrap());
    assert_eq!(
        state
            .projected_objects()
            .get(live.native_object())
            .expect("complete snapshot keeps a tombstone record")
            .content_state(),
        SourceContentStateV1::AuthoritativeDeleted
    );
    assert_eq!(
        state.revision_history(live.native_object()).unwrap().len(),
        2
    );
    assert_eq!(
        state.lineage()[0].transition(),
        SourceObjectTransitionV1::Tombstone
    );
    assert!(matches!(
        apply_source_commit(Some(&state), first).unwrap(),
        SourceCommitApplyOutcomeV1::ExactDuplicate(_)
    ));
}

#[test]
fn complete_snapshot_tombstones_only_its_partition() {
    let definition = definition();
    let binding = binding(&definition);
    let first_partition = partition_with_seed('b');
    let second_partition = partition_with_seed('9');
    let first_object = object_with('c', 'd', 'e', SourceContentStateV1::Live);
    let second_object = object_with('6', '7', '8', SourceContentStateV1::Live);

    let first = mutation(
        &binding,
        &first_partition,
        first_object.clone(),
        None,
        SourceObjectTransitionV1::Initial,
        '2',
    );
    let state = committed(
        apply_source_commit(
            None,
            commit_for_partition(
                &definition,
                &binding,
                first_partition.clone(),
                None,
                SourceCoverageV1::Complete,
                vec![first],
                Some(BTreeSet::from([first_object.native_object().clone()])),
                '2',
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let moved = object_with('c', '5', '6', SourceContentStateV1::Live);
    let moved = mutation(
        &binding,
        &second_partition,
        moved,
        Some(first_object.revision().clone()),
        SourceObjectTransitionV1::Successor,
        '3',
    );
    assert!(matches!(
        apply_source_commit(
            Some(&state),
            commit_for_partition(
                &definition,
                &binding,
                second_partition.clone(),
                Some(state.source_frontier().clone()),
                SourceCoverageV1::Partial,
                vec![moved],
                None,
                '3',
            )
            .unwrap(),
        ),
        Err(SourceStoreErrorV1::ObjectPartitionConflict)
    ));
    let second = mutation(
        &binding,
        &second_partition,
        second_object.clone(),
        None,
        SourceObjectTransitionV1::Initial,
        '3',
    );
    let state = committed(
        apply_source_commit(
            Some(&state),
            commit_for_partition(
                &definition,
                &binding,
                second_partition.clone(),
                Some(state.source_frontier().clone()),
                SourceCoverageV1::Complete,
                vec![second],
                Some(BTreeSet::from([second_object.native_object().clone()])),
                '3',
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let state = committed(
        apply_source_commit(
            Some(&state),
            commit_for_partition(
                &definition,
                &binding,
                second_partition,
                Some(state.source_frontier().clone()),
                SourceCoverageV1::Complete,
                Vec::new(),
                Some(BTreeSet::new()),
                '4',
            )
            .unwrap(),
        )
        .unwrap(),
    );

    assert_eq!(
        state.projected_objects()[first_object.native_object()].content_state(),
        SourceContentStateV1::Live
    );
    assert_eq!(
        state.projected_objects()[second_object.native_object()].content_state(),
        SourceContentStateV1::AuthoritativeDeleted
    );
    assert_eq!(
        state.object_partition(first_object.native_object()),
        Some(&first_partition)
    );
}

#[test]
fn definition_partition_limit_is_enforced() {
    let definition = definition_with_max(1);
    let binding = binding(&definition);
    let first_partition = partition_with_seed('b');
    let first_object = object();
    let first = mutation(
        &binding,
        &first_partition,
        first_object.clone(),
        None,
        SourceObjectTransitionV1::Initial,
        '2',
    );
    let state = committed(
        apply_source_commit(
            None,
            commit_for_partition(
                &definition,
                &binding,
                first_partition,
                None,
                SourceCoverageV1::Complete,
                vec![first],
                Some(BTreeSet::from([first_object.native_object().clone()])),
                '2',
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let second_partition = partition_with_seed('9');
    let second_object = object_with('6', '7', '8', SourceContentStateV1::Live);
    let second = mutation(
        &binding,
        &second_partition,
        second_object.clone(),
        None,
        SourceObjectTransitionV1::Initial,
        '3',
    );

    assert!(matches!(
        commit_for_partition(
            &definition,
            &binding,
            second_partition,
            Some(state.source_frontier().clone()),
            SourceCoverageV1::Complete,
            vec![second],
            Some(BTreeSet::from([second_object.native_object().clone()])),
            '3',
        ),
        Err(SourceStoreErrorV1::TooManyPartitions)
    ));
}

#[test]
fn revision_history_and_explicit_lineage_are_immutable() {
    let definition = definition();
    let binding = binding(&definition);
    let partition = partition();
    let initial = object();
    let first = mutation(
        &binding,
        &partition,
        initial.clone(),
        None,
        SourceObjectTransitionV1::Initial,
        '2',
    );
    let state = committed(
        apply_source_commit(
            None,
            commit(
                &definition,
                &binding,
                None,
                SourceCoverageV1::Partial,
                vec![first],
                None,
                '2',
            ),
        )
        .unwrap(),
    );
    let correction = object_with('c', '6', '7', SourceContentStateV1::Live);
    let corrected = mutation(
        &binding,
        &partition,
        correction.clone(),
        Some(initial.revision().clone()),
        SourceObjectTransitionV1::Correction,
        '3',
    );
    let state = committed(
        apply_source_commit(
            Some(&state),
            commit(
                &definition,
                &binding,
                Some(state.source_frontier().clone()),
                SourceCoverageV1::Partial,
                vec![corrected],
                None,
                '3',
            ),
        )
        .unwrap(),
    );
    let deleted = object_with('c', '8', '9', SourceContentStateV1::AuthoritativeDeleted);
    let tombstone = mutation(
        &binding,
        &partition,
        deleted.clone(),
        Some(correction.revision().clone()),
        SourceObjectTransitionV1::Tombstone,
        '4',
    );
    let state = committed(
        apply_source_commit(
            Some(&state),
            commit(
                &definition,
                &binding,
                Some(state.source_frontier().clone()),
                SourceCoverageV1::Partial,
                vec![tombstone],
                None,
                '4',
            ),
        )
        .unwrap(),
    );
    let reappeared = object_with('c', 'a', 'b', SourceContentStateV1::Live);
    let reappearance = mutation(
        &binding,
        &partition,
        reappeared.clone(),
        Some(deleted.revision().clone()),
        SourceObjectTransitionV1::Reappearance,
        '5',
    );
    let state = committed(
        apply_source_commit(
            Some(&state),
            commit(
                &definition,
                &binding,
                Some(state.source_frontier().clone()),
                SourceCoverageV1::Partial,
                vec![reappearance],
                None,
                '5',
            ),
        )
        .unwrap(),
    );

    let history = state.revision_history(initial.native_object()).unwrap();
    assert_eq!(history.len(), 4);
    assert_eq!(history[0].observation(), &initial);
    assert_eq!(history[1].observation(), &correction);
    assert_eq!(history[2].observation(), &deleted);
    assert_eq!(history[3].observation(), &reappeared);
    assert_eq!(
        state
            .lineage()
            .iter()
            .map(|edge| edge.transition())
            .collect::<Vec<_>>(),
        vec![
            SourceObjectTransitionV1::Correction,
            SourceObjectTransitionV1::Tombstone,
            SourceObjectTransitionV1::Reappearance,
        ]
    );
}

#[test]
fn evidence_must_match_binding_privacy_and_object_revision() {
    let definition = definition();
    let binding = binding(&definition);
    let observation = object();
    let valid_evidence = evidence(&binding, &partition(), &observation, '2');
    let mut authorization = valid_evidence.authorization().clone();
    authorization.privacy_domain_id = PrivacyDomainId::new("privacy.wrong").unwrap();

    assert!(matches!(
        SourceObservationEvidenceV1::new(
            binding.immutable_identity().unwrap(),
            partition(),
            &observation,
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.external-source.bad").unwrap(),
                ComponentVersion::new("sanitizer.external-source.v1").unwrap(),
            )
            .unwrap(),
            RetrievalAnchorId::new("retrieval.external-source.bad").unwrap(),
            authorization,
            digest('f'),
        ),
        Err(SourceStoreErrorV1::EvidenceConflict)
    ));

    let mut wire = serde_json::to_value(valid_evidence).unwrap();
    wire.as_object_mut()
        .unwrap()
        .remove("source_authorization_digest");
    assert!(serde_json::from_value::<SourceObservationEvidenceV1>(wire).is_err());
}

#[test]
fn stale_binding_revision_cannot_replay_or_advance_source_state() {
    let definition = definition();
    let binding = binding(&definition);
    let live = object();
    let first = commit(
        &definition,
        &binding,
        None,
        SourceCoverageV1::Complete,
        vec![mutation(
            &binding,
            &partition(),
            live.clone(),
            None,
            SourceObjectTransitionV1::Initial,
            '2',
        )],
        Some(BTreeSet::from([live.native_object().clone()])),
        '2',
    );
    let state = committed(apply_source_commit(None, first).unwrap());
    let stale_binding = SourceBindingV1::new(
        &definition,
        binding.owner.clone(),
        binding.privacy_domain.clone(),
        binding.native_root.clone(),
        binding.binding_revision + 1,
    )
    .unwrap();

    let replay = commit(
        &definition,
        &stale_binding,
        None,
        SourceCoverageV1::Complete,
        Vec::new(),
        Some(BTreeSet::new()),
        '2',
    );
    assert!(matches!(
        apply_source_commit(Some(&state), replay),
        Err(SourceStoreErrorV1::BindingConflict)
    ));

    let advance = commit(
        &definition,
        &stale_binding,
        Some(state.source_frontier().clone()),
        SourceCoverageV1::Partial,
        Vec::new(),
        None,
        '3',
    );
    assert!(matches!(
        apply_source_commit(Some(&state), advance),
        Err(SourceStoreErrorV1::BindingConflict)
    ));
}

#[test]
fn sequential_definition_and_binding_revisions_preserve_immutable_history() {
    let definition_v1 = definition();
    let binding_v1 = binding(&definition_v1);
    let first = commit(
        &definition_v1,
        &binding_v1,
        None,
        SourceCoverageV1::Partial,
        vec![mutation(
            &binding_v1,
            &partition(),
            object(),
            None,
            SourceObjectTransitionV1::Initial,
            '2',
        )],
        None,
        '2',
    );
    let state = committed(apply_source_commit(None, first).unwrap());
    let definition_v2 = revised_definition(2, 8);
    let binding_v2 = SourceBindingV1::new(
        &definition_v2,
        binding_v1.owner.clone(),
        binding_v1.privacy_domain.clone(),
        binding_v1.native_root.clone(),
        2,
    )
    .unwrap();
    let publication = SourceAuthorityPublicationV1::new(
        &definition_v2,
        &binding_v2,
        definition_v1.definition_digest.clone(),
        binding_v1.binding_digest.clone(),
        digest('3'),
        digest('4'),
    )
    .unwrap();

    let revised = apply_source_authority_publication(&state, publication).unwrap();

    assert_eq!(
        revised
            .definition_history()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        revised
            .binding_history()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(revised.definition(), &definition_v2);
    assert_eq!(revised.binding(), &binding_v2);
}

/// Build a state whose validation touches every memoized record kind: a
/// multi-revision object with explicit lineage, two commit receipts, and a
/// projection carrying mutations, effects, and lineage edges.
fn layered_state() -> SourceStoreStateV1 {
    let definition = definition();
    let binding = binding(&definition);
    let partition = partition();
    let initial = object();
    let state = committed(
        apply_source_commit(
            None,
            commit(
                &definition,
                &binding,
                None,
                SourceCoverageV1::Partial,
                vec![mutation(
                    &binding,
                    &partition,
                    initial.clone(),
                    None,
                    SourceObjectTransitionV1::Initial,
                    '2',
                )],
                None,
                '2',
            ),
        )
        .unwrap(),
    );
    let correction = object_with('c', '6', '7', SourceContentStateV1::Live);
    committed(
        apply_source_commit(
            Some(&state),
            commit(
                &definition,
                &binding,
                Some(state.source_frontier().clone()),
                SourceCoverageV1::Partial,
                vec![mutation(
                    &binding,
                    &partition,
                    correction,
                    Some(initial.revision().clone()),
                    SourceObjectTransitionV1::Correction,
                    '3',
                )],
                None,
                '3',
            ),
        )
        .unwrap(),
    )
}

/// Replace the first string leaf stored under `key`, anywhere in a document.
fn overwrite_first(value: &mut serde_json::Value, key: &str, replacement: &str) -> bool {
    match value {
        serde_json::Value::Object(entries) => {
            if let Some(slot) = entries.get_mut(key)
                && slot.is_string()
            {
                *slot = serde_json::Value::String(replacement.to_owned());
                return true;
            }
            entries
                .values_mut()
                .any(|nested| overwrite_first(nested, key, replacement))
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .any(|nested| overwrite_first(nested, key, replacement)),
        _ => false,
    }
}

/// Verification memoization is provenance, never content and never a verdict.
///
/// The durable encoding must not gain a field, a decoded state must always
/// start unverified and re-derive its own verdict, and every tampered digest
/// must still be rejected even though an identical untampered record was
/// verified earlier in this process.
#[test]
fn validation_memoization_preserves_encoding_and_verdicts() {
    let state = layered_state();
    let encoded = serde_json::to_string(&state).unwrap();

    // The memo is never serialized, so durable bytes — and every digest taken
    // over these records — are unchanged.
    assert!(!encoded.contains("verified"));
    assert!(state.validate().is_ok());
    assert_eq!(serde_json::to_string(&state).unwrap(), encoded);

    // A decoded copy carries no memo: it is fully validated on first contact
    // and agrees with the value that was verified at construction.
    let decoded: SourceStoreStateV1 = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, state);
    assert!(decoded.validate().is_ok());
    assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);

    // Repeat validation is stable for a memoized and for a decoded value.
    for _ in 0..3 {
        assert!(state.validate().is_ok());
        assert!(decoded.validate().is_ok());
    }

    // Every digest a memo could have skipped is still checked on decode.
    let foreign = format!("sha256:{}", "9".repeat(64));
    for key in [
        "evidence_digest",
        "mutation_digest",
        "lineage_digest",
        "receipt_digest",
        "sanitized_digest",
        "source_authorization_digest",
    ] {
        let mut document: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert!(
            overwrite_first(&mut document, key, &foreign),
            "fixture must contain {key}"
        );
        let tampered: SourceStoreStateV1 = serde_json::from_value(document).unwrap();
        assert!(
            tampered.validate().is_err(),
            "tampered {key} must not be admitted"
        );
    }
}

/// A memoized state must not smuggle its verdict into a mutated successor.
///
/// `apply_source_authority_publication` clones an already-verified state and
/// then replaces its authority fields, so the successor has to be re-verified
/// from scratch rather than inheriting the predecessor's memo.
#[test]
fn authority_publication_revalidates_the_mutated_successor() {
    let definition_v1 = definition();
    let binding_v1 = binding(&definition_v1);
    let state = layered_state();
    assert!(state.validate().is_ok());

    // A publication that skips a revision must still be refused even though
    // the state it clones was verified moments ago.
    let skipped = revised_definition(3, 4);
    let skipped_binding = SourceBindingV1::new(
        &skipped,
        binding_v1.owner.clone(),
        binding_v1.privacy_domain.clone(),
        binding_v1.native_root.clone(),
        3,
    )
    .unwrap();
    assert!(matches!(
        apply_source_authority_publication(
            &state,
            SourceAuthorityPublicationV1::new(
                &skipped,
                &skipped_binding,
                definition_v1.definition_digest.clone(),
                binding_v1.binding_digest.clone(),
                digest('7'),
                digest('8'),
            )
            .unwrap(),
        ),
        Err(SourceStoreErrorV1::AuthorityRevisionConflict)
    ));

    let definition_v2 = revised_definition(2, 4);
    let binding_v2 = SourceBindingV1::new(
        &definition_v2,
        binding_v1.owner.clone(),
        binding_v1.privacy_domain.clone(),
        binding_v1.native_root.clone(),
        2,
    )
    .unwrap();
    let revised = apply_source_authority_publication(
        &state,
        SourceAuthorityPublicationV1::new(
            &definition_v2,
            &binding_v2,
            definition_v1.definition_digest.clone(),
            binding_v1.binding_digest.clone(),
            digest('7'),
            digest('8'),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(revised.definition(), &definition_v2);
    assert_eq!(revised.binding(), &binding_v2);
    assert!(revised.validate().is_ok());

    // The successor is a genuinely different record, not the memoized parent.
    let revised_encoded = serde_json::to_string(&revised).unwrap();
    assert_ne!(revised_encoded, serde_json::to_string(&state).unwrap());
    let round_tripped: SourceStoreStateV1 = serde_json::from_str(&revised_encoded).unwrap();
    assert_eq!(round_tripped, revised);
    assert!(round_tripped.validate().is_ok());
}
