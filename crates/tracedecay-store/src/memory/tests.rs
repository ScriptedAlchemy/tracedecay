use serde_json::json;
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, AnchorDurabilityClass, AnchorLineageRefV2,
    AnchorProvenanceRelationV2, AnchorSourceGenerationV2, CapabilityId, ComponentVersion,
    CoverageReportV1, EntityId, EntityKind, EntityRef, EvidenceClass, FactAssertionKindV1,
    FactCategoryV1, FactCurationActionV1, FactEvidenceRefV1, FactEvidenceRelationV1,
    FactIdentityMaterialV1, FactIdentitySourceV1, ObservationScopeV1, PayloadReferenceV1,
    PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectionGenerationId, ProvenanceId,
    ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorRecordV2Parts,
    RetrievalAnchorTargetV2, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, ScopeResolutionId, SensitivityV1,
    VectorWatermark,
};

use super::*;

mod add_material;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String, Error = DomainError>,
{
    T::try_from(value.to_owned()).unwrap()
}

fn fact_id(owner: FactOwnerV1, operation: &str) -> FactId {
    FactId::derive(
        &FactIdentityMaterialV1::new(
            owner,
            FactIdentitySourceV1::Application {
                operation_id: id::<ProvenanceId>(operation),
            },
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn fact_read_control_observes_live_interruption_state() {
    let interrupted = std::sync::Arc::new(std::sync::RwLock::new(false));
    let observed = std::sync::Arc::clone(&interrupted);
    let control = FactReadControl::new(std::sync::Arc::new(move || {
        *observed.read().expect("read interruption fixture")
    }));

    assert!(!control.interrupted());
    *interrupted.write().expect("write interruption fixture") = true;
    assert!(control.interrupted());
}

fn receipt_for(material: &serde_json::Value) -> SanitizationReceiptV1 {
    receipt_for_disposition(material, SanitizerDispositionV1::Accepted)
}

fn receipt_for_disposition(
    material: &serde_json::Value,
    disposition: SanitizerDispositionV1,
) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            id::<SanitizationReceiptId>("receipt.fact.store.fixture"),
            id::<ComponentVersion>("sanitizer.fixture.v1"),
        )
        .unwrap(),
        disposition,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(material).unwrap()),
    )
    .unwrap()
}

fn payload() -> FactPayloadV1 {
    let material = json!({
        "content": "The daemon is the only writer.",
        "category": "project",
        "tags": ["database"],
        "entities": ["TraceDecay"],
        "metadata": {},
    });
    let receipt = receipt_for(&material);
    FactPayloadV1::new(
        "The daemon is the only writer.".to_owned(),
        FactCategoryV1::Project,
        vec!["database".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({}),
        None,
        receipt,
        RetentionClass::new("durable.fact").unwrap(),
    )
    .unwrap()
}

fn payload_event(fact_id: FactId, owner: FactOwnerV1, occurred_at: i64) -> FactLineageEventV1 {
    FactLineageEventV1::new(
        fact_id,
        owner,
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Deleted,
        },
        UtcMicros(occurred_at),
        None,
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn normalized_tag_batch(
    owner: FactOwnerV1,
    fact_id: FactId,
    evidence_fact_ids: Vec<FactId>,
    assertion_kind: FactAssertionKindV1,
    asserted_at: i64,
    recorded_at: i64,
    normalized_at: i64,
) -> FactStoreResult<FactWriteBatch> {
    let actor = Some(id::<ActorId>("actor.normalized-tags"));
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        assertion_kind,
        payload(),
        vec![],
        UtcMicros(asserted_at),
        actor.clone(),
    )?;
    let recorded = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        UtcMicros(recorded_at),
        actor.clone(),
    )?;
    let normalized = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::TagsNormalized {
                evidence_fact_ids,
                confidence: Confidence::new(0.8)?,
            },
            evidence_ids: vec![],
        },
        UtcMicros(normalized_at),
        actor,
    )?;
    FactWriteBatch::new(
        fact_id,
        owner,
        Some(assertion),
        vec![recorded, normalized],
        vec![],
        vec![],
        None,
    )
}

fn projected_fact(
    projected_as_of: UtcMicros,
    telemetry_updated_at: UtcMicros,
) -> ProjectMemoryFactV1 {
    let owner = FactOwnerV1::Profile;
    let source = FactIdentitySourceV1::Application {
        operation_id: id("operation.projected-fact"),
    };
    let fact_id =
        FactId::derive(&FactIdentityMaterialV1::new(owner.clone(), source.clone()).unwrap())
            .unwrap();
    ProjectMemoryFactV1::new(
        fact_id,
        owner,
        payload(),
        Confidence::new(0.5).unwrap(),
        id("assertion.projected-fact"),
        id("event.projected-fact"),
        projected_as_of,
        source,
        ProjectMemoryFactTelemetryV1::new(
            0,
            0,
            0,
            0,
            UtcMicros(1),
            telemetry_updated_at,
            None,
            None,
            None,
        )
        .unwrap(),
    )
    .unwrap()
}

fn anchor(entity_id: &str, source_anchors: Vec<AnchorLineageRefV2>) -> RetrievalAnchorRecordV2 {
    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::Entity(EntityRef {
            id: EntityId::new(entity_id).unwrap(),
            kind: EntityKind::Document,
        }),
        owner: ObservationScopeV1::Profile,
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(1),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::Unknown,
        projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors,
        authorization: ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
            privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
            access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
            capability_id: CapabilityId::new("capability.fixture").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
        },
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.fixture").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

fn anchor_source(anchor_id: RetrievalAnchorId) -> AnchorLineageRefV2 {
    AnchorLineageRefV2::new(
        AnchorProvenanceRelationV2::DerivedFrom,
        anchor_id,
        ObservationScopeV1::Profile,
    )
    .unwrap()
}

#[test]
fn batch_rejects_owner_mismatch() {
    let fact_id = fact_id(FactOwnerV1::Profile, "operation.owner");
    let event = payload_event(fact_id.clone(), FactOwnerV1::Profile, 1);
    let error = FactWriteBatch::new(
        fact_id,
        FactOwnerV1::Project {
            project_id: id("project.other"),
        },
        None,
        vec![event],
        vec![],
        vec![],
        None,
    )
    .unwrap_err();
    assert!(matches!(error, FactStoreError::OwnerMismatch));
}

#[test]
fn batch_rejects_missing_and_cyclic_anchor_lineage() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.anchor-lineage");
    let event = payload_event(fact_id.clone(), owner.clone(), 1);
    let missing_id: RetrievalAnchorId = id("retrieval.missing-source");
    let missing = anchor("entity.missing", vec![anchor_source(missing_id.clone())]);
    let error = FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        vec![event.clone()],
        vec![missing],
        vec![],
        None,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        FactStoreError::MissingAnchorLineageSource { anchor_id }
            if anchor_id == missing_id
    ));

    let base_a = anchor("entity.cycle.a", vec![]);
    let base_b = anchor("entity.cycle.b", vec![]);
    let cycle_a = anchor(
        "entity.cycle.a",
        vec![anchor_source(base_b.anchor_id().clone())],
    );
    let cycle_b = anchor(
        "entity.cycle.b",
        vec![anchor_source(base_a.anchor_id().clone())],
    );
    let error = FactWriteBatch::new(
        fact_id,
        owner,
        None,
        vec![event],
        vec![cycle_a, cycle_b],
        vec![],
        None,
    )
    .unwrap_err();
    assert!(matches!(error, FactStoreError::CyclicAnchorLineage { .. }));
}

#[test]
fn batch_accepts_order_independent_acyclic_anchor_lineage() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.anchor-dag");
    let root = anchor("entity.dag.root", vec![]);
    let child = anchor(
        "entity.dag.child",
        vec![anchor_source(root.anchor_id().clone())],
    );

    FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        vec![payload_event(fact_id, owner, 1)],
        vec![child, root],
        vec![],
        None,
    )
    .unwrap();
}

#[test]
fn batch_rejects_missing_evidence_anchor() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.anchor");
    let evidence = FactEvidenceRefV1::new(
        fact_id.clone(),
        id("retrieval.missing"),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        payload(),
        vec![evidence],
        UtcMicros(1),
        None,
    )
    .unwrap();
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        UtcMicros(1),
        None,
    )
    .unwrap();

    let error = FactWriteBatch::new(
        fact_id,
        owner,
        Some(assertion),
        vec![event],
        vec![],
        vec![],
        None,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        FactStoreError::MissingEvidenceAnchor { .. }
    ));
}

#[test]
fn batch_rejects_duplicate_replay_shape() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.replay");
    let event = payload_event(fact_id.clone(), owner.clone(), 1);
    let error = FactWriteBatch::new(
        fact_id,
        owner,
        None,
        vec![event.clone(), event],
        vec![],
        vec![],
        None,
    )
    .unwrap_err();
    assert!(matches!(error, FactStoreError::DuplicateEventId { .. }));
}

#[test]
fn normalized_tag_batch_rejects_standalone_existing_evidence_shape() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.normalized-tag-standalone");
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::TagsNormalized {
                evidence_fact_ids: vec![fact_id.clone()],
                confidence: Confidence::new(0.8).unwrap(),
            },
            evidence_ids: vec![],
        },
        UtcMicros(11),
        None,
    )
    .unwrap();

    let error =
        FactWriteBatch::new(fact_id, owner, None, vec![event], vec![], vec![], None).unwrap_err();
    assert!(matches!(
        error,
        FactStoreError::Contract(DomainError::NonCanonical {
            field: "normalized tag curation batch"
        })
    ));
}

#[test]
fn normalized_tag_batch_rejects_non_correction_and_timestamp_mismatch() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.normalized-tag-invalid");
    let correction = || FactAssertionKindV1::Correction {
        supersedes: id("assertion.normalized-tags.previous"),
    };
    let cases = [
        (FactAssertionKindV1::Initial, 10, 10, 11),
        (correction(), 10, 10, 12),
    ];

    for (assertion_kind, asserted_at, recorded_at, normalized_at) in cases {
        let error = normalized_tag_batch(
            owner.clone(),
            fact_id.clone(),
            vec![fact_id.clone()],
            assertion_kind,
            asserted_at,
            recorded_at,
            normalized_at,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            FactStoreError::Contract(DomainError::NonCanonical {
                field: "normalized tag curation batch"
            })
        ));
    }
}

#[test]
fn batch_accepts_item_counts_at_the_limit() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.batch-limit.boundary");
    let events = (1..=MAX_FACT_WRITE_BATCH_EVENTS)
        .map(|offset| payload_event(fact_id.clone(), owner.clone(), offset as i64))
        .collect();
    let new_anchors = (0..MAX_FACT_WRITE_BATCH_NEW_ANCHORS)
        .map(|index| anchor(&format!("entity.batch-limit.{index}"), vec![]))
        .collect();

    FactWriteBatch::new(fact_id, owner, None, events, new_anchors, vec![], None).unwrap();
}

#[test]
fn batch_rejects_item_counts_over_the_limit() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.batch-limit.overflow");
    let events = (1..=MAX_FACT_WRITE_BATCH_EVENTS + 1)
        .map(|offset| payload_event(fact_id.clone(), owner.clone(), offset as i64))
        .collect();
    let error = FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        events,
        vec![],
        vec![],
        None,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        FactStoreError::BatchLimitExceeded { field, count, max }
            if field == "fact write batch events"
                && count == MAX_FACT_WRITE_BATCH_EVENTS + 1
                && max == MAX_FACT_WRITE_BATCH_EVENTS
    ));

    let new_anchors = (0..=MAX_FACT_WRITE_BATCH_NEW_ANCHORS)
        .map(|index| anchor(&format!("entity.batch-limit.overflow.{index}"), vec![]))
        .collect();
    let error = FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        vec![payload_event(fact_id, owner, 1)],
        new_anchors,
        vec![],
        None,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        FactStoreError::BatchLimitExceeded { field, count, max }
            if field == "fact write batch new anchors"
                && count == MAX_FACT_WRITE_BATCH_NEW_ANCHORS + 1
                && max == MAX_FACT_WRITE_BATCH_NEW_ANCHORS
    ));
}

#[test]
fn creation_identity_material_must_derive_the_batch_fact() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.identity.expected");
    let event = payload_event(fact_id.clone(), owner.clone(), 1);
    let batch = FactWriteBatch::new(
        fact_id,
        owner.clone(),
        None,
        vec![event],
        vec![],
        vec![],
        None,
    )
    .unwrap();
    let unrelated = FactIdentityMaterialV1::new(
        owner,
        FactIdentitySourceV1::Application {
            operation_id: id("operation.identity.unrelated"),
        },
    )
    .unwrap();

    assert!(matches!(
        batch.with_identity_material(unrelated),
        Err(FactStoreError::FactMismatch)
    ));
}

#[test]
fn tombstone_rejects_payload() {
    let owner = FactOwnerV1::Profile;
    let tombstone_fact_id = fact_id(owner.clone(), "operation.tombstone");
    let error = StoredFactV1::new(
        tombstone_fact_id,
        owner,
        Some(payload()),
        PayloadAccessState::Deleted,
        Confidence::new(1.0).unwrap(),
        id("assertion.fixture"),
        id("event.fixture"),
        UtcMicros(2),
    )
    .unwrap_err();
    assert!(matches!(error, FactStoreError::PayloadAccessMismatch));

    let fact_id = fact_id(FactOwnerV1::Profile, "operation.missing-payload");
    let error = StoredFactV1::new(
        fact_id,
        FactOwnerV1::Profile,
        None,
        PayloadAccessState::Eligible,
        Confidence::new(1.0).unwrap(),
        id("assertion.fixture"),
        id("event.fixture"),
        UtcMicros(2),
    )
    .unwrap_err();
    assert!(matches!(error, FactStoreError::PayloadAccessMismatch));
}

#[test]
fn available_projections_reject_redacted_payload_receipts() {
    let owner = FactOwnerV1::Profile;
    let source = FactIdentitySourceV1::Application {
        operation_id: id("operation.redacted-payload"),
    };
    let fact_id =
        FactId::derive(&FactIdentityMaterialV1::new(owner.clone(), source.clone()).unwrap())
            .unwrap();
    let material = json!({
        "content": "redacted payload",
        "category": "project",
        "tags": [],
        "entities": [],
        "metadata": {},
    });
    let payload = FactPayloadV1::new(
        "redacted payload".to_owned(),
        FactCategoryV1::Project,
        vec![],
        vec![],
        json!({}),
        None,
        receipt_for_disposition(&material, SanitizerDispositionV1::Redacted),
        RetentionClass::new("durable.fact").unwrap(),
    )
    .unwrap();

    let stored_error = StoredFactV1::new(
        fact_id.clone(),
        owner.clone(),
        Some(payload.clone()),
        PayloadAccessState::Eligible,
        Confidence::new(0.5).unwrap(),
        id("assertion.redacted-payload"),
        id("event.redacted-payload"),
        UtcMicros(2),
    )
    .unwrap_err();
    assert!(matches!(
        stored_error,
        FactStoreError::PayloadAccessMismatch
    ));

    let projection_error = ProjectMemoryFactV1::new(
        fact_id,
        owner,
        payload,
        Confidence::new(0.5).unwrap(),
        id("assertion.redacted-payload"),
        id("event.redacted-payload"),
        UtcMicros(2),
        source,
        ProjectMemoryFactTelemetryV1::new(0, 0, 0, 0, UtcMicros(1), UtcMicros(2), None, None, None)
            .unwrap(),
    )
    .unwrap_err();
    assert!(matches!(
        projection_error,
        FactStoreError::PayloadAccessMismatch
    ));
}

#[test]
fn available_projection_requires_one_snapshot_timestamp() {
    let owner = FactOwnerV1::Profile;
    let source = FactIdentitySourceV1::Application {
        operation_id: id("operation.projection-snapshot-mismatch"),
    };
    let fact_id =
        FactId::derive(&FactIdentityMaterialV1::new(owner.clone(), source.clone()).unwrap())
            .unwrap();
    let error = ProjectMemoryFactV1::new(
        fact_id,
        owner,
        payload(),
        Confidence::new(0.5).unwrap(),
        id("assertion.projection-snapshot-mismatch"),
        id("event.projection-snapshot-mismatch"),
        UtcMicros(2),
        source,
        ProjectMemoryFactTelemetryV1::new(0, 0, 0, 0, UtcMicros(1), UtcMicros(3), None, None, None)
            .unwrap(),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        FactStoreError::Contract(DomainError::NonCanonical {
            field: "fact projection snapshot"
        })
    ));
}

#[test]
fn inspection_requires_eligible_status_from_the_same_snapshot() {
    let fact = projected_fact(UtcMicros(2), UtcMicros(2));
    let history =
        ProjectMemoryFactHistoryV1::new(fact.owner().clone(), fact.fact_id().clone(), vec![], None)
            .unwrap();
    let deleted = ProjectMemoryFactStatusV1::new(
        fact.owner().clone(),
        fact.fact_id().clone(),
        PayloadAccessState::Deleted,
        UtcMicros(2),
    )
    .unwrap();
    let error = ProjectMemoryFactInspectionV1::new(fact.clone(), history.clone(), vec![], deleted)
        .unwrap_err();
    assert!(matches!(error, FactStoreError::PayloadAccessMismatch));

    let stale = ProjectMemoryFactStatusV1::new(
        fact.owner().clone(),
        fact.fact_id().clone(),
        PayloadAccessState::Eligible,
        UtcMicros(3),
    )
    .unwrap();
    let error = ProjectMemoryFactInspectionV1::new(fact, history, vec![], stale).unwrap_err();
    assert!(matches!(
        error,
        FactStoreError::Contract(DomainError::NonCanonical {
            field: "fact inspection snapshot"
        })
    ));
}

#[test]
fn feedback_history_available_requires_actual_details() {
    let error = ProjectMemoryFactFeedbackHistoryEntryV1::new(
        id("event.feedback-details"),
        UtcMicros(2),
        ProjectMemoryFactFeedbackActionV1::Helpful,
        Confidence::new(0.5).unwrap(),
        Confidence::new(0.55).unwrap(),
        None,
        None,
        ProjectMemoryFactFeedbackDetailsAvailabilityV1::Available,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        FactStoreError::Contract(DomainError::NonCanonical {
            field: "fact feedback details availability"
        })
    ));
}

#[test]
fn queries_enforce_bounds() {
    assert!(matches!(
        CurrentFactsQuery::new(FactOwnerV1::Profile, None, 0),
        Err(FactStoreError::InvalidQueryLimit { .. })
    ));
    let fact_id = fact_id(FactOwnerV1::Profile, "operation.query");
    assert!(matches!(
        FactLineageQuery::new(FactOwnerV1::Profile, fact_id, None, MAX_LINEAGE_LIMIT + 1,),
        Err(FactStoreError::InvalidQueryLimit { .. })
    ));
}

#[test]
fn positive_contradictions_are_bounded_in_the_public_constructor() {
    let mut contradicted_by = (0..=MAX_FACT_QUERY_CONTRADICTIONS)
        .map(|index| {
            fact_id(
                FactOwnerV1::Profile,
                &format!("operation.contradiction-{index}"),
            )
        })
        .collect::<Vec<_>>();
    contradicted_by.push(contradicted_by[0].clone());
    contradicted_by.reverse();

    let state = FactContradictionStateV1::from_positive(contradicted_by);

    assert_eq!(state.contradicted_by().len(), MAX_FACT_QUERY_CONTRADICTIONS);
    assert!(
        state
            .contradicted_by()
            .windows(2)
            .all(|ids| ids[0] < ids[1])
    );
}

#[test]
fn projections_queries_and_receipts_reject_cross_owner_fact_ids() {
    let profile_fact_id = fact_id(FactOwnerV1::Profile, "operation.cross-owner");
    let project_owner = FactOwnerV1::Project {
        project_id: id("project.other"),
    };

    assert!(matches!(
        StoredFactV1::new(
            profile_fact_id.clone(),
            project_owner.clone(),
            None,
            PayloadAccessState::Deleted,
            Confidence::new(1.0).unwrap(),
            id("assertion.fixture"),
            id("event.fixture"),
            UtcMicros(2),
        ),
        Err(FactStoreError::OwnerMismatch)
    ));
    assert!(matches!(
        CurrentFactsQuery::new(project_owner.clone(), Some(profile_fact_id.clone()), 10,),
        Err(FactStoreError::OwnerMismatch)
    ));
    assert!(matches!(
        FactCurrentQuery::new(project_owner.clone(), profile_fact_id.clone()),
        Err(FactStoreError::OwnerMismatch)
    ));
    assert!(matches!(
        FactAsOfQuery::new(project_owner.clone(), profile_fact_id.clone(), UtcMicros(2),),
        Err(FactStoreError::OwnerMismatch)
    ));
    assert!(matches!(
        FactLineageQuery::new(project_owner.clone(), profile_fact_id.clone(), None, 10,),
        Err(FactStoreError::OwnerMismatch)
    ));

    let event_id: FactEventId = id("event.fixture");
    assert!(matches!(
        FactCommitReceipt::new(
            profile_fact_id,
            project_owner,
            vec![event_id.clone()],
            event_id,
            None,
        ),
        Err(FactStoreError::OwnerMismatch)
    ));
}

#[test]
fn durable_memory_receipts_expose_infallible_stable_state_digests() {
    let owner = FactOwnerV1::Profile;
    let fact_id = fact_id(owner.clone(), "operation.commit-receipt-digest");
    let event_id: FactEventId = id("event.commit-receipt-digest");
    let commit = FactCommitReceipt::new(
        fact_id.clone(),
        owner.clone(),
        vec![event_id.clone()],
        event_id.clone(),
        None,
    )
    .unwrap();
    assert_eq!(
        commit.committed_state_digest(),
        &tracedecay_domain::canonical_sha256(&(
            "tracedecay.fact-commit-receipt.committed-state.v1",
            &fact_id,
            &owner,
            &[event_id.clone()],
            &event_id,
            Option::<&FactAssertionId>::None,
        ))
        .unwrap()
    );
    let decoded = serde_json::from_slice::<FactCommitReceipt>(
        &serde_json::to_vec(&commit).expect("serialize commit receipt"),
    )
    .expect("deserialize commit receipt");
    assert_eq!(
        decoded.committed_state_digest(),
        commit.committed_state_digest()
    );

    let target = ProjectMemoryFactIdV1::new(owner.clone(), fact_id.clone()).unwrap();
    let input_digest = "a".repeat(64);
    let operation_id: ProvenanceId = id("operation.retrieval-receipt-digest");
    let recorded = ProjectMemoryFactRetrievalReceiptV1::recorded(
        owner.clone(),
        operation_id.clone(),
        input_digest.clone(),
        vec![target.clone()],
        true,
    )
    .unwrap();
    let replayed = ProjectMemoryFactRetrievalReceiptV1::from_replay(
        owner.clone(),
        operation_id.clone(),
        input_digest.clone(),
        vec![target],
        true,
    )
    .unwrap();
    let expected = tracedecay_domain::canonical_sha256(&(
        "tracedecay.project-memory.fact-retrieval-receipt.committed-state.v1",
        &owner,
        &operation_id,
        &input_digest,
        vec![&fact_id],
        true,
    ))
    .unwrap();
    assert_eq!(recorded.committed_state_digest(), &expected);
    assert_eq!(replayed.committed_state_digest(), &expected);
    assert!(!recorded.replayed());
    assert!(replayed.replayed());
}

#[test]
fn automatic_fact_receipt_preserves_typed_automation_run_id() {
    let owner = FactOwnerV1::Profile;
    let material = serde_json::json!({
        "content": "durable automatic fact",
        "category": "decision",
        "tags": [],
        "entities": [],
        "metadata": {},
    });
    let request = ProjectMemoryFactAddMaterialV1::new(
        owner.clone(),
        "durable automatic fact".to_owned(),
        FactCategoryV1::Decision,
        None,
        vec![],
        vec![],
        serde_json::json!({}),
        receipt_for(&material),
        Some("run.fixture.1".to_owned()),
        Confidence::new(0.5).unwrap(),
        None,
    )
    .unwrap()
    .into_command(id("operation.automatic-fact"))
    .unwrap();
    let receipt = ProjectMemoryAutomaticFactReceiptV1::new(
        id("automatic-fact.automation.fixture"),
        owner,
        ProjectMemoryAutomaticFactStateV1::Quarantined,
        request,
        ProjectMemoryAutomaticFactEvidenceV1::default(),
        ProjectMemoryAutomaticFactEffectV1::Quarantined {
            reason: "privacy sanitizer declined the automatic apply".to_owned(),
        },
        UtcMicros(1),
    )
    .unwrap();

    assert_eq!(receipt.automation_run_id(), Some("run.fixture.1"));
    assert_eq!(receipt.request().actor(), None);
}

#[test]
fn automatic_fact_state_wire_contract_is_terminal_only() {
    for (state, expected) in [
        (ProjectMemoryAutomaticFactStateV1::Applied, "applied"),
        (
            ProjectMemoryAutomaticFactStateV1::Quarantined,
            "quarantined",
        ),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(expected));
        assert_eq!(
            serde_json::from_value::<ProjectMemoryAutomaticFactStateV1>(json!(expected)).unwrap(),
            state
        );
    }
    for retired in ["pending", "pending_approval", "applying", "rejected"] {
        assert!(
            serde_json::from_value::<ProjectMemoryAutomaticFactStateV1>(json!(retired)).is_err(),
            "retired state {retired:?} must not deserialize"
        );
    }
}

#[test]
fn automatic_fact_evidence_rejects_unknown_persisted_fields() {
    assert!(
        serde_json::from_value::<ProjectMemoryAutomaticFactEvidenceV1>(json!({
            "evidence_hash": "evidence.fixture",
            "unexpected": true,
        }))
        .is_err()
    );
}

#[test]
fn dashboard_queries_bound_the_finite_read_surface() {
    assert!(matches!(
        ProjectMemoryDashboardMemoryOverviewQueryV1::new(FactOwnerV1::Profile, 0, 1),
        Err(FactStoreError::InvalidQueryLimit { .. })
    ));
    assert!(matches!(
        ProjectMemoryDashboardVectorPointsQueryV1::new(
            FactOwnerV1::Profile,
            None,
            MAX_PROJECT_MEMORY_DASHBOARD_VECTORS + 1,
        ),
        Err(FactStoreError::InvalidQueryLimit { .. })
    ));
    assert!(matches!(
        ProjectMemoryDashboardOplogQueryV1::new(
            FactOwnerV1::Profile,
            MAX_PROJECT_MEMORY_DASHBOARD_OPLOG + 1,
        ),
        Err(FactStoreError::InvalidQueryLimit { .. })
    ));
}
