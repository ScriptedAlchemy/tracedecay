use super::*;
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorLineageRefV3, AnchorOwnerBindingV1,
    AnchorProvenanceRelationV2, AnchorSourceGenerationV3, CoverageReportV1,
    EvidenceAssemblyPublicationReceiptIdV1, EvidenceClass, ManifestDigest,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadAccessState,
    PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProjectionGenerationId,
    ProviderId, ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorId,
    RetrievalAnchorRecordV3, RetrievalAnchorRecordV3Parts, RetrievalAnchorTargetV3,
    SanitizationReceiptId, SanitizationReceiptRefV1, ScopeResolutionId, SessionId, UserProfileId,
    UtcMicros, VectorWatermark,
};
use tracedecay_store::{
    CanonicalSourceOccurrenceSetIdentityProjectionV1, CanonicalSourceOccurrenceSetRecordV1,
    EvidenceAssemblyIdempotencyKeyV1, EvidenceAssemblyOwnerV1,
    EvidenceAssemblyPublicationReceiptV1, EvidenceSourceOccurrenceRecordV1,
    EvidenceSourceTimelineV1, EvidenceSpanCatalogBindingV1, EvidenceSpanHorizonV1,
    EvidenceSpanIdentityProjectionV1, EvidenceSpanMemberReceiptBindingV1,
    EvidenceSpanProjectionReceiptIdentityProjectionV1, EvidenceSpanProjectionReceiptV1,
    EvidenceSpanRecordV1, EvidenceSpanRunV1, PrivacyBoundRequestDigestV1,
    PrivacyBoundRequestEnvelopeV1, RetrieverContributionIdentityProjectionV1,
    RetrieverContributionRecordV1, RetrieverIdentityV1, RetrieverWatermarkBindingV1,
    SanitizedObservationByteRangeV1, SourceCapabilityCatalogBindingV1,
    SourceOccurrenceCoordinateV1, SourceOccurrenceIdentityProjectionV1, SourceOccurrenceKindV1,
    SourceOccurrenceSanitizationV1, VerifiedSourceOrderingProofV1,
    derive_canonical_source_occurrence_set_id_v1,
    derive_evidence_assembly_publication_receipt_id_v1, derive_evidence_span_id_v1,
    derive_evidence_span_projection_receipt_id_v1, derive_retriever_contribution_id_v1,
    derive_source_occurrence_id_v1,
};
#[cfg(test)]
use tracedecay_store::{
    RetrievalAnchorReadOperationV1, RetrievalAnchorReadResultV1, StoredRetrievalAnchorRecordV1,
};

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn owner(project_id: ProjectId) -> EvidenceAssemblyOwnerV1 {
    EvidenceAssemblyOwnerV1 {
        owner: AnchorOwnerBindingV1::for_project(
            UserProfileId::new("profile.fixture").unwrap(),
            project_id,
            PrivacyDomainId::new("privacy.fixture").unwrap(),
        )
        .unwrap(),
        scope_digest: ManifestDigest::new(DIGEST).unwrap(),
        key_epoch: 1,
    }
}

fn timeline(project_id: ProjectId) -> EvidenceSourceTimelineV1 {
    EvidenceSourceTimelineV1 {
        source: ObservationSourceIdentityV1::for_provider(
            ProviderId::new("provider.fixture").unwrap(),
            SessionId::new("session.fixture").unwrap(),
        )
        .unwrap(),
        scope: ObservationScopeV1::Project { project_id },
        source_generation: ObservationSourceGenerationV1::new(1).unwrap(),
        ordering_domain: ObservationOrderingDomainV1::DaemonSequence,
    }
}

fn catalog_binding() -> SourceCapabilityCatalogBindingV1 {
    SourceCapabilityCatalogBindingV1 {
        connector_id: "connector.fixture".to_owned(),
        root_id: "root.fixture".to_owned(),
        capability_id: tracedecay_domain::CapabilityId::new("capability.fixture").unwrap(),
        catalog_digest: ManifestDigest::new(DIGEST).unwrap(),
        integration_manifest_digest: ManifestDigest::new(DIGEST).unwrap(),
        configuration_digest: ManifestDigest::new(DIGEST).unwrap(),
        authorization_scope_digest: ManifestDigest::new(DIGEST).unwrap(),
        projector_revision: tracedecay_domain::ComponentVersion::new("projector.fixture").unwrap(),
        source_watermark: ManifestDigest::new(DIGEST).unwrap(),
    }
}

fn sanitization() -> SourceOccurrenceSanitizationV1 {
    SourceOccurrenceSanitizationV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.capture.fixture").unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.fixture").unwrap(),
        )
        .unwrap(),
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.projection.fixture").unwrap(),
            tracedecay_domain::ComponentVersion::new("sanitizer.fixture").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn anchor(
    target: RetrievalAnchorTargetV3,
    owner: &EvidenceAssemblyOwnerV1,
    sources: Vec<RetrievalAnchorId>,
) -> RetrievalAnchorRecordV3 {
    let source_anchors = sources
        .into_iter()
        .enumerate()
        .map(|(ordinal, source)| {
            AnchorLineageRefV3::new(
                u64::try_from(ordinal).unwrap(),
                AnchorProvenanceRelationV2::DerivedFrom,
                source,
                owner.owner.clone(),
            )
            .unwrap()
        })
        .collect();
    RetrievalAnchorRecordV3::new(RetrievalAnchorRecordV3Parts {
        target,
        owner: owner.owner.clone(),
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(1),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV3::Unknown,
        projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors,
        authorization: ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
            privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
            access_policy_digest: AccessPolicyDigest::new(DIGEST).unwrap(),
            capability_id: tracedecay_domain::CapabilityId::new("capability.fixture").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST).unwrap(),
        },
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.fixture").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

pub fn write_fixture_for_project(
    component_version: &str,
    project_id: ProjectId,
) -> (RetrievalAnchorRecordV3, EvidenceAssemblyWriteV1) {
    let owner = owner(project_id.clone());
    let timeline = timeline(project_id);
    let source_anchor = anchor(
        RetrievalAnchorTargetV3::Entity(tracedecay_domain::EntityRef {
            id: tracedecay_domain::EntityId::new("entity.source.fixture".to_owned()).unwrap(),
            kind: tracedecay_domain::EntityKind::Document,
        }),
        &owner,
        Vec::new(),
    );
    let source = source_anchor.anchor_id().clone();
    let coordinate = SourceOccurrenceCoordinateV1::ObservationProjection {
        canonical_observation_id: tracedecay_domain::CanonicalObservationIdV1::new(format!(
            "sha256:{}",
            "33".repeat(32)
        ))
        .unwrap(),
        source_range: ObservationSourceRangeV1::new(7, 8).unwrap(),
        projection_output_ordinal: 0,
        sanitized_byte_range: SanitizedObservationByteRangeV1::new(0, 8).unwrap(),
    };
    let occurrence_id = derive_source_occurrence_id_v1(&SourceOccurrenceIdentityProjectionV1 {
        owner: owner.owner.clone(),
        timeline: timeline.clone(),
        exact_source_anchor: source.clone(),
        source_order: 7,
        coordinate: coordinate.clone(),
        occurrence_kind: SourceOccurrenceKindV1::Message,
        relations: Vec::new(),
        projector_version: tracedecay_domain::ComponentVersion::new("projector.fixture").unwrap(),
    })
    .unwrap();
    let occurrence_anchor = anchor(
        RetrievalAnchorTargetV3::ExactSourceOccurrence(occurrence_id.clone()),
        &owner,
        vec![source.clone()],
    );
    let occurrence = EvidenceSourceOccurrenceRecordV1 {
        occurrence_id: occurrence_id.clone(),
        owner: owner.owner.clone(),
        timeline,
        exact_source_anchor: source.clone(),
        occurrence_anchor: occurrence_anchor.clone(),
        source_order: 7,
        coordinate,
        occurrence_kind: SourceOccurrenceKindV1::Message,
        relations: Vec::new(),
        projector_version: tracedecay_domain::ComponentVersion::new("projector.fixture").unwrap(),
        sanitization: sanitization(),
        knowledge_time: UtcMicros(1),
        valid_time: Some(UtcMicros(1)),
    };
    let occurrence_set_id = derive_canonical_source_occurrence_set_id_v1(
        &CanonicalSourceOccurrenceSetIdentityProjectionV1 {
            owner: owner.owner.clone(),
            canonical_members: vec![occurrence_id.clone()],
        },
    )
    .unwrap();
    let run = EvidenceSpanRunV1 {
        assembly_ordinal: 0,
        timeline: occurrence.timeline.clone(),
        ordering_proof: VerifiedSourceOrderingProofV1::verify(
            occurrence.timeline.clone(),
            catalog_binding(),
            catalog_binding(),
            vec![occurrence_id.clone()],
            vec![7],
        )
        .unwrap(),
        timeline_digest: occurrence.timeline.digest().unwrap(),
        first_source_order: 7,
        last_source_order: 7,
        occurrence_ids: vec![occurrence_id.clone()],
    };
    let span_id = derive_evidence_span_id_v1(&EvidenceSpanIdentityProjectionV1 {
        owner: owner.owner.clone(),
        occurrence_set_id: occurrence_set_id.clone(),
        ordered_runs: vec![run.clone()],
        exact_source_anchors: vec![source.clone()],
        projector_version: tracedecay_domain::ComponentVersion::new("projector.fixture").unwrap(),
        horizon: EvidenceSpanHorizonV1 {
            knowledge_through: UtcMicros(1),
            valid_through: Some(UtcMicros(1)),
            contains_unknown_valid_time: false,
        },
        catalog_binding: EvidenceSpanCatalogBindingV1::SourceCapability {
            binding: catalog_binding(),
        },
    })
    .unwrap();
    let span_anchor = anchor(
        RetrievalAnchorTargetV3::ExactEvidenceSpan(span_id.clone()),
        &owner,
        vec![occurrence_anchor.anchor_id().clone()],
    );
    let span = EvidenceSpanRecordV1 {
        span_id: span_id.clone(),
        anchor: span_anchor.clone(),
        owner: owner.owner.clone(),
        occurrence_set_id: occurrence_set_id.clone(),
        runs: vec![run],
        exact_source_anchors: vec![source.clone()],
        projector_version: tracedecay_domain::ComponentVersion::new("projector.fixture").unwrap(),
        horizon: EvidenceSpanHorizonV1 {
            knowledge_through: UtcMicros(1),
            valid_through: Some(UtcMicros(1)),
            contains_unknown_valid_time: false,
        },
        catalog_binding: EvidenceSpanCatalogBindingV1::SourceCapability {
            binding: catalog_binding(),
        },
    };
    let member_receipts = vec![EvidenceSpanMemberReceiptBindingV1 {
        occurrence_id: occurrence_id.clone(),
        sanitization: sanitization(),
    }];
    let projection_receipt_id = derive_evidence_span_projection_receipt_id_v1(
        &EvidenceSpanProjectionReceiptIdentityProjectionV1 {
            span_id: span_id.clone(),
            projector_snapshot: "projector.snapshot.fixture".to_owned(),
            projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
            projection_watermark: VectorWatermark::default(),
            source_watermark: ManifestDigest::new(DIGEST).unwrap(),
            member_receipts: member_receipts.clone(),
            ordered_occurrence_ids: vec![occurrence_id.clone()],
            exact_source_anchors: vec![source.clone()],
        },
    )
    .unwrap();
    let horizon = EvidenceSpanHorizonV1 {
        knowledge_through: UtcMicros(1),
        valid_through: Some(UtcMicros(1)),
        contains_unknown_valid_time: false,
    };
    let request_digest = PrivacyBoundRequestDigestV1::derive(
        owner.owner.privacy_domain_id().clone(),
        owner.key_epoch,
        b"fixture-privacy-key",
        &PrivacyBoundRequestEnvelopeV1 {
            use_case_id: tracedecay_domain::UseCaseId::new("use-case.fixture").unwrap(),
            scope_resolution_id: ScopeResolutionId::new("scope.fixture").unwrap(),
            temporal_mode: tracedecay_domain::TemporalModeV1::Current,
            horizon: horizon.clone(),
            requested_capabilities: vec![
                tracedecay_domain::CapabilityId::new("capability.fixture").unwrap(),
            ],
        },
    )
    .unwrap();
    let retriever = RetrieverIdentityV1 {
        capability_id: tracedecay_domain::CapabilityId::new("capability.fixture").unwrap(),
        component_version: tracedecay_domain::ComponentVersion::new(component_version).unwrap(),
    };
    let watermarks = RetrieverWatermarkBindingV1 {
        source_watermark: ManifestDigest::new(DIGEST).unwrap(),
        projection_watermark: VectorWatermark::default(),
        index_watermark: None,
        summary_watermark: None,
    };
    let contribution_id =
        derive_retriever_contribution_id_v1(&RetrieverContributionIdentityProjectionV1 {
            owner: owner.clone(),
            retriever: retriever.clone(),
            catalog_binding: catalog_binding(),
            request_digest: request_digest.clone(),
            scope_resolution_id: ScopeResolutionId::new("scope.fixture").unwrap(),
            temporal_mode: tracedecay_domain::TemporalModeV1::Current,
            watermarks: watermarks.clone(),
            horizon: horizon.clone(),
            occurrence_set_id: occurrence_set_id.clone(),
            span_id: span_id.clone(),
            span_anchor_id: span_anchor.anchor_id().clone(),
            exact_source_anchors: vec![source.clone()],
            coverage: CoverageReportV1::default(),
        })
        .unwrap();
    let contribution_anchor = anchor(
        RetrievalAnchorTargetV3::RetrieverContribution(contribution_id.clone()),
        &owner,
        vec![span_anchor.anchor_id().clone()],
    );
    let mut write = EvidenceAssemblyWriteV1 {
        owner: owner.clone(),
        idempotency_key: EvidenceAssemblyIdempotencyKeyV1::new(
            ManifestDigest::new(format!("sha256:{}", "cc".repeat(32))).unwrap(),
        )
        .unwrap(),
        occurrences: vec![occurrence],
        occurrence_set: CanonicalSourceOccurrenceSetRecordV1 {
            occurrence_set_id: occurrence_set_id.clone(),
            owner: owner.owner.clone(),
            members: vec![occurrence_id.clone()],
        },
        span,
        projection_receipt: EvidenceSpanProjectionReceiptV1 {
            projection_receipt_id: projection_receipt_id.clone(),
            span_id: span_id.clone(),
            projector_snapshot: "projector.snapshot.fixture".to_owned(),
            projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
            projection_watermark: VectorWatermark::default(),
            source_watermark: ManifestDigest::new(DIGEST).unwrap(),
            member_receipts,
            ordered_occurrence_ids: vec![occurrence_id.clone()],
            exact_source_anchors: vec![source.clone()],
        },
        contribution: RetrieverContributionRecordV1 {
            contribution_id: contribution_id.clone(),
            anchor: contribution_anchor.clone(),
            owner: owner.clone(),
            retriever,
            catalog_binding: catalog_binding(),
            request_digest,
            scope_resolution_id: ScopeResolutionId::new("scope.fixture").unwrap(),
            temporal_mode: tracedecay_domain::TemporalModeV1::Current,
            watermarks,
            horizon,
            occurrence_set_id: occurrence_set_id.clone(),
            span_id: span_id.clone(),
            span_anchor_id: span_anchor.anchor_id().clone(),
            exact_source_anchors: vec![source.clone()],
            coverage: CoverageReportV1::default(),
            created_at: UtcMicros(2),
        },
        receipt: EvidenceAssemblyPublicationReceiptV1 {
            publication_receipt_id: EvidenceAssemblyPublicationReceiptIdV1::new(
                "publication.fixture",
            )
            .unwrap(),
            owner,
            assembly_digest: ManifestDigest::new(DIGEST).unwrap(),
            occurrence_set_id,
            span_id,
            span_anchor_id: span_anchor.anchor_id().clone(),
            contribution_id,
            contribution_anchor_id: contribution_anchor.anchor_id().clone(),
            projection_receipt_id,
            ordered_occurrence_ids: vec![occurrence_id],
            exact_source_anchors: vec![source],
        },
    };
    write.receipt.assembly_digest = write.compute_assembly_digest().unwrap();
    write.receipt.publication_receipt_id = derive_evidence_assembly_publication_receipt_id_v1(
        &write
            .receipt
            .identity_projection(write.idempotency_key.clone()),
    )
    .unwrap();
    write.validate().unwrap();
    (source_anchor, write)
}

#[cfg(test)]
pub(crate) fn write_fixture(component_version: &str) -> EvidenceAssemblyWriteV1 {
    write_fixture_for_project(
        component_version,
        ProjectId::new("project.fixture").unwrap(),
    )
    .1
}

#[cfg(test)]
fn install(connection: &rusqlite::Connection) {
    // The anchors table is installed from the canonical production DDL, not
    // a relaxed local copy: this executor writes anchors in production, so
    // a fixture without the real CHECK and UNIQUE clauses would accept rows
    // the live table rejects.
    connection
        .execute_batch(tracedecay_store::RETRIEVAL_ANCHORS_SCHEMA_DDL)
        .unwrap();
    connection
            .execute_batch(
                "CREATE TABLE retrieval_anchor_dispositions (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT, disposition_id TEXT UNIQUE,
                    anchor_id TEXT, owner_json TEXT, state TEXT, superseded_by TEXT,
                    reason_class TEXT, effective_at INTEGER, record_json TEXT
                 );
                 CREATE TABLE retrieval_anchor_reverse_lineage (
                    source_anchor_id TEXT, owner_json TEXT, derivative_kind TEXT,
                    derivative_id TEXT, direct_evidence INTEGER,
                    PRIMARY KEY(source_anchor_id, owner_json, derivative_kind, derivative_id)
                 );
                 CREATE TABLE evidence_source_occurrences (
                    occurrence_id TEXT PRIMARY KEY, owner_digest TEXT, timeline_digest TEXT,
                    source_anchor_id TEXT, source_order INTEGER, record_digest TEXT, record_json TEXT
                 );
                 CREATE TABLE evidence_occurrence_sets (
                    occurrence_set_id TEXT PRIMARY KEY, owner_digest TEXT,
                    record_digest TEXT, record_json TEXT
                 );
                 CREATE TABLE evidence_occurrence_set_members (
                    occurrence_set_id TEXT, canonical_ordinal INTEGER, occurrence_id TEXT,
                    PRIMARY KEY(occurrence_set_id, canonical_ordinal)
                 );
                 CREATE TABLE evidence_spans (
                    span_id TEXT PRIMARY KEY, owner_digest TEXT, occurrence_set_id TEXT,
                    anchor_id TEXT, producer_kind TEXT, record_digest TEXT, record_json TEXT
                 );
                 CREATE TABLE evidence_span_members (
                    span_id TEXT, assembly_ordinal INTEGER, run_ordinal INTEGER,
                    run_member_ordinal INTEGER, occurrence_id TEXT,
                    PRIMARY KEY(span_id, assembly_ordinal)
                 );
                 CREATE TABLE evidence_span_projection_receipts (
                    projection_receipt_id TEXT PRIMARY KEY, span_id TEXT,
                    record_digest TEXT, record_json TEXT
                 );
                 CREATE TABLE evidence_retriever_contributions (
                    contribution_id TEXT PRIMARY KEY, owner_digest TEXT, span_id TEXT,
                    anchor_id TEXT, record_digest TEXT, record_json TEXT
                 );
                 CREATE TABLE evidence_derived_anchors (
                    anchor_id TEXT PRIMARY KEY, owner_digest TEXT, target_kind TEXT,
                    target_id TEXT, anchor_json TEXT
                 );
                 CREATE TABLE evidence_assembly_receipts (
                    publication_receipt_id TEXT PRIMARY KEY, owner_digest TEXT,
                    privacy_domain_id TEXT, key_epoch INTEGER, idempotency_key TEXT,
                    assembly_digest TEXT, occurrence_set_id TEXT, span_id TEXT,
                    contribution_id TEXT, projection_receipt_id TEXT, receipt_json TEXT,
                    UNIQUE(owner_digest, privacy_domain_id, key_epoch, idempotency_key)
                 );",
            )
            .unwrap();
}

#[cfg(test)]
fn evidence_table_counts(connection: &rusqlite::Connection) -> Vec<i64> {
    [
        "retrieval_anchors",
        "retrieval_anchor_reverse_lineage",
        "evidence_source_occurrences",
        "evidence_occurrence_sets",
        "evidence_occurrence_set_members",
        "evidence_spans",
        "evidence_span_members",
        "evidence_span_projection_receipts",
        "evidence_retriever_contributions",
        "evidence_derived_anchors",
        "evidence_assembly_receipts",
    ]
    .into_iter()
    .map(|table| {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    })
    .collect()
}

#[test]
fn publish_replay_conflict_and_drilldown_are_atomic() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    install(&connection);
    let write = write_fixture("1");
    connection
        .execute(
            "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES (?1, '{}', ?2, 'source.fixture')",
            params![
                write.occurrences[0].exact_source_anchor.as_str(),
                encode(&write.owner.owner).unwrap(),
            ],
        )
        .unwrap();
    let mut executor = EvidenceAssemblyExecutor;
    for _ in 0..2 {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        executor.execute_write(&savepoint, &write).unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    let snapshot = connection.transaction().unwrap();
    let page = executor
        .execute_read(
            &snapshot,
            &EvidenceAssemblyReadOperationV1::ContributionPage {
                owner: write.owner.clone(),
                contribution_id: write.contribution.contribution_id.clone(),
                start_ordinal: 0,
                page_size: 1,
            },
        )
        .unwrap();
    assert!(matches!(
        page,
        EvidenceAssemblyReadResultV1::ContributionPage(Some(ref page))
            if page.occurrences.len() == 1 && page.next_ordinal.is_none()
    ));
    let mut wrong_owner = write.owner.clone();
    wrong_owner.scope_digest = ManifestDigest::new(format!("sha256:{}", "bb".repeat(32))).unwrap();
    assert_eq!(
        executor
            .execute_read(
                &snapshot,
                &EvidenceAssemblyReadOperationV1::ContributionPage {
                    owner: wrong_owner,
                    contribution_id: write.contribution.contribution_id.clone(),
                    start_ordinal: 0,
                    page_size: 1,
                },
            )
            .unwrap(),
        EvidenceAssemblyReadResultV1::ContributionPage(None)
    );
    let mut anchor_executor = super::super::RetrievalAnchorExecutor;
    assert!(matches!(
        anchor_executor
            .execute_read(
                &snapshot,
                &RetrievalAnchorReadOperationV1::AnchorById {
                    anchor_id: write.contribution.anchor.anchor_id().clone(),
                    owner: write.owner.owner.clone().into(),
                },
            )
            .unwrap(),
        RetrievalAnchorReadResultV1::Anchor(Some(StoredRetrievalAnchorRecordV1::V3(record)))
            if record == write.contribution.anchor
    ));
    snapshot.commit().unwrap();
    connection
        .execute(
            "UPDATE retrieval_anchors SET projection_generation = 'tampered'
                 WHERE anchor_id = ?1",
            [write.contribution.anchor.anchor_id().as_str()],
        )
        .unwrap();
    let snapshot = connection.transaction().unwrap();
    assert!(
        anchor_executor
            .execute_read(
                &snapshot,
                &RetrievalAnchorReadOperationV1::AnchorById {
                    anchor_id: write.contribution.anchor.anchor_id().clone(),
                    owner: write.owner.owner.clone().into(),
                },
            )
            .is_err()
    );
    snapshot.commit().unwrap();

    let counts_before_conflict = evidence_table_counts(&connection);
    let conflict = write_fixture("2");
    let mut transaction = connection.transaction().unwrap();
    {
        let mut savepoint = transaction.savepoint().unwrap();
        assert!(executor.execute_write(&savepoint, &conflict).is_err());
        savepoint.rollback().unwrap();
    }
    transaction.rollback().unwrap();
    assert_eq!(
        evidence_table_counts(&connection),
        counts_before_conflict,
        "a replay conflict must not partially mutate any evidence table"
    );
}

#[test]
fn canonical_identity_validation_rejects_tampered_material() {
    let write = write_fixture("1");
    let replay = write_fixture("1");
    assert_eq!(write, replay);

    let changed = write_fixture("2");
    assert_ne!(
        write.contribution.contribution_id,
        changed.contribution.contribution_id
    );
    assert_ne!(
        write.receipt.publication_receipt_id,
        changed.receipt.publication_receipt_id
    );

    let mut tampered = write;
    tampered.contribution.retriever.component_version =
        tracedecay_domain::ComponentVersion::new("2").unwrap();
    assert!(tampered.validate().is_err());
}

#[test]
fn typed_catalog_order_horizon_privacy_watermark_and_owner_tampering_is_rejected() {
    let baseline = write_fixture("1");

    let mut catalog = baseline.clone();
    catalog.span.runs[0]
        .ordering_proof
        .catalog_binding
        .catalog_digest = ManifestDigest::new(format!("sha256:{}", "ab".repeat(32))).unwrap();
    assert!(catalog.validate().is_err());

    let mut ordering = baseline.clone();
    ordering.span.runs[0].ordering_proof.source_orders[0] = 8;
    assert!(ordering.validate().is_err());

    let mut horizon = baseline.clone();
    horizon.span.horizon.knowledge_through = UtcMicros(0);
    assert!(matches!(
        horizon.span.horizon.validate_members(&horizon.occurrences),
        Err(tracedecay_store::EvidenceAssemblyStoreError::HorizonMismatch)
    ));
    assert!(horizon.validate().is_err());

    let mut privacy = baseline.clone();
    privacy.contribution.request_digest.key_epoch =
        privacy.contribution.owner.key_epoch.saturating_add(1);
    assert!(matches!(
        privacy.validate(),
        Err(tracedecay_store::EvidenceAssemblyStoreError::RequestPrivacyBindingMismatch)
    ));

    let mut watermark = baseline.clone();
    watermark.contribution.watermarks.source_watermark =
        ManifestDigest::new(format!("sha256:{}", "bc".repeat(32))).unwrap();
    assert!(watermark.validate().is_err());

    let mut owner = baseline;
    owner.occurrences[0].owner = AnchorOwnerBindingV1::for_project(
        UserProfileId::new("profile.fixture").unwrap(),
        ProjectId::new("project.other").unwrap(),
        PrivacyDomainId::new("privacy.fixture").unwrap(),
    )
    .unwrap();
    assert!(owner.validate().is_err());
}

#[test]
fn drilldown_and_receipt_reads_reject_physical_index_tampering() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    install(&connection);
    let write = write_fixture("1");
    connection
        .execute(
            "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES (?1, '{}', ?2, 'source.fixture')",
            params![
                write.occurrences[0].exact_source_anchor.as_str(),
                encode(&write.owner.owner).unwrap(),
            ],
        )
        .unwrap();
    let mut executor = EvidenceAssemblyExecutor;
    let mut transaction = connection.transaction().unwrap();
    let savepoint = transaction.savepoint().unwrap();
    executor.execute_write(&savepoint, &write).unwrap();
    savepoint.commit().unwrap();
    transaction.commit().unwrap();

    connection
        .execute(
            "UPDATE evidence_occurrence_set_members
                 SET canonical_ordinal = 7 WHERE occurrence_set_id = ?1",
            [write.occurrence_set.occurrence_set_id.as_str()],
        )
        .unwrap();
    let snapshot = connection.transaction().unwrap();
    assert!(
        executor
            .execute_read(
                &snapshot,
                &EvidenceAssemblyReadOperationV1::ContributionPage {
                    owner: write.owner.clone(),
                    contribution_id: write.contribution.contribution_id.clone(),
                    start_ordinal: 0,
                    page_size: 1,
                },
            )
            .is_err()
    );
    snapshot.commit().unwrap();

    connection
        .execute(
            "UPDATE evidence_assembly_receipts
                 SET span_id = 'span.tampered' WHERE publication_receipt_id = ?1",
            [write.receipt.publication_receipt_id.as_str()],
        )
        .unwrap();
    let snapshot = connection.transaction().unwrap();
    assert!(
        executor
            .execute_read(
                &snapshot,
                &EvidenceAssemblyReadOperationV1::PublicationByIdempotency {
                    owner: write.owner.clone(),
                    idempotency_key: write.idempotency_key.clone(),
                },
            )
            .is_err()
    );
}

#[test]
fn publication_rejects_cross_project_source_anchor_without_partial_rows() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    install(&connection);
    connection
        .execute(
            "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES ('retrieval.source.fixture', '{}', ?1, 'source.fixture')",
            [encode(
                &AnchorOwnerBindingV1::for_project(
                    UserProfileId::new("profile.fixture").unwrap(),
                    ProjectId::new("project.other").unwrap(),
                    PrivacyDomainId::new("privacy.fixture").unwrap(),
                )
                .unwrap(),
            )
            .unwrap()],
        )
        .unwrap();
    let write = write_fixture("1");
    let mut transaction = connection.transaction().unwrap();
    {
        let mut savepoint = transaction.savepoint().unwrap();
        assert!(
            EvidenceAssemblyExecutor
                .execute_write(&savepoint, &write)
                .is_err()
        );
        savepoint.rollback().unwrap();
    }
    transaction.rollback().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM evidence_assembly_receipts",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM evidence_source_occurrences",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn publication_rejects_unresolved_v2_owner_without_partial_rows() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    install(&connection);
    connection
        .execute(
            "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES ('retrieval.source.fixture', '{}', ?1, 'source.fixture')",
            [encode(&tracedecay_domain::FactOwnerV1::Project {
                project_id: ProjectId::new("project.fixture").unwrap(),
            })
            .unwrap()],
        )
        .unwrap();
    let write = write_fixture("1");
    let mut transaction = connection.transaction().unwrap();
    {
        let mut savepoint = transaction.savepoint().unwrap();
        assert!(
            EvidenceAssemblyExecutor
                .execute_write(&savepoint, &write)
                .is_err()
        );
        savepoint.rollback().unwrap();
    }
    transaction.rollback().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM evidence_assembly_receipts",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM evidence_source_occurrences",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[test]
fn batched_anchor_liveness_matches_row_at_a_time() {
    use std::collections::BTreeSet;

    fn dispose(
        connection: &rusqlite::Connection,
        anchor_id: &str,
        owner_json: &str,
        disposition_id: &str,
        state: &str,
    ) {
        connection
            .execute(
                "INSERT INTO retrieval_anchor_dispositions
                    (disposition_id, anchor_id, owner_json, state)
                 VALUES (?1, ?2, ?3, ?4)",
                params![disposition_id, anchor_id, owner_json, state],
            )
            .unwrap();
    }

    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    install(&connection);
    let write = write_fixture("1");
    let owner_json = encode(&write.owner.owner).unwrap();
    connection
        .execute(
            "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES (?1, '{}', ?2, 'source.fixture')",
            params![
                write.occurrences[0].exact_source_anchor.as_str(),
                owner_json
            ],
        )
        .unwrap();
    {
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        EvidenceAssemblyExecutor
            .execute_write(&savepoint, &write)
            .unwrap();
        savepoint.commit().unwrap();
        transaction.commit().unwrap();
    }
    let occurrence = write.occurrences[0].clone();

    // Compares the batched cache against the row-at-a-time free functions it
    // replaced, asserting they agree, and hands back the shared outcome.
    let compare = |connection: &rusqlite::Connection| {
        let mut anchor_ids = BTreeSet::new();
        anchor_ids.insert(occurrence.occurrence_anchor.anchor_id().as_str().to_owned());
        anchor_ids.insert(occurrence.exact_source_anchor.as_str().to_owned());
        let cache = super::anchor_state::load_anchor_liveness(connection, &anchor_ids).unwrap();

        let free_current = super::anchor_state::evidence_anchor_is_current(
            connection,
            &occurrence.occurrence_anchor,
        )
        .map_err(|error| error.to_string());
        let cached_current = cache
            .evidence_anchor_is_current(&occurrence.occurrence_anchor)
            .map_err(|error| error.to_string());
        assert_eq!(free_current, cached_current);

        let free_source =
            super::anchor_state::require_source_anchor_current(connection, &occurrence)
                .map(|_| ())
                .map_err(|error| error.to_string());
        let cached_source = cache
            .require_source_anchor_current(&occurrence)
            .map_err(|error| error.to_string());
        assert_eq!(free_source, cached_source);

        (free_current, free_source)
    };

    // Active: both anchors resolve as current.
    assert_eq!(compare(&connection), (Ok(true), Ok(())));

    // A disposed occurrence anchor makes the drilldown page read as absent.
    dispose(
        &connection,
        occurrence.occurrence_anchor.anchor_id().as_str(),
        &owner_json,
        "disposition.occurrence.revoked",
        "revoked",
    );
    assert_eq!(compare(&connection).0, Ok(false));

    // A newer active disposition supersedes the revocation (latest by
    // sequence), while a disposed source anchor is rejected.
    dispose(
        &connection,
        occurrence.occurrence_anchor.anchor_id().as_str(),
        &owner_json,
        "disposition.occurrence.reactivated",
        "active",
    );
    dispose(
        &connection,
        occurrence.exact_source_anchor.as_str(),
        &owner_json,
        "disposition.source.revoked",
        "revoked",
    );
    let (current, source) = compare(&connection);
    assert_eq!(current, Ok(true));
    assert!(source.is_err());
}

#[test]
fn reverse_lineage_reuses_source_owner_json() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    install(&connection);
    let write = write_fixture("1");
    let owner_json = encode(&write.owner.owner).unwrap();
    connection
        .execute(
            "INSERT INTO retrieval_anchors (
                    anchor_id, anchor_json, owner_json, projection_generation
                 ) VALUES (?1, '{}', ?2, 'source.fixture')",
            params![
                write.occurrences[0].exact_source_anchor.as_str(),
                owner_json
            ],
        )
        .unwrap();
    let mut transaction = connection.transaction().unwrap();
    let savepoint = transaction.savepoint().unwrap();
    EvidenceAssemblyExecutor
        .execute_write(&savepoint, &write)
        .unwrap();
    savepoint.commit().unwrap();
    transaction.commit().unwrap();

    let source_anchor_id = write.occurrences[0].exact_source_anchor.as_str().to_owned();
    let stored_owner_json: String = connection
        .query_row(
            "SELECT owner_json FROM retrieval_anchors WHERE anchor_id = ?1",
            [source_anchor_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    let lineage_owner_jsons: Vec<String> = connection
        .prepare(
            "SELECT owner_json FROM retrieval_anchor_reverse_lineage
             WHERE source_anchor_id = ?1",
        )
        .unwrap()
        .query_map([source_anchor_id.as_str()], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        lineage_owner_jsons.len(),
        2,
        "one reverse-lineage row per derivative kind (span, contribution)"
    );
    assert!(
        lineage_owner_jsons
            .iter()
            .all(|json| *json == stored_owner_json),
        "threaded owner_json must equal the source anchor's stored owner_json"
    );
}
