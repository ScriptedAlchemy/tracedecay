use std::sync::Arc;

use super::auth::{
    OpaqueRemoteCredential, RemoteEnrollmentAuthorityErrorV1, RemoteEnrollmentCommitReceiptV1,
    RemoteEnrollmentCredentialLookupPortV1,
};
use super::composition::{
    AuthenticityClaimV1, AuthorizationClaimV1, ExpectedRemoteShardV1, IntegrityClaimV1,
    PendingLocalObservationsV1, QueryManifestBindingV1, RemoteCompletenessV1, RemoteFreshnessV1,
    RemoteQueryCompositionV1, ShardCoverageStateV1, ShardQueryContributionV1,
};
use super::protocol::{RemoteProtocolPortV1, RemoteProtocolRequestV1};
use super::query::{
    REMOTE_EXACT_OBSERVATION_QUERY_USE_CASE_V1, REMOTE_QUERY_SCHEMA_REVISION_V1,
    RemoteExactObservationQueryCommandV1, RemoteExactObservationQueryErrorV1,
    RemoteExactObservationQueryOutcomeV1, RemoteExactObservationQueryProtocolAdapterV1,
    RemoteExactObservationQueryReadPortV1, RemoteExactObservationQueryServiceV1,
    RemoteExactObservationResultV1, RemoteQueryAuthorizationEvidenceV1,
    RemoteQueryAuthorizationPortV1, RemoteQueryClockPortV1, RemoteQueryCompleteValueV1,
    RemoteQueryOperationV1, RemoteQueryRequestV1, RemoteQueryResultV1, query_protocol_failure,
    remote_exact_observation_query_result_contract_v1, validate_composition,
    validate_protocol_authority_binding, validate_result_identity, validate_returned_authority,
    validate_returned_observation_identity, validate_returned_provenance,
};
use crate::{RequestId, ResolvedScope};
use tracedecay_domain::{
    AuthorityEpoch, BrainId, BrainNodeId, CanonicalObservationIdV1, CurrentRemoteAuthorityStateV1,
    CurrentRemoteAuthorityV1, EnrollmentCredentialRecordV1, EntityId, EvidenceAvailabilityV1,
    GenerationBoundRepositoryProvenanceV1, ObservationScopeV1, PrivacyDomainBoundLocatorDigest,
    ProjectId, ProjectionGenerationId, RefId, RemotePlacementRevisionV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, RepositoryEvidenceV1, RepositoryId, RepositoryProvenanceV1,
    RepositoryRemoteIdentityV1, RepositoryStateSnapshotId, ShardId, UtcMicros, WorktreeId,
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
    }
}

fn composition_result() -> RemoteQueryResultV1 {
    RemoteQueryResultV1 {
        composition: RemoteQueryCompositionV1 {
            contributions: vec![ShardQueryContributionV1 {
                manifest: QueryManifestBindingV1 {
                    brain_id: "brain.remote-query".into(),
                    shard_id: "shard.remote-query.1".into(),
                    generation_id: "generation.remote-query.1".into(),
                    schema_digest: [1; 32],
                    watermark_sequence: 1,
                    placement_revision: 1,
                    authority_epoch: 1,
                    cache_age_millis: 0,
                    cache_lag_commits: 0,
                },
                integrity: IntegrityClaimV1::Verified,
                authenticity: AuthenticityClaimV1::Authenticated,
                freshness: RemoteFreshnessV1::Current,
                completeness: RemoteCompletenessV1::Complete,
                authorization: AuthorizationClaimV1::Authorized,
                coverage: ShardCoverageStateV1::Complete,
                authority_receipt: None,
                value: None,
                reason_code: None,
            }],
            pending_local: PendingLocalObservationsV1 {
                count: 0,
                oldest_age_millis: None,
                has_sequence_gap: false,
                has_quarantined: false,
            }
            .into(),
            coverage: ShardCoverageStateV1::Complete,
        },
        observation: RemoteExactObservationResultV1::NotFound,
    }
}

#[test]
fn remote_complete_value_is_wire_distinct_from_null() {
    let value = RemoteQueryCompleteValueV1 {
        returned_observations: 1,
    };

    let json = serde_json::to_string(&value).expect("serialize complete value");
    assert_eq!(json, r#"{"returned_observations":1}"#);
    let round_trip: RemoteQueryCompleteValueV1 =
        serde_json::from_str(&json).expect("deserialize complete value");
    round_trip.validate().expect("validate complete value");
}

#[test]
fn exact_observation_absence_round_trips_as_explicit_state() {
    let json = serde_json::to_string(&RemoteExactObservationResultV1::NotFound).unwrap();
    assert_eq!(json, r#"{"state":"not_found"}"#);
    assert_eq!(
        serde_json::from_str::<RemoteExactObservationResultV1>(&json).unwrap(),
        RemoteExactObservationResultV1::NotFound
    );
}

#[test]
fn remote_query_request_enforces_shard_inventory_bounds_and_identity() {
    assert!(request(Vec::new()).validate().is_err());
    assert!(request(vec![shard(1)]).validate().is_ok());
    assert!(request(vec![shard(1), shard(2)]).validate().is_err());
    assert!(request(vec![shard(1), shard(1)]).validate().is_err());

    let mut mixed = shard(2);
    mixed.brain_id = "brain.other".to_owned();
    assert!(request(vec![shard(1), mixed]).validate().is_err());
}

#[test]
fn remote_query_request_binds_inventory_to_expected_fence() {
    let mut mismatched_brain = request(vec![shard(1)]);
    mismatched_brain.expected_shards[0].brain_id = "brain.other".into();
    assert!(mismatched_brain.validate().is_err());

    let mut mismatched_shard = request(vec![shard(1)]);
    mismatched_shard.expected_shards[0].shard_id = "shard.other".into();
    assert!(mismatched_shard.validate().is_err());

    let mut mismatched_generation = request(vec![shard(1)]);
    mismatched_generation.expected_shards[0].generation_id = "generation.other".into();
    assert!(mismatched_generation.validate().is_err());
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

#[test]
fn protocol_and_body_authority_must_match_exactly() {
    let body = request(vec![shard(1)]);
    let exact = RemoteProtocolRequestV1::new(
        RequestId::new("request.remote-query").unwrap(),
        body.expected_authority.brain_id.clone(),
        BrainNodeId::new("node.remote-query").unwrap(),
        1,
        Some(body.expected_authority.clone()),
        tracedecay_domain::UtcMicros(10),
        body.clone(),
    )
    .unwrap();
    assert!(validate_protocol_authority_binding(&exact).is_ok());

    let mut missing = exact.clone();
    missing.expected_authority = None;
    assert!(validate_protocol_authority_binding(&missing).is_err());

    let mut mismatched = exact;
    mismatched
        .expected_authority
        .as_mut()
        .unwrap()
        .authority_epoch = AuthorityEpoch(2);
    assert!(validate_protocol_authority_binding(&mismatched).is_err());
}

#[test]
fn faulty_composition_identity_is_rejected_fail_closed() {
    let expected = shard(1);
    let fence = request(vec![expected.clone()]).expected_authority;
    assert!(validate_composition(&composition_result(), &expected, &fence).is_ok());

    for field in ["brain", "shard", "generation", "placement", "epoch"] {
        let mut result = composition_result();
        let manifest = &mut result.composition.contributions[0].manifest;
        match field {
            "brain" => manifest.brain_id = "brain.other".into(),
            "shard" => manifest.shard_id = "shard.other".into(),
            "generation" => manifest.generation_id = "generation.other".into(),
            "placement" => manifest.placement_revision += 1,
            "epoch" => manifest.authority_epoch += 1,
            _ => unreachable!(),
        }
        assert!(validate_composition(&result, &expected, &fence).is_err());
    }
}

#[test]
fn receipt_mismatch_maps_to_unavailable_not_scope_concealment() {
    assert_eq!(
        query_protocol_failure(RemoteExactObservationQueryErrorV1::ReceiptMismatch),
        super::protocol::RemoteProtocolFailureV1::AuthorityUnavailable
    );
    assert_eq!(
        query_protocol_failure(RemoteExactObservationQueryErrorV1::ScopeMismatch),
        super::protocol::RemoteProtocolFailureV1::ScopeMismatch
    );
}

#[test]
fn faulty_adapter_result_identity_is_rejected() {
    let expected_request = RequestId::new("request.remote-query").unwrap();
    let expected_scope = scope();
    let resolved = ResolvedScope::new(
        expected_scope.project_id.clone(),
        expected_scope.repository_id.clone(),
        expected_scope.worktree_id.clone(),
        expected_scope.reference.clone(),
    )
    .unwrap();
    let contract = remote_exact_observation_query_result_contract_v1();
    assert!(
        validate_result_identity(
            &contract,
            &expected_request,
            &resolved,
            &expected_request,
            &expected_scope
        )
        .is_ok()
    );

    let wrong_contract = super::protocol::remote_replay_result_contract_v1();
    assert!(
        validate_result_identity(
            &wrong_contract,
            &expected_request,
            &resolved,
            &expected_request,
            &expected_scope
        )
        .is_err()
    );
    assert!(
        validate_result_identity(
            &contract,
            &RequestId::new("request.other").unwrap(),
            &resolved,
            &expected_request,
            &expected_scope
        )
        .is_err()
    );
    let wrong_scope = ResolvedScope::new(
        ProjectId::new("project.other").unwrap(),
        expected_scope.repository_id.clone(),
        expected_scope.worktree_id.clone(),
        expected_scope.reference.clone(),
    )
    .unwrap();
    assert!(
        validate_result_identity(
            &contract,
            &expected_request,
            &wrong_scope,
            &expected_request,
            &expected_scope
        )
        .is_err()
    );
}

#[test]
fn faulty_adapter_observation_identity_is_rejected() {
    let expected_scope = scope();
    let expected_id = CanonicalObservationIdV1::new(format!("sha256:{}", "a".repeat(64))).unwrap();
    let generation = ProjectionGenerationId::new("generation.remote-query.1").unwrap();
    let observation_scope = ObservationScopeV1::Project {
        project_id: expected_scope.project_id.clone(),
    };
    assert!(
        validate_returned_observation_identity(
            &expected_id,
            &generation,
            &observation_scope,
            &expected_id,
            &generation,
            &expected_scope,
        )
        .is_ok()
    );
    let wrong_id = CanonicalObservationIdV1::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    assert!(
        validate_returned_observation_identity(
            &wrong_id,
            &generation,
            &observation_scope,
            &expected_id,
            &generation,
            &expected_scope,
        )
        .is_err()
    );
    assert!(
        validate_returned_observation_identity(
            &expected_id,
            &ProjectionGenerationId::new("generation.other").unwrap(),
            &observation_scope,
            &expected_id,
            &generation,
            &expected_scope,
        )
        .is_err()
    );
    assert!(
        validate_returned_observation_identity(
            &expected_id,
            &generation,
            &ObservationScopeV1::Project {
                project_id: ProjectId::new("project.other").unwrap(),
            },
            &expected_id,
            &generation,
            &expected_scope,
        )
        .is_err()
    );
}

#[test]
fn faulty_adapter_current_authority_is_rejected() {
    let expected = request(vec![shard(1)]).expected_authority;
    let available = CurrentRemoteAuthorityStateV1::Available(CurrentRemoteAuthorityV1 {
        fence: expected.clone(),
        credential_revision: 1,
        observed_at: UtcMicros(10),
    });
    assert!(validate_returned_authority(&available, &expected).is_ok());

    let mut wrong = expected.clone();
    wrong.authority_epoch = AuthorityEpoch(2);
    let stale = CurrentRemoteAuthorityStateV1::Available(CurrentRemoteAuthorityV1 {
        fence: wrong,
        credential_revision: 1,
        observed_at: UtcMicros(10),
    });
    assert_eq!(
        validate_returned_authority(&stale, &expected),
        Err(RemoteExactObservationQueryErrorV1::StaleFence)
    );
    assert_eq!(
        validate_returned_authority(
            &CurrentRemoteAuthorityStateV1::Unavailable {
                reason: tracedecay_domain::RemoteAuthorityUnavailableReasonV1::FenceUnverified,
                observed_at: UtcMicros(10),
            },
            &expected,
        ),
        Err(RemoteExactObservationQueryErrorV1::AuthorityUnavailable)
    );
}

fn provenance(
    scope: &RemoteRepositoryScopeV1,
    generation: &ProjectionGenerationId,
    observation_id: &CanonicalObservationIdV1,
) -> EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> {
    let evidence = RepositoryEvidenceV1::new(
        scope.reference.clone().map_or(
            EvidenceAvailabilityV1::Unavailable,
            EvidenceAvailabilityV1::Known,
        ),
        EvidenceAvailabilityV1::Unavailable,
        EvidenceAvailabilityV1::Unavailable,
        EvidenceAvailabilityV1::Unavailable,
        RepositoryRemoteIdentityV1::Unknown,
        EvidenceAvailabilityV1::Unavailable,
    )
    .unwrap();
    let capture = RepositoryProvenanceV1::new(
        scope.repository_id.clone(),
        Some(scope.project_id.clone()),
        Some(scope.worktree_id.clone()),
        PrivacyDomainBoundLocatorDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        evidence,
        UtcMicros(9),
    )
    .unwrap();
    EvidenceAvailabilityV1::Known(
        GenerationBoundRepositoryProvenanceV1::new(
            generation.clone(),
            capture,
            Some(observation_id.clone()),
        )
        .unwrap(),
    )
}

#[test]
fn faulty_adapter_repository_provenance_is_rejected() {
    let expected_scope = scope();
    let observation_id =
        CanonicalObservationIdV1::new(format!("sha256:{}", "a".repeat(64))).unwrap();
    let generation = ProjectionGenerationId::new("generation.remote-query.1").unwrap();
    let valid = provenance(&expected_scope, &generation, &observation_id);
    assert!(
        validate_returned_provenance(
            &valid,
            Some(&generation),
            &observation_id,
            &generation,
            &expected_scope,
        )
        .is_ok()
    );
    assert!(
        validate_returned_provenance(
            &EvidenceAvailabilityV1::Unavailable,
            Some(&generation),
            &observation_id,
            &generation,
            &expected_scope,
        )
        .is_err()
    );
    let wrong_generation = ProjectionGenerationId::new("generation.other").unwrap();
    assert!(
        validate_returned_provenance(
            &valid,
            Some(&wrong_generation),
            &observation_id,
            &generation,
            &expected_scope,
        )
        .is_err()
    );
    let mut wrong_scope = expected_scope.clone();
    wrong_scope.repository_id = RepositoryId::new("repository.other").unwrap();
    assert!(
        validate_returned_provenance(
            &provenance(&wrong_scope, &generation, &observation_id),
            Some(&generation),
            &observation_id,
            &generation,
            &expected_scope,
        )
        .is_err()
    );
}

struct UnavailableCredentials;

impl RemoteEnrollmentCredentialLookupPortV1 for UnavailableCredentials {
    fn enrollment_by_id(
        &self,
        _enrollment_id: &EntityId,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
        Err(RemoteEnrollmentAuthorityErrorV1::Unavailable)
    }

    fn authority_enrollment(
        &self,
        _brain_id: &BrainId,
        _node_id: &BrainNodeId,
        _revision: u64,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
        Err(RemoteEnrollmentAuthorityErrorV1::Unavailable)
    }

    fn enrollment_commit_receipt(
        &self,
        _enrollment_id: &EntityId,
    ) -> Result<RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentAuthorityErrorV1> {
        Err(RemoteEnrollmentAuthorityErrorV1::Unavailable)
    }
}

struct UnreachableRead;

impl RemoteExactObservationQueryReadPortV1 for UnreachableRead {
    fn read_exact_observation(
        &self,
        _command: &RemoteExactObservationQueryCommandV1,
    ) -> Result<RemoteExactObservationQueryOutcomeV1, RemoteExactObservationQueryErrorV1> {
        panic!("credential or cancellation denial must precede storage")
    }
}

struct UnreachableAuthorization;

impl RemoteQueryAuthorizationPortV1 for UnreachableAuthorization {
    fn authorize(
        &self,
        _scope: &ResolvedScope,
        _repository_scope: &RemoteRepositoryScopeV1,
        _observation_id: &CanonicalObservationIdV1,
        _expected_authority: &RemoteWriterFenceV1,
        _observed_at: UtcMicros,
    ) -> Result<RemoteQueryAuthorizationEvidenceV1, RemoteExactObservationQueryErrorV1> {
        panic!("credential denial must precede query policy")
    }
}

struct FixedClock(UtcMicros);

impl RemoteQueryClockPortV1 for FixedClock {
    fn now(&self) -> Result<UtcMicros, RemoteExactObservationQueryErrorV1> {
        Ok(self.0)
    }
}

fn protocol_request(sent_at: UtcMicros) -> RemoteProtocolRequestV1<RemoteQueryRequestV1> {
    let body = request(vec![shard(1)]);
    RemoteProtocolRequestV1::new(
        RequestId::new("request.remote-query-runtime").unwrap(),
        body.expected_authority.brain_id.clone(),
        BrainNodeId::new("node.remote-query").unwrap(),
        1,
        Some(body.expected_authority.clone()),
        sent_at,
        body,
    )
    .unwrap()
}

fn unavailable_service() -> RemoteExactObservationQueryServiceV1 {
    RemoteExactObservationQueryServiceV1::new_with_clock(
        Arc::new(UnavailableCredentials),
        Arc::new(UnreachableAuthorization),
        Arc::new(UnreachableRead),
        Arc::new(FixedClock(UtcMicros(77))),
    )
}

#[test]
fn protocol_failure_uses_server_clock_and_never_returns_partial_success() {
    let adapter = RemoteExactObservationQueryProtocolAdapterV1::new(unavailable_service());
    let response = adapter.execute(
        protocol_request(UtcMicros(10)),
        OpaqueRemoteCredential::new(vec![b'q'; 32].into_boxed_slice()).unwrap(),
    );

    assert!(response.result.is_err());
    assert!(matches!(
        response.authority,
        CurrentRemoteAuthorityStateV1::Partial {
            observed_at: UtcMicros(77),
            ..
        }
    ));
}
