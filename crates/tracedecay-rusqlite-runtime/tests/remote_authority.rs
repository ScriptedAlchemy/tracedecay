use tracedecay_application::remote::capture::RemoteWriterAuthorityV1;
use tracedecay_domain::{
    AuthorityEpoch, BrainId, BrainNodeId, CurrentRemoteAuthorityV1, EntityVersionId, ProjectId,
    ProjectionGenerationId, RefId, RemoteAuthorityUnavailableReasonV1, RemoteRepositoryScopeV1,
    RepositoryId, RepositoryStateSnapshotId, ShardId, UserProfileId, UtcMicros, WorktreeId,
};
use tracedecay_rusqlite_runtime::remote_authority::{
    RemoteAuthorityStorageErrorV1, RusqliteRemoteAuthorityStoreV1,
};
use tracedecay_rusqlite_runtime::remote_spool::RemoteAuthorityReachabilityPortV1;
use tracedecay_store::{
    AuthorityCasV1, CommitSequenceV1, ShardWatermarkV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn binding(epoch: u64) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::profile(
            id::<BrainId>("brain.remote"),
            id::<UserProfileId>("profile.remote"),
        ),
        StoreIncarnationV1::new(7).unwrap(),
        StoreAuthorityEpochV1::new(epoch).unwrap(),
    )
}

fn watermark(binding: &StoreRuntimeBindingV1, sequence: u64) -> ShardWatermarkV1 {
    ShardWatermarkV1 {
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        commit_sequence: CommitSequenceV1(sequence),
    }
}

fn writer(epoch: u64, placement: &str) -> RemoteWriterAuthorityV1 {
    RemoteWriterAuthorityV1 {
        project_id: id::<ProjectId>("project.remote"),
        scope: RemoteRepositoryScopeV1 {
            repository_id: id::<RepositoryId>("repository.remote"),
            worktree_id: id::<WorktreeId>("worktree.remote"),
            reference: Some(id::<RefId>("refs/heads/main")),
            snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote").unwrap(),
        },
        authority: CurrentRemoteAuthorityV1 {
            fence: tracedecay_domain::RemoteWriterFenceV1 {
                brain_id: id::<BrainId>("brain.remote"),
                shard_id: id::<ShardId>("shard.remote"),
                generation_id: id::<ProjectionGenerationId>("generation.remote"),
                placement_revision: id::<EntityVersionId>(placement),
                authority_epoch: AuthorityEpoch(epoch),
                authority_node_id: id::<BrainNodeId>("node.authority"),
            },
            credential_revision: epoch,
            observed_at: UtcMicros(10),
        },
    }
}

#[test]
fn authority_cas_rejects_stale_expected_binding() {
    let store = RusqliteRemoteAuthorityStoreV1::open_in_memory().unwrap();
    let initial = binding(4);
    let replacement = binding(5);
    let initial_writer = writer(4, "placement.remote.11");
    let replacement_writer = writer(5, "placement.remote.12");
    store
        .initialize_authority(&initial_writer, &initial, 11, &watermark(&initial, 9))
        .unwrap();
    let cas = AuthorityCasV1 {
        shard_id: initial.shard_id.clone(),
        expected_binding: initial.clone(),
        replacement_binding: replacement.clone(),
        expected_placement_revision: 11,
        replacement_placement_revision: 12,
    };

    let committed = store
        .compare_and_swap(&cas, &initial_writer, &replacement_writer)
        .unwrap();
    assert_eq!(committed.previous_binding, initial);
    assert_eq!(committed.installed_binding, replacement);
    assert_eq!(
        store.compare_and_swap(&cas, &initial_writer, &replacement_writer),
        Err(RemoteAuthorityStorageErrorV1::CasConflict)
    );
}

#[test]
fn reachability_resolves_the_replacement_authority_for_an_old_writer() {
    let store = RusqliteRemoteAuthorityStoreV1::open_in_memory().unwrap();
    let initial = binding(4);
    let replacement = binding(5);
    let initial_writer = writer(4, "placement.remote.11");
    let replacement_writer = writer(5, "placement.remote.12");
    store
        .initialize_authority(&initial_writer, &initial, 11, &watermark(&initial, 9))
        .unwrap();
    store
        .compare_and_swap(
            &AuthorityCasV1 {
                shard_id: initial.shard_id.clone(),
                expected_binding: initial,
                replacement_binding: replacement,
                expected_placement_revision: 11,
                replacement_placement_revision: 12,
            },
            &initial_writer,
            &replacement_writer,
        )
        .unwrap();

    assert_eq!(
        store.current_writer_authority(&initial_writer).unwrap(),
        tracedecay_domain::CurrentRemoteAuthorityStateV1::Available(
            replacement_writer.authority.clone()
        )
    );
    let mut unknown = initial_writer;
    unknown.project_id = id::<ProjectId>("project.other");
    assert!(matches!(
        store.current_writer_authority(&unknown).unwrap(),
        tracedecay_domain::CurrentRemoteAuthorityStateV1::Unavailable {
            reason: RemoteAuthorityUnavailableReasonV1::RegistryUnavailable,
            ..
        }
    ));
}

#[test]
fn publication_waits_for_every_durable_fence_and_rejects_old_epochs() {
    let store = RusqliteRemoteAuthorityStoreV1::open_in_memory().unwrap();
    let initial = binding(4);
    let replacement = binding(5);
    let initial_writer = writer(4, "placement.remote.11");
    let replacement_writer = writer(5, "placement.remote.12");
    let replacement_frontier = watermark(&replacement, 9);
    store
        .initialize_authority(&initial_writer, &initial, 11, &watermark(&initial, 9))
        .unwrap();
    store
        .compare_and_swap(
            &AuthorityCasV1 {
                shard_id: initial.shard_id.clone(),
                expected_binding: initial.clone(),
                replacement_binding: replacement.clone(),
                expected_placement_revision: 11,
                replacement_placement_revision: 12,
            },
            &initial_writer,
            &replacement_writer,
        )
        .unwrap();

    store
        .fence_old_authority_read_only(&initial_writer, &replacement_writer, &replacement)
        .unwrap();
    store
        .install_fence("writer", &replacement_writer, &replacement, 12)
        .unwrap();
    assert_eq!(
        store.publish_and_enable_serving(
            &replacement,
            &replacement_writer,
            12,
            &replacement_frontier,
            &["writer", "receipt"],
        ),
        Err(RemoteAuthorityStorageErrorV1::MissingFence {
            sink_id: "receipt".to_owned(),
        })
    );
    store
        .install_fence("receipt", &replacement_writer, &replacement, 12)
        .unwrap();
    let publication = store
        .publish_and_enable_serving(
            &replacement,
            &replacement_writer,
            12,
            &replacement_frontier,
            &["writer", "receipt"],
        )
        .unwrap();
    assert_eq!(publication.binding, replacement);
    assert_eq!(publication.writer, replacement_writer);
    assert_eq!(publication.frontier, replacement_frontier);
    assert_eq!(
        store.install_fence("writer", &initial_writer, &initial, 11),
        Err(RemoteAuthorityStorageErrorV1::StaleFence)
    );
}
