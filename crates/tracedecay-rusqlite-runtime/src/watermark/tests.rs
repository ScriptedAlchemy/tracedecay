use tracedecay_store::{
    BrainId, CommitSequenceV1, ProjectId, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId,
};

use super::*;

use tracedecay_domain::test_fixtures::id;

fn binding(project: &str) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            id::<BrainId>("brain.primary"),
            id::<UserProfileId>("profile.primary"),
            id::<ProjectId>(project),
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(7).unwrap(),
    )
}

fn watermark(binding: &StoreRuntimeBindingV1, sequence: u64) -> tracedecay_store::ShardWatermarkV1 {
    tracedecay_store::ShardWatermarkV1 {
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        commit_sequence: CommitSequenceV1(sequence),
    }
}

#[test]
fn wrong_epoch_and_non_monotonic_publications_are_rejected() {
    let binding = binding("project.fenced");
    let publisher = CommittedWatermarkPublisher::new(binding.clone());
    publisher
        .publish_committed_watermark(watermark(&binding, 3))
        .unwrap();

    let mut wrong_epoch = watermark(&binding, 4);
    wrong_epoch.authority_epoch = StoreAuthorityEpochV1::new(8).unwrap();
    assert!(matches!(
        publisher.publish_committed_watermark(wrong_epoch),
        Err(CommitWatermarkPublicationError::WrongAuthorityEpoch(_))
    ));
    let non_monotonic = publisher
        .publish_committed_watermark(watermark(&binding, 2))
        .expect_err("non-monotonic publication must fail");
    assert!(matches!(
        non_monotonic,
        CommitWatermarkPublicationError::NonMonotonic { .. }
    ));
    let rendered = non_monotonic.to_string();
    assert!(
        rendered.contains("non-monotonic"),
        "Display must describe the fence: {rendered}"
    );
    assert_eq!(
        publisher.current(&binding.shard_id),
        Some(watermark(&binding, 3))
    );
}

#[test]
fn one_source_tracks_multiple_shards_without_crossing_histories() {
    let first = binding("project.first");
    let second = binding("project.second");
    let publisher =
        CommittedWatermarkPublisher::from_bindings([first.clone(), second.clone()]).unwrap();
    publisher
        .publish_committed_watermark(watermark(&second, 5))
        .unwrap();
    publisher
        .publish_committed_watermark(watermark(&first, 2))
        .unwrap();

    assert_eq!(
        publisher.current(&first.shard_id),
        Some(watermark(&first, 2))
    );
    assert_eq!(
        publisher.current(&second.shard_id),
        Some(watermark(&second, 5))
    );
}
