//! Product-path tombstone coverage for memory_v2 facts: retention expiry,
//! redaction, and deletion must resolve as safe typed tombstones with deletion
//! lineage preserved and no payload bytes in any returned structure.

use std::sync::Arc;

use tempfile::TempDir;
use tracedecay::db::Database;
use tracedecay::store::memory::DatabaseFactStore;
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId,
    ComponentVersion, Confidence, CoverageReportV1, DomainError, EntityId, EntityKind, EntityRef,
    EvidenceClass, FactAssertionKindV1, FactAssertionV1, FactCategoryV1, FactEventId,
    FactEvidenceRefV1, FactEvidenceRelationV1, FactId, FactIdentityMaterialV1,
    FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1,
    ObservationScopeV1, PayloadAccessState, PayloadReferenceV1, PrivacyDomainBoundLocatorDigest,
    PrivacyDomainId, ProjectId, ProjectionGenerationId, ResolutionAuthorizationV1, RetentionClass,
    RetrievalAnchorId, RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts,
    RetrievalAnchorTargetV2, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, ScopeResolutionId, SensitivityV1, UtcMicros,
    VectorWatermark,
};
use tracedecay_store::{
    CurrentFactsQuery, FactAsOfQuery, FactCommitOutcome, FactCurrentQuery, FactLineageQuery,
    FactWriteBatch, FactWriteControl, RetrievalAnchorQuery,
};
use tracedecay_usecases::memory::MemoryApplication;

use crate::common::open_graph_db_from_template;

const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn id<T>(value: &str) -> T
where
    T: TryFrom<String, Error = DomainError>,
{
    T::try_from(value.to_owned()).unwrap()
}

fn owner() -> FactOwnerV1 {
    FactOwnerV1::Profile
}

fn write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

async fn make_store() -> (Database, TempDir) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("tracedecay.db");
    let db = open_graph_db_from_template(&db_path).await;
    (db, tmp)
}

struct CommittedFact {
    fact_id: FactId,
    anchor_id: RetrievalAnchorId,
    content: String,
    last_event_id: FactEventId,
}

fn evidence_anchor(operation: &str, ingested_at: UtcMicros) -> RetrievalAnchorRecordV2 {
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::Entity(EntityRef {
            id: EntityId::new(format!("entity.tombstone.{operation}")).unwrap(),
            kind: EntityKind::Document,
        }),
        owner: ObservationScopeV1::Profile,
        aliases: vec![],
        occurred_at: None,
        ingested_at,
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::Unknown,
        projection_generation: ProjectionGenerationId::new(format!(
            "projection.tombstone.{operation}"
        ))
        .unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors: vec![],
        authorization: ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new(format!("scope.tombstone.{operation}"))
                .unwrap(),
            privacy_domain_id: PrivacyDomainId::new(format!("privacy.tombstone.{operation}"))
                .unwrap(),
            access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
            capability_id: CapabilityId::new(format!("capability.tombstone.{operation}")).unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
        },
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new(format!("retention.tombstone.{operation}")).unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

fn fact_payload(content: &str, operation: &str) -> FactPayloadV1 {
    let tags = vec!["tombstone".to_owned()];
    let entities = vec!["memory-v2".to_owned()];
    let metadata = serde_json::json!({ "source": operation });
    let material = serde_json::json!({
        "content": content,
        "category": "project",
        "tags": tags,
        "entities": entities,
        "metadata": metadata,
    });
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            id::<SanitizationReceiptId>(&format!("receipt.tombstone.{operation}")),
            id::<ComponentVersion>("sanitizer.tombstone.v1"),
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
        tags,
        entities,
        metadata,
        None,
        receipt,
        RetentionClass::new(format!("retention.tombstone.{operation}")).unwrap(),
    )
    .unwrap()
}

async fn commit_fact(
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    operation: &str,
    at: UtcMicros,
) -> CommittedFact {
    let identity = FactIdentityMaterialV1::new(
        owner(),
        FactIdentitySourceV1::Application {
            operation_id: id(&format!("operation.tombstone.{operation}")),
        },
    )
    .unwrap();
    let fact_id = FactId::derive(&identity).unwrap();
    let anchor = evidence_anchor(operation, at);
    let anchor_id = anchor.anchor_id().clone();
    let evidence = FactEvidenceRefV1::new(
        fact_id.clone(),
        anchor_id.clone(),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    let content = format!("tombstone secret payload {operation}");
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner(),
        FactAssertionKindV1::Initial,
        fact_payload(&content, operation),
        vec![evidence],
        at,
        None,
    )
    .unwrap();
    let recorded = FactLineageEventV1::new(
        fact_id.clone(),
        owner(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        at,
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(
        fact_id.clone(),
        owner(),
        Some(assertion),
        vec![recorded],
        vec![anchor],
        vec![],
        None,
    )
    .unwrap()
    .with_identity_material(identity)
    .unwrap();
    let receipt = match memory.commit_fact(batch, &write_control()).await.unwrap() {
        FactCommitOutcome::Committed(receipt) => receipt,
        outcome => panic!("initial fact commit must commit, got {outcome:?}"),
    };
    CommittedFact {
        fact_id,
        anchor_id,
        content,
        last_event_id: receipt.last_event_id().clone(),
    }
}

async fn change_payload_access(
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    fact: &mut CommittedFact,
    current: PayloadAccessState,
    at: UtcMicros,
) {
    let event = FactLineageEventV1::new(
        fact.fact_id.clone(),
        owner(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current,
        },
        at,
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(
        fact.fact_id.clone(),
        owner(),
        None,
        vec![event],
        vec![],
        vec![],
        Some(fact.last_event_id.clone()),
    )
    .unwrap();
    let receipt = match memory.commit_fact(batch, &write_control()).await.unwrap() {
        FactCommitOutcome::Committed(receipt) => receipt,
        outcome => panic!("payload access change must commit, got {outcome:?}"),
    };
    fact.last_event_id = receipt.last_event_id().clone();
}

async fn current_fact(
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    fact_id: &FactId,
) -> Option<tracedecay_store::StoredFactV1> {
    memory
        .query_fact_current(FactCurrentQuery::new(owner(), fact_id.clone()).unwrap())
        .await
        .unwrap()
}

async fn as_of_fact(
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    fact_id: &FactId,
    as_of: UtcMicros,
) -> Option<tracedecay_store::StoredFactV1> {
    memory
        .query_fact_as_of(FactAsOfQuery::new(owner(), fact_id.clone(), as_of).unwrap())
        .await
        .unwrap()
}

async fn lineage(
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    fact_id: &FactId,
) -> Vec<FactLineageEventV1> {
    memory
        .query_fact_lineage(FactLineageQuery::new(owner(), fact_id.clone(), None, 16).unwrap())
        .await
        .unwrap()
}

async fn scalar_i64(db: &Database, sql: &str, fact_id: &FactId) -> i64 {
    db.query_scalar_i64_with_text("inspect anchor tombstone fixture", sql, fact_id.as_str())
        .await
        .unwrap()
}

fn assert_no_payload_bytes(markers: &[&str], rendered: &[String]) {
    for structure in rendered {
        for marker in markers {
            assert!(
                !structure.contains(marker),
                "returned structure leaked payload bytes for {marker:?}: {structure}"
            );
        }
    }
}

#[tokio::test]
async fn retention_expiry_returns_typed_tombstone_with_lineage_and_no_payload() {
    let (db, _tmp) = make_store().await;
    let memory = MemoryApplication::new(owner(), DatabaseFactStore::new(&db)).unwrap();
    let mut fact = commit_fact(&memory, "expiry", UtcMicros(10)).await;

    let eligible = current_fact(&memory, &fact.fact_id)
        .await
        .expect("fresh fact resolves");
    assert_eq!(eligible.payload_access(), PayloadAccessState::Eligible);
    assert_eq!(
        eligible.payload().map(FactPayloadV1::content),
        Some(fact.content.as_str())
    );

    change_payload_access(
        &memory,
        &mut fact,
        PayloadAccessState::RetentionExpired,
        UtcMicros(20),
    )
    .await;

    let tombstone = current_fact(&memory, &fact.fact_id)
        .await
        .expect("expired fact resolves as a typed tombstone");
    assert_eq!(
        tombstone.payload_access(),
        PayloadAccessState::RetentionExpired
    );
    assert_eq!(tombstone.payload(), None);
    assert_eq!(tombstone.last_event_id(), &fact.last_event_id);
    assert_eq!(tombstone.trust(), Confidence::new(0.5).unwrap());

    let page = memory
        .query_current_facts(CurrentFactsQuery::new(owner(), None, 16).unwrap())
        .await
        .unwrap();
    let listed = page
        .iter()
        .find(|stored| stored.fact_id() == &fact.fact_id)
        .expect("expired fact is listed as a tombstone");
    assert_eq!(
        listed.payload_access(),
        PayloadAccessState::RetentionExpired
    );
    assert_eq!(listed.payload(), None);

    // As-of knowledge before the expiry frontier keeps the then-eligible
    // payload; at and after expiry every returned structure is payload-free.
    let before = as_of_fact(&memory, &fact.fact_id, UtcMicros(15))
        .await
        .expect("as-of before expiry resolves");
    assert_eq!(before.payload_access(), PayloadAccessState::Eligible);
    assert_eq!(
        before.payload().map(FactPayloadV1::content),
        Some(fact.content.as_str())
    );
    let after = as_of_fact(&memory, &fact.fact_id, UtcMicros(25))
        .await
        .expect("as-of after expiry resolves as a tombstone");
    assert_eq!(after.payload_access(), PayloadAccessState::RetentionExpired);
    assert_eq!(after.payload(), None);

    let lineage = lineage(&memory, &fact.fact_id).await;
    assert_eq!(lineage.len(), 2);
    assert!(matches!(
        lineage.first().map(FactLineageEventV1::kind),
        Some(FactLineageEventKindV1::AssertionRecorded { .. })
    ));
    assert_eq!(
        lineage.last().map(FactLineageEventV1::kind),
        Some(&FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::RetentionExpired,
        })
    );
    let anchor = memory
        .get_retrieval_anchor(RetrievalAnchorQuery::new(owner(), fact.anchor_id.clone()).unwrap())
        .await
        .unwrap()
        .expect("evidence anchor survives expiry");
    assert_eq!(anchor.anchor_id(), &fact.anchor_id);

    assert_no_payload_bytes(
        &[fact.content.as_str()],
        &[
            format!("{tombstone:?}"),
            format!("{listed:?}"),
            format!("{after:?}"),
            serde_json::to_string(&lineage).unwrap(),
            format!("{anchor:?}"),
        ],
    );
}

#[tokio::test]
async fn redaction_returns_typed_tombstone_with_its_own_reason() {
    let (db, _tmp) = make_store().await;
    let memory = MemoryApplication::new(owner(), DatabaseFactStore::new(&db)).unwrap();
    let mut fact = commit_fact(&memory, "redaction", UtcMicros(10)).await;

    change_payload_access(
        &memory,
        &mut fact,
        PayloadAccessState::Redacted,
        UtcMicros(20),
    )
    .await;

    let tombstone = current_fact(&memory, &fact.fact_id)
        .await
        .expect("redacted fact resolves as a typed tombstone");
    assert_eq!(tombstone.payload_access(), PayloadAccessState::Redacted);
    assert_eq!(tombstone.payload(), None);
    assert_eq!(tombstone.last_event_id(), &fact.last_event_id);

    let page = memory
        .query_current_facts(CurrentFactsQuery::new(owner(), None, 16).unwrap())
        .await
        .unwrap();
    let listed = page
        .iter()
        .find(|stored| stored.fact_id() == &fact.fact_id)
        .expect("redacted fact is listed as a tombstone");
    assert_eq!(listed.payload_access(), PayloadAccessState::Redacted);
    assert_eq!(listed.payload(), None);

    let after = as_of_fact(&memory, &fact.fact_id, UtcMicros(25))
        .await
        .expect("as-of after redaction resolves as a tombstone");
    assert_eq!(after.payload_access(), PayloadAccessState::Redacted);
    assert_eq!(after.payload(), None);

    // The redaction frontier is its own recorded reason, distinct from
    // retention expiry.
    let lineage = lineage(&memory, &fact.fact_id).await;
    assert_eq!(lineage.len(), 2);
    assert_eq!(
        lineage.last().map(FactLineageEventV1::kind),
        Some(&FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Redacted,
        })
    );

    assert_no_payload_bytes(
        &[fact.content.as_str()],
        &[
            format!("{tombstone:?}"),
            format!("{listed:?}"),
            format!("{after:?}"),
            serde_json::to_string(&lineage).unwrap(),
        ],
    );
}

#[tokio::test]
async fn deletion_is_terminal_and_the_tombstone_prevents_id_reuse() {
    let (db, _tmp) = make_store().await;
    let memory = MemoryApplication::new(owner(), DatabaseFactStore::new(&db)).unwrap();
    let mut fact = commit_fact(&memory, "deletion", UtcMicros(10)).await;

    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM memory_v2_assertion_payloads WHERE fact_id = ?1",
            &fact.fact_id,
        )
        .await,
        1,
        "the eligible fact retains exactly one payload row before deletion"
    );

    change_payload_access(
        &memory,
        &mut fact,
        PayloadAccessState::Deleted,
        UtcMicros(20),
    )
    .await;

    // Deletion physically erases the payload while the minimum safe tombstone
    // (identity row, deleted projection, and lineage) persists.
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM memory_v2_assertion_payloads WHERE fact_id = ?1",
            &fact.fact_id,
        )
        .await,
        0,
        "deletion purges every payload row"
    );
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM memory_v2_facts WHERE fact_id = ?1",
            &fact.fact_id,
        )
        .await,
        1,
        "the immutable fact identity row persists as the tombstone"
    );

    // No payload-bearing resolution survives deletion.
    assert!(current_fact(&memory, &fact.fact_id).await.is_none());
    let page = memory
        .query_current_facts(CurrentFactsQuery::new(owner(), None, 16).unwrap())
        .await
        .unwrap();
    assert!(
        page.iter().all(|stored| stored.fact_id() != &fact.fact_id),
        "a deleted fact is excluded from current listings"
    );

    // The purged payload is never resurrected: an as-of projection predating
    // the deletion reports the typed unavailable tombstone instead of bytes.
    let before = as_of_fact(&memory, &fact.fact_id, UtcMicros(15))
        .await
        .expect("as-of before deletion still resolves as a tombstone");
    assert_eq!(before.payload_access(), PayloadAccessState::Unavailable);
    assert_eq!(before.payload(), None);
    assert!(
        as_of_fact(&memory, &fact.fact_id, UtcMicros(25))
            .await
            .is_none(),
        "after deletion there is no payload-bearing as-of state"
    );

    // Deletion lineage is preserved and explains the target state.
    let lineage = lineage(&memory, &fact.fact_id).await;
    assert_eq!(lineage.len(), 2);
    assert_eq!(
        lineage.last().map(FactLineageEventV1::kind),
        Some(&FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Deleted,
        })
    );
    let anchor = memory
        .get_retrieval_anchor(RetrievalAnchorQuery::new(owner(), fact.anchor_id.clone()).unwrap())
        .await
        .unwrap()
        .expect("evidence anchor survives deletion");
    assert_eq!(anchor.anchor_id(), &fact.anchor_id);

    // Deletion is terminal: the id can never transition back to any
    // payload-bearing state, and the owner-bound identity cannot be claimed
    // by another owner.
    assert!(
        FactLineageEventV1::new(
            fact.fact_id.clone(),
            owner(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Deleted,
                current: PayloadAccessState::Eligible,
            },
            UtcMicros(30),
            None,
        )
        .is_err(),
        "a deleted fact id cannot be reactivated"
    );
    let foreign_owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.tombstone.foreign").unwrap(),
    };
    assert!(
        FactLineageEventV1::new(
            fact.fact_id.clone(),
            foreign_owner,
            FactLineageEventKindV1::AssertionRecorded {
                assertion_id: id("assertion.tombstone.foreign"),
            },
            UtcMicros(30),
            None,
        )
        .is_err(),
        "a deleted fact id cannot be reused under another owner"
    );

    assert_no_payload_bytes(
        &[fact.content.as_str()],
        &[
            format!("{before:?}"),
            format!("{page:?}"),
            serde_json::to_string(&lineage).unwrap(),
            format!("{anchor:?}"),
        ],
    );
}

#[tokio::test]
async fn terminal_facts_stay_tombstoned_or_excluded_in_current_and_as_of_results() {
    let (db, _tmp) = make_store().await;
    let memory = MemoryApplication::new(owner(), DatabaseFactStore::new(&db)).unwrap();
    let mut expired = commit_fact(&memory, "mixed-expired", UtcMicros(10)).await;
    let mut redacted = commit_fact(&memory, "mixed-redacted", UtcMicros(10)).await;
    let mut deleted = commit_fact(&memory, "mixed-deleted", UtcMicros(10)).await;
    let control = commit_fact(&memory, "mixed-control", UtcMicros(10)).await;

    change_payload_access(
        &memory,
        &mut expired,
        PayloadAccessState::RetentionExpired,
        UtcMicros(20),
    )
    .await;
    change_payload_access(
        &memory,
        &mut redacted,
        PayloadAccessState::Redacted,
        UtcMicros(20),
    )
    .await;
    change_payload_access(
        &memory,
        &mut deleted,
        PayloadAccessState::Deleted,
        UtcMicros(20),
    )
    .await;

    let page = memory
        .query_current_facts(CurrentFactsQuery::new(owner(), None, 16).unwrap())
        .await
        .unwrap();
    assert_eq!(page.len(), 3, "deleted facts leave the current listing");
    let listed = |fact_id: &FactId| page.iter().find(|stored| stored.fact_id() == fact_id);
    let listed_expired = listed(&expired.fact_id).expect("expired fact is tombstoned");
    assert_eq!(
        listed_expired.payload_access(),
        PayloadAccessState::RetentionExpired
    );
    assert_eq!(listed_expired.payload(), None);
    let listed_redacted = listed(&redacted.fact_id).expect("redacted fact is tombstoned");
    assert_eq!(
        listed_redacted.payload_access(),
        PayloadAccessState::Redacted
    );
    assert_eq!(listed_redacted.payload(), None);
    let listed_control = listed(&control.fact_id).expect("eligible control stays listed");
    assert_eq!(
        listed_control.payload_access(),
        PayloadAccessState::Eligible
    );
    assert_eq!(
        listed_control.payload().map(FactPayloadV1::content),
        Some(control.content.as_str())
    );

    let expired_after = as_of_fact(&memory, &expired.fact_id, UtcMicros(25))
        .await
        .expect("expired fact resolves as a tombstone");
    assert_eq!(
        expired_after.payload_access(),
        PayloadAccessState::RetentionExpired
    );
    assert_eq!(expired_after.payload(), None);
    let redacted_after = as_of_fact(&memory, &redacted.fact_id, UtcMicros(25))
        .await
        .expect("redacted fact resolves as a tombstone");
    assert_eq!(
        redacted_after.payload_access(),
        PayloadAccessState::Redacted
    );
    assert_eq!(redacted_after.payload(), None);
    assert!(
        as_of_fact(&memory, &deleted.fact_id, UtcMicros(25))
            .await
            .is_none(),
        "deleted facts have no post-deletion as-of state"
    );

    let expired_lineage = lineage(&memory, &expired.fact_id).await;
    let redacted_lineage = lineage(&memory, &redacted.fact_id).await;
    let deleted_lineage = lineage(&memory, &deleted.fact_id).await;
    assert_eq!(expired_lineage.len(), 2);
    assert_eq!(redacted_lineage.len(), 2);
    assert_eq!(deleted_lineage.len(), 2);

    assert_no_payload_bytes(
        &[
            expired.content.as_str(),
            redacted.content.as_str(),
            deleted.content.as_str(),
        ],
        &[
            format!("{listed_expired:?}"),
            format!("{listed_redacted:?}"),
            format!("{expired_after:?}"),
            format!("{redacted_after:?}"),
            serde_json::to_string(&expired_lineage).unwrap(),
            serde_json::to_string(&redacted_lineage).unwrap(),
            serde_json::to_string(&deleted_lineage).unwrap(),
        ],
    );
}
