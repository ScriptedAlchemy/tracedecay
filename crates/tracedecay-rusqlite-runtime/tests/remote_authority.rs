use std::sync::Arc;

use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_application::remote::capture::RemoteWriterAuthorityV1;
use tracedecay_domain::{
    AuthorityEpoch, BrainId, BrainNodeId, CurrentRemoteAuthorityV1, ProjectId,
    ProjectionGenerationId, RefId, RemoteAuthorityUnavailableReasonV1, RemotePlacementRevisionV1,
    RemoteRepositoryScopeV1, RepositoryId, RepositoryStateSnapshotId, ShardId, UserProfileId,
    UtcMicros, WorktreeId,
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

fn writer(epoch: u64, placement: u64) -> RemoteWriterAuthorityV1 {
    RemoteWriterAuthorityV1 {
        project_id: id::<ProjectId>("project.remote"),
        scope: RemoteRepositoryScopeV1 {
            project_id: id::<ProjectId>("project.remote"),
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
                placement_revision: RemotePlacementRevisionV1::new(placement).unwrap(),
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
    let initial_writer = writer(4, 11);
    let replacement_writer = writer(5, 12);
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
fn reachability_withholds_the_replacement_until_publication_is_complete() {
    let store = RusqliteRemoteAuthorityStoreV1::open_in_memory().unwrap();
    let initial = binding(4);
    let replacement = binding(5);
    let initial_writer = writer(4, 11);
    let replacement_writer = writer(5, 12);
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
        tracedecay_domain::CurrentRemoteAuthorityStateV1::Partial {
            known_fence: Some(replacement_writer.authority.fence.clone()),
            missing: std::collections::BTreeSet::from([
                RemoteAuthorityUnavailableReasonV1::FenceUnverified
            ]),
            observed_at: replacement_writer.authority.observed_at,
        }
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
fn placement_identity_must_match_the_persisted_revision() {
    let store = RusqliteRemoteAuthorityStoreV1::open_in_memory().unwrap();
    let binding = binding(4);
    let writer = writer(4, 11);
    assert_eq!(
        store.initialize_authority(&writer, &binding, 12, &watermark(&binding, 9)),
        Err(RemoteAuthorityStorageErrorV1::InvalidContract)
    );
}

#[test]
fn initial_authority_publication_does_not_require_an_old_writer_fence() {
    let store = RusqliteRemoteAuthorityStoreV1::open_in_memory().unwrap();
    let initial = binding(4);
    let initial_writer = writer(4, 11);
    let frontier = watermark(&initial, 9);
    store
        .initialize_authority(&initial_writer, &initial, 11, &frontier)
        .unwrap();
    store
        .install_fence("writer", &initial_writer, &initial, 11)
        .unwrap();

    assert!(
        store
            .publish_and_enable_serving(&initial, &initial_writer, 11, &frontier, &["writer"],)
            .is_ok()
    );
}

#[test]
fn exact_query_snapshot_is_atomic_and_publication_gated() {
    let store = RusqliteRemoteAuthorityStoreV1::open_in_memory().unwrap();
    let binding = binding(4);
    let writer = writer(4, 11);
    let frontier = watermark(&binding, 9);
    store
        .initialize_authority(&writer, &binding, 11, &frontier)
        .unwrap();

    let snapshot = store
        .query_authority_snapshot(
            &writer.project_id,
            &writer.scope,
            &writer.authority.fence,
            writer.authority.observed_at,
        )
        .unwrap();
    assert!(matches!(
        snapshot.authority,
        tracedecay_domain::CurrentRemoteAuthorityStateV1::Partial { .. }
    ));
    assert_eq!(snapshot.binding, binding);
    assert_eq!(snapshot.frontier, Some(frontier.clone()));

    store
        .install_fence("writer", &writer, &binding, 11)
        .unwrap();
    store
        .publish_and_enable_serving(&binding, &writer, 11, &frontier, &["writer"])
        .unwrap();
    let observed_at = UtcMicros(99);
    let snapshot = store
        .query_authority_snapshot(
            &writer.project_id,
            &writer.scope,
            &writer.authority.fence,
            observed_at,
        )
        .unwrap();
    let mut expected_authority = writer.authority.clone();
    expected_authority.observed_at = observed_at;
    assert_eq!(
        snapshot.authority,
        tracedecay_domain::CurrentRemoteAuthorityStateV1::Available(expected_authority)
    );
    assert_eq!(snapshot.project_id, writer.project_id);
    assert_eq!(snapshot.scope, writer.scope);
    assert_eq!(snapshot.placement_revision, 11);
    assert_eq!(snapshot.binding, binding);
    assert_eq!(snapshot.frontier, Some(frontier));
    assert_eq!(snapshot.observed_at, observed_at);

    let mut stale_generation = writer.authority.fence.clone();
    stale_generation.generation_id = id::<ProjectionGenerationId>("generation.stale");
    let stale = store
        .query_authority_snapshot(
            &writer.project_id,
            &writer.scope,
            &stale_generation,
            observed_at,
        )
        .unwrap();
    assert!(matches!(
        stale.authority,
        tracedecay_domain::CurrentRemoteAuthorityStateV1::Available(_)
    ));

    let other_project = id::<ProjectId>("project.other");
    assert!(
        store
            .query_authority_snapshot(
                &other_project,
                &writer.scope,
                &writer.authority.fence,
                observed_at,
            )
            .is_err()
    );
}

#[test]
fn exact_query_snapshots_never_mix_authority_rotation_fields() {
    let store = Arc::new(RusqliteRemoteAuthorityStoreV1::open_in_memory().unwrap());
    let initial_binding = binding(4);
    let replacement_binding = binding(5);
    let initial_writer = writer(4, 11);
    let replacement_writer = writer(5, 12);
    store
        .initialize_authority(
            &initial_writer,
            &initial_binding,
            11,
            &watermark(&initial_binding, 9),
        )
        .unwrap();
    let reader_store = Arc::clone(&store);
    let requested_project = initial_writer.project_id.clone();
    let requested_scope = initial_writer.scope.clone();
    let requested_fence = initial_writer.authority.fence.clone();
    let reader = std::thread::spawn(move || {
        let mut snapshots = Vec::new();
        for observed in 20..2_020 {
            if let Ok(snapshot) = reader_store.query_authority_snapshot(
                &requested_project,
                &requested_scope,
                &requested_fence,
                UtcMicros(observed),
            ) {
                snapshots.push(snapshot);
            }
            std::thread::yield_now();
        }
        snapshots
    });
    store
        .compare_and_swap(
            &AuthorityCasV1 {
                shard_id: initial_binding.shard_id.clone(),
                expected_binding: initial_binding.clone(),
                replacement_binding: replacement_binding.clone(),
                expected_placement_revision: 11,
                replacement_placement_revision: 12,
            },
            &initial_writer,
            &replacement_writer,
        )
        .unwrap();

    let snapshots = reader.join().unwrap();
    assert!(!snapshots.is_empty());
    for snapshot in snapshots {
        let fence = match snapshot.authority {
            tracedecay_domain::CurrentRemoteAuthorityStateV1::Available(authority) => {
                authority.fence
            }
            tracedecay_domain::CurrentRemoteAuthorityStateV1::Partial {
                known_fence: Some(fence),
                ..
            } => fence,
            other => panic!("unexpected query authority state: {other:?}"),
        };
        assert!(
            (fence == initial_writer.authority.fence
                && snapshot.binding == initial_binding
                && snapshot.placement_revision == 11)
                || (fence == replacement_writer.authority.fence
                    && snapshot.binding == replacement_binding
                    && snapshot.placement_revision == 12)
        );
    }
}

#[test]
fn publication_waits_for_every_durable_fence_and_rejects_old_epochs() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("remote-authority.sqlite");
    let store =
        RusqliteRemoteAuthorityStoreV1::from_connection(Connection::open(&path).unwrap()).unwrap();
    let initial = binding(4);
    let replacement = binding(5);
    let initial_writer = writer(4, 11);
    let replacement_writer = writer(5, 12);
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
    assert_eq!(
        store.publish_and_enable_serving(
            &replacement,
            &replacement_writer,
            12,
            &replacement_frontier,
            &["writer", "receipt"],
        ),
        Err(RemoteAuthorityStorageErrorV1::OldAuthorityStillWritable)
    );
    store
        .fence_old_authority_read_only(&initial_writer, &replacement_writer, &replacement)
        .unwrap();
    drop(store);
    let store =
        RusqliteRemoteAuthorityStoreV1::from_connection(Connection::open(&path).unwrap()).unwrap();
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
        store.current_writer_authority(&initial_writer).unwrap(),
        tracedecay_domain::CurrentRemoteAuthorityStateV1::Available(
            publication.writer.authority.clone()
        )
    );
    assert_eq!(
        store.install_fence("writer", &initial_writer, &initial, 11),
        Err(RemoteAuthorityStorageErrorV1::StaleFence)
    );
}
