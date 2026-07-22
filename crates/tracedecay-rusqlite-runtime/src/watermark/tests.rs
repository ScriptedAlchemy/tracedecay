use std::future::Future;

use tracedecay_store::{
    BrainId, CommitSequenceV1, ProjectId, StoreAuthorityEpochV1, StoreCommitReceiptV1,
    StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId,
};

use super::*;
use crate::read_consistency::{CommitWatermarkSource, WatermarkSourceState};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

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

fn run<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn notification_before_subscribe_is_visible() {
    run(async {
        let binding = binding("project.before");
        let publisher = CommittedWatermarkPublisher::new(binding.clone());
        publisher
            .publish_committed_watermark(watermark(&binding, 1))
            .unwrap();
        let source = publisher.subscribe();

        assert_eq!(
            source
                .wait_for_change(&binding.shard_id, &watermark(&binding, 0))
                .await,
            WatermarkSourceState::Available(watermark(&binding, 1))
        );
    });
}

#[test]
fn writer_receipt_is_the_public_commit_input() {
    let metadata = crate::test_support::metadata("operation.watermark", "key.watermark", 'a');
    let binding = StoreRuntimeBindingV1::new(
        metadata.shard_id.clone(),
        metadata.incarnation,
        metadata.authority_epoch,
    );
    let publisher = CommittedWatermarkPublisher::new(binding.clone());
    let receipt = StoreCommitReceiptV1 {
        operation_id: metadata.operation_id,
        idempotency: metadata.idempotency,
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        commit_sequence: CommitSequenceV1(1),
        committed_at: metadata.admitted_at,
    };

    publisher.publish_committed(&receipt).unwrap();
    assert_eq!(
        publisher.subscribe().current(&binding.shard_id),
        WatermarkSourceState::Available(watermark(&binding, 1))
    );
}

#[test]
fn notification_after_subscribe_and_missed_notifications_yield_latest() {
    run(async {
        let binding = binding("project.after");
        let publisher = CommittedWatermarkPublisher::new(binding.clone());
        let source = publisher.subscribe();
        let initial = watermark(&binding, 0);
        let waiting = source.wait_for_change(&binding.shard_id, &initial);

        publisher
            .publish_committed_watermark(watermark(&binding, 1))
            .unwrap();
        publisher
            .publish_committed_watermark(watermark(&binding, 2))
            .unwrap();

        assert_eq!(
            waiting.await,
            WatermarkSourceState::Available(watermark(&binding, 2))
        );
    });
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
        publisher.subscribe().current(&binding.shard_id),
        WatermarkSourceState::Available(watermark(&binding, 3))
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
    let source = publisher.subscribe();

    assert_eq!(
        source.current(&first.shard_id),
        WatermarkSourceState::Available(watermark(&first, 2))
    );
    assert_eq!(
        source.current(&second.shard_id),
        WatermarkSourceState::Available(watermark(&second, 5))
    );
}
