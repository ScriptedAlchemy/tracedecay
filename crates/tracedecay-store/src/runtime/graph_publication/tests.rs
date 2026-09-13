use super::*;
use crate::runtime::{BrainId, ProjectId, StoreShardIdV1, UserProfileId};

fn projection(project: &str) -> GraphProjectionIdentityV1 {
    GraphProjectionIdentityV1 {
        shard_id: StoreShardIdV1::project(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
            ProjectId::new(project).unwrap(),
        ),
        namespace: GraphNamespaceV1::new("project").unwrap(),
        projection: GraphProjectionIdV1::new("code").unwrap(),
    }
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

#[test]
fn replay_payload_and_digests_are_closed_and_validated() {
    assert!(GraphPublicationInputDigestV1::new(digest('a')).is_ok());
    assert!(GraphRecoveredGenerationDigestV1::new(digest('b')).is_ok());
    assert!(GraphPublicationInputDigestV1::new("a".repeat(64)).is_err());
    assert!(GraphPublicationInputDigestV1::new(format!("sha256:{}", "A".repeat(64))).is_err());

    let publication = GraphPublicationReplayV1 {
        key: GraphPublicationKeyV1 {
            projection: projection("project.fixture"),
            generation: GraphGenerationIdV1::new("generation.fixture").unwrap(),
            idempotency_key: GraphPublicationIdempotencyKeyV1::new("publish.fixture").unwrap(),
        },
        input_digest: GraphPublicationInputDigestV1::new(digest('a')).unwrap(),
        dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1::new(
            digest('c'),
        )
        .unwrap(),
        direct_dependency_generations: Vec::new(),
        expected_prior_head: None,
        expected_recovered_digest: GraphRecoveredGenerationDigestV1::new(digest('b')).unwrap(),
        canonical_replay_source_digest: GraphCanonicalReplaySourceDigestV1::for_source(&[]),
        canonical_replay_source: Vec::new(),
    };
    assert_eq!(
        publication.validate(),
        Err(StorageRuntimeContractErrorV1::Empty {
            field: "graph replay source"
        })
    );
    let mut oversized = publication;
    oversized.canonical_replay_source = vec![0; MAX_GRAPH_REPLAY_SOURCE_BYTES_V1 + 1];
    assert_eq!(
        oversized.validate(),
        Err(StorageRuntimeContractErrorV1::TooLong {
            field: "graph replay source",
            actual: MAX_GRAPH_REPLAY_SOURCE_BYTES_V1 + 1,
            max: MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
        })
    );
    let mut payload_oversized = oversized;
    payload_oversized.canonical_replay_source = vec![0; MAX_GRAPH_REPLAY_SOURCE_BYTES_V1];
    payload_oversized.canonical_replay_source_digest =
        GraphCanonicalReplaySourceDigestV1::for_source(&payload_oversized.canonical_replay_source);
    assert_eq!(
        payload_oversized.validate(),
        Err(StorageRuntimeContractErrorV1::TooLong {
            field: "graph replay payload",
            actual: MAX_GRAPH_REPLAY_SOURCE_BYTES_V1 + 2,
            max: MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
        })
    );
}

#[test]
fn replay_page_bounds_are_closed_and_cursor_bound() {
    assert_eq!(
        GraphPublicationSequenceV1::new(i64::MAX.unsigned_abs() + 1),
        Err(StorageRuntimeContractErrorV1::LimitExceeded {
            field: "graph publication sequence",
            actual: i64::MAX.unsigned_abs() + 1,
            max: i64::MAX.unsigned_abs(),
        })
    );
    assert!(
        GraphPublicationReplayPageRequestV1::new(projection("project.fixture"), None, 1).is_ok()
    );
    assert_eq!(
        GraphPublicationReplayPageRequestV1::new(projection("project.fixture"), None, 0),
        Err(StorageRuntimeContractErrorV1::Zero {
            field: "graph replay page records"
        })
    );
    assert!(
        GraphPublicationReplayPageRequestV1::new(
            projection("project.fixture"),
            None,
            MAX_GRAPH_REPLAY_PAGE_RECORDS_V1 + 1,
        )
        .is_err()
    );

    let publication = GraphPublicationReplayV1::new(
        GraphPublicationKeyV1::new(
            projection("project.fixture"),
            GraphGenerationIdV1::new("generation.page").unwrap(),
            GraphPublicationIdempotencyKeyV1::new("publish.page").unwrap(),
        ),
        GraphPublicationInputDigestV1::new(digest('a')).unwrap(),
        GraphDependencyGenerationClosureDigestV1::new(digest('b')).unwrap(),
        Vec::new(),
        None,
        GraphRecoveredGenerationDigestV1::new(digest('c')).unwrap(),
        vec![1],
    )
    .unwrap();
    let first = GraphPublicationReplayRecordV1::new(
        GraphPublicationSequenceV1::new(1).unwrap(),
        publication.clone(),
    )
    .unwrap();
    let duplicate = GraphPublicationReplayRecordV1::new(
        GraphPublicationSequenceV1::new(1).unwrap(),
        publication.clone(),
    )
    .unwrap();
    assert!(GraphPublicationReplayPageV1::new(vec![first.clone(), duplicate], None).is_err());

    let mut foreign = publication;
    foreign.key.projection = projection("project.foreign");
    let foreign =
        GraphPublicationReplayRecordV1::new(GraphPublicationSequenceV1::new(2).unwrap(), foreign)
            .unwrap();
    assert!(GraphPublicationReplayPageV1::new(vec![first, foreign], None).is_err());

    let foreign_cursor = GraphPublicationReplayCursorV1::new(
        projection("project.foreign"),
        GraphPublicationSequenceV1::new(1).unwrap(),
    )
    .unwrap();
    assert_eq!(
        GraphPublicationReplayPageRequestV1::new(
            projection("project.fixture"),
            Some(foreign_cursor),
            1,
        ),
        Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "graph replay page cursor projection"
        })
    );
}

#[test]
fn graph_publication_contracts_refuse_non_project_shards() {
    let profile_projection = GraphProjectionIdentityV1 {
        shard_id: StoreShardIdV1::profile(
            BrainId::new("brain.fixture").unwrap(),
            UserProfileId::new("profile.fixture").unwrap(),
        ),
        namespace: GraphNamespaceV1::new("profile").unwrap(),
        projection: GraphProjectionIdV1::new("code").unwrap(),
    };
    assert!(matches!(
        GraphPublicationReplayPageRequestV1::new(profile_projection.clone(), None, 1),
        Err(StorageRuntimeContractErrorV1::OperationScopeMismatch { .. })
    ));
    assert!(matches!(
        GraphPublicationProjectionPageRequestV1::new(profile_projection.shard_id, None, 1,),
        Err(StorageRuntimeContractErrorV1::OperationScopeMismatch { .. })
    ));
}

#[test]
fn verification_prior_head_must_belong_to_the_same_projection() {
    let prior_record = GraphPublicationReplayRecordV1::new(
        GraphPublicationSequenceV1::new(1).unwrap(),
        GraphPublicationReplayV1::new(
            GraphPublicationKeyV1::new(
                projection("project.other"),
                GraphGenerationIdV1::new("generation.prior").unwrap(),
                GraphPublicationIdempotencyKeyV1::new("publish.prior").unwrap(),
            ),
            GraphPublicationInputDigestV1::new(digest('a')).unwrap(),
            GraphDependencyGenerationClosureDigestV1::new(digest('c')).unwrap(),
            Vec::new(),
            None,
            GraphRecoveredGenerationDigestV1::new(digest('b')).unwrap(),
            vec![1],
        )
        .unwrap(),
    )
    .unwrap();
    let prior_head = GraphVerifiedHeadV1::from_replay(
        &prior_record,
        GraphRecoveredGenerationDigestV1::new(digest('b')).unwrap(),
    )
    .unwrap();
    let request = GraphVerifiedHeadCompareAndSwapV1 {
        publication_key: GraphPublicationKeyV1::new(
            projection("project.fixture"),
            GraphGenerationIdV1::new("generation.next").unwrap(),
            GraphPublicationIdempotencyKeyV1::new("publish.next").unwrap(),
        ),
        input_digest: GraphPublicationInputDigestV1::new(digest('c')).unwrap(),
        dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1::new(
            digest('e'),
        )
        .unwrap(),
        recovered_digest: GraphRecoveredGenerationDigestV1::new(digest('d')).unwrap(),
        expected_prior_head: Some(prior_head.clone()),
    };

    assert_eq!(
        request.validate(),
        Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "graph verified prior projection"
        })
    );

    let same_generation = GraphVerifiedHeadCompareAndSwapV1 {
        publication_key: prior_head.key.clone(),
        input_digest: GraphPublicationInputDigestV1::new(digest('c')).unwrap(),
        dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1::new(
            digest('e'),
        )
        .unwrap(),
        recovered_digest: GraphRecoveredGenerationDigestV1::new(digest('d')).unwrap(),
        expected_prior_head: Some(prior_head.clone()),
    };
    assert_eq!(
        same_generation.validate(),
        Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "graph verified prior generation"
        })
    );
    assert_eq!(
        GraphPublicationReplayV1::new(
            prior_head.key.clone(),
            GraphPublicationInputDigestV1::new(digest('c')).unwrap(),
            GraphDependencyGenerationClosureDigestV1::new(digest('e')).unwrap(),
            Vec::new(),
            Some(prior_head),
            GraphRecoveredGenerationDigestV1::new(digest('d')).unwrap(),
            vec![1],
        ),
        Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "graph replay prior generation"
        })
    );
}

#[test]
fn replay_direct_dependency_generations_require_canonical_binding_and_order() {
    let owner = projection("project.fixture");
    let dependency = |project: &str, projection_id: &str, generation: &str| {
        GraphDependencyGenerationIdentityV1 {
            projection: GraphProjectionIdentityV1 {
                shard_id: projection(project).shard_id,
                namespace: GraphNamespaceV1::new("project").unwrap(),
                projection: GraphProjectionIdV1::new(projection_id).unwrap(),
            },
            generation: GraphGenerationIdV1::new(generation).unwrap(),
        }
    };
    let replay = |direct_dependency_generations| GraphPublicationReplayV1 {
        key: GraphPublicationKeyV1::new(
            owner.clone(),
            GraphGenerationIdV1::new("generation.fixture").unwrap(),
            GraphPublicationIdempotencyKeyV1::new("publish.fixture").unwrap(),
        ),
        input_digest: GraphPublicationInputDigestV1::new(digest('a')).unwrap(),
        dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1::new(
            digest('b'),
        )
        .unwrap(),
        direct_dependency_generations,
        expected_prior_head: None,
        expected_recovered_digest: GraphRecoveredGenerationDigestV1::new(digest('c')).unwrap(),
        canonical_replay_source_digest: GraphCanonicalReplaySourceDigestV1::for_source(&[1]),
        canonical_replay_source: vec![1],
    };

    let ast = dependency("project.fixture", "ast", "generation.ast");
    let sessions = dependency("project.fixture", "sessions", "generation.sessions");
    assert!(
        replay(vec![ast.clone(), sessions.clone()])
            .validate()
            .is_ok()
    );
    assert_eq!(
        replay(vec![sessions.clone(), ast]).validate(),
        Err(StorageRuntimeContractErrorV1::NonCanonical {
            field: "graph replay direct dependency order"
        })
    );
    assert_eq!(
        replay(vec![sessions.clone(), sessions]).validate(),
        Err(StorageRuntimeContractErrorV1::NonCanonical {
            field: "graph replay direct dependency order"
        })
    );
    assert_eq!(
        replay(vec![dependency(
            "project.foreign",
            "sessions",
            "generation.sessions"
        )])
        .validate(),
        Err(StorageRuntimeContractErrorV1::ShardMismatch {
            field: "graph replay direct dependency"
        })
    );
    assert_eq!(
        replay(vec![GraphDependencyGenerationIdentityV1 {
            projection: owner.clone(),
            generation: GraphGenerationIdV1::new("generation.prior").unwrap(),
        }])
        .validate(),
        Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "graph replay self dependency"
        })
    );
    let too_many = (0..=MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1)
        .map(|index| {
            dependency(
                "project.fixture",
                &format!("dependency.{index:03}"),
                "generation.dependency",
            )
        })
        .collect();
    assert_eq!(
        replay(too_many).validate(),
        Err(StorageRuntimeContractErrorV1::TooLong {
            field: "graph replay direct dependencies",
            actual: MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1 + 1,
            max: MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1,
        })
    );
}
