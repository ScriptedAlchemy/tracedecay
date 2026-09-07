use tracedecay_domain::{BrainId, UserProfileId};
use tracedecay_store::{
    GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1,
    GraphPublicationOperationContextV1, GraphPublicationStoreErrorV1, GraphPublicationStoreV1,
    GraphReplayAppendOutcomeV1, GraphVerifiedHeadCasOutcomeV1, GraphVerifiedHeadCompareAndSwapV1,
    StorageRuntimeContractErrorV1, StoreShardIdV1,
};

use super::{
    Fixture, advance_head, append_with_fresh_context, control_and_probe, projection, replay,
};
use crate::repository::GraphPublicationExactSqlStorage;

fn profile_memory_shard() -> StoreShardIdV1 {
    StoreShardIdV1::profile_memory(
        BrainId::new("brain.fixture").unwrap(),
        UserProfileId::new("profile.fixture").unwrap(),
    )
}

fn profile_memory_projection() -> GraphProjectionIdentityV1 {
    GraphProjectionIdentityV1 {
        shard_id: profile_memory_shard(),
        namespace: GraphNamespaceV1::new("profile-memory").unwrap(),
        projection: GraphProjectionIdV1::new("facts").unwrap(),
    }
}

#[test]
fn profile_memory_replay_and_head_cas_are_exact_and_conflict_on_changed_input() {
    let fixture = Fixture::new_for_shard(profile_memory_shard());
    let projection = profile_memory_projection();
    let publication = replay(
        projection.clone(),
        "generation.1",
        "publish.1",
        'a',
        'b',
        None,
        b"profile-memory",
    );
    let mut storage = fixture.storage();
    assert!(matches!(
        append_with_fresh_context(&mut storage, &publication, "profile-memory.append").unwrap(),
        GraphReplayAppendOutcomeV1::Appended(_)
    ));
    let head = advance_head(&mut storage, &publication);
    assert!(matches!(
        append_with_fresh_context(&mut storage, &publication, "profile-memory.replay").unwrap(),
        GraphReplayAppendOutcomeV1::ExactVerifiedReplay { .. }
    ));

    let (control, probe) = control_and_probe("profile-memory.cas-replay", None);
    let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
    let request = GraphVerifiedHeadCompareAndSwapV1 {
        publication_key: publication.key.clone(),
        input_digest: publication.input_digest.clone(),
        dependency_generation_closure_digest: publication
            .dependency_generation_closure_digest
            .clone(),
        recovered_digest: publication.expected_recovered_digest.clone(),
        expected_prior_head: None,
    };
    assert_eq!(
        storage.compare_and_swap_verified_head(&request, &context),
        Ok(GraphVerifiedHeadCasOutcomeV1::ExactReplay(head.clone()))
    );

    let changed = replay(
        projection.clone(),
        "generation.1",
        "publish.1",
        'c',
        'b',
        None,
        b"changed-profile-memory",
    );
    assert!(matches!(
        append_with_fresh_context(&mut storage, &changed, "profile-memory.changed").unwrap(),
        GraphReplayAppendOutcomeV1::Conflict { .. }
    ));
    let (read_control, read_probe) = control_and_probe("profile-memory.read", None);
    let read_context = GraphPublicationOperationContextV1::new(&read_control, &read_probe).unwrap();
    assert_eq!(
        storage.verified_head(&projection, &read_context).unwrap(),
        Some(head)
    );
}

#[test]
fn attachment_rejects_other_scopes_and_cross_family_owners() {
    let profile = Fixture::new_for_shard(StoreShardIdV1::profile(
        BrainId::new("brain.fixture").unwrap(),
        UserProfileId::new("profile.fixture").unwrap(),
    ));
    assert!(matches!(
        GraphPublicationExactSqlStorage::from_authorized_handle(profile.handle.clone()),
        Err(GraphPublicationStoreErrorV1::InvalidRequest(
            StorageRuntimeContractErrorV1::OperationScopeMismatch {
                operation: "attach graph publication exact SQL storage",
                shard_family: "non-graph-publication",
            }
        ))
    ));

    let project = Fixture::new();
    let profile_publication = replay(
        profile_memory_projection(),
        "generation.foreign",
        "publish.foreign",
        'a',
        'b',
        None,
        b"profile-foreign",
    );
    let project_publication = replay(
        projection("code"),
        "generation.foreign",
        "publish.foreign",
        'a',
        'b',
        None,
        b"project-foreign",
    );
    let profile_memory = Fixture::new_for_shard(profile_memory_shard());
    for (fixture, publication, label) in [
        (&project, &profile_publication, "project.reject-profile"),
        (
            &profile_memory,
            &project_publication,
            "profile.reject-project",
        ),
    ] {
        let (control, probe) = control_and_probe(label, None);
        let context = GraphPublicationOperationContextV1::new(&control, &probe).unwrap();
        assert!(matches!(
            fixture.storage().append_replay(publication, &context),
            Err(GraphPublicationStoreErrorV1::InvalidRequest(
                StorageRuntimeContractErrorV1::ShardMismatch { .. }
            ))
        ));
    }
}
