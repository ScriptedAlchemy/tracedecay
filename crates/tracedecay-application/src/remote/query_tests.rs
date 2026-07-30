use super::composition::ExpectedRemoteShardV1;
use super::query::{
    MAX_REMOTE_QUERY_CURSOR_BYTES_V1, MAX_REMOTE_QUERY_EXPECTED_SHARDS_V1,
    REMOTE_EXACT_OBSERVATION_QUERY_USE_CASE_V1, REMOTE_QUERY_SCHEMA_REVISION_V1,
    RemoteQueryCompleteValueV1, RemoteQueryOperationV1, RemoteQueryPageBoundsV1,
    RemoteQueryRequestV1, remote_exact_observation_query_result_contract_v1,
};
use tracedecay_domain::{
    AuthorityEpoch, BrainId, BrainNodeId, CanonicalObservationIdV1, ProjectId,
    ProjectionGenerationId, RefId, RemotePlacementRevisionV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, RepositoryId, RepositoryStateSnapshotId, ShardId, WorktreeId,
};

fn scope() -> RemoteRepositoryScopeV1 {
    RemoteRepositoryScopeV1 {
        project_id: ProjectId::new("project.remote-query").expect("project"),
        repository_id: RepositoryId::new("repository.remote-query").expect("repository"),
        worktree_id: WorktreeId::new("worktree.remote-query").expect("worktree"),
        reference: Some(RefId::new("refs/heads/main").expect("reference")),
        snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote-query").expect("snapshot"),
    }
}

fn shard(index: usize) -> ExpectedRemoteShardV1 {
    ExpectedRemoteShardV1 {
        brain_id: "brain.remote-query".to_owned(),
        shard_id: format!("shard.remote-query.{index}"),
        generation_id: format!("generation.remote-query.{index}"),
    }
}

fn request(shards: Vec<ExpectedRemoteShardV1>) -> RemoteQueryRequestV1 {
    RemoteQueryRequestV1 {
        schema_revision: REMOTE_QUERY_SCHEMA_REVISION_V1,
        scope: scope(),
        expected_shards: shards,
        expected_authority: RemoteWriterFenceV1 {
            brain_id: BrainId::new("brain.remote-query").unwrap(),
            shard_id: ShardId::new("shard.remote-query.1").unwrap(),
            generation_id: ProjectionGenerationId::new("generation.remote-query.1").unwrap(),
            placement_revision: RemotePlacementRevisionV1::new(1).unwrap(),
            authority_epoch: AuthorityEpoch(1),
            authority_node_id: BrainNodeId::new("node.remote-query").unwrap(),
        },
        operation: RemoteQueryOperationV1::ExactObservation {
            observation_id: CanonicalObservationIdV1::new(format!("sha256:{}", "a".repeat(64)))
                .unwrap(),
        },
        page: RemoteQueryPageBoundsV1::new(1, None).expect("page bounds"),
    }
}

#[test]
fn remote_query_page_bounds_reject_zero_page_size() {
    assert!(RemoteQueryPageBoundsV1::new(0, None).is_err());
}

#[test]
fn remote_complete_value_is_wire_distinct_from_null() {
    let value = RemoteQueryCompleteValueV1 {
        complete_value_present: true,
    };

    let json = serde_json::to_string(&value).expect("serialize complete value");
    assert_eq!(json, r#"{"complete_value_present":true}"#);
    let round_trip: RemoteQueryCompleteValueV1 =
        serde_json::from_str(&json).expect("deserialize complete value");
    round_trip.validate().expect("validate complete value");
}

#[test]
fn remote_query_page_bounds_enforce_page_and_cursor_limits() {
    for page_size in [0, 101] {
        assert!(RemoteQueryPageBoundsV1::new(page_size, None).is_err());
    }
    for page_size in [1, 100] {
        assert!(RemoteQueryPageBoundsV1::new(page_size, None).is_ok());
    }
    assert!(
        RemoteQueryPageBoundsV1::new(1, Some("x".repeat(MAX_REMOTE_QUERY_CURSOR_BYTES_V1))).is_ok()
    );
    assert!(
        RemoteQueryPageBoundsV1::new(1, Some("x".repeat(MAX_REMOTE_QUERY_CURSOR_BYTES_V1 + 1)))
            .is_err()
    );
}

#[test]
fn remote_query_request_enforces_shard_inventory_bounds_and_identity() {
    assert!(request(Vec::new()).validate().is_err());
    assert!(
        request(
            (0..MAX_REMOTE_QUERY_EXPECTED_SHARDS_V1)
                .map(shard)
                .collect()
        )
        .validate()
        .is_ok()
    );
    assert!(
        request(
            (0..=MAX_REMOTE_QUERY_EXPECTED_SHARDS_V1)
                .map(shard)
                .collect()
        )
        .validate()
        .is_err()
    );
    assert!(request(vec![shard(1), shard(1)]).validate().is_err());

    let mut mixed = shard(2);
    mixed.brain_id = "brain.other".to_owned();
    assert!(request(vec![shard(1), mixed]).validate().is_err());
}

#[test]
fn remote_query_request_rejects_invalid_shard_identifiers() {
    let mut invalid = shard(1);
    invalid.generation_id = " generation.remote-query ".to_owned();
    assert!(request(vec![invalid]).validate().is_err());
}

#[test]
fn remote_query_request_rejects_unknown_wire_fields() {
    let mut json = serde_json::to_value(request(vec![shard(1)])).expect("serialize request");
    json.as_object_mut()
        .expect("object request")
        .insert("unexpected".to_owned(), serde_json::Value::Null);

    assert!(serde_json::from_value::<RemoteQueryRequestV1>(json).is_err());
}

#[test]
fn exact_observation_query_has_operation_specific_contract_identity() {
    assert_eq!(
        REMOTE_EXACT_OBSERVATION_QUERY_USE_CASE_V1,
        "use-case.remote.query.exact-observation"
    );
    assert_ne!(
        remote_exact_observation_query_result_contract_v1(),
        super::protocol::remote_replay_result_contract_v1()
    );
    assert!(matches!(
        request(vec![shard(1)]).operation,
        RemoteQueryOperationV1::ExactObservation { .. }
    ));
}
