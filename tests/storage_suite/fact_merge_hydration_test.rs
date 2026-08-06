#![allow(clippy::too_many_arguments, clippy::cloned_ref_to_slice_refs)] // test builders
//! Store-level fact merge/hydration integration tests against a real database.
//!
//! NEXT.md requires provenance preservation, contradiction, supersession,
//! as-of knowledge, denied payloads, redacted frontiers, and unknown
//! denominators to be exercised through fact merge and hydration — end to end
//! through the real [`DatabaseFactStore`] over a real sqlite file, not domain
//! unit tests or mock authorities.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::json;
use tempfile::TempDir;
use tracedecay::db::Database;
use tracedecay::store::memory::DatabaseFactStore;
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId,
    ComponentVersion, Confidence, CoverageReportV1, CoverageUniverseKnowledgeV1, DomainError,
    EntityId, EntityKind, EntityRef, EvidenceClass, FactAssertionId, FactAssertionKindV1,
    FactAssertionV1, FactCategoryV1, FactCurationActionV1, FactEventId, FactEvidenceRefV1,
    FactEvidenceRelationV1, FactId, FactIdentityMaterialV1, FactIdentitySourceV1,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, LegacyFactMappingV1,
    LegacyHistoryCoverageV1, ObservationScopeV1, PayloadAccessState, PayloadReferenceV1,
    PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectionGenerationId,
    ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2,
    RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, ScopeResolutionId,
    SensitivityV1, ShardDispositionV1, ShardId, SourceStoreId, UtcMicros, VectorWatermark,
};
use tracedecay_store::{
    CurrentFactsQuery, FactAsOfQuery, FactCommitConflict, FactCommitOutcome, FactCommitReceipt,
    FactContradictionStateV1, FactCurrentQuery, FactLineageQuery, FactStore, FactStoreError,
    FactWriteBatch, LegacyFactQuery, MAX_FACT_QUERY_CONTRADICTIONS, RetrievalAnchorQuery,
    StoredFactV1,
};

struct TestDb {
    db: Database,
    _dir: TempDir,
}

async fn setup_db() -> TestDb {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    crate::support::seed_latest_graph_db(&db_path).await;
    let (db, migrated) = crate::common::open_test_database(&db_path)
        .await
        .expect("failed to open template database");
    assert!(
        !migrated,
        "fresh test database should not require migration"
    );
    TestDb { db, _dir: dir }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String, Error = DomainError>,
{
    T::try_from(value.to_owned()).unwrap()
}

fn sqlite_text_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn application_identity(owner: &FactOwnerV1, operation: &str) -> FactIdentityMaterialV1 {
    FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: id(operation),
        },
    )
    .unwrap()
}

fn payload(content: &str, receipt_id: &str) -> FactPayloadV1 {
    let material = json!({
        "content": content,
        "category": "project",
        "tags": ["fmh"],
        "entities": ["TraceDecay"],
        "metadata": {},
    });
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            id::<SanitizationReceiptId>(receipt_id),
            id::<ComponentVersion>("sanitizer.fmh.v1"),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&material).unwrap()),
    )
    .unwrap();
    FactPayloadV1::new(
        content.to_owned(),
        FactCategoryV1::Project,
        vec!["fmh".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({}),
        receipt,
        RetentionClass::new("durable.fmh").unwrap(),
    )
    .unwrap()
}

fn anchor(
    scope: ObservationScopeV1,
    entity: &str,
    privacy_domain: &str,
    payload_access: PayloadAccessState,
    coverage: CoverageReportV1,
) -> RetrievalAnchorRecordV2 {
    const POLICY_DIGEST: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const REQUEST_DIGEST: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::Entity(EntityRef {
            id: EntityId::new(entity).unwrap(),
            kind: EntityKind::Document,
        }),
        owner: scope,
        aliases: Vec::new(),
        occurred_at: None,
        ingested_at: UtcMicros(1),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::Unknown,
        projection_generation: ProjectionGenerationId::new("projection.fmh").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage,
        source_observations: Vec::new(),
        source_anchors: Vec::new(),
        authorization: ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.fmh").unwrap(),
            privacy_domain_id: PrivacyDomainId::new(privacy_domain).unwrap(),
            access_policy_digest: AccessPolicyDigest::new(POLICY_DIGEST).unwrap(),
            capability_id: CapabilityId::new("capability.fmh").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(REQUEST_DIGEST).unwrap(),
        },
        payload_access,
        retention_class: RetentionClass::new("retention.fmh").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

fn known_coverage(dispositions: BTreeMap<ShardId, ShardDispositionV1>) -> CoverageReportV1 {
    CoverageReportV1 {
        dispositions,
        freshness: BTreeMap::new(),
        retention_watermark: None,
        universe: CoverageUniverseKnowledgeV1::Known,
        remote: None,
    }
}

fn evidence(
    fact_id: &FactId,
    anchor_id: &RetrievalAnchorId,
    relation: FactEvidenceRelationV1,
) -> FactEvidenceRefV1 {
    FactEvidenceRefV1::new(
        fact_id.clone(),
        anchor_id.clone(),
        relation,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap()
}

fn recorded_event(
    fact_id: &FactId,
    owner: &FactOwnerV1,
    assertion_id: &FactAssertionId,
    occurred_at: i64,
) -> FactLineageEventV1 {
    FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion_id.clone(),
        },
        UtcMicros(occurred_at),
        None,
    )
    .unwrap()
}

async fn commit(store: &DatabaseFactStore<'_>, batch: FactWriteBatch) -> FactCommitReceipt {
    match store.commit_fact(batch).await.unwrap() {
        FactCommitOutcome::Committed(receipt) => receipt,
        other => panic!("expected a committed outcome, got {other:?}"),
    }
}

async fn current(
    store: &DatabaseFactStore<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> Option<StoredFactV1> {
    store
        .query_fact_current(FactCurrentQuery::new(owner.clone(), fact_id.clone()).unwrap())
        .await
        .unwrap()
}

async fn as_of(
    store: &DatabaseFactStore<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    at: i64,
) -> Option<StoredFactV1> {
    store
        .query_fact_as_of(
            FactAsOfQuery::new(owner.clone(), fact_id.clone(), UtcMicros(at)).unwrap(),
        )
        .await
        .unwrap()
}

async fn lineage(
    store: &DatabaseFactStore<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> Vec<FactLineageEventV1> {
    store
        .query_fact_lineage(
            FactLineageQuery::new(owner.clone(), fact_id.clone(), None, 100).unwrap(),
        )
        .await
        .unwrap()
}

async fn current_page(store: &DatabaseFactStore<'_>, owner: &FactOwnerV1) -> Vec<StoredFactV1> {
    store
        .query_current_facts(CurrentFactsQuery::new(owner.clone(), None, 100).unwrap())
        .await
        .unwrap()
}

async fn anchor_record(
    store: &DatabaseFactStore<'_>,
    owner: &FactOwnerV1,
    anchor_id: &RetrievalAnchorId,
) -> RetrievalAnchorRecordV2 {
    store
        .get_retrieval_anchor(RetrievalAnchorQuery::new(owner.clone(), anchor_id.clone()).unwrap())
        .await
        .unwrap()
        .expect("retrieval anchor should be retained")
}

struct InitialFact {
    fact_id: FactId,
    anchor: RetrievalAnchorRecordV2,
    assertion: FactAssertionV1,
    receipt: FactCommitReceipt,
}

/// Commits a new fact with one initial assertion backed by one new anchor.
async fn commit_initial(
    store: &DatabaseFactStore<'_>,
    owner: &FactOwnerV1,
    operation: &str,
    anchor: RetrievalAnchorRecordV2,
    content: &str,
    occurred_at: i64,
) -> InitialFact {
    let identity = application_identity(owner, operation);
    let fact_id = FactId::derive(&identity).unwrap();
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        payload(content, &format!("receipt.{operation}")),
        vec![evidence(
            &fact_id,
            anchor.anchor_id(),
            FactEvidenceRelationV1::Supports,
        )],
        UtcMicros(occurred_at),
        None,
    )
    .unwrap();
    let event = recorded_event(&fact_id, owner, assertion.assertion_id(), occurred_at);
    let batch = FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        Some(assertion.clone()),
        vec![event],
        vec![anchor.clone()],
        Vec::new(),
        None,
        None,
    )
    .unwrap()
    .with_identity_material(identity)
    .unwrap();
    let receipt = commit(store, batch).await;
    InitialFact {
        fact_id,
        anchor,
        assertion,
        receipt,
    }
}

/// Appends one assertion (correction, merge, or additional initial) to an
/// existing fact, referencing already-committed anchors.
#[allow(clippy::too_many_arguments)]
async fn commit_assertion(
    store: &DatabaseFactStore<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    kind: FactAssertionKindV1,
    content: &str,
    occurred_at: i64,
    referenced_anchor_ids: Vec<RetrievalAnchorId>,
    evidence: Vec<FactEvidenceRefV1>,
    expected_last: &FactEventId,
    receipt_id: &str,
) -> (FactAssertionV1, FactCommitReceipt) {
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        kind,
        payload(content, receipt_id),
        evidence,
        UtcMicros(occurred_at),
        None,
    )
    .unwrap();
    let event = recorded_event(fact_id, owner, assertion.assertion_id(), occurred_at);
    let batch = FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        Some(assertion.clone()),
        vec![event],
        Vec::new(),
        referenced_anchor_ids,
        None,
        Some(expected_last.clone()),
    )
    .unwrap();
    let receipt = commit(store, batch).await;
    (assertion, receipt)
}

#[tokio::test]
async fn merge_and_hydration_preserve_source_and_privacy_identity() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let profile = FactOwnerV1::Profile;

    // Two profile facts with distinct evidence anchors and privacy domains.
    let keep_anchor = anchor(
        ObservationScopeV1::Profile,
        "entity.fmh.merge.keep",
        "privacy.fmh.alpha",
        PayloadAccessState::Eligible,
        CoverageReportV1::default(),
    );
    let keep = commit_initial(
        &store,
        &profile,
        "operation.fmh.merge.keep",
        keep_anchor,
        "alpha content",
        1_000,
    )
    .await;
    let drop_anchor = anchor(
        ObservationScopeV1::Profile,
        "entity.fmh.merge.drop",
        "privacy.fmh.beta",
        PayloadAccessState::Eligible,
        CoverageReportV1::default(),
    );
    let drop = commit_initial(
        &store,
        &profile,
        "operation.fmh.merge.drop",
        drop_anchor,
        "beta content",
        2_000,
    )
    .await;

    // Merge drop into keep: the winner records a correction that carries both
    // evidence anchors, and the loser is curated as merged and then denied —
    // its retained assertions are never edited in place.
    let (merged_assertion, _) = commit_assertion(
        &store,
        &profile,
        &keep.fact_id,
        FactAssertionKindV1::Correction {
            supersedes: keep.assertion.assertion_id().clone(),
        },
        "merged alpha+beta content",
        3_000,
        vec![
            keep.anchor.anchor_id().clone(),
            drop.anchor.anchor_id().clone(),
        ],
        vec![
            evidence(
                &keep.fact_id,
                keep.anchor.anchor_id(),
                FactEvidenceRelationV1::Supports,
            ),
            evidence(
                &keep.fact_id,
                drop.anchor.anchor_id(),
                FactEvidenceRelationV1::DerivedFrom,
            ),
        ],
        keep.receipt.last_event_id(),
        "receipt.fmh.merge.winner",
    )
    .await;
    let loser_batch = FactWriteBatch::new(
        drop.fact_id.clone(),
        profile.clone(),
        None,
        vec![
            FactLineageEventV1::new(
                drop.fact_id.clone(),
                profile.clone(),
                FactLineageEventKindV1::Curated {
                    action: FactCurationActionV1::MergedInto {
                        fact_id: keep.fact_id.clone(),
                    },
                    evidence_ids: Vec::new(),
                },
                UtcMicros(4_000),
                None,
            )
            .unwrap(),
            FactLineageEventV1::new(
                drop.fact_id.clone(),
                profile.clone(),
                FactLineageEventKindV1::PayloadAccessChanged {
                    previous: PayloadAccessState::Eligible,
                    current: PayloadAccessState::Deleted,
                },
                UtcMicros(4_001),
                None,
            )
            .unwrap(),
        ],
        Vec::new(),
        Vec::new(),
        None,
        Some(drop.receipt.last_event_id().clone()),
    )
    .unwrap();
    commit(&store, loser_batch).await;

    // Winner hydration: owner and merged payload survive the merge.
    let winner = current(&store, &profile, &keep.fact_id)
        .await
        .expect("winner fact should hydrate");
    assert_eq!(winner.owner(), &profile);
    winner.fact_id().validate_owner(&profile).unwrap();
    assert_eq!(
        winner.active_assertion_id(),
        merged_assertion.assertion_id()
    );
    assert_eq!(winner.payload_access(), PayloadAccessState::Eligible);
    assert_eq!(
        winner.payload().map(FactPayloadV1::content),
        Some("merged alpha+beta content")
    );

    // Loser hydration: hidden from current projections; lineage retains the
    // explicit merge instead of an in-place edit.
    assert!(current(&store, &profile, &drop.fact_id).await.is_none());
    let profile_facts = current_page(&store, &profile).await;
    assert!(
        profile_facts
            .iter()
            .any(|fact| fact.fact_id() == &keep.fact_id)
    );
    assert!(
        !profile_facts
            .iter()
            .any(|fact| fact.fact_id() == &drop.fact_id)
    );
    let loser_lineage = lineage(&store, &profile, &drop.fact_id).await;
    assert!(loser_lineage.iter().any(|event| matches!(
        event.kind(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::MergedInto { fact_id },
            ..
        } if fact_id == &keep.fact_id
    )));
    assert!(loser_lineage.iter().any(|event| matches!(
        event.kind(),
        FactLineageEventKindV1::PayloadAccessChanged {
            current: PayloadAccessState::Deleted,
            ..
        }
    )));

    // Anchor hydration: source and privacy-domain identity survive the merge.
    let keep_anchor_record = anchor_record(&store, &profile, keep.anchor.anchor_id()).await;
    assert_eq!(&keep_anchor_record, &keep.anchor);
    assert_eq!(keep_anchor_record.owner(), &ObservationScopeV1::Profile);
    assert_eq!(
        keep_anchor_record.authorization().privacy_domain_id,
        PrivacyDomainId::new("privacy.fmh.alpha").unwrap()
    );
    let drop_anchor_record = anchor_record(&store, &profile, drop.anchor.anchor_id()).await;
    assert_eq!(&drop_anchor_record, &drop.anchor);
    assert_eq!(
        drop_anchor_record.authorization().privacy_domain_id,
        PrivacyDomainId::new("privacy.fmh.beta").unwrap()
    );

    // A project-owned assertion merge keeps its project identity and privacy
    // domain through hydration, and never leaks into the profile listing.
    let project = FactOwnerV1::Project {
        project_id: id("project.fmh.merge"),
    };
    let project_scope = ObservationScopeV1::Project {
        project_id: id("project.fmh.merge"),
    };
    let project_anchor = anchor(
        project_scope.clone(),
        "entity.fmh.merge.project",
        "privacy.fmh.project",
        PayloadAccessState::Eligible,
        CoverageReportV1::default(),
    );
    let project_fact = commit_initial(
        &store,
        &project,
        "operation.fmh.merge.project",
        project_anchor,
        "project v1",
        5_000,
    )
    .await;
    let (second, second_receipt) = commit_assertion(
        &store,
        &project,
        &project_fact.fact_id,
        FactAssertionKindV1::Initial,
        "project v2",
        6_000,
        vec![project_fact.anchor.anchor_id().clone()],
        vec![evidence(
            &project_fact.fact_id,
            project_fact.anchor.anchor_id(),
            FactEvidenceRelationV1::Supports,
        )],
        project_fact.receipt.last_event_id(),
        "receipt.fmh.merge.project.second",
    )
    .await;
    let (merge_assertion, _) = commit_assertion(
        &store,
        &project,
        &project_fact.fact_id,
        FactAssertionKindV1::Merge {
            supersedes: vec![
                project_fact.assertion.assertion_id().clone(),
                second.assertion_id().clone(),
            ],
        },
        "project merged",
        7_000,
        vec![project_fact.anchor.anchor_id().clone()],
        vec![evidence(
            &project_fact.fact_id,
            project_fact.anchor.anchor_id(),
            FactEvidenceRelationV1::Supports,
        )],
        second_receipt.last_event_id(),
        "receipt.fmh.merge.project.merged",
    )
    .await;

    let hydrated = current(&store, &project, &project_fact.fact_id)
        .await
        .expect("project fact should hydrate");
    assert_eq!(hydrated.owner(), &project);
    hydrated.fact_id().validate_owner(&project).unwrap();
    assert_eq!(
        hydrated.active_assertion_id(),
        merge_assertion.assertion_id()
    );
    assert_eq!(
        hydrated.payload().map(FactPayloadV1::content),
        Some("project merged")
    );
    let project_anchor_record =
        anchor_record(&store, &project, project_fact.anchor.anchor_id()).await;
    assert_eq!(project_anchor_record.owner(), &project_scope);
    assert_eq!(
        project_anchor_record.authorization().privacy_domain_id,
        PrivacyDomainId::new("privacy.fmh.project").unwrap()
    );

    // Owner scoping survives hydration in both directions.
    assert!(
        current_page(&store, &profile)
            .await
            .iter()
            .all(|fact| fact.owner() == &profile)
    );
    let project_facts = current_page(&store, &project).await;
    assert_eq!(project_facts.len(), 1);
    assert_eq!(project_facts[0].fact_id(), &project_fact.fact_id);
}

#[tokio::test]
async fn corrections_supersede_without_editing_prior_evidence() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let original_anchor = anchor(
        ObservationScopeV1::Profile,
        "entity.fmh.correction",
        "privacy.fmh.correction",
        PayloadAccessState::Eligible,
        CoverageReportV1::default(),
    );
    let fixture = commit_initial(
        &store,
        &owner,
        "operation.fmh.correction",
        original_anchor,
        "original claim",
        1_000,
    )
    .await;

    let (correction, _) = commit_assertion(
        &store,
        &owner,
        &fixture.fact_id,
        FactAssertionKindV1::Correction {
            supersedes: fixture.assertion.assertion_id().clone(),
        },
        "corrected claim",
        2_000,
        vec![fixture.anchor.anchor_id().clone()],
        vec![evidence(
            &fixture.fact_id,
            fixture.anchor.anchor_id(),
            FactEvidenceRelationV1::Corrects,
        )],
        fixture.receipt.last_event_id(),
        "receipt.fmh.correction",
    )
    .await;

    // The current projection points at the correction.
    let hydrated = current(&store, &owner, &fixture.fact_id)
        .await
        .expect("fact should hydrate");
    assert_eq!(hydrated.active_assertion_id(), correction.assertion_id());
    assert_eq!(
        hydrated.payload().map(FactPayloadV1::content),
        Some("corrected claim")
    );

    // Prior evidence is untouched: an as-of replay still hydrates the original
    // assertion, and lineage retains both records in order.
    let before = as_of(&store, &owner, &fixture.fact_id, 1_000)
        .await
        .expect("as-of projection before the correction should exist");
    assert_eq!(
        before.active_assertion_id(),
        fixture.assertion.assertion_id()
    );
    assert_eq!(
        before.payload().map(FactPayloadV1::content),
        Some("original claim")
    );
    let events = lineage(&store, &owner, &fixture.fact_id).await;
    let recorded: Vec<FactAssertionId> = events
        .iter()
        .filter_map(|event| match event.kind() {
            FactLineageEventKindV1::AssertionRecorded { assertion_id } => {
                Some(assertion_id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        recorded,
        vec![
            fixture.assertion.assertion_id().clone(),
            correction.assertion_id().clone(),
        ]
    );

    // The evidence anchor is byte-identical after the correction.
    let hydrated_anchor = anchor_record(&store, &owner, fixture.anchor.anchor_id()).await;
    assert_eq!(&hydrated_anchor, &fixture.anchor);
}

#[tokio::test]
async fn contradictions_are_recorded_explicitly_in_lineage() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let first = commit_initial(
        &store,
        &owner,
        "operation.fmh.contradiction.first",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.contradiction.first",
            "privacy.fmh.contradiction",
            PayloadAccessState::Eligible,
            CoverageReportV1::default(),
        ),
        "first claim",
        1_000,
    )
    .await;
    let second = commit_initial(
        &store,
        &owner,
        "operation.fmh.contradiction.second",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.contradiction.second",
            "privacy.fmh.contradiction",
            PayloadAccessState::Eligible,
            CoverageReportV1::default(),
        ),
        "conflicting claim",
        2_000,
    )
    .await;

    // A contradiction is explicit curation lineage, not a projection rewrite.
    let curated = FactLineageEventV1::new(
        first.fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::ContradictedBy {
                fact_id: second.fact_id.clone(),
            },
            evidence_ids: Vec::new(),
        },
        UtcMicros(3_000),
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(
        first.fact_id.clone(),
        owner.clone(),
        None,
        vec![curated],
        Vec::new(),
        Vec::new(),
        None,
        Some(first.receipt.last_event_id().clone()),
    )
    .unwrap();
    commit(&store, batch).await;

    let hydrated = current(&store, &owner, &first.fact_id)
        .await
        .expect("contradicted fact should still hydrate");
    assert_eq!(hydrated.payload_access(), PayloadAccessState::Eligible);
    assert_eq!(
        hydrated.payload().map(FactPayloadV1::content),
        Some("first claim")
    );
    let events = lineage(&store, &owner, &first.fact_id).await;
    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::ContradictedBy { fact_id },
            ..
        } if fact_id == &second.fact_id
    )));
    let current_response = store
        .query_fact_current_response(
            FactCurrentQuery::new(owner.clone(), first.fact_id.clone()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        current_response.contradiction().contradicted_by(),
        std::slice::from_ref(&second.fact_id)
    );
    let lineage_response = store
        .query_fact_lineage_response(
            FactLineageQuery::new(owner.clone(), first.fact_id.clone(), None, 100).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        lineage_response.contradiction().contradicted_by(),
        std::slice::from_ref(&second.fact_id)
    );

    // A contradiction against a fact the authority does not retain is rejected
    // atomically: no partial lineage survives the failed batch.
    let missing = FactId::derive(&application_identity(
        &owner,
        "operation.fmh.contradiction.missing",
    ))
    .unwrap();
    let rejected = FactLineageEventV1::new(
        first.fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::ContradictedBy { fact_id: missing },
            evidence_ids: Vec::new(),
        },
        UtcMicros(4_000),
        None,
    )
    .unwrap();
    let last_event_id = events
        .last()
        .map(FactLineageEventV1::event_id)
        .unwrap()
        .clone();
    let batch = FactWriteBatch::new(
        first.fact_id.clone(),
        owner.clone(),
        None,
        vec![rejected],
        Vec::new(),
        Vec::new(),
        None,
        Some(last_event_id),
    )
    .unwrap();
    let error = store.commit_fact(batch).await.unwrap_err();
    assert!(
        matches!(error, FactStoreError::Storage { .. }),
        "missing curation target should fail as a storage error, got {error:?}"
    );
    assert_eq!(lineage(&store, &owner, &first.fact_id).await.len(), 2);
}

#[tokio::test]
async fn failed_fact_batch_rolls_back_identity_assertion_anchor_and_lineage() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let identity = application_identity(&owner, "operation.fmh.atomic-rollback");
    let fact_id = FactId::derive(&identity).unwrap();
    let new_anchor = anchor(
        ObservationScopeV1::Profile,
        "entity.fmh.atomic-rollback",
        "privacy.fmh.atomic-rollback",
        PayloadAccessState::Eligible,
        CoverageReportV1::default(),
    );
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        payload(
            "must not survive a failed batch",
            "receipt.fmh.atomic-rollback",
        ),
        vec![evidence(
            &fact_id,
            new_anchor.anchor_id(),
            FactEvidenceRelationV1::Supports,
        )],
        UtcMicros(1_000),
        None,
    )
    .unwrap();
    let assertion_id = assertion.assertion_id().clone();
    let recorded = recorded_event(&fact_id, &owner, assertion.assertion_id(), 1_000);
    let missing = FactId::derive(&application_identity(
        &owner,
        "operation.fmh.atomic-rollback.missing",
    ))
    .unwrap();
    let rejected = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::ContradictedBy { fact_id: missing },
            evidence_ids: Vec::new(),
        },
        UtcMicros(2_000),
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        Some(assertion),
        vec![recorded, rejected],
        vec![new_anchor.clone()],
        Vec::new(),
        None,
        None,
    )
    .unwrap()
    .with_identity_material(identity)
    .unwrap();

    let error = store.commit_fact(batch).await.unwrap_err();
    assert!(
        matches!(error, FactStoreError::Storage { .. }),
        "missing curation target should fail after staged writes, got {error:?}"
    );
    assert!(current(&store, &owner, &fact_id).await.is_none());
    assert!(lineage(&store, &owner, &fact_id).await.is_empty());
    assert!(
        store
            .get_retrieval_anchor(
                RetrievalAnchorQuery::new(owner.clone(), new_anchor.anchor_id().clone()).unwrap(),
            )
            .await
            .unwrap()
            .is_none()
    );
    for (table, sql, id) in [
        (
            "memory_v2_facts",
            "SELECT COUNT(*) FROM memory_v2_facts WHERE fact_id = ?1",
            fact_id.as_str(),
        ),
        (
            "memory_v2_assertions",
            "SELECT COUNT(*) FROM memory_v2_assertions WHERE assertion_id = ?1",
            assertion_id.as_str(),
        ),
        (
            "retrieval_anchors",
            "SELECT COUNT(*) FROM retrieval_anchors WHERE anchor_id = ?1",
            new_anchor.anchor_id().as_str(),
        ),
        (
            "memory_v2_lineage_events",
            "SELECT COUNT(*) FROM memory_v2_lineage_events WHERE fact_id = ?1",
            fact_id.as_str(),
        ),
    ] {
        let count = test
            .db
            .query_scalar_i64_with_text("inspect failed fact batch rollback", sql, id)
            .await
            .unwrap();
        assert_eq!(count, 0, "{table} retained a partial batch row");
    }
}

#[tokio::test]
async fn as_of_projection_cannot_see_future_knowledge() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let fixture = commit_initial(
        &store,
        &owner,
        "operation.fmh.as-of",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.as-of",
            "privacy.fmh.as-of",
            PayloadAccessState::Eligible,
            CoverageReportV1::default(),
        ),
        "version one",
        100,
    )
    .await;
    let (correction, correction_receipt) = commit_assertion(
        &store,
        &owner,
        &fixture.fact_id,
        FactAssertionKindV1::Correction {
            supersedes: fixture.assertion.assertion_id().clone(),
        },
        "version two",
        200,
        vec![fixture.anchor.anchor_id().clone()],
        vec![evidence(
            &fixture.fact_id,
            fixture.anchor.anchor_id(),
            FactEvidenceRelationV1::Corrects,
        )],
        fixture.receipt.last_event_id(),
        "receipt.fmh.as-of.correction",
    )
    .await;
    let trust_event = FactLineageEventV1::new(
        fixture.fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::TrustChanged {
            previous: Confidence::new(0.5).unwrap(),
            current: Confidence::new(0.9).unwrap(),
            evidence_ids: Vec::new(),
        },
        UtcMicros(300),
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(
        fixture.fact_id.clone(),
        owner.clone(),
        None,
        vec![trust_event],
        Vec::new(),
        Vec::new(),
        None,
        Some(correction_receipt.last_event_id().clone()),
    )
    .unwrap();
    commit(&store, batch).await;

    // Before the first event the fact does not exist at all.
    assert!(as_of(&store, &owner, &fixture.fact_id, 99).await.is_none());

    // At the first event only the initial assertion is visible.
    let initial = as_of(&store, &owner, &fixture.fact_id, 100)
        .await
        .expect("as-of projection at the initial assertion");
    assert_eq!(
        initial.active_assertion_id(),
        fixture.assertion.assertion_id()
    );
    assert_eq!(
        initial.payload().map(FactPayloadV1::content),
        Some("version one")
    );
    assert_eq!(initial.projected_as_of(), UtcMicros(100));

    // Between the correction and the trust change the new payload is active
    // but the later trust is not yet known.
    let corrected = as_of(&store, &owner, &fixture.fact_id, 250)
        .await
        .expect("as-of projection after the correction");
    assert_eq!(corrected.active_assertion_id(), correction.assertion_id());
    assert_eq!(
        corrected.payload().map(FactPayloadV1::content),
        Some("version two")
    );
    assert_eq!(corrected.trust(), Confidence::new(0.5).unwrap());
    assert_eq!(corrected.projected_as_of(), UtcMicros(200));

    // At and after the trust change the full state is visible.
    let trusted = as_of(&store, &owner, &fixture.fact_id, 300)
        .await
        .expect("as-of projection at the trust change");
    assert_eq!(trusted.trust(), Confidence::new(0.9).unwrap());
    assert_eq!(trusted.projected_as_of(), UtcMicros(300));

    let hydrated = current(&store, &owner, &fixture.fact_id)
        .await
        .expect("current projection");
    assert_eq!(hydrated.active_assertion_id(), correction.assertion_id());
    assert_eq!(hydrated.trust(), Confidence::new(0.9).unwrap());
}

#[tokio::test]
async fn denied_payloads_never_hydrate() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let fixture = commit_initial(
        &store,
        &owner,
        "operation.fmh.denied",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.denied",
            "privacy.fmh.denied",
            PayloadAccessState::Eligible,
            CoverageReportV1::default(),
        ),
        "secret claim",
        1_000,
    )
    .await;
    let deleted = FactLineageEventV1::new(
        fixture.fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Deleted,
        },
        UtcMicros(2_000),
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(
        fixture.fact_id.clone(),
        owner.clone(),
        None,
        vec![deleted],
        Vec::new(),
        Vec::new(),
        None,
        Some(fixture.receipt.last_event_id().clone()),
    )
    .unwrap();
    commit(&store, batch).await;

    // The denied fact leaves the current projections entirely.
    assert!(current(&store, &owner, &fixture.fact_id).await.is_none());
    let current_response = store
        .query_fact_current_response(
            FactCurrentQuery::new(owner.clone(), fixture.fact_id.clone()).unwrap(),
        )
        .await
        .unwrap();
    assert!(current_response.fact().is_none());
    assert_eq!(current_response.coverage().hidden(), 1);
    assert_eq!(current_response.coverage().unknown(), 0);
    assert_eq!(current_response.coverage().redacted(), 0);
    assert!(
        current_page(&store, &owner)
            .await
            .iter()
            .all(|fact| fact.fact_id() != &fixture.fact_id)
    );

    // As-of at the denial is denied as well.
    assert!(
        as_of(&store, &owner, &fixture.fact_id, 2_000)
            .await
            .is_none()
    );

    // As-of before the denial must not resurrect the purged payload: the
    // lineage survives, but hydration reports the payload unavailable.
    let before = as_of(&store, &owner, &fixture.fact_id, 1_000)
        .await
        .expect("as-of projection before the denial should retain lineage");
    assert_eq!(before.payload_access(), PayloadAccessState::Unavailable);
    assert!(before.payload().is_none());
    assert_eq!(
        before.active_assertion_id(),
        fixture.assertion.assertion_id()
    );
    let before_response = store
        .query_fact_as_of_response(
            FactAsOfQuery::new(owner.clone(), fixture.fact_id.clone(), UtcMicros(1_000)).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(before_response.coverage().hidden(), 1);
    assert_eq!(before_response.coverage().unknown(), 0);

    // The full lineage, including the denial, is retained.
    let events = lineage(&store, &owner, &fixture.fact_id).await;
    assert_eq!(events.len(), 2);
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        FactLineageEventKindV1::PayloadAccessChanged {
            current: PayloadAccessState::Deleted,
            ..
        }
    )));
}

#[tokio::test]
async fn redacted_frontiers_stay_redacted() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let fixture = commit_initial(
        &store,
        &owner,
        "operation.fmh.redacted",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.redacted",
            "privacy.fmh.redacted",
            PayloadAccessState::Eligible,
            CoverageReportV1::default(),
        ),
        "sensitive claim",
        1_000,
    )
    .await;
    let redacted = FactLineageEventV1::new(
        fixture.fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Redacted,
        },
        UtcMicros(2_000),
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(
        fixture.fact_id.clone(),
        owner.clone(),
        None,
        vec![redacted],
        Vec::new(),
        Vec::new(),
        None,
        Some(fixture.receipt.last_event_id().clone()),
    )
    .unwrap();
    commit(&store, batch).await;

    // The redacted frontier never hydrates payload bytes.
    let hydrated = current(&store, &owner, &fixture.fact_id)
        .await
        .expect("redacted fact should retain its projection");
    assert_eq!(hydrated.payload_access(), PayloadAccessState::Redacted);
    assert!(hydrated.payload().is_none());
    let current_response = store
        .query_fact_current_response(
            FactCurrentQuery::new(owner.clone(), fixture.fact_id.clone()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(current_response.coverage().redacted(), 1);
    assert_eq!(current_response.coverage().hidden(), 0);
    assert_eq!(current_response.coverage().unknown(), 1);
    let after = as_of(&store, &owner, &fixture.fact_id, 2_000)
        .await
        .expect("as-of projection at the redaction");
    assert_eq!(after.payload_access(), PayloadAccessState::Redacted);
    assert!(after.payload().is_none());
    let after_response = store
        .query_fact_as_of_response(
            FactAsOfQuery::new(owner.clone(), fixture.fact_id.clone(), UtcMicros(2_000)).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(after_response.coverage().redacted(), 1);

    // Access state is projected rather than blanket-denied: before the
    // redaction the fact was eligible and its retained bytes still hydrate.
    let before = as_of(&store, &owner, &fixture.fact_id, 1_000)
        .await
        .expect("as-of projection before the redaction");
    assert_eq!(before.payload_access(), PayloadAccessState::Eligible);
    assert_eq!(
        before.payload().map(FactPayloadV1::content),
        Some("sensitive claim")
    );

    // An anchor committed behind a redacted frontier stays redacted after it
    // is used as merge evidence and hydrated.
    let redacted_anchor = anchor(
        ObservationScopeV1::Profile,
        "entity.fmh.redacted.anchor",
        "privacy.fmh.redacted.anchor",
        PayloadAccessState::Redacted,
        CoverageReportV1::default(),
    );
    let anchored = commit_initial(
        &store,
        &owner,
        "operation.fmh.redacted.anchor",
        redacted_anchor,
        "redacted frontier fact",
        3_000,
    )
    .await;
    let hydrated_anchor = anchor_record(&store, &owner, anchored.anchor.anchor_id()).await;
    assert_eq!(&hydrated_anchor, &anchored.anchor);
    assert_eq!(
        hydrated_anchor.payload_access(),
        PayloadAccessState::Redacted
    );

    // Re-declaring the same anchor identity behind an eligible frontier
    // conflicts without overwriting the redacted record.
    let unredacted = anchor(
        ObservationScopeV1::Profile,
        "entity.fmh.redacted.anchor",
        "privacy.fmh.redacted.anchor",
        PayloadAccessState::Eligible,
        CoverageReportV1::default(),
    );
    assert_eq!(unredacted.anchor_id(), anchored.anchor.anchor_id());
    let retained = FactLineageEventV1::new(
        anchored.fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::Retained,
            evidence_ids: Vec::new(),
        },
        UtcMicros(4_000),
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(
        anchored.fact_id.clone(),
        owner.clone(),
        None,
        vec![retained],
        vec![unredacted],
        Vec::new(),
        None,
        Some(anchored.receipt.last_event_id().clone()),
    )
    .unwrap();
    let outcome = store.commit_fact(batch).await.unwrap();
    assert!(
        matches!(
            outcome,
            FactCommitOutcome::Conflict(FactCommitConflict::IdentityCollision { kind, .. })
                if kind == "retrieval anchor"
        ),
        "rewriting a redacted anchor as eligible should conflict, got {outcome:?}"
    );
    let hydrated_anchor = anchor_record(&store, &owner, anchored.anchor.anchor_id()).await;
    assert_eq!(
        hydrated_anchor.payload_access(),
        PayloadAccessState::Redacted
    );
}

#[tokio::test]
async fn unknown_denominators_report_unknown_not_fabricated() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;

    // A coverage report whose shard universe was unknown at capture hydrates
    // as unknown, never as a fabricated complete denominator.
    let unknown_coverage = CoverageReportV1 {
        dispositions: BTreeMap::from([(
            ShardId::new("shard.fmh.a").unwrap(),
            ShardDispositionV1::Searched,
        )]),
        freshness: BTreeMap::new(),
        retention_watermark: None,
        universe: CoverageUniverseKnowledgeV1::Unknown,
        remote: None,
    };
    assert!(!unknown_coverage.is_complete());
    let fixture = commit_initial(
        &store,
        &owner,
        "operation.fmh.coverage",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.coverage",
            "privacy.fmh.coverage",
            PayloadAccessState::Eligible,
            unknown_coverage,
        ),
        "coverage fact",
        1_000,
    )
    .await;
    let hydrated_anchor = anchor_record(&store, &owner, fixture.anchor.anchor_id()).await;
    assert_eq!(
        hydrated_anchor.coverage().universe,
        CoverageUniverseKnowledgeV1::Unknown
    );
    assert!(
        !hydrated_anchor.coverage().is_complete(),
        "an unknown shard universe must not hydrate as complete coverage"
    );
    let current_response = store
        .query_fact_current_response(
            FactCurrentQuery::new(owner.clone(), fixture.fact_id.clone()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(current_response.coverage().visible(), 0);
    assert_eq!(current_response.coverage().hidden(), 0);
    assert_eq!(current_response.coverage().unknown(), 1);
    assert_eq!(current_response.coverage().redacted(), 0);

    // A legacy import with unknown history coverage hydrates as unknown
    // through the compatibility mapping instead of inventing history.
    let source_store = SourceStoreId::new("store.fmh.legacy").unwrap();
    let identity = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Legacy {
            source_store_id: source_store.clone(),
            legacy_fact_id: 42,
        },
    )
    .unwrap();
    let fact_id = FactId::derive(&identity).unwrap();
    let mapping = LegacyFactMappingV1::new(
        owner.clone(),
        source_store.clone(),
        42,
        fact_id.clone(),
        LegacyHistoryCoverageV1::Unknown,
        UtcMicros(2_002),
    )
    .unwrap();
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        payload("legacy claim", "receipt.fmh.legacy"),
        Vec::new(),
        UtcMicros(2_001),
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        Some(assertion.clone()),
        vec![
            FactLineageEventV1::new(
                fact_id.clone(),
                owner.clone(),
                FactLineageEventKindV1::LegacyImported {
                    mapping: mapping.clone(),
                },
                UtcMicros(2_000),
                None,
            )
            .unwrap(),
            recorded_event(&fact_id, &owner, assertion.assertion_id(), 2_001),
        ],
        Vec::new(),
        Vec::new(),
        Some(mapping.clone()),
        None,
    )
    .unwrap()
    .with_identity_material(identity)
    .unwrap();
    commit(&store, batch).await;

    let hydrated = current(&store, &owner, &fact_id)
        .await
        .expect("legacy-imported fact should hydrate");
    assert_eq!(
        hydrated
            .legacy_mapping()
            .expect("legacy mapping should hydrate")
            .history_coverage(),
        LegacyHistoryCoverageV1::Unknown
    );
    let resolved = store
        .resolve_legacy_fact(LegacyFactQuery::new(owner.clone(), source_store, 42).unwrap())
        .await
        .unwrap();
    assert_eq!(resolved.as_ref(), Some(&fact_id));

    // As-of before the recorded migration time the mapping is absent rather
    // than back-filled.
    let imported = as_of(&store, &owner, &fact_id, 2_001)
        .await
        .expect("as-of projection at the import");
    assert!(imported.legacy_mapping().is_none());
    let migrated = as_of(&store, &owner, &fact_id, 2_002)
        .await
        .expect("as-of projection at the migration time");
    assert_eq!(
        migrated
            .legacy_mapping()
            .expect("mapping should be visible at its migration time")
            .history_coverage(),
        LegacyHistoryCoverageV1::Unknown
    );
    let migrated_response = store
        .query_fact_as_of_response(FactAsOfQuery::new(owner, fact_id, UtcMicros(2_002)).unwrap())
        .await
        .unwrap();
    assert_eq!(migrated_response.coverage().unknown(), 1);
}

#[tokio::test]
async fn known_multishard_coverage_counts_each_searched_frontier() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let fixture = commit_initial(
        &store,
        &owner,
        "operation.fmh.coverage.multishard",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.coverage.multishard",
            "privacy.fmh.coverage.multishard",
            PayloadAccessState::Eligible,
            known_coverage(BTreeMap::from([
                (
                    ShardId::new("shard.fmh.coverage.multishard.a").unwrap(),
                    ShardDispositionV1::Searched,
                ),
                (
                    ShardId::new("shard.fmh.coverage.multishard.b").unwrap(),
                    ShardDispositionV1::Searched,
                ),
            ])),
        ),
        "multishard coverage",
        1_000,
    )
    .await;

    let response = store
        .query_fact_current_response(FactCurrentQuery::new(owner, fixture.fact_id).unwrap())
        .await
        .unwrap();
    assert_eq!(response.coverage().visible(), 2);
    assert_eq!(response.coverage().hidden(), 0);
    assert_eq!(response.coverage().unknown(), 0);
    assert_eq!(response.coverage().redacted(), 0);
}

#[tokio::test]
async fn mixed_shard_coverage_counts_each_frontier_bucket() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let fixture = commit_initial(
        &store,
        &owner,
        "operation.fmh.coverage.mixed",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.coverage.mixed",
            "privacy.fmh.coverage.mixed",
            PayloadAccessState::Eligible,
            known_coverage(BTreeMap::from([
                (
                    ShardId::new("shard.fmh.coverage.mixed.searched").unwrap(),
                    ShardDispositionV1::Searched,
                ),
                (
                    ShardId::new("shard.fmh.coverage.mixed.skipped").unwrap(),
                    ShardDispositionV1::Skipped,
                ),
                (
                    ShardId::new("shard.fmh.coverage.mixed.stale").unwrap(),
                    ShardDispositionV1::Stale,
                ),
                (
                    ShardId::new("shard.fmh.coverage.mixed.redacted").unwrap(),
                    ShardDispositionV1::Redacted,
                ),
            ])),
        ),
        "mixed coverage",
        1_000,
    )
    .await;

    let response = store
        .query_fact_current_response(FactCurrentQuery::new(owner, fixture.fact_id).unwrap())
        .await
        .unwrap();
    assert_eq!(response.coverage().visible(), 1);
    assert_eq!(response.coverage().hidden(), 2);
    assert_eq!(response.coverage().unknown(), 0);
    assert_eq!(response.coverage().redacted(), 1);
}

#[tokio::test]
async fn known_empty_coverage_reports_zero_denominators() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let fixture = commit_initial(
        &store,
        &owner,
        "operation.fmh.coverage.empty",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.coverage.empty",
            "privacy.fmh.coverage.empty",
            PayloadAccessState::Eligible,
            known_coverage(BTreeMap::new()),
        ),
        "empty coverage",
        1_000,
    )
    .await;

    let response = store
        .query_fact_current_response(FactCurrentQuery::new(owner, fixture.fact_id).unwrap())
        .await
        .unwrap();
    assert_eq!(response.coverage().visible(), 0);
    assert_eq!(response.coverage().hidden(), 0);
    assert_eq!(response.coverage().unknown(), 0);
    assert_eq!(response.coverage().redacted(), 0);
}

#[tokio::test]
async fn anchor_redaction_preserves_frontier_denominators() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let fixture = commit_initial(
        &store,
        &owner,
        "operation.fmh.coverage.anchor-redaction",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.coverage.anchor-redaction",
            "privacy.fmh.coverage.anchor-redaction",
            PayloadAccessState::Redacted,
            known_coverage(BTreeMap::from([
                (
                    ShardId::new("shard.fmh.coverage.anchor-redaction.searched").unwrap(),
                    ShardDispositionV1::Searched,
                ),
                (
                    ShardId::new("shard.fmh.coverage.anchor-redaction.skipped").unwrap(),
                    ShardDispositionV1::Skipped,
                ),
            ])),
        ),
        "anchor-redacted coverage",
        1_000,
    )
    .await;

    let response = store
        .query_fact_current_response(FactCurrentQuery::new(owner, fixture.fact_id).unwrap())
        .await
        .unwrap();
    assert_eq!(response.coverage().visible(), 1);
    assert_eq!(response.coverage().hidden(), 1);
    assert_eq!(response.coverage().unknown(), 0);
    assert_eq!(response.coverage().redacted(), 2);
}

#[tokio::test]
async fn payload_redaction_preserves_frontier_denominators() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let fixture = commit_initial(
        &store,
        &owner,
        "operation.fmh.coverage.payload-redaction",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.coverage.payload-redaction",
            "privacy.fmh.coverage.payload-redaction",
            PayloadAccessState::Eligible,
            known_coverage(BTreeMap::from([
                (
                    ShardId::new("shard.fmh.coverage.payload-redaction.searched").unwrap(),
                    ShardDispositionV1::Searched,
                ),
                (
                    ShardId::new("shard.fmh.coverage.payload-redaction.skipped").unwrap(),
                    ShardDispositionV1::Skipped,
                ),
            ])),
        ),
        "payload-redacted coverage",
        1_000,
    )
    .await;
    let redacted = FactLineageEventV1::new(
        fixture.fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Redacted,
        },
        UtcMicros(2_000),
        None,
    )
    .unwrap();
    commit(
        &store,
        FactWriteBatch::new(
            fixture.fact_id.clone(),
            owner.clone(),
            None,
            vec![redacted],
            Vec::new(),
            Vec::new(),
            None,
            Some(fixture.receipt.last_event_id().clone()),
        )
        .unwrap(),
    )
    .await;

    let response = store
        .query_fact_current_response(FactCurrentQuery::new(owner, fixture.fact_id).unwrap())
        .await
        .unwrap();
    assert_eq!(response.coverage().visible(), 1);
    assert_eq!(response.coverage().hidden(), 1);
    assert_eq!(response.coverage().unknown(), 0);
    assert_eq!(response.coverage().redacted(), 2);
}

#[tokio::test]
async fn contradiction_metadata_transitions_at_the_as_of_cutoff() {
    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let source = commit_initial(
        &store,
        &owner,
        "operation.fmh.contradiction.temporal.source",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.contradiction.temporal.source",
            "privacy.fmh.contradiction.temporal.source",
            PayloadAccessState::Eligible,
            known_coverage(BTreeMap::from([(
                ShardId::new("shard.fmh.contradiction.temporal").unwrap(),
                ShardDispositionV1::Searched,
            )])),
        ),
        "temporal source",
        1_000,
    )
    .await;
    let first_target = commit_initial(
        &store,
        &owner,
        "operation.fmh.contradiction.temporal.first",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.contradiction.temporal.first",
            "privacy.fmh.contradiction.temporal.first",
            PayloadAccessState::Eligible,
            CoverageReportV1::default(),
        ),
        "temporal first target",
        1_100,
    )
    .await;
    let second_target = commit_initial(
        &store,
        &owner,
        "operation.fmh.contradiction.temporal.second",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.contradiction.temporal.second",
            "privacy.fmh.contradiction.temporal.second",
            PayloadAccessState::Eligible,
            CoverageReportV1::default(),
        ),
        "temporal second target",
        1_200,
    )
    .await;
    let first_contradiction = FactLineageEventV1::new(
        source.fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::ContradictedBy {
                fact_id: first_target.fact_id.clone(),
            },
            evidence_ids: Vec::new(),
        },
        UtcMicros(2_000),
        None,
    )
    .unwrap();
    let first_receipt = commit(
        &store,
        FactWriteBatch::new(
            source.fact_id.clone(),
            owner.clone(),
            None,
            vec![first_contradiction],
            Vec::new(),
            Vec::new(),
            None,
            Some(source.receipt.last_event_id().clone()),
        )
        .unwrap(),
    )
    .await;

    let before = store
        .query_fact_as_of_response(
            FactAsOfQuery::new(owner.clone(), source.fact_id.clone(), UtcMicros(1_999)).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        before.contradiction(),
        &FactContradictionStateV1::NotObserved
    );
    let at_first = store
        .query_fact_as_of_response(
            FactAsOfQuery::new(owner.clone(), source.fact_id.clone(), UtcMicros(2_000)).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        at_first.contradiction().contradicted_by(),
        std::slice::from_ref(&first_target.fact_id)
    );

    let second_contradiction = FactLineageEventV1::new(
        source.fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::ContradictedBy {
                fact_id: second_target.fact_id.clone(),
            },
            evidence_ids: Vec::new(),
        },
        UtcMicros(3_000),
        None,
    )
    .unwrap();
    commit(
        &store,
        FactWriteBatch::new(
            source.fact_id.clone(),
            owner.clone(),
            None,
            vec![second_contradiction],
            Vec::new(),
            Vec::new(),
            None,
            Some(first_receipt.last_event_id().clone()),
        )
        .unwrap(),
    )
    .await;

    let after_second = store
        .query_fact_as_of_response(
            FactAsOfQuery::new(owner, source.fact_id, UtcMicros(3_000)).unwrap(),
        )
        .await
        .unwrap();
    let mut expected = vec![first_target.fact_id, second_target.fact_id];
    expected.sort_unstable();
    assert_eq!(after_second.contradiction().contradicted_by(), expected);
}

#[tokio::test]
async fn contradiction_metadata_is_bounded_and_sorted() {
    const HISTORY_SIZE: usize = 1_001;

    let test = setup_db().await;
    let store = DatabaseFactStore::new(&test.db);
    let owner = FactOwnerV1::Profile;
    let source = commit_initial(
        &store,
        &owner,
        "operation.fmh.contradiction.history.source",
        anchor(
            ObservationScopeV1::Profile,
            "entity.fmh.contradiction.history.source",
            "privacy.fmh.contradiction.history.source",
            PayloadAccessState::Eligible,
            known_coverage(BTreeMap::from([(
                ShardId::new("shard.fmh.contradiction.history").unwrap(),
                ShardDispositionV1::Searched,
            )])),
        ),
        "history source",
        1_000,
    )
    .await;
    let mut expected = Vec::with_capacity(HISTORY_SIZE);
    let mut seed_sql = String::new();
    for index in 0..HISTORY_SIZE {
        let target = FactId::derive(&application_identity(
            &owner,
            &format!("operation.fmh.contradiction.history.target.{index}"),
        ))
        .unwrap();
        let event = FactLineageEventV1::new(
            source.fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::Curated {
                action: FactCurationActionV1::ContradictedBy {
                    fact_id: target.clone(),
                },
                evidence_ids: Vec::new(),
            },
            UtcMicros(2_000 + index as i64),
            None,
        )
        .unwrap();
        let event_json = serde_json::to_string(&event).unwrap();
        writeln!(
            seed_sql,
            "INSERT INTO memory_v2_lineage_events(
                event_id, fact_id, owner_kind, project_id,
                event_json, occurred_at, recorded_at
             ) VALUES({}, {}, 'profile', '', {}, {}, {});",
            sqlite_text_literal(event.event_id().as_str()),
            sqlite_text_literal(source.fact_id.as_str()),
            sqlite_text_literal(&event_json),
            event.occurred_at().0,
            event.occurred_at().0,
        )
        .unwrap();
        expected.push(target);
    }
    test.db
        .execute_write_batch("seed bounded contradiction metadata history", &seed_sql)
        .await
        .unwrap();
    expected.sort_unstable();

    let response = store
        .query_fact_current_response(FactCurrentQuery::new(owner, source.fact_id).unwrap())
        .await
        .unwrap();
    assert!(response.contradiction().is_positive());
    assert_eq!(
        response.contradiction().contradicted_by(),
        &expected[..MAX_FACT_QUERY_CONTRADICTIONS]
    );
}
