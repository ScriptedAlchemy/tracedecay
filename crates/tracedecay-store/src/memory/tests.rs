use serde_json::json;
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorLineageRefV2, AnchorProvenanceRelationV2,
    AnchorSourceGenerationV2, CapabilityId, ComponentVersion, CoverageReportV1, EntityId,
    EntityKind, EntityRef, EvidenceClass, FactAssertionKindV1, FactCategoryV1, FactEvidenceRefV1,
    FactEvidenceRelationV1, FactIdentityMaterialV1, FactIdentitySourceV1, ObservationScopeV1,
    PayloadReferenceV1, PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectionGenerationId,
    ProvenanceId, ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorRecordV2Parts,
    RetrievalAnchorTargetV2, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, ScopeResolutionId, SensitivityV1,
    VectorWatermark,
};

use super::*;

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

fn receipt_for(material: &serde_json::Value) -> SanitizationReceiptV1 {
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            id::<SanitizationReceiptId>("receipt.fact.store.fixture"),
            id::<ComponentVersion>("sanitizer.fixture.v1"),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
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
        None,
    )
    .unwrap_err();
    assert!(matches!(error, FactStoreError::DuplicateEventId { .. }));
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

    FactWriteBatch::new(
        fact_id,
        owner,
        None,
        events,
        new_anchors,
        vec![],
        None,
        None,
    )
    .unwrap();
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
        None,
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
        None,
        UtcMicros(2),
    )
    .unwrap_err();
    assert!(matches!(error, FactStoreError::PayloadAccessMismatch));
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
    assert!(matches!(
        LegacyFactQuery::new(FactOwnerV1::Profile, id("store.v1"), 0),
        Err(FactStoreError::InvalidLegacyFactId { .. })
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
            None,
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

    let legacy = LegacyFactQuery::new(project_owner.clone(), id("store.v1"), 7).unwrap();
    assert!(matches!(
        legacy.validate_resolved_fact_id(&profile_fact_id),
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
fn proposal_record_projects_typed_automation_run_id() {
    let owner = FactOwnerV1::Profile;
    let material = serde_json::json!({
        "content": "durable proposal",
        "category": "decision",
        "tags": [],
        "entities": [],
        "metadata": {},
    });
    let request = ProjectMemoryFactAddCommandV1::new(
        owner.clone(),
        id("operation.automation-proposal"),
        "durable proposal".to_owned(),
        FactCategoryV1::Decision,
        None,
        vec![],
        vec![],
        serde_json::json!({}),
        receipt_for(&material),
        Confidence::new(0.5).unwrap(),
        None,
    )
    .unwrap()
    .with_automation_run_id("run.fixture.1".to_owned())
    .unwrap();
    let record = ProjectMemoryFactProposalRecordV1::new(
        id("proposal.automation.fixture"),
        owner,
        ProjectMemoryFactProposalRevisionV1::new(1).unwrap(),
        ProjectMemoryFactProposalStateV1::PendingApproval,
        request,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    assert_eq!(record.automation_run_id(), Some("run.fixture.1"));
}

#[test]
fn repair_stats_preserve_the_atomic_feedback_batch_outcome() {
    let stats = ProjectMemoryMemoryRepairStatsV1::new(3, 2).with_feedback_history_repair(
        ProjectMemoryFeedbackRepairProgressV1::Incomplete {
            processed: 512,
            remaining: Some(9),
        },
    );

    assert_eq!(stats.missing_vectors_repaired(), 3);
    assert_eq!(stats.banks_rebuilt(), 2);
    assert_eq!(
        stats.feedback_history_repair(),
        ProjectMemoryFeedbackRepairProgressV1::Incomplete {
            processed: 512,
            remaining: Some(9),
        }
    );
    assert_eq!(
        ProjectMemoryMemoryRepairStatsV1::default().feedback_history_repair(),
        ProjectMemoryFeedbackRepairProgressV1::Unknown
    );
    // Saturation defaults off and round-trips through the builder without
    // disturbing the feedback-history outcome.
    assert!(!stats.saturated());
    assert!(!ProjectMemoryMemoryRepairStatsV1::default().saturated());
    assert!(stats.with_saturated(true).saturated());
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
