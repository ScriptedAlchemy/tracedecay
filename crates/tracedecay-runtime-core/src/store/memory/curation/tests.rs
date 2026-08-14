use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_domain::{
    ActorId, Confidence, DomainError, FactAssertionKindV1, FactAssertionV1, FactCurationActionV1,
    FactEventId, FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1,
    FactLineageEventV1, FactOwnerV1, FactRelationKindV1, FactRelationProvenanceV1, FactRelationV1,
    ProvenanceId, RunId, UtcMicros,
};
use tracedecay_store::{
    FactCommitConflict, FactCommitOutcome, FactCommitReceipt, FactReadControl, FactStore,
    FactStoreError, FactWriteBatch, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationOperationEffectV1, ProjectMemoryFactCurationOperationV1,
    ProjectMemoryFactCurationReceiptV1, ProjectMemoryFactCurationReviewRefV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactLinkV1, ProjectMemoryFactNormalizeTagsV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactStore,
};

use crate::db::engine::params;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use crate::store::memory::crud::{initial_batch, load_current_fact_tx, sanitize_payload};
use crate::store::memory::graph::relation_kinds_from_canonical_source_for_test;
use crate::store::memory::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, row_optional_string, row_string,
};
use crate::store::memory::{DatabaseFactStore, FactWriteControl};

use super::apply::{apply_project_memory_fact_curation_tx, curation_receipt_from_value};

mod duplicate_identity_tests;
mod graph_source_tests;
mod replay_conflict_tests;

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredOperationReceipt {
    operation_kind: String,
    request_digest: String,
    fact_id: Option<FactId>,
    event_id: Option<FactEventId>,
    receipt: Value,
}

struct Fixture {
    db: Database,
    path: PathBuf,
    owner: FactOwnerV1,
    control: FactWriteControl,
    _temp: TempDir,
}

impl Fixture {
    async fn new(label: &str) -> Self {
        let temp = tempfile::tempdir().expect("curation fixture root");
        let path = temp.path().join(format!("{label}.db"));
        let authority =
            DatabaseAuthority::acquire_test(&path, "canonical relation curation fixture")
                .expect("database authority");
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("canonical relation database");
        Self {
            db,
            path,
            owner: FactOwnerV1::Profile,
            control: accepting_write_control(),
            _temp: temp,
        }
    }

    async fn seed_commit(&self, label: &str, occurred_at: i64) -> FactCommitReceipt {
        let operation_id = provenance_id(&format!("fixture.seed.{label}"));
        let sanitized = sanitize_payload(
            &format!("canonical relation fixture {label}"),
            tracedecay_domain::FactCategoryV1::General,
            &[],
            &[],
            &json!({"fixture": label}),
            None,
        )
        .expect("sanitize seed payload")
        .expect("fixture payload is durable");
        let batch = initial_batch(
            &self.owner,
            &operation_id,
            sanitized.payload,
            sanitized.access,
            Confidence::new(0.5).expect("default confidence"),
            None,
            UtcMicros(occurred_at),
        )
        .expect("canonical seed batch");
        let outcome = DatabaseFactStore::new(&self.db)
            .commit_fact(batch, &self.control)
            .await
            .expect("commit canonical seed fact");
        let FactCommitOutcome::Committed(receipt) = outcome else {
            panic!("fixture seed must commit exactly once");
        };
        receipt
    }

    async fn seed(&self, label: &str, occurred_at: i64) -> FactId {
        self.seed_commit(label, occurred_at).await.fact_id().clone()
    }

    async fn reopen(self) -> Self {
        let Self {
            db,
            path,
            owner,
            control,
            _temp,
        } = self;
        drop(db);
        let authority = DatabaseAuthority::acquire_test(&path, "reopen curation fixture")
            .expect("reopen database authority");
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Existing)
                .await
                .expect("reopen canonical curation database");
        Self {
            db,
            path,
            owner,
            control,
            _temp,
        }
    }
}

fn accepting_write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

fn accepting_read_control() -> FactReadControl {
    FactReadControl::new(Arc::new(|| false))
}

fn primary_commit(effect: &ProjectMemoryFactCurationOperationEffectV1) -> &FactCommitReceipt {
    effect.primary_commit().expect("committed curation effect")
}

fn replay_event(receipt: &ProjectMemoryFactCurationReceiptV1) -> &FactEventId {
    receipt.replay_event_id().expect("curation replay event")
}

fn provenance_id(value: &str) -> ProvenanceId {
    ProvenanceId::new(value.to_owned()).expect("canonical provenance id")
}

fn fact_id_for(owner: &FactOwnerV1, operation: &str) -> FactId {
    FactId::derive(
        &FactIdentityMaterialV1::new(
            owner.clone(),
            FactIdentitySourceV1::Application {
                operation_id: provenance_id(operation),
            },
        )
        .expect("fact identity material"),
    )
    .expect("derived fact id")
}

fn relation_provenance(label: &str) -> FactRelationProvenanceV1 {
    let material = json!({
        "source_label": label,
        "metadata": {"fixture": "canonical-link"},
    });
    let MemoryFactSanitizationV1::Durable { payload, receipt } =
        sanitize_memory_fact_payload(material).expect("sanitize relation provenance")
    else {
        panic!("relation fixture provenance must be durable");
    };
    let source_label = payload
        .get("source_label")
        .and_then(Value::as_str)
        .expect("sanitized relation source label")
        .to_owned();
    let metadata = payload
        .get("metadata")
        .cloned()
        .expect("sanitized relation metadata");
    FactRelationProvenanceV1::new(source_label, metadata, receipt)
        .expect("receipt-bound relation provenance")
}

fn relation(
    owner: &FactOwnerV1,
    source: &FactId,
    target: &FactId,
    evidence: Vec<FactId>,
    kind: FactRelationKindV1,
) -> FactRelationV1 {
    FactRelationV1::new(
        owner.clone(),
        source.clone(),
        target.clone(),
        kind,
        evidence,
        Confidence::new(0.8).expect("relation confidence"),
        relation_provenance("curation.fixture"),
    )
    .expect("canonical fact relation")
}

async fn reviewed_ref(
    db: &Database,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> ProjectMemoryFactCurationReviewRefV1 {
    let transaction = db
        .begin_memory_write_transaction(PROJECT_MEMORY_WRITE_OPERATION)
        .await
        .expect("begin reviewed fact transaction");
    let key = OwnerKey::new(owner).expect("reviewed fact owner key");
    let fact = load_current_fact_tx(&transaction, &key, owner, fact_id)
        .await
        .expect("load reviewed fact")
        .expect("reviewed fact exists");
    transaction
        .rollback()
        .await
        .expect("close reviewed fact read");
    ProjectMemoryFactCurationReviewRefV1::new(
        ProjectMemoryFactIdV1::new(owner.clone(), fact_id.clone()).unwrap(),
        fact.last_event_id().clone(),
    )
}

async fn request(
    db: &Database,
    owner: &FactOwnerV1,
    operation_id: &str,
    relation: FactRelationV1,
) -> ProjectMemoryFactCurationBatchV1 {
    let source = reviewed_ref(db, owner, relation.source_fact_id()).await;
    let target = reviewed_ref(db, owner, relation.target_fact_id()).await;
    let mut evidence = Vec::new();
    for fact_id in relation.evidence_fact_ids() {
        evidence.push(reviewed_ref(db, owner, fact_id).await);
    }
    ProjectMemoryFactCurationBatchV1::new(
        owner.clone(),
        provenance_id(operation_id),
        None,
        Confidence::new(0.5).expect("minimum confidence"),
        vec![ProjectMemoryFactCurationOperationV1::LinkFacts(
            ProjectMemoryFactLinkV1::new(relation, source, target, evidence)
                .expect("link operation"),
        )],
    )
    .expect("canonical relation curation request")
}

async fn normalize_request(
    db: &Database,
    owner: &FactOwnerV1,
    operation_id: &str,
    actor: Option<ActorId>,
    fact_id: &FactId,
    tags: Vec<String>,
    evidence_fact_ids: Vec<FactId>,
    confidence: f64,
) -> ProjectMemoryFactCurationBatchV1 {
    let mut evidence = Vec::new();
    for fact_id in evidence_fact_ids {
        evidence.push(reviewed_ref(db, owner, &fact_id).await);
    }
    ProjectMemoryFactCurationBatchV1::new(
        owner.clone(),
        provenance_id(operation_id),
        actor,
        Confidence::new(0.5).expect("minimum confidence"),
        vec![ProjectMemoryFactCurationOperationV1::NormalizeTags(
            ProjectMemoryFactNormalizeTagsV1::new(
                reviewed_ref(db, owner, fact_id).await,
                tags,
                evidence,
                Confidence::new(confidence).expect("normalization confidence"),
            )
            .expect("canonical tag normalization operation"),
        )],
    )
    .expect("canonical tag normalization request")
}

fn correction_assertion(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    supersedes: &tracedecay_domain::FactAssertionId,
    actor: Option<ActorId>,
    asserted_at: UtcMicros,
    tags: &[String],
) -> FactAssertionV1 {
    let sanitized = sanitize_payload(
        "canonical normalized-tag correction",
        tracedecay_domain::FactCategoryV1::General,
        tags,
        &[],
        &json!({"fixture": "normalized-tag-correction"}),
        None,
    )
    .expect("sanitize normalized-tag correction")
    .expect("normalized-tag correction is durable");
    FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Correction {
            supersedes: supersedes.clone(),
        },
        sanitized.payload,
        Vec::new(),
        asserted_at,
        actor,
    )
    .expect("typed normalized-tag correction assertion")
}

async fn lineage_events_for_fact(
    db: &Database,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> Vec<FactLineageEventV1> {
    let key = OwnerKey::new(owner).expect("owner key");
    let writer = db
        .writer_connection("read canonical fact lineage")
        .await
        .expect("writer connection");
    let mut rows = writer
        .query_engine(
            "SELECT event_json
             FROM memory_v2_lineage_events
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
             ORDER BY event_sequence",
            params![key.kind, key.project_id.as_str(), fact_id.as_str()],
        )
        .await
        .expect("query canonical fact lineage");
    let mut events = Vec::new();
    while let Some(row) = rows.next().await.expect("next lineage row") {
        events.push(
            serde_json::from_str::<FactLineageEventV1>(
                &row_string(&row, 0, "read canonical fact lineage").expect("lineage event json"),
            )
            .expect("typed lineage event"),
        );
    }
    events
}

async fn current_fact_tags(db: &Database, owner: &FactOwnerV1, fact_id: &FactId) -> Vec<String> {
    let transaction = db
        .begin_memory_write_transaction(PROJECT_MEMORY_WRITE_OPERATION)
        .await
        .expect("begin current fact read transaction");
    let key = OwnerKey::new(owner).expect("current fact owner key");
    let fact = load_current_fact_tx(&transaction, &key, owner, fact_id)
        .await
        .expect("load current fact")
        .expect("current fact exists");
    transaction
        .rollback()
        .await
        .expect("close current fact read transaction");
    fact.payload()
        .expect("current fact payload remains eligible")
        .tags()
        .to_vec()
}

async fn linked_events(db: &Database, owner: &FactOwnerV1) -> Vec<FactLineageEventV1> {
    let key = OwnerKey::new(owner).expect("owner key");
    let writer = db
        .writer_connection("read canonical linked events")
        .await
        .expect("writer connection");
    let mut rows = writer
        .query_engine(
            "SELECT event_json
             FROM memory_v2_lineage_events
             WHERE owner_kind = ?1 AND project_id = ?2
             ORDER BY event_sequence",
            params![key.kind, key.project_id.as_str()],
        )
        .await
        .expect("query canonical lineage");
    let mut events = Vec::new();
    while let Some(row) = rows.next().await.expect("next lineage row") {
        let event = serde_json::from_str::<FactLineageEventV1>(
            &row_string(&row, 0, "read canonical linked events").expect("lineage event json"),
        )
        .expect("typed lineage event");
        if matches!(
            event.kind(),
            FactLineageEventKindV1::Curated {
                action: FactCurationActionV1::Linked { .. },
                ..
            }
        ) {
            events.push(event);
        }
    }
    events
}

async fn operation_receipts(
    db: &Database,
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
) -> Vec<StoredOperationReceipt> {
    let key = OwnerKey::new(owner).expect("owner key");
    let writer = db
        .writer_connection("read canonical curation receipts")
        .await
        .expect("writer connection");
    let mut rows = writer
        .query_engine(
            "SELECT operation_kind, request_digest, fact_id, event_id, receipt_json
             FROM memory_v2_operation_receipts
             WHERE owner_kind = ?1 AND project_id = ?2 AND operation_id = ?3
             ORDER BY recorded_at, operation_id",
            params![key.kind, key.project_id.as_str(), operation_id.as_str(),],
        )
        .await
        .expect("query curation operation receipt");
    let mut receipts = Vec::new();
    while let Some(row) = rows.next().await.expect("next operation receipt") {
        receipts.push(StoredOperationReceipt {
            operation_kind: row_string(&row, 0, "read canonical curation receipts")
                .expect("operation kind"),
            request_digest: row_string(&row, 1, "read canonical curation receipts")
                .expect("request digest"),
            fact_id: row_optional_string(&row, 2, "read canonical curation receipts")
                .expect("optional receipt fact id")
                .map(FactId::new)
                .transpose()
                .expect("typed receipt fact id"),
            event_id: row_optional_string(&row, 3, "read canonical curation receipts")
                .expect("optional receipt event id")
                .map(FactEventId::new)
                .transpose()
                .expect("typed receipt event id"),
            receipt: serde_json::from_str(
                &row_string(&row, 4, "read canonical curation receipts").expect("receipt json"),
            )
            .expect("typed operation receipt"),
        });
    }
    receipts
}

async fn rebind_curation_operation_receipt(
    db: &Database,
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
    event_id: &FactEventId,
    receipt: &Value,
) {
    let key = OwnerKey::new(owner).expect("receipt tamper owner key");
    let transaction = db
        .begin_memory_write_transaction("tamper curation replay receipt")
        .await
        .expect("begin curation receipt tamper");
    transaction
        .execute_batch("DROP TRIGGER memory_v2_operation_receipts_no_update;")
        .await
        .expect("disable receipt immutability in isolated curation fixture");
    assert_eq!(
        transaction
            .execute(
                "UPDATE memory_v2_operation_receipts
                 SET event_id = ?1, receipt_json = ?2
                 WHERE owner_kind = ?3 AND project_id = ?4 AND operation_id = ?5",
                params![
                    event_id.as_str(),
                    serde_json::to_string(receipt).expect("serialize rebound receipt"),
                    key.kind,
                    key.project_id.as_str(),
                    operation_id.as_str(),
                ],
            )
            .await
            .expect("rebind curation receipt"),
        1
    );
    transaction
        .execute_batch(
            "CREATE TRIGGER memory_v2_operation_receipts_no_update
             BEFORE UPDATE ON memory_v2_operation_receipts BEGIN
                 SELECT RAISE(ABORT, 'memory_v2 operation receipts are immutable');
             END;",
        )
        .await
        .expect("restore curation receipt immutability");
    transaction
        .commit()
        .await
        .expect("commit curation receipt rebind");
}

async fn assert_no_link_or_receipt(fixture: &Fixture, operation_id: &ProvenanceId) {
    assert!(linked_events(&fixture.db, &fixture.owner).await.is_empty());
    assert!(
        operation_receipts(&fixture.db, &fixture.owner, operation_id)
            .await
            .is_empty()
    );
}

fn json_contains_key(value: &Value, forbidden: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key(forbidden)
                || object
                    .values()
                    .any(|value| json_contains_key(value, forbidden))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| json_contains_key(value, forbidden)),
        _ => false,
    }
}

#[tokio::test]
async fn normalized_tag_provenance_survives_reopen_and_exact_replay_without_a_graph_edge() {
    let fixture = Fixture::new("normalize-reopen-replay").await;
    let normalized = fixture.seed("normalize-subject", 10).await;
    let evidence = fixture.seed("normalize-evidence", 20).await;
    let operation_id = provenance_id("fixture.normalize.reopen-replay");
    let request = normalize_request(
        &fixture.db,
        &fixture.owner,
        operation_id.as_str(),
        None,
        &normalized,
        vec!["Cache Policy".to_owned(), "canonical-tag".to_owned()],
        vec![evidence.clone(), normalized.clone()],
        0.91,
    )
    .await;
    let first = DatabaseFactStore::new(&fixture.db)
        .apply_project_memory_fact_curation(request.clone(), &fixture.control)
        .await
        .expect("commit tag normalization");
    let first_events = lineage_events_for_fact(&fixture.db, &fixture.owner, &normalized).await;
    let first_receipts = operation_receipts(&fixture.db, &fixture.owner, &operation_id).await;
    let normalized_actions = first_events
        .iter()
        .filter_map(|event| match event.kind() {
            FactLineageEventKindV1::Curated {
                action:
                    FactCurationActionV1::TagsNormalized {
                        evidence_fact_ids,
                        confidence,
                    },
                evidence_ids,
            } => Some((evidence_fact_ids, *confidence, evidence_ids)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(normalized_actions.len(), 1);
    let (persisted_evidence, persisted_confidence, generic_evidence) = normalized_actions[0];
    assert_eq!(
        persisted_confidence,
        Confidence::new(0.91).expect("confidence")
    );
    assert!(generic_evidence.is_empty());
    assert_eq!(persisted_evidence.len(), 2);
    assert!(persisted_evidence.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(persisted_evidence.contains(&normalized));
    assert!(persisted_evidence.contains(&evidence));
    assert_eq!(
        primary_commit(&first.operation_effects()[0])
            .committed_event_ids()
            .len(),
        2
    );
    let mut reversed_event_tail = first_receipts[0].receipt.clone();
    reversed_event_tail["operation_effects"][0]["commit"]["committed_event_ids"]
        .as_array_mut()
        .expect("committed event array")
        .reverse();
    assert!(curation_receipt_from_value(&reversed_event_tail).is_err());
    assert_eq!(
        current_fact_tags(&fixture.db, &fixture.owner, &normalized).await,
        vec!["cache_policy".to_owned(), "canonical_tag".to_owned()]
    );
    assert!(linked_events(&fixture.db, &fixture.owner).await.is_empty());
    assert!(
        relation_kinds_from_canonical_source_for_test(
            &fixture.db,
            &fixture.owner,
            &accepting_read_control(),
        )
        .await
        .expect("reconstruct graph without normalized-tag relation")
        .is_empty()
    );

    let fixture = fixture.reopen().await;
    assert_eq!(
        lineage_events_for_fact(&fixture.db, &fixture.owner, &normalized).await,
        first_events
    );
    assert_eq!(
        operation_receipts(&fixture.db, &fixture.owner, &operation_id).await,
        first_receipts
    );
    assert_eq!(
        current_fact_tags(&fixture.db, &fixture.owner, &normalized).await,
        vec!["cache_policy".to_owned(), "canonical_tag".to_owned()]
    );
    let replay = DatabaseFactStore::new(&fixture.db)
        .apply_project_memory_fact_curation(request, &fixture.control)
        .await
        .expect("replay tag normalization after reopen");
    assert!(replay.replayed());
    assert_eq!(
        serde_json::to_value(replay).expect("serialize replay receipt"),
        serde_json::to_value(first).expect("serialize committed receipt")
    );
    assert_eq!(
        lineage_events_for_fact(&fixture.db, &fixture.owner, &normalized).await,
        first_events
    );
    assert!(linked_events(&fixture.db, &fixture.owner).await.is_empty());
}

#[tokio::test]
async fn stale_reviewed_normalize_evidence_rejects_before_any_write() {
    let fixture = Fixture::new("normalize-stale-reviewed-evidence").await;
    let target = fixture.seed("stale-normalize-target", 10).await;
    let evidence = fixture.seed("stale-normalize-evidence", 20).await;
    let operation_id = provenance_id("fixture.normalize.stale-reviewed-evidence");
    let target_ref = reviewed_ref(&fixture.db, &fixture.owner, &target).await;
    let stale_event = FactEventId::new("event.stale-reviewed-evidence".to_owned()).unwrap();
    let evidence_ref = ProjectMemoryFactCurationReviewRefV1::new(
        ProjectMemoryFactIdV1::new(fixture.owner.clone(), evidence.clone()).unwrap(),
        stale_event.clone(),
    );
    let request = ProjectMemoryFactCurationBatchV1::new(
        fixture.owner.clone(),
        operation_id.clone(),
        None,
        Confidence::new(0.5).unwrap(),
        vec![ProjectMemoryFactCurationOperationV1::NormalizeTags(
            ProjectMemoryFactNormalizeTagsV1::new(
                target_ref,
                vec!["canonical".to_owned()],
                vec![evidence_ref],
                Confidence::new(0.9).unwrap(),
            )
            .unwrap(),
        )],
    )
    .unwrap();
    let before_target = lineage_events_for_fact(&fixture.db, &fixture.owner, &target).await;
    let before_evidence = lineage_events_for_fact(&fixture.db, &fixture.owner, &evidence).await;

    assert!(matches!(
        DatabaseFactStore::new(&fixture.db)
            .apply_project_memory_fact_curation(request, &fixture.control)
            .await,
        Err(FactStoreError::CommitConflict {
            conflict: FactCommitConflict::LastEventMismatch {
                expected: Some(expected),
                actual: Some(_),
            },
        }) if expected == stale_event
    ));
    assert_eq!(
        lineage_events_for_fact(&fixture.db, &fixture.owner, &target).await,
        before_target
    );
    assert_eq!(
        lineage_events_for_fact(&fixture.db, &fixture.owner, &evidence).await,
        before_evidence
    );
    assert!(
        operation_receipts(&fixture.db, &fixture.owner, &operation_id)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn replay_rejects_receipt_rebound_to_different_normalized_tags() {
    let fixture = Fixture::new("normalize-replay-tag-rebind").await;
    let normalized = fixture.seed("rebound-subject", 10).await;
    let evidence = fixture.seed("rebound-evidence", 20).await;
    let first_operation_id = provenance_id("fixture.normalize.rebound-first");
    let first_request = normalize_request(
        &fixture.db,
        &fixture.owner,
        first_operation_id.as_str(),
        None,
        &normalized,
        vec!["first-tag".to_owned()],
        vec![evidence.clone()],
        0.9,
    )
    .await;
    let store = DatabaseFactStore::new(&fixture.db);
    let first = store
        .apply_project_memory_fact_curation(first_request.clone(), &fixture.control)
        .await
        .expect("commit first normalized tags");
    let second_request = normalize_request(
        &fixture.db,
        &fixture.owner,
        "fixture.normalize.rebound-second",
        None,
        &normalized,
        vec!["second-tag".to_owned()],
        vec![evidence],
        0.9,
    )
    .await;
    let second = store
        .apply_project_memory_fact_curation(second_request, &fixture.control)
        .await
        .expect("commit different normalized tags");
    let mut rebound = serde_json::to_value(&first).expect("serialize first curation receipt");
    let second_value = serde_json::to_value(&second).expect("serialize second curation receipt");
    rebound["operation_effects"] = second_value["operation_effects"].clone();
    rebound["replay_event_id"] = second_value["replay_event_id"].clone();
    rebind_curation_operation_receipt(
        &fixture.db,
        &fixture.owner,
        &first_operation_id,
        replay_event(&second),
        &rebound,
    )
    .await;
    let before_events = lineage_events_for_fact(&fixture.db, &fixture.owner, &normalized).await;

    assert!(matches!(
        store
            .apply_project_memory_fact_curation(first_request, &fixture.control)
            .await,
        Err(FactStoreError::Storage { .. })
    ));
    assert_eq!(
        lineage_events_for_fact(&fixture.db, &fixture.owner, &normalized).await,
        before_events
    );
}

#[tokio::test]
async fn replay_rejects_a_self_consistent_reordered_multi_effect_receipt() {
    let fixture = Fixture::new("link-replay-reordered-effects").await;
    let source = fixture.seed("reorder-source", 10).await;
    let first_target = fixture.seed("reorder-first-target", 20).await;
    let second_target = fixture.seed("reorder-second-target", 30).await;
    let evidence = fixture.seed("reorder-evidence", 40).await;
    let operation_id = provenance_id("fixture.link.reordered-effects");
    let source_ref = reviewed_ref(&fixture.db, &fixture.owner, &source).await;
    let first_target_ref = reviewed_ref(&fixture.db, &fixture.owner, &first_target).await;
    let second_target_ref = reviewed_ref(&fixture.db, &fixture.owner, &second_target).await;
    let evidence_ref = reviewed_ref(&fixture.db, &fixture.owner, &evidence).await;
    let request = ProjectMemoryFactCurationBatchV1::new(
        fixture.owner.clone(),
        operation_id.clone(),
        None,
        Confidence::new(0.5).unwrap(),
        vec![
            ProjectMemoryFactCurationOperationV1::LinkFacts(
                ProjectMemoryFactLinkV1::new(
                    relation(
                        &fixture.owner,
                        &source,
                        &first_target,
                        vec![evidence.clone()],
                        FactRelationKindV1::Supports,
                    ),
                    source_ref.clone(),
                    first_target_ref,
                    vec![evidence_ref.clone()],
                )
                .unwrap(),
            ),
            ProjectMemoryFactCurationOperationV1::LinkFacts(
                ProjectMemoryFactLinkV1::new(
                    relation(
                        &fixture.owner,
                        &source,
                        &second_target,
                        vec![evidence],
                        FactRelationKindV1::DerivedFrom,
                    ),
                    source_ref,
                    second_target_ref,
                    vec![evidence_ref],
                )
                .unwrap(),
            ),
        ],
    )
    .unwrap();
    let store = DatabaseFactStore::new(&fixture.db);
    let committed = store
        .apply_project_memory_fact_curation(request.clone(), &fixture.control)
        .await
        .expect("commit ordered relation effects");
    let before_events = linked_events(&fixture.db, &fixture.owner).await;
    let mut reordered = serde_json::to_value(&committed).unwrap();
    reordered["operation_effects"]
        .as_array_mut()
        .unwrap()
        .reverse();
    reordered["changed_fact_ids"] = json!([
        source.as_str(),
        second_target.as_str(),
        first_target.as_str()
    ]);
    let reordered_event = primary_commit(&committed.operation_effects()[1])
        .last_event_id()
        .clone();
    reordered["replay_event_id"] = json!(reordered_event.as_str());
    assert!(curation_receipt_from_value(&reordered).is_ok());
    rebind_curation_operation_receipt(
        &fixture.db,
        &fixture.owner,
        &operation_id,
        &reordered_event,
        &reordered,
    )
    .await;

    assert!(matches!(
        store
            .apply_project_memory_fact_curation(request, &fixture.control)
            .await,
        Err(FactStoreError::Storage { .. })
    ));
    assert_eq!(
        linked_events(&fixture.db, &fixture.owner).await,
        before_events
    );
}

#[tokio::test]
async fn replay_rejects_a_valid_relation_effect_rebound_to_another_target() {
    let fixture = Fixture::new("link-replay-target-rebind").await;
    let source = fixture.seed("target-rebind-source", 10).await;
    let original_target = fixture.seed("target-rebind-original", 20).await;
    let substituted_target = fixture.seed("target-rebind-substituted", 30).await;
    let evidence = fixture.seed("target-rebind-evidence", 40).await;
    let original_operation_id = provenance_id("fixture.link.target-rebind-original");
    let original_request = request(
        &fixture.db,
        &fixture.owner,
        original_operation_id.as_str(),
        relation(
            &fixture.owner,
            &source,
            &original_target,
            vec![evidence.clone()],
            FactRelationKindV1::Supports,
        ),
    )
    .await;
    let store = DatabaseFactStore::new(&fixture.db);
    let original = store
        .apply_project_memory_fact_curation(original_request.clone(), &fixture.control)
        .await
        .unwrap();
    let substitute_request = request(
        &fixture.db,
        &fixture.owner,
        "fixture.link.target-rebind-substitute",
        relation(
            &fixture.owner,
            &source,
            &substituted_target,
            vec![evidence],
            FactRelationKindV1::Supports,
        ),
    )
    .await;
    let substitute = store
        .apply_project_memory_fact_curation(substitute_request, &fixture.control)
        .await
        .unwrap();
    let mut rebound = serde_json::to_value(&original).unwrap();
    let substitute_value = serde_json::to_value(&substitute).unwrap();
    rebound["operation_effects"] = substitute_value["operation_effects"].clone();
    rebound["replay_event_id"] = substitute_value["replay_event_id"].clone();
    rebound["changed_fact_ids"] = substitute_value["changed_fact_ids"].clone();
    assert!(curation_receipt_from_value(&rebound).is_ok());
    rebind_curation_operation_receipt(
        &fixture.db,
        &fixture.owner,
        &original_operation_id,
        replay_event(&substitute),
        &rebound,
    )
    .await;
    let before_events = linked_events(&fixture.db, &fixture.owner).await;

    assert!(matches!(
        store
            .apply_project_memory_fact_curation(original_request, &fixture.control)
            .await,
        Err(FactStoreError::Storage { .. })
    ));
    assert_eq!(
        linked_events(&fixture.db, &fixture.owner).await,
        before_events
    );
}

#[tokio::test]
async fn same_transaction_rollback_leaves_no_linked_event_or_operation_receipt() {
    let fixture = Fixture::new("link-rollback").await;
    let source = fixture.seed("rollback-source", 10).await;
    let target = fixture.seed("rollback-target", 20).await;
    let evidence = fixture.seed("rollback-evidence", 30).await;
    let operation_id = provenance_id("fixture.link.rollback");
    let request = request(
        &fixture.db,
        &fixture.owner,
        operation_id.as_str(),
        relation(
            &fixture.owner,
            &source,
            &target,
            vec![evidence],
            FactRelationKindV1::Supports,
        ),
    )
    .await
    .with_automation_run_id(RunId::new("run.curation-replay").unwrap())
    .expect("automation run binding");

    let transaction = fixture
        .db
        .begin_memory_write_transaction(PROJECT_MEMORY_WRITE_OPERATION)
        .await
        .expect("begin relation transaction");
    apply_project_memory_fact_curation_tx(&transaction, &request)
        .await
        .expect("apply relation inside transaction");
    transaction
        .rollback()
        .await
        .expect("rollback relation transaction");

    assert_no_link_or_receipt(&fixture, &operation_id).await;
}

#[tokio::test]
async fn exact_operation_replay_reports_replayed_with_stable_canonical_material() {
    let fixture = Fixture::new("link-replay").await;
    let source = fixture.seed("replay-source", 10).await;
    let target = fixture.seed("replay-target", 20).await;
    let evidence = fixture.seed("replay-evidence", 30).await;
    let operation_id = provenance_id("fixture.link.replay");
    let request = request(
        &fixture.db,
        &fixture.owner,
        operation_id.as_str(),
        relation(
            &fixture.owner,
            &source,
            &target,
            vec![evidence],
            FactRelationKindV1::Supports,
        ),
    )
    .await;
    let store = DatabaseFactStore::new(&fixture.db);

    let first = store
        .apply_project_memory_fact_curation(request.clone(), &fixture.control)
        .await
        .expect("first relation commit");
    let first_events = linked_events(&fixture.db, &fixture.owner).await;
    let first_receipts = operation_receipts(&fixture.db, &fixture.owner, &operation_id).await;
    let replay = store
        .apply_project_memory_fact_curation(request.clone(), &fixture.control)
        .await
        .expect("exact relation replay");

    assert!(!first.replayed());
    assert!(replay.replayed());
    assert_ne!(replay, first);
    let committed_material =
        serde_json::to_value(&first).expect("serialize commit stable material");
    let replayed_material =
        serde_json::to_value(&replay).expect("serialize replay stable material");
    assert_eq!(replayed_material, committed_material);
    assert_eq!(
        serde_json::to_vec(&replay).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
    assert_eq!(committed_material, first_receipts[0].receipt);
    assert!(!json_contains_key(&first_receipts[0].receipt, "metadata"));
    assert!(!json_contains_key(&first_receipts[0].receipt, "payload"));
    assert!(first_receipts[0].receipt.get("replayed").is_none());
    assert_eq!(first.operation_id(), &operation_id);
    assert_eq!(first.automation_run_id(), None);
    assert_eq!(first.input_digest(), first_receipts[0].request_digest);
    assert_eq!(first.operation_effects().len(), 1);
    assert_eq!(
        primary_commit(&first.operation_effects()[0]).fact_id(),
        &source
    );
    assert_eq!(first.replay_fact_id(), Some(&source));
    assert_eq!(first.replay_event_id(), Some(first_events[0].event_id()));
    assert_eq!(
        curation_receipt_from_value(&first_receipts[0].receipt)
            .expect("typed durable curation receipt"),
        first
    );
    assert_eq!(
        first
            .changed_facts()
            .iter()
            .map(ProjectMemoryFactIdV1::fact_id)
            .collect::<Vec<_>>(),
        vec![&source, &target]
    );
    assert!(matches!(
        &first.operation_effects()[0],
        tracedecay_store::ProjectMemoryFactCurationOperationEffectV1::LinkFacts {
            relation,
            ..
        } if relation.source_fact_id() == &source
            && relation.target_fact_id() == &target
            && relation.relation() == FactRelationKindV1::Supports
    ));
    let mut omitted_target = first_receipts[0].receipt.clone();
    omitted_target["changed_fact_ids"] = json!([source.as_str()]);
    assert!(curation_receipt_from_value(&omitted_target).is_err());
    for (field, changed) in [
        ("target_fact_id", json!(source.as_str())),
        ("evidence_fact_ids", json!([])),
        (
            "evidence_fact_ids",
            json!([source.as_str(), source.as_str()]),
        ),
        ("source_label", json!("")),
        ("sanitization_disposition", json!("rejected")),
        ("sanitization_sensitivity", json!("unclassified")),
    ] {
        let mut invalid_relation = first_receipts[0].receipt.clone();
        invalid_relation["operation_effects"][0]["relation"][field] = changed;
        assert!(curation_receipt_from_value(&invalid_relation).is_err());
    }
    let mut invalid_reference = first_receipts[0].receipt.clone();
    invalid_reference["operation_effects"][0]["relation"]["provenance_reference"]["byte_len"] =
        json!(0);
    assert!(curation_receipt_from_value(&invalid_reference).is_err());
    let mut foreign_owner = first_receipts[0].receipt.clone();
    foreign_owner["operation_effects"][0]["relation"]["owner"] =
        json!({"kind":"project", "project_id":"fixture.foreign-owner"});
    assert!(curation_receipt_from_value(&foreign_owner).is_err());
    let mut duplicated_effect = first_receipts[0].receipt.clone();
    let duplicate = duplicated_effect["operation_effects"][0].clone();
    duplicated_effect["operation_effects"]
        .as_array_mut()
        .expect("operation effect array")
        .push(duplicate);
    duplicated_effect["facts_linked"] = json!(2);
    assert!(curation_receipt_from_value(&duplicated_effect).is_err());
    for (field, changed) in [
        ("source_label", json!("different-source")),
        ("confidence", json!(0.2)),
        ("evidence_fact_ids", json!([target.as_str()])),
    ] {
        let mut changed_relation = first_receipts[0].receipt.clone();
        changed_relation["operation_effects"][0]["relation"][field] = changed;
        assert!(curation_receipt_from_value(&changed_relation).is_ok());
        rebind_curation_operation_receipt(
            &fixture.db,
            &fixture.owner,
            &operation_id,
            replay_event(&first),
            &changed_relation,
        )
        .await;
        assert!(matches!(
            store
                .apply_project_memory_fact_curation(request.clone(), &fixture.control)
                .await,
            Err(FactStoreError::Storage { .. })
        ));
        rebind_curation_operation_receipt(
            &fixture.db,
            &fixture.owner,
            &operation_id,
            replay_event(&first),
            &first_receipts[0].receipt,
        )
        .await;
    }
    let mut changed_sanitizer = first_receipts[0].receipt.clone();
    changed_sanitizer["operation_effects"][0]["relation"]["provenance_reference"]["byte_len"] =
        json!(1);
    assert!(curation_receipt_from_value(&changed_sanitizer).is_ok());
    rebind_curation_operation_receipt(
        &fixture.db,
        &fixture.owner,
        &operation_id,
        replay_event(&first),
        &changed_sanitizer,
    )
    .await;
    assert!(matches!(
        store
            .apply_project_memory_fact_curation(request.clone(), &fixture.control)
            .await,
        Err(FactStoreError::Storage { .. })
    ));
    rebind_curation_operation_receipt(
        &fixture.db,
        &fixture.owner,
        &operation_id,
        replay_event(&first),
        &first_receipts[0].receipt,
    )
    .await;
    let mut tampered_pointer = first_receipts[0].receipt.clone();
    tampered_pointer["replay_event_id"] =
        Value::String(format!("{}.tampered", first_events[0].event_id().as_str()));
    assert!(curation_receipt_from_value(&tampered_pointer).is_err());
    let mut unknown_field = first_receipts[0].receipt.clone();
    unknown_field["unexpected"] = Value::Bool(true);
    assert!(curation_receipt_from_value(&unknown_field).is_err());
    let rebound_run = request
        .clone()
        .with_automation_run_id(RunId::new("run.curation-rebound").unwrap())
        .expect("changed automation run binding");
    assert!(matches!(
        store
            .apply_project_memory_fact_curation(rebound_run, &fixture.control)
            .await,
        Err(FactStoreError::OperationConflict)
    ));
    assert_eq!(
        linked_events(&fixture.db, &fixture.owner).await,
        first_events
    );
    assert_eq!(
        operation_receipts(&fixture.db, &fixture.owner, &operation_id).await,
        first_receipts
    );
    assert_eq!(first_events.len(), 1);
    assert_eq!(first_receipts.len(), 1);
    assert_eq!(first_receipts[0].fact_id.as_ref(), Some(&source));
    assert_eq!(
        first_receipts[0].event_id.as_ref(),
        Some(first_events[0].event_id())
    );
    assert!(matches!(
        first_events[0].kind(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::Linked { relation },
            ..
        } if relation.kind() == FactRelationKindV1::Supports
    ));
}

#[tokio::test]
async fn repeated_exact_relation_is_a_noop_without_fresh_linked_events() {
    let fixture = Fixture::new("link-semantic-noop").await;
    let source = fixture.seed("semantic-noop-source", 10).await;
    let target = fixture.seed("semantic-noop-target", 20).await;
    let evidence = fixture.seed("semantic-noop-evidence", 30).await;
    let alternate_evidence = fixture.seed("semantic-noop-alternate-evidence", 40).await;
    let relation = relation(
        &fixture.owner,
        &source,
        &target,
        vec![evidence],
        FactRelationKindV1::Supports,
    );
    let first = request(
        &fixture.db,
        &fixture.owner,
        "fixture.link.semantic-noop.first",
        relation.clone(),
    )
    .await;
    let store = DatabaseFactStore::new(&fixture.db);
    store
        .apply_project_memory_fact_curation(first, &fixture.control)
        .await
        .expect("first semantic relation commit");
    let events = linked_events(&fixture.db, &fixture.owner).await;
    let changed_material = FactRelationV1::new(
        fixture.owner.clone(),
        source.clone(),
        target.clone(),
        FactRelationKindV1::Supports,
        vec![alternate_evidence],
        Confidence::new(0.61).unwrap(),
        relation_provenance("curation.changed-model-material"),
    )
    .unwrap();
    let repeat = request(
        &fixture.db,
        &fixture.owner,
        "fixture.link.semantic-noop.repeat",
        changed_material,
    )
    .await;
    let settled = store
        .apply_project_memory_fact_curation(repeat, &fixture.control)
        .await
        .expect("repeated semantic relation settles as no-op");

    assert_eq!(settled.facts_linked(), 0);
    assert!(settled.changed_facts().is_empty());
    assert!(settled.replay_fact_id().is_none());
    assert!(matches!(
        &settled.operation_effects()[0],
        ProjectMemoryFactCurationOperationEffectV1::LinkFacts {
            disposition:
                tracedecay_store::ProjectMemoryFactCurationLinkDispositionV1::AlreadyLinked,
            commit: None,
            ..
        }
    ));
    assert_eq!(linked_events(&fixture.db, &fixture.owner).await, events);
}

#[tokio::test]
async fn automation_bound_replay_preserves_outer_run_and_rejects_a_changed_run() {
    let fixture = Fixture::new("link-automation-replay").await;
    let source = fixture.seed("automation-replay-source", 10).await;
    let target = fixture.seed("automation-replay-target", 20).await;
    let evidence = fixture.seed("automation-replay-evidence", 30).await;
    let operation_id = provenance_id("fixture.link.automation-replay");
    let outer_run = RunId::new("run.curation-automation-replay").unwrap();
    let request = request(
        &fixture.db,
        &fixture.owner,
        operation_id.as_str(),
        relation(
            &fixture.owner,
            &source,
            &target,
            vec![evidence],
            FactRelationKindV1::DerivedFrom,
        ),
    )
    .await
    .with_automation_run_id(outer_run.clone())
    .unwrap();
    let store = DatabaseFactStore::new(&fixture.db);
    let committed = store
        .apply_project_memory_fact_curation(request.clone(), &fixture.control)
        .await
        .expect("automation-bound relation commit");
    let replayed = store
        .apply_project_memory_fact_curation(request.clone(), &fixture.control)
        .await
        .expect("automation-bound exact replay");

    assert_eq!(committed.automation_run_id(), Some(&outer_run));
    assert_eq!(replayed.automation_run_id(), Some(&outer_run));
    assert!(replayed.replayed());
    assert_eq!(
        serde_json::to_value(&replayed).unwrap(),
        serde_json::to_value(&committed).unwrap()
    );
    let before_events = linked_events(&fixture.db, &fixture.owner).await;
    let changed_run = request
        .with_automation_run_id(RunId::new("run.curation-automation-changed").unwrap())
        .unwrap();
    assert!(matches!(
        store
            .apply_project_memory_fact_curation(changed_run, &fixture.control)
            .await,
        Err(FactStoreError::OperationConflict)
    ));
    assert_eq!(
        linked_events(&fixture.db, &fixture.owner).await,
        before_events
    );
}

#[tokio::test]
async fn changed_relation_under_the_same_operation_id_conflicts_without_mutation() {
    let fixture = Fixture::new("link-changed-input").await;
    let source = fixture.seed("changed-source", 10).await;
    let target = fixture.seed("changed-target", 20).await;
    let evidence = fixture.seed("changed-evidence", 30).await;
    let operation_id = provenance_id("fixture.link.changed-input");
    let first_request = request(
        &fixture.db,
        &fixture.owner,
        operation_id.as_str(),
        relation(
            &fixture.owner,
            &source,
            &target,
            vec![evidence.clone()],
            FactRelationKindV1::Supports,
        ),
    )
    .await;
    let changed_request = request(
        &fixture.db,
        &fixture.owner,
        operation_id.as_str(),
        relation(
            &fixture.owner,
            &source,
            &target,
            vec![evidence],
            FactRelationKindV1::Supersedes,
        ),
    )
    .await;
    let store = DatabaseFactStore::new(&fixture.db);
    let first = store
        .apply_project_memory_fact_curation(first_request.clone(), &fixture.control)
        .await
        .expect("first relation commit");
    let before_events = linked_events(&fixture.db, &fixture.owner).await;
    let before_receipts = operation_receipts(&fixture.db, &fixture.owner, &operation_id).await;

    assert!(matches!(
        store
            .apply_project_memory_fact_curation(changed_request, &fixture.control)
            .await,
        Err(FactStoreError::OperationConflict)
    ));
    assert_eq!(
        linked_events(&fixture.db, &fixture.owner).await,
        before_events
    );
    assert_eq!(
        operation_receipts(&fixture.db, &fixture.owner, &operation_id).await,
        before_receipts
    );
    let replay = store
        .apply_project_memory_fact_curation(first_request, &fixture.control)
        .await
        .expect("original request remains replayable");
    assert!(replay.replayed());
    assert_eq!(
        serde_json::to_value(&replay).expect("serialize replay stable material"),
        serde_json::to_value(&first).expect("serialize commit stable material")
    );
}

#[tokio::test]
async fn conflicting_relation_kind_is_rejected_without_event_or_receipt_mutation() {
    let fixture = Fixture::new("link-semantic-conflict").await;
    let source = fixture.seed("conflict-source", 10).await;
    let target = fixture.seed("conflict-target", 20).await;
    let evidence = fixture.seed("conflict-evidence", 30).await;
    let accepted_operation = provenance_id("fixture.link.conflict.supports");
    let rejected_operation = provenance_id("fixture.link.conflict.contradicts");
    let accepted = request(
        &fixture.db,
        &fixture.owner,
        accepted_operation.as_str(),
        relation(
            &fixture.owner,
            &source,
            &target,
            vec![evidence.clone()],
            FactRelationKindV1::Supports,
        ),
    )
    .await;
    let store = DatabaseFactStore::new(&fixture.db);
    let accepted_receipt = store
        .apply_project_memory_fact_curation(accepted.clone(), &fixture.control)
        .await
        .expect("commit accepted relation");
    let rejected = request(
        &fixture.db,
        &fixture.owner,
        rejected_operation.as_str(),
        relation(
            &fixture.owner,
            &source,
            &target,
            vec![evidence],
            FactRelationKindV1::Contradicts,
        ),
    )
    .await;
    let before_events = linked_events(&fixture.db, &fixture.owner).await;

    let error = store
        .apply_project_memory_fact_curation(rejected, &fixture.control)
        .await
        .expect_err("opposite relation must conflict");
    assert!(matches!(
        error,
        FactStoreError::RelationConflict {
            source_fact_id,
            target_fact_id,
            existing: FactRelationKindV1::Supports,
            requested: FactRelationKindV1::Contradicts,
        } if source_fact_id == source && target_fact_id == target
    ));
    assert_eq!(
        linked_events(&fixture.db, &fixture.owner).await,
        before_events
    );
    assert!(
        operation_receipts(&fixture.db, &fixture.owner, &rejected_operation)
            .await
            .is_empty()
    );
    let replay = store
        .apply_project_memory_fact_curation(accepted, &fixture.control)
        .await
        .expect("accepted relation remains replayable");
    assert!(replay.replayed());
    assert_eq!(
        serde_json::to_value(&replay).expect("serialize replay stable material"),
        serde_json::to_value(&accepted_receipt).expect("serialize commit stable material")
    );
}

#[tokio::test]
async fn every_fact_relation_kind_reconstructs_through_the_runtime_graph_source() {
    let fixture = Fixture::new("link-graph-source").await;
    let kinds = [
        FactRelationKindV1::Supports,
        FactRelationKindV1::Contradicts,
        FactRelationKindV1::Supersedes,
        FactRelationKindV1::DerivedFrom,
    ];
    let store = DatabaseFactStore::new(&fixture.db);
    for (index, kind) in kinds.into_iter().enumerate() {
        let source = fixture
            .seed(&format!("graph-source-{index}"), 100 + index as i64 * 10)
            .await;
        let target = fixture
            .seed(&format!("graph-target-{index}"), 101 + index as i64 * 10)
            .await;
        let evidence = fixture
            .seed(&format!("graph-evidence-{index}"), 102 + index as i64 * 10)
            .await;
        store
            .apply_project_memory_fact_curation(
                request(
                    &fixture.db,
                    &fixture.owner,
                    &format!("fixture.link.graph-source.{index}"),
                    relation(&fixture.owner, &source, &target, vec![evidence], kind),
                )
                .await,
                &fixture.control,
            )
            .await
            .expect("commit relation kind");
    }

    assert_eq!(
        relation_kinds_from_canonical_source_for_test(
            &fixture.db,
            &fixture.owner,
            &accepting_read_control(),
        )
        .await
        .expect("reconstruct graph relation source"),
        BTreeSet::from(kinds)
    );
}

#[tokio::test]
async fn graph_source_excludes_relations_after_an_endpoint_becomes_unavailable() {
    let fixture = Fixture::new("link-graph-hidden-endpoint").await;
    let source = fixture.seed("hidden-source", 10).await;
    let target_receipt = fixture.seed_commit("hidden-target", 20).await;
    let target = target_receipt.fact_id().clone();
    let evidence = fixture.seed("hidden-evidence", 30).await;
    let store = DatabaseFactStore::new(&fixture.db);
    store
        .apply_project_memory_fact_curation(
            request(
                &fixture.db,
                &fixture.owner,
                "fixture.link.graph-hidden-endpoint",
                relation(
                    &fixture.owner,
                    &source,
                    &target,
                    vec![evidence],
                    FactRelationKindV1::Supports,
                ),
            )
            .await,
            &fixture.control,
        )
        .await
        .expect("commit visible relation");
    assert_eq!(
        relation_kinds_from_canonical_source_for_test(
            &fixture.db,
            &fixture.owner,
            &accepting_read_control(),
        )
        .await
        .expect("visible relation source"),
        BTreeSet::from([FactRelationKindV1::Supports])
    );

    store
        .remove_project_memory_fact(
            ProjectMemoryFactRemoveCommandV1::new(
                ProjectMemoryFactIdV1::new(fixture.owner.clone(), target)
                    .expect("owner-bound target"),
                provenance_id("fixture.remove.graph-hidden-endpoint"),
                Some(target_receipt.last_event_id().clone()),
                None,
            )
            .expect("remove command"),
            &fixture.control,
        )
        .await
        .expect("remove relation endpoint");

    assert!(
        relation_kinds_from_canonical_source_for_test(
            &fixture.db,
            &fixture.owner,
            &accepting_read_control(),
        )
        .await
        .expect("hidden endpoint graph source")
        .is_empty()
    );
}

#[tokio::test]
async fn graph_source_row_scan_observes_live_read_cancellation() {
    let fixture = Fixture::new("link-graph-source-cancelled").await;
    for index in 0..4 {
        fixture
            .seed(&format!("cancelled-source-{index}"), 10 + index)
            .await;
    }
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&checks);
    let read_control = FactReadControl::new(Arc::new(move || {
        observed.fetch_add(1, Ordering::AcqRel) >= 3
    }));

    assert!(matches!(
        relation_kinds_from_canonical_source_for_test(&fixture.db, &fixture.owner, &read_control,)
            .await,
        Err(FactStoreError::ReadCancelled)
    ));
    assert!(checks.load(Ordering::Acquire) > 3);
}

#[tokio::test]
async fn cross_owner_and_invalid_evidence_are_rejected_without_mutation() {
    let fixture = Fixture::new("link-invalid-evidence").await;
    let source = fixture.seed("invalid-source", 10).await;
    let target = fixture.seed("invalid-target", 20).await;
    let valid_evidence = fixture.seed("valid-evidence", 30).await;
    let foreign_owner = FactOwnerV1::Project {
        project_id: tracedecay_domain::ProjectId::new("fixture.foreign-owner".to_owned())
            .expect("foreign project id"),
    };
    let foreign_batch = ProjectMemoryFactCurationBatchV1::new(
        foreign_owner,
        provenance_id("fixture.link.foreign-owner"),
        None,
        Confidence::new(0.5).expect("minimum confidence"),
        vec![ProjectMemoryFactCurationOperationV1::LinkFacts(
            ProjectMemoryFactLinkV1::new(
                relation(
                    &fixture.owner,
                    &source,
                    &target,
                    vec![valid_evidence.clone()],
                    FactRelationKindV1::Supports,
                ),
                reviewed_ref(&fixture.db, &fixture.owner, &source).await,
                reviewed_ref(&fixture.db, &fixture.owner, &target).await,
                vec![reviewed_ref(&fixture.db, &fixture.owner, &valid_evidence).await],
            )
            .expect("link operation"),
        )],
    );
    assert!(matches!(foreign_batch, Err(FactStoreError::OwnerMismatch)));

    assert!(matches!(
        FactRelationV1::new(
            fixture.owner.clone(),
            source.clone(),
            target.clone(),
            FactRelationKindV1::Supports,
            Vec::new(),
            Confidence::new(0.8).expect("relation confidence"),
            relation_provenance("curation.fixture"),
        ),
        Err(DomainError::Empty { .. })
    ));
    assert!(matches!(
        FactRelationV1::new(
            fixture.owner.clone(),
            source.clone(),
            target.clone(),
            FactRelationKindV1::Supports,
            vec![valid_evidence.clone(), valid_evidence],
            Confidence::new(0.8).expect("relation confidence"),
            relation_provenance("curation.fixture"),
        ),
        Err(DomainError::DuplicateId { .. })
    ));

    let missing_evidence = fact_id_for(&fixture.owner, "fixture.seed.missing-evidence");
    let operation_id = provenance_id("fixture.link.missing-evidence");
    let missing_event = FactEventId::new("event.fixture.missing-evidence".to_owned())
        .expect("missing evidence event id");
    let missing_evidence_ref = ProjectMemoryFactCurationReviewRefV1::new(
        ProjectMemoryFactIdV1::new(fixture.owner.clone(), missing_evidence.clone())
            .expect("owner-bound missing evidence"),
        missing_event.clone(),
    );
    let invalid_request = ProjectMemoryFactCurationBatchV1::new(
        fixture.owner.clone(),
        operation_id.clone(),
        None,
        Confidence::new(0.5).expect("minimum confidence"),
        vec![ProjectMemoryFactCurationOperationV1::LinkFacts(
            ProjectMemoryFactLinkV1::new(
                relation(
                    &fixture.owner,
                    &source,
                    &target,
                    vec![missing_evidence],
                    FactRelationKindV1::Supports,
                ),
                reviewed_ref(&fixture.db, &fixture.owner, &source).await,
                reviewed_ref(&fixture.db, &fixture.owner, &target).await,
                vec![missing_evidence_ref],
            )
            .expect("link with missing reviewed evidence"),
        )],
    )
    .expect("missing evidence request");
    let result = DatabaseFactStore::new(&fixture.db)
        .apply_project_memory_fact_curation(invalid_request, &fixture.control)
        .await;
    assert!(matches!(
        result,
        Err(FactStoreError::CommitConflict {
            conflict: FactCommitConflict::LastEventMismatch {
                expected: Some(expected),
                actual: None,
            },
        }) if expected == missing_event
    ));
    assert_no_link_or_receipt(&fixture, &operation_id).await;
}

#[tokio::test]
async fn controlled_precommit_cancellation_rolls_back_link_and_receipt() {
    let fixture = Fixture::new("link-cancelled").await;
    let source = fixture.seed("cancel-source", 10).await;
    let target = fixture.seed("cancel-target", 20).await;
    let evidence = fixture.seed("cancel-evidence", 30).await;
    let operation_id = provenance_id("fixture.link.cancelled");
    let commit_attempted = Arc::new(AtomicBool::new(false));
    let commit_attempted_for_control = Arc::clone(&commit_attempted);
    let control = FactWriteControl::new(
        Arc::new(|| false),
        Arc::new(move || {
            commit_attempted_for_control.store(true, Ordering::Release);
            false
        }),
    );
    let store = DatabaseFactStore::new(&fixture.db);
    let result = store
        .apply_project_memory_fact_curation(
            request(
                &fixture.db,
                &fixture.owner,
                operation_id.as_str(),
                relation(
                    &fixture.owner,
                    &source,
                    &target,
                    vec![evidence],
                    FactRelationKindV1::Contradicts,
                ),
            )
            .await,
            &control,
        )
        .await;

    assert!(matches!(result, Err(FactStoreError::Storage { .. })));
    assert!(commit_attempted.load(Ordering::Acquire));
    assert_no_link_or_receipt(&fixture, &operation_id).await;
}

#[tokio::test]
async fn generic_commit_rejects_missing_link_target_and_evidence_without_mutation() {
    let fixture = Fixture::new("link-generic-validation").await;
    let source_receipt = fixture.seed_commit("generic-source", 10).await;
    let source = source_receipt.fact_id().clone();
    let target = fixture.seed("generic-target", 20).await;
    let evidence = fixture.seed("generic-evidence", 30).await;
    let missing_target = fact_id_for(&fixture.owner, "fixture.seed.generic-missing-target");
    let missing_evidence = fact_id_for(&fixture.owner, "fixture.seed.generic-missing-evidence");
    let before_events = linked_events(&fixture.db, &fixture.owner).await;
    let store = DatabaseFactStore::new(&fixture.db);
    let cases = [
        (missing_target.clone(), vec![evidence], missing_target),
        (target, vec![missing_evidence.clone()], missing_evidence),
    ];

    for (index, (target, evidence, missing_fact)) in cases.into_iter().enumerate() {
        let linked = FactLineageEventV1::new(
            source.clone(),
            fixture.owner.clone(),
            FactLineageEventKindV1::Curated {
                action: FactCurationActionV1::Linked {
                    relation: Box::new(relation(
                        &fixture.owner,
                        &source,
                        &target,
                        evidence,
                        FactRelationKindV1::DerivedFrom,
                    )),
                },
                evidence_ids: Vec::new(),
            },
            UtcMicros(40 + index as i64),
            None,
        )
        .expect("typed linked lineage event");
        let batch = FactWriteBatch::new(
            source.clone(),
            fixture.owner.clone(),
            None,
            vec![linked],
            Vec::new(),
            Vec::new(),
            Some(source_receipt.last_event_id().clone()),
        )
        .expect("generic relation batch");

        assert!(matches!(
            store.commit_fact(batch, &fixture.control).await,
            Err(FactStoreError::FactNotFound { fact_id }) if fact_id == missing_fact
        ));
        assert_eq!(
            linked_events(&fixture.db, &fixture.owner).await,
            before_events
        );
    }
}

#[tokio::test]
async fn generic_commit_rejects_missing_normalized_tag_evidence_without_mutation() {
    let fixture = Fixture::new("normalize-missing-evidence-validation").await;
    let normalized_receipt = fixture.seed_commit("missing-evidence-normalized", 10).await;
    let normalized = normalized_receipt.fact_id().clone();
    let missing = fact_id_for(&fixture.owner, "fixture.seed.missing-normalized-evidence");
    let assertion = correction_assertion(
        &fixture.owner,
        &normalized,
        normalized_receipt
            .active_assertion_id()
            .expect("normalized active assertion"),
        None,
        UtcMicros(30),
        &["canonical_tag".to_owned()],
    );
    let recorded = FactLineageEventV1::new(
        normalized.clone(),
        fixture.owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        UtcMicros(30),
        None,
    )
    .expect("record correction assertion");
    let normalized_event = FactLineageEventV1::new(
        normalized.clone(),
        fixture.owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::TagsNormalized {
                evidence_fact_ids: vec![missing.clone()],
                confidence: Confidence::new(0.87).expect("normalization confidence"),
            },
            evidence_ids: Vec::new(),
        },
        UtcMicros(31),
        None,
    )
    .expect("normalized-tag action event");
    let batch = FactWriteBatch::new(
        normalized.clone(),
        fixture.owner.clone(),
        Some(assertion),
        vec![recorded, normalized_event],
        Vec::new(),
        Vec::new(),
        Some(normalized_receipt.last_event_id().clone()),
    )
    .expect("structurally valid normalized-tag batch");
    let before_events = lineage_events_for_fact(&fixture.db, &fixture.owner, &normalized).await;

    assert!(matches!(
        DatabaseFactStore::new(&fixture.db)
            .commit_fact(batch, &fixture.control)
            .await,
        Err(FactStoreError::FactNotFound { fact_id }) if fact_id == missing
    ));
    assert_eq!(
        lineage_events_for_fact(&fixture.db, &fixture.owner, &normalized).await,
        before_events
    );
}

#[tokio::test]
async fn normalized_tag_batch_rejects_mismatched_correction_fact_and_assertion() {
    let fixture = Fixture::new("normalize-correction-identity-validation").await;
    let normalized_receipt = fixture.seed_commit("identity-normalized", 10).await;
    let normalized = normalized_receipt.fact_id().clone();
    let other_receipt = fixture.seed_commit("identity-other", 20).await;
    let other = other_receipt.fact_id().clone();
    let evidence = fixture.seed("identity-evidence", 30).await;
    let assertion = correction_assertion(
        &fixture.owner,
        &other,
        other_receipt
            .active_assertion_id()
            .expect("other active assertion"),
        None,
        UtcMicros(40),
        &["other_tag".to_owned()],
    );
    let recorded = FactLineageEventV1::new(
        other,
        fixture.owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        UtcMicros(40),
        None,
    )
    .expect("foreign-fact correction event");
    let normalized_event = FactLineageEventV1::new(
        normalized.clone(),
        fixture.owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::TagsNormalized {
                evidence_fact_ids: vec![evidence.clone()],
                confidence: Confidence::new(0.87).expect("normalization confidence"),
            },
            evidence_ids: Vec::new(),
        },
        UtcMicros(41),
        None,
    )
    .expect("normalized-tag action event");
    assert!(matches!(
        FactWriteBatch::new(
            normalized.clone(),
            fixture.owner.clone(),
            Some(assertion),
            vec![recorded, normalized_event],
            Vec::new(),
            Vec::new(),
            Some(normalized_receipt.last_event_id().clone()),
        ),
        Err(FactStoreError::FactMismatch)
    ));

    let assertion = correction_assertion(
        &fixture.owner,
        &normalized,
        normalized_receipt
            .active_assertion_id()
            .expect("normalized active assertion"),
        None,
        UtcMicros(50),
        &["canonical_tag".to_owned()],
    );
    let mismatched_record = FactLineageEventV1::new(
        normalized.clone(),
        fixture.owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: other_receipt
                .active_assertion_id()
                .expect("other active assertion")
                .clone(),
        },
        UtcMicros(50),
        None,
    )
    .expect("mismatched assertion event");
    let normalized_event = FactLineageEventV1::new(
        normalized.clone(),
        fixture.owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::TagsNormalized {
                evidence_fact_ids: vec![evidence],
                confidence: Confidence::new(0.87).expect("normalization confidence"),
            },
            evidence_ids: Vec::new(),
        },
        UtcMicros(51),
        None,
    )
    .expect("normalized-tag action event");
    assert!(matches!(
        FactWriteBatch::new(
            normalized,
            fixture.owner,
            Some(assertion.clone()),
            vec![mismatched_record, normalized_event],
            Vec::new(),
            Vec::new(),
            Some(normalized_receipt.last_event_id().clone()),
        ),
        Err(FactStoreError::MissingAssertionEvent { assertion_id })
            if assertion_id == assertion.assertion_id().clone()
    ));
}

#[tokio::test]
async fn graph_rebuild_rejects_a_dangling_canonical_link_event() {
    for (label, dangling_target) in [("target", true), ("evidence", false)] {
        let fixture = Fixture::new(&format!("link-graph-dangling-{label}")).await;
        let source = fixture.seed(&format!("dangling-{label}-source"), 10).await;
        let target = if dangling_target {
            fact_id_for(
                &fixture.owner,
                &format!("fixture.seed.dangling-{label}-target"),
            )
        } else {
            fixture.seed(&format!("dangling-{label}-target"), 20).await
        };
        let evidence = if dangling_target {
            fixture
                .seed(&format!("dangling-{label}-evidence"), 21)
                .await
        } else {
            fact_id_for(
                &fixture.owner,
                &format!("fixture.seed.dangling-{label}-evidence"),
            )
        };
        let event = FactLineageEventV1::new(
            source.clone(),
            fixture.owner.clone(),
            FactLineageEventKindV1::Curated {
                action: FactCurationActionV1::Linked {
                    relation: Box::new(relation(
                        &fixture.owner,
                        &source,
                        &target,
                        vec![evidence],
                        FactRelationKindV1::Supports,
                    )),
                },
                evidence_ids: Vec::new(),
            },
            UtcMicros(30),
            None,
        )
        .expect("typed dangling lineage event");
        let key = OwnerKey::new(&fixture.owner).expect("owner key");
        let writer = fixture
            .db
            .writer_connection("inject dangling canonical lineage fixture")
            .await
            .expect("writer connection");
        writer
            .execute_engine(
                "INSERT INTO memory_v2_lineage_events(
                    event_id, fact_id, owner_kind, project_id,
                    event_json, occurred_at, recorded_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.event_id().as_str(),
                    event.fact_id().as_str(),
                    key.kind,
                    key.project_id.as_str(),
                    serde_json::to_string(&event).expect("serialize typed dangling event"),
                    event.occurred_at().0,
                    event.occurred_at().0,
                ],
            )
            .await
            .expect("inject dangling event outside generic commit boundary");

        assert!(
            relation_kinds_from_canonical_source_for_test(
                &fixture.db,
                &fixture.owner,
                &accepting_read_control(),
            )
            .await
            .is_err()
        );
    }
}
