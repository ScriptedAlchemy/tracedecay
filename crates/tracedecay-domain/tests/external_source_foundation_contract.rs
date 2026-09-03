use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::{
    LocatorDigest, ManifestDigest, PrivacyDomainId, ProjectId, ProviderId,
    SourceAcquisitionCapabilitiesV1, SourceAcquisitionContractV1, SourceAggregateFrontierV1,
    SourceBindingOwnerV1, SourceBindingV1, SourceCaptureModeV1, SourceContentStateV1,
    SourceCoverageV1, SourceCursorV1, SourceDefinitionV1, SourceDeletionSemanticsV1,
    SourceInstanceId, SourceNativeObjectIdV1, SourceObjectObservationV1, SourceObjectRevisionV1,
    SourcePartitionFrontierV1, SourcePartitionIdV1, SourceRefetchStrategyV1, SourceSnapshotIdV1,
    UserProfileId, canonical_sha256,
};

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
}

fn definition() -> SourceDefinitionV1 {
    let capabilities = SourceAcquisitionCapabilitiesV1::new(
        BTreeSet::from([SourceCaptureModeV1::Poll]),
        BTreeSet::from([SourceRefetchStrategyV1::WholeRoot]),
        BTreeSet::from([SourceDeletionSemanticsV1::CompleteSnapshotAbsence]),
    )
    .unwrap();
    let acquisition = SourceAcquisitionContractV1::new(
        ProviderId::new("provider.fixture").unwrap(),
        capabilities,
    )
    .unwrap();
    SourceDefinitionV1::new(
        SourceInstanceId::new("source.fixture").unwrap(),
        1,
        acquisition,
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
        4,
    )
    .unwrap()
}

fn binding(owner: SourceBindingOwnerV1) -> SourceBindingV1 {
    SourceBindingV1::new(
        &definition(),
        owner,
        PrivacyDomainId::new("privacy.fixture").unwrap(),
        LocatorDigest::new(digest('a').as_str()).unwrap(),
        1,
    )
    .unwrap()
}

fn complete_frontier(
    binding: &SourceBindingV1,
    partition: SourcePartitionIdV1,
    sequence: u64,
) -> SourcePartitionFrontierV1 {
    SourcePartitionFrontierV1::new(
        binding.immutable_identity().unwrap(),
        partition,
        Some(SourceCursorV1::new(digest('b'))),
        Some(SourceSnapshotIdV1::new(digest('c'))),
        None,
        SourceCoverageV1::Complete,
        sequence,
        None,
        digest('d'),
    )
    .unwrap()
}

#[test]
fn canonical_wire_rejects_unknown_fields() {
    let binding = binding(SourceBindingOwnerV1::Project(
        ProjectId::new("owner.fixture").unwrap(),
    ));
    let frontier = complete_frontier(&binding, SourcePartitionIdV1::new(digest('e')), 1);
    let bytes = serde_json::to_vec(&frontier).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&frontier).unwrap());

    let mut value = serde_json::to_value(frontier).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("revision".to_owned(), serde_json::json!(1));
    assert!(serde_json::from_value::<SourcePartitionFrontierV1>(value).is_err());
}

#[test]
fn identical_native_key_does_not_collapse_project_and_profile_owners() {
    let project = binding(SourceBindingOwnerV1::Project(
        ProjectId::new("owner.fixture").unwrap(),
    ));
    let profile = binding(SourceBindingOwnerV1::Profile(
        UserProfileId::new("owner.fixture").unwrap(),
    ));

    assert_eq!(project.native_root, profile.native_root);
    assert_eq!(project.privacy_domain, profile.privacy_domain);
    assert_ne!(project.binding_id, profile.binding_id);
    assert_ne!(project.binding_digest, profile.binding_digest);
}

#[test]
fn aggregate_digest_is_partition_order_independent_and_binds_coverage() {
    let binding = binding(SourceBindingOwnerV1::Project(
        ProjectId::new("project.fixture").unwrap(),
    ));
    let first_id = SourcePartitionIdV1::new(digest('1'));
    let second_id = SourcePartitionIdV1::new(digest('2'));
    let first = complete_frontier(&binding, first_id.clone(), 1);
    let second = complete_frontier(&binding, second_id.clone(), 2);

    let forward = SourceAggregateFrontierV1::new(
        binding.immutable_identity().unwrap(),
        BTreeMap::from([
            (first_id.clone(), first.clone()),
            (second_id.clone(), second.clone()),
        ]),
    )
    .unwrap();
    let reverse = SourceAggregateFrontierV1::new(
        binding.immutable_identity().unwrap(),
        [(second_id, second), (first_id.clone(), first)]
            .into_iter()
            .collect(),
    )
    .unwrap();
    assert_eq!(forward.digest(), reverse.digest());

    let identity = binding.immutable_identity().unwrap();
    let mut encoded_partitions = forward
        .partitions()
        .iter()
        .map(|(partition, frontier)| (partition.clone(), serde_json::to_value(frontier).unwrap()))
        .collect::<BTreeMap<_, _>>();
    let baseline = canonical_sha256(&(
        "tracedecay.external-source.aggregate-frontier.v1",
        &identity,
        &encoded_partitions,
    ))
    .unwrap();
    assert_eq!(&baseline, forward.digest());
    encoded_partitions
        .get_mut(&first_id)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("coverage".to_owned(), serde_json::json!("partial"));
    let coverage_only = canonical_sha256(&(
        "tracedecay.external-source.aggregate-frontier.v1",
        &identity,
        &encoded_partitions,
    ))
    .unwrap();
    assert_ne!(baseline, coverage_only);

    let partial = SourcePartitionFrontierV1::new(
        binding.immutable_identity().unwrap(),
        first_id.clone(),
        Some(SourceCursorV1::new(digest('b'))),
        Some(SourceSnapshotIdV1::new(digest('c'))),
        Some(SourceCursorV1::new(digest('f'))),
        SourceCoverageV1::Partial,
        1,
        Some(SourceSnapshotIdV1::new(digest('c'))),
        digest('d'),
    )
    .unwrap();
    let changed = SourceAggregateFrontierV1::new(
        binding.immutable_identity().unwrap(),
        BTreeMap::from([
            (first_id, partial),
            (
                SourcePartitionIdV1::new(digest('2')),
                complete_frontier(&binding, SourcePartitionIdV1::new(digest('2')), 2),
            ),
        ]),
    )
    .unwrap();

    assert_eq!(forward.coverage(), SourceCoverageV1::Complete);
    assert_eq!(changed.coverage(), SourceCoverageV1::Partial);
    assert_ne!(forward.digest(), changed.digest());
    assert!(
        serde_json::to_value(changed)
            .unwrap()
            .get("coverage")
            .is_none()
    );
}

#[test]
fn object_revision_and_partition_cursor_remain_separate_frontier_axes() {
    let revision = SourceObjectRevisionV1::new(digest('3'));
    let observation = SourceObjectObservationV1::new(
        SourceNativeObjectIdV1::new(digest('4')),
        revision,
        digest('5'),
        SourceContentStateV1::Live,
    )
    .unwrap();
    let binding = binding(SourceBindingOwnerV1::Project(
        ProjectId::new("project.fixture").unwrap(),
    ));
    let frontier = complete_frontier(&binding, SourcePartitionIdV1::new(digest('6')), 1);

    let observation_wire = serde_json::to_value(observation).unwrap();
    let frontier_wire = serde_json::to_value(frontier).unwrap();
    assert!(observation_wire.get("revision").is_some());
    assert!(observation_wire.get("cursor").is_none());
    assert!(frontier_wire.get("cursor").is_some());
    assert!(frontier_wire.get("revision").is_none());

    assert!(
        SourcePartitionFrontierV1::new(
            binding.immutable_identity().unwrap(),
            SourcePartitionIdV1::new(digest('6')),
            Some(SourceCursorV1::new(digest('b'))),
            None,
            None,
            SourceCoverageV1::Unknown,
            0,
            None,
            digest('d'),
        )
        .is_err()
    );
}
