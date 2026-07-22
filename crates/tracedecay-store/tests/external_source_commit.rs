use std::collections::BTreeSet;

use tracedecay_domain::{
    ComponentVersion, LocatorDigest, ManifestDigest, PrivacyDomainId, ProjectId, ProviderId,
    SourceAggregateFrontierV1, SourceBindingOwnerV1, SourceBindingV1, SourceCaptureModeV1,
    SourceContentStateV1, SourceCoverageV1, SourceCursorV1, SourceDefinitionV1,
    SourceDeletionSemanticsV1, SourceInstanceId, SourceNativeObjectIdV1, SourceObjectObservationV1,
    SourceObjectRevisionV1, SourcePartitionFrontierV1, SourcePartitionIdV1,
    SourceRefetchStrategyV1, SourceSnapshotCompletionV1, SourceSnapshotIdV1,
};
use tracedecay_store::{
    SourceCommitApplyOutcomeV1, SourceCommitV1, SourceStoreStateV1, apply_source_commit,
};

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
}

fn definition() -> SourceDefinitionV1 {
    SourceDefinitionV1::new(
        SourceInstanceId::new("source.github-review").unwrap(),
        ProviderId::new("github").unwrap(),
        1,
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
        4,
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

fn object() -> SourceObjectObservationV1 {
    SourceObjectObservationV1::new(
        SourceNativeObjectIdV1::new(digest('c')),
        SourceObjectRevisionV1::new(digest('d')),
        digest('e'),
        SourceContentStateV1::Live,
    )
    .unwrap()
}

fn commit(
    definition: &SourceDefinitionV1,
    binding: &SourceBindingV1,
    expected: Option<SourceAggregateFrontierV1>,
    coverage: SourceCoverageV1,
    observations: Vec<SourceObjectObservationV1>,
    present_objects: Option<BTreeSet<SourceNativeObjectIdV1>>,
    idempotency_seed: char,
) -> SourceCommitV1 {
    let previous_partition = expected
        .as_ref()
        .and_then(|frontier| frontier.partition(&partition()));
    let snapshot = (coverage == SourceCoverageV1::Complete)
        .then(|| SourceSnapshotIdV1::new(digest(idempotency_seed)));
    let continuation =
        (coverage == SourceCoverageV1::Partial).then(|| SourceCursorV1::new(digest('f')));
    let next_partition = SourcePartitionFrontierV1::new(
        binding.immutable_identity().unwrap(),
        partition(),
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
            partition(),
            snapshot,
            present_objects.expect("complete snapshots declare their staged object set"),
        )
        .unwrap()
    });
    SourceCommitV1::new(
        definition.clone(),
        binding.clone(),
        partition(),
        ComponentVersion::new("github-review-source-projector-v1").unwrap(),
        digest(idempotency_seed),
        digest('1'),
        expected,
        next_frontier,
        observations,
        snapshot_completion,
    )
    .unwrap()
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
    let first = commit(
        &definition,
        &binding,
        None,
        SourceCoverageV1::Complete,
        vec![live.clone()],
        Some(BTreeSet::from([live.native_object().clone()])),
        '2',
    );
    let state = committed(apply_source_commit(None, first.clone()).unwrap());

    let restarted: SourceStoreStateV1 =
        serde_json::from_str(&serde_json::to_string(&state).unwrap())
            .expect("source state survives a durable restart encoding");
    assert!(matches!(
        apply_source_commit(Some(&restarted), first).unwrap(),
        SourceCommitApplyOutcomeV1::ExactDuplicate(_)
    ));

    let partial = commit(
        &definition,
        &binding,
        Some(restarted.source_frontier().clone()),
        SourceCoverageV1::Partial,
        Vec::new(),
        None,
        '3',
    );
    let state = committed(apply_source_commit(Some(&restarted), partial).unwrap());
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
        '4',
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
}
