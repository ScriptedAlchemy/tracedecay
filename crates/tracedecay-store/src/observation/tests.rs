use serde_json::json;
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId,
    ComponentVersion, CoverageReportV1, EvidenceClass, NativeAliasKindV2, ObservationId,
    ObservationIdentityMaterialV1, PayloadAccessState, PayloadReferenceV1,
    PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProviderId,
    ResolutionAuthorizationV1, RetrievalAnchorRecordV2Parts, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizerDispositionV1, ScopeResolutionId, SensitivityV1, SessionId,
    UtcMicros, VectorWatermark,
};

use super::*;

const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn projection_generation() -> ProjectionGenerationId {
    ProjectionGenerationId::new("projection.observation-anchor.v4").unwrap()
}

fn observation(seed: &str, scope: ObservationScopeV1) -> DurableObservationV1 {
    let provider = ProviderId::new("provider.fixture").unwrap();
    let session_id = SessionId::new(format!("session.{seed}")).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(provider, session_id).unwrap();
    let generation = ObservationSourceGenerationV1::new(7).unwrap();
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let record_id = ObservationId::new(format!("record.{seed}")).unwrap();
    let payload = json!({"kind": "assistant_message", "body": seed});
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.{seed}")).unwrap(),
            ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source,
            scope,
            generation,
            range,
            ObservationOrderingDomainV1::SqliteRowId,
            record_id,
        )
        .unwrap(),
        receipt,
        tracedecay_domain::RetentionClass::new("retention.fixture").unwrap(),
        payload,
    )
    .unwrap()
}

fn write(observation: DurableObservationV1) -> ObservationWrite {
    let identity = observation.identity();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .unwrap();
    ObservationWrite::new(observation, None, next_cursor).unwrap()
}

fn authorization() -> ResolutionAuthorizationV1 {
    ResolutionAuthorizationV1 {
        resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
        privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
        access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
        capability_id: CapabilityId::new("capability.fixture").unwrap(),
        canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
    }
}

fn anchor(
    observation: &DurableObservationV1,
    owner: ObservationScopeV1,
    aliases: Vec<NativeAliasV2>,
    ingested_at: i64,
) -> RetrievalAnchorRecordV2 {
    anchor_with_provenance(
        observation,
        owner,
        aliases,
        ingested_at,
        AnchorSourceGenerationV2::Observation(observation.identity().generation()),
        vec![observation.observation_id().clone()],
    )
}

fn anchor_with_provenance(
    observation: &DurableObservationV1,
    owner: ObservationScopeV1,
    aliases: Vec<NativeAliasV2>,
    ingested_at: i64,
    source_generation: AnchorSourceGenerationV2,
    source_observations: Vec<CanonicalObservationIdV1>,
) -> RetrievalAnchorRecordV2 {
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::ExactObservation(observation.observation_id().clone()),
        owner,
        aliases,
        occurred_at: None,
        ingested_at: UtcMicros(ingested_at),
        evidence_class: EvidenceClass::Observed,
        source_generation,
        projection_generation: projection_generation(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations,
        source_anchors: vec![],
        authorization: authorization(),
        payload_access: PayloadAccessState::Eligible,
        retention_class: tracedecay_domain::RetentionClass::new("retention.fixture").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

#[test]
fn anchored_write_and_replay_receipt_keep_the_original_anchor() {
    let observation = observation("replay", ObservationScopeV1::Profile);
    let original_anchor = anchor(&observation, ObservationScopeV1::Profile, vec![], 1);
    let replay_anchor = anchor(&observation, ObservationScopeV1::Profile, vec![], 99);
    assert_eq!(original_anchor.anchor_id(), replay_anchor.anchor_id());
    assert_ne!(original_anchor, replay_anchor);

    let anchored = AnchoredObservationWrite::new(
        write(observation.clone()),
        replay_anchor,
        projection_generation(),
    )
    .unwrap();
    assert_eq!(anchored.observation(), &observation);
    assert_eq!(
        anchored.retrieval_anchor().anchor_id(),
        original_anchor.anchor_id()
    );

    let receipt = ObservationCommitReceipt::new(
        1,
        observation,
        anchored.next_cursor().clone(),
        original_anchor.clone(),
        projection_generation(),
    )
    .unwrap();
    let replay = ObservationPersistOutcome::ExactDuplicate(receipt);
    assert_eq!(replay.receipt().retrieval_anchor(), &original_anchor);
    assert_eq!(
        replay.receipt().projection_generation(),
        &projection_generation()
    );
}

#[test]
fn anchored_write_rejects_identity_owner_and_projection_mismatches() {
    let candidate = observation("candidate", ObservationScopeV1::Profile);
    let other = observation("other", ObservationScopeV1::Profile);
    assert!(matches!(
        AnchoredObservationWrite::new(
            write(candidate.clone()),
            anchor(&other, ObservationScopeV1::Profile, vec![], 1),
            projection_generation(),
        ),
        Err(ObservationStoreError::RetrievalAnchorObservationMismatch)
    ));

    let project_owner = ObservationScopeV1::Project {
        project_id: ProjectId::new("project.fixture").unwrap(),
    };
    assert!(matches!(
        AnchoredObservationWrite::new(
            write(candidate.clone()),
            anchor(&candidate, project_owner, vec![], 1),
            projection_generation(),
        ),
        Err(ObservationStoreError::RetrievalAnchorOwnerMismatch)
    ));

    assert!(matches!(
        AnchoredObservationWrite::new(
            write(candidate.clone()),
            anchor(&candidate, ObservationScopeV1::Profile, vec![], 1),
            ProjectionGenerationId::new("projection.wrong").unwrap(),
        ),
        Err(ObservationStoreError::RetrievalAnchorProjectionGenerationMismatch)
    ));
}

#[test]
fn anchored_write_rejects_mismatched_source_generation_and_lineage() {
    let candidate = observation("source-binding", ObservationScopeV1::Profile);
    let other = observation("source-binding-other", ObservationScopeV1::Profile);
    assert!(matches!(
        AnchoredObservationWrite::new(
            write(candidate.clone()),
            anchor_with_provenance(
                &candidate,
                ObservationScopeV1::Profile,
                vec![],
                1,
                AnchorSourceGenerationV2::Observation(
                    ObservationSourceGenerationV1::new(8).unwrap()
                ),
                vec![candidate.observation_id().clone()],
            ),
            projection_generation(),
        ),
        Err(ObservationStoreError::RetrievalAnchorSourceGenerationMismatch)
    ));
    assert!(matches!(
        AnchoredObservationWrite::new(
            write(candidate.clone()),
            anchor_with_provenance(
                &candidate,
                ObservationScopeV1::Profile,
                vec![],
                1,
                AnchorSourceGenerationV2::Observation(candidate.identity().generation()),
                vec![
                    candidate.observation_id().clone(),
                    other.observation_id().clone(),
                ],
            ),
            projection_generation(),
        ),
        Err(ObservationStoreError::RetrievalAnchorSourceLineageMismatch)
    ));
}

#[test]
fn commit_receipt_rejects_a_partial_mismatched_aggregate() {
    let candidate = observation("rollback", ObservationScopeV1::Profile);
    let other = observation("rollback-other", ObservationScopeV1::Profile);
    let next_cursor = write(candidate.clone()).next_cursor().clone();
    assert!(matches!(
        ObservationCommitReceipt::new(
            1,
            candidate,
            next_cursor,
            anchor(&other, ObservationScopeV1::Profile, vec![], 1),
            projection_generation(),
        ),
        Err(ObservationStoreError::RetrievalAnchorObservationMismatch)
    ));
}

#[test]
fn alias_collision_is_typed_without_a_partial_commit_receipt() {
    let alias = NativeAliasV2::new(
        NativeAliasKindV2::ProviderRecord,
        PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
    )
    .unwrap();
    let first_observation = observation("alias-first", ObservationScopeV1::Profile);
    let second_observation = observation("alias-second", ObservationScopeV1::Profile);
    let first = anchor(
        &first_observation,
        ObservationScopeV1::Profile,
        vec![alias.clone()],
        1,
    );
    let second = anchor(
        &second_observation,
        ObservationScopeV1::Profile,
        vec![alias.clone()],
        2,
    );
    let result: ObservationStoreResult<ObservationPersistOutcome> =
        Err(ObservationStoreError::RetrievalAnchorAliasCollision {
            alias: Box::new(alias.clone()),
            existing_anchor_id: Box::new(first.anchor_id().clone()),
            candidate_anchor_id: Box::new(second.anchor_id().clone()),
        });
    assert!(matches!(
        result,
        Err(ObservationStoreError::RetrievalAnchorAliasCollision {
            alias: collided,
            existing_anchor_id,
            candidate_anchor_id,
        }) if collided.as_ref() == &alias
            && existing_anchor_id.as_ref() == first.anchor_id()
            && candidate_anchor_id.as_ref() == second.anchor_id()
    ));
}

/// The memoized access-policy digest must equal the eager derivation it
/// replaced, on the cold read that populates the memo and on every warm read
/// after it, for more than one authority namespace.
#[test]
fn memoized_access_policy_digest_equals_eager_derivation() {
    for authority_namespace in [
        "memoized-access-policy.alpha.v1",
        "memoized-access-policy.beta.v1",
    ] {
        let expected = PayloadReferenceV1::for_payload(&json!({
            "domain": "tracedecay.observation-anchor.authorization.v1",
            "authority": authority_namespace,
        }))
        .unwrap()
        .digest()
        .as_str()
        .to_owned();
        let subject = observation("memo-policy", ObservationScopeV1::Profile);

        for _ in 0..3 {
            let authorization =
                build_observation_resolution_authorization_v1(&subject, authority_namespace)
                    .unwrap();
            assert_eq!(authorization.access_policy_digest.as_str(), expected);
        }
    }
}
