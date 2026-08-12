use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use tracedecay_domain::{
    Confidence, FactCategoryV1, FactEventId, FactId, FactOwnerV1, ProvenanceId,
};
use tracedecay_store::{
    FactStoreError, ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddMaterialV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeTargetV1,
    ProjectMemoryFactStore, ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdatePatchV1,
};

use crate::db::engine::params;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use crate::store::memory::primitives::{OwnerKey, row_optional_string, row_string};
use crate::store::memory::{DatabaseFactStore, FactWriteControl};

struct Fixture {
    database: Database,
    owner: FactOwnerV1,
    control: FactWriteControl,
    _directory: TempDir,
}

impl Fixture {
    async fn new(label: &str) -> Self {
        let directory = tempdir().expect("create canonical merge fixture directory");
        let path = directory.path().join(format!("{label}.db"));
        let authority = DatabaseAuthority::acquire_test(&path, "canonical merge test authority")
            .expect("acquire canonical merge test authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("publish canonical merge test runtime");
        Self {
            database,
            owner: FactOwnerV1::Profile,
            control: FactWriteControl::new(Arc::new(|| false), Arc::new(|| true)),
            _directory: directory,
        }
    }

    fn store(&self) -> DatabaseFactStore<'_> {
        DatabaseFactStore::new(&self.database)
    }

    async fn seed(&self, label: &str) -> ProjectMemoryFactIdV1 {
        let command = add_command(self.owner.clone(), label);
        let outcome = self
            .store()
            .add_project_memory_fact(command, &self.control)
            .await
            .expect("seed canonical merge fact");
        ProjectMemoryFactIdV1::new(self.owner.clone(), outcome.fact().fact_id().clone())
            .expect("owner-bound merge fact")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StoredMergeReceipt {
    request_digest: String,
    fact_id: Option<FactId>,
    event_id: Option<FactEventId>,
    receipt: Value,
}

fn provenance_id(value: &str) -> ProvenanceId {
    ProvenanceId::new(value.to_owned()).expect("canonical merge operation identity")
}

fn add_command(owner: FactOwnerV1, label: &str) -> ProjectMemoryFactAddCommandV1 {
    let content = format!("Canonical merge fixture {label}.");
    let material = json!({
        "content": content,
        "category": "project",
        "tags": ["canonical-merge"],
        "entities": ["TraceDecay"],
        "metadata": {"fixture": label},
    });
    let receipt = match sanitize_memory_fact_payload(material.clone())
        .expect("sanitize canonical merge fixture")
    {
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            assert_eq!(payload, material);
            receipt
        }
        MemoryFactSanitizationV1::Quarantined => {
            panic!("canonical merge fixture must remain durable")
        }
    };
    ProjectMemoryFactAddMaterialV1::new(
        owner,
        material["content"]
            .as_str()
            .expect("canonical merge fixture content")
            .to_owned(),
        FactCategoryV1::Project,
        None,
        vec!["canonical-merge".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({"fixture": label}),
        receipt,
        None,
        Confidence::new(0.8).expect("canonical merge fixture trust"),
        None,
    )
    .and_then(|material| material.into_command(provenance_id(&format!("seed.{label}"))))
    .expect("canonical merge add command")
}

async fn merge_target(
    database: &Database,
    owner: &FactOwnerV1,
    target: &ProjectMemoryFactIdV1,
) -> ProjectMemoryFactMergeTargetV1 {
    let key = OwnerKey::new(owner).expect("canonical merge owner key");
    let writer = database
        .writer_connection("read canonical merge target event")
        .await
        .expect("canonical merge writer connection");
    let mut rows = writer
        .query_engine(
            "SELECT last_event_id FROM memory_v2_current_facts
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3",
            params![key.kind, key.project_id.as_str(), target.fact_id().as_str()],
        )
        .await
        .expect("query canonical merge target event");
    let row = rows
        .next()
        .await
        .expect("next canonical merge target event")
        .expect("canonical merge target event");
    let event_id = FactEventId::new(
        row_string(&row, 0, "read canonical merge target event").expect("merge target event id"),
    )
    .expect("typed merge target event id");
    ProjectMemoryFactMergeTargetV1::new(target.clone(), event_id)
        .expect("snapshot canonical merge target")
}

async fn merge_command(
    database: &Database,
    owner: &FactOwnerV1,
    operation_id: &str,
    winner: &ProjectMemoryFactIdV1,
    loser: &ProjectMemoryFactIdV1,
    merged_content: Option<&str>,
) -> ProjectMemoryFactMergeCommandV1 {
    ProjectMemoryFactMergeCommandV1::new(
        owner.clone(),
        provenance_id(operation_id),
        merge_target(database, owner, winner).await,
        vec![merge_target(database, owner, loser).await],
        merged_content.map(ToOwned::to_owned),
        None,
    )
    .expect("canonical merge command")
}

async fn lineage_event_ids(database: &Database, owner: &FactOwnerV1) -> Vec<FactEventId> {
    let key = OwnerKey::new(owner).expect("canonical merge owner key");
    let writer = database
        .writer_connection("read canonical merge lineage")
        .await
        .expect("canonical merge writer connection");
    let mut rows = writer
        .query_engine(
            "SELECT event_id FROM memory_v2_lineage_events
             WHERE owner_kind = ?1 AND project_id = ?2
             ORDER BY event_sequence",
            params![key.kind, key.project_id.as_str()],
        )
        .await
        .expect("query canonical merge lineage");
    let mut event_ids = Vec::new();
    while let Some(row) = rows.next().await.expect("next canonical merge lineage row") {
        event_ids.push(
            FactEventId::new(
                row_string(&row, 0, "read canonical merge lineage").expect("merge event id"),
            )
            .expect("typed canonical merge event id"),
        );
    }
    event_ids
}

async fn stored_merge_receipt(
    database: &Database,
    owner: &FactOwnerV1,
    operation_id: &ProvenanceId,
) -> StoredMergeReceipt {
    let key = OwnerKey::new(owner).expect("canonical merge owner key");
    let writer = database
        .writer_connection("read canonical merge operation receipt")
        .await
        .expect("canonical merge writer connection");
    let mut rows = writer
        .query_engine(
            "SELECT request_digest, fact_id, event_id, receipt_json
             FROM memory_v2_operation_receipts
             WHERE owner_kind = ?1 AND project_id = ?2
               AND operation_id = ?3 AND operation_kind = 'merge'",
            params![key.kind, key.project_id.as_str(), operation_id.as_str()],
        )
        .await
        .expect("query canonical merge operation receipt");
    let row = rows
        .next()
        .await
        .expect("next canonical merge receipt row")
        .expect("canonical merge operation receipt");
    let receipt = StoredMergeReceipt {
        request_digest: row_string(&row, 0, "read canonical merge operation receipt")
            .expect("merge request digest"),
        fact_id: row_optional_string(&row, 1, "read canonical merge operation receipt")
            .expect("optional merge fact id")
            .map(FactId::new)
            .transpose()
            .expect("typed merge fact id"),
        event_id: row_optional_string(&row, 2, "read canonical merge operation receipt")
            .expect("optional merge event id")
            .map(FactEventId::new)
            .transpose()
            .expect("typed merge event id"),
        receipt: serde_json::from_str(
            &row_string(&row, 3, "read canonical merge operation receipt")
                .expect("merge receipt json"),
        )
        .expect("typed merge receipt json"),
    };
    assert!(
        rows.next()
            .await
            .expect("check duplicate canonical merge receipt")
            .is_none()
    );
    receipt
}

#[tokio::test]
async fn initial_merge_returns_exact_canonical_commit_receipts() {
    let fixture = Fixture::new("initial-receipts").await;
    let winner = fixture.seed("initial-winner").await;
    let loser = fixture.seed("initial-loser").await;
    let operation_id = provenance_id("merge.initial.receipts");
    let command = merge_command(
        &fixture.database,
        &fixture.owner,
        operation_id.as_str(),
        &winner,
        &loser,
        Some("The merged canonical fact retains exact receipt evidence."),
    )
    .await;
    let expected_input_digest = command.input_digest().expect("canonical merge digest");
    assert_eq!(
        merge_command(
            &fixture.database,
            &fixture.owner,
            "merge.initial.receipts.equivalent-operation",
            &winner,
            &loser,
            Some("The merged canonical fact retains exact receipt evidence."),
        )
        .await
        .input_digest()
        .expect("equivalent canonical merge digest"),
        expected_input_digest
    );

    let outcome = fixture
        .store()
        .merge_project_memory_facts(command, &fixture.control)
        .await
        .expect("commit canonical merge");
    let durable = stored_merge_receipt(&fixture.database, &fixture.owner, &operation_id).await;

    assert!(!outcome.replayed());
    assert_eq!(outcome.owner(), &fixture.owner);
    assert_eq!(outcome.operation_id(), &operation_id);
    assert_eq!(outcome.input_digest(), expected_input_digest);
    assert_eq!(outcome.input_digest(), durable.request_digest);
    assert_eq!(outcome.winner(), &winner);
    assert!(outcome.content_updated());
    assert_eq!(outcome.deleted_losers(), &[loser.clone()]);
    assert_eq!(outcome.commit_receipts().len(), 2);
    assert_eq!(outcome.commit_receipts()[0].fact_id(), winner.fact_id());
    assert_eq!(outcome.commit_receipts()[0].committed_event_ids().len(), 2);
    assert!(outcome.commit_receipts()[0].active_assertion_id().is_some());
    assert_eq!(outcome.commit_receipts()[1].fact_id(), loser.fact_id());
    assert_eq!(outcome.commit_receipts()[1].committed_event_ids().len(), 2);
    assert!(outcome.commit_receipts()[1].active_assertion_id().is_none());
    assert_eq!(durable.fact_id.as_ref(), Some(winner.fact_id()));
    assert_eq!(
        durable.event_id.as_ref(),
        Some(outcome.commit_receipts()[0].last_event_id())
    );
    assert_eq!(
        durable.receipt,
        serde_json::to_value(&outcome).expect("serialize canonical merge outcome")
    );
    assert!(durable.receipt.get("replayed").is_none());
}

#[tokio::test]
async fn exact_merge_replay_verifies_and_returns_the_durable_receipt_without_writes() {
    let fixture = Fixture::new("exact-replay").await;
    let winner = fixture.seed("replay-winner").await;
    let loser = fixture.seed("replay-loser").await;
    let operation_id = provenance_id("merge.exact.replay");
    let command = merge_command(
        &fixture.database,
        &fixture.owner,
        operation_id.as_str(),
        &winner,
        &loser,
        Some("The exact canonical merge replays its committed evidence."),
    )
    .await;
    let committed = fixture
        .store()
        .merge_project_memory_facts(command.clone(), &fixture.control)
        .await
        .expect("commit replay fixture merge");
    let before_events = lineage_event_ids(&fixture.database, &fixture.owner).await;
    let before_receipt =
        stored_merge_receipt(&fixture.database, &fixture.owner, &operation_id).await;

    let replayed = fixture
        .store()
        .merge_project_memory_facts(command, &fixture.control)
        .await
        .expect("replay canonical merge");

    assert!(!committed.replayed());
    assert!(replayed.replayed());
    assert_eq!(replayed.commit_receipts(), committed.commit_receipts());
    assert_eq!(replayed.input_digest(), committed.input_digest());
    assert_eq!(
        serde_json::to_value(&replayed).expect("serialize replayed merge outcome"),
        serde_json::to_value(&committed).expect("serialize committed merge outcome")
    );
    assert_eq!(
        lineage_event_ids(&fixture.database, &fixture.owner).await,
        before_events
    );
    assert_eq!(
        stored_merge_receipt(&fixture.database, &fixture.owner, &operation_id).await,
        before_receipt
    );
}

#[tokio::test]
async fn changed_merge_input_conflicts_without_mutating_canonical_evidence() {
    let fixture = Fixture::new("changed-input").await;
    let winner = fixture.seed("changed-winner").await;
    let loser = fixture.seed("changed-loser").await;
    let operation_id = provenance_id("merge.changed.input");
    let committed_command = merge_command(
        &fixture.database,
        &fixture.owner,
        operation_id.as_str(),
        &winner,
        &loser,
        None,
    )
    .await;
    fixture
        .store()
        .merge_project_memory_facts(committed_command, &fixture.control)
        .await
        .expect("commit canonical merge before conflict");
    let before_events = lineage_event_ids(&fixture.database, &fixture.owner).await;
    let before_receipt =
        stored_merge_receipt(&fixture.database, &fixture.owner, &operation_id).await;

    let changed = merge_command(
        &fixture.database,
        &fixture.owner,
        operation_id.as_str(),
        &winner,
        &loser,
        Some("Changed input cannot reuse a committed merge identity."),
    )
    .await;
    assert!(matches!(
        fixture
            .store()
            .merge_project_memory_facts(changed, &fixture.control)
            .await,
        Err(FactStoreError::OperationConflict)
    ));
    assert_eq!(
        lineage_event_ids(&fixture.database, &fixture.owner).await,
        before_events
    );
    assert_eq!(
        stored_merge_receipt(&fixture.database, &fixture.owner, &operation_id).await,
        before_receipt
    );
}

#[tokio::test]
async fn merge_without_content_update_never_fabricates_a_winner_commit_receipt() {
    let fixture = Fixture::new("no-content-update").await;
    let winner = fixture.seed("no-content-winner").await;
    let loser = fixture.seed("no-content-loser").await;
    let operation_id = provenance_id("merge.no.content.update");

    let outcome = fixture
        .store()
        .merge_project_memory_facts(
            merge_command(
                &fixture.database,
                &fixture.owner,
                operation_id.as_str(),
                &winner,
                &loser,
                None,
            )
            .await,
            &fixture.control,
        )
        .await
        .expect("commit deletion-only canonical merge");
    let durable = stored_merge_receipt(&fixture.database, &fixture.owner, &operation_id).await;

    assert!(!outcome.content_updated());
    assert_eq!(outcome.commit_receipts().len(), 1);
    assert_eq!(outcome.commit_receipts()[0].fact_id(), loser.fact_id());
    assert_eq!(outcome.commit_receipts()[0].committed_event_ids().len(), 2);
    assert!(outcome.commit_receipts()[0].active_assertion_id().is_none());
    assert_ne!(outcome.commit_receipts()[0].fact_id(), winner.fact_id());
    assert_eq!(durable.fact_id.as_ref(), Some(loser.fact_id()));
    assert_eq!(
        durable.event_id.as_ref(),
        Some(outcome.commit_receipts()[0].last_event_id())
    );
}

#[tokio::test]
async fn stale_last_loser_cas_rejects_before_any_merge_write() {
    let fixture = Fixture::new("all-loser-cas").await;
    let winner = fixture.seed("cas-winner").await;
    let loser_a = fixture.seed("cas-loser-a").await;
    let loser_b = fixture.seed("cas-loser-b").await;
    let winner_target = merge_target(&fixture.database, &fixture.owner, &winner).await;
    let loser_a_target = merge_target(&fixture.database, &fixture.owner, &loser_a).await;
    let loser_b_target = merge_target(&fixture.database, &fixture.owner, &loser_b).await;
    let update = ProjectMemoryFactUpdateCommandV1::new(
        loser_b.clone(),
        provenance_id("merge.cas.advance.last-loser"),
        Some(loser_b_target.expected_last_event_id().clone()),
        ProjectMemoryFactUpdatePatchV1::new(
            Some("The last loser advanced after the merge review.".to_owned()),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("last-loser update patch"),
        None,
    )
    .expect("last-loser update command");
    fixture
        .store()
        .update_project_memory_fact(update, &fixture.control)
        .await
        .expect("advance the final loser after review");
    let before = lineage_event_ids(&fixture.database, &fixture.owner).await;
    let merge = ProjectMemoryFactMergeCommandV1::new(
        fixture.owner.clone(),
        provenance_id("merge.cas.all-participants"),
        winner_target,
        vec![loser_a_target, loser_b_target],
        Some("This merge must never partially commit.".to_owned()),
        None,
    )
    .expect("all-participant CAS merge command");

    assert!(matches!(
        fixture
            .store()
            .merge_project_memory_facts(merge, &fixture.control)
            .await,
        Err(FactStoreError::CommitConflict { .. })
    ));
    assert_eq!(
        lineage_event_ids(&fixture.database, &fixture.owner).await,
        before,
        "winner and earlier losers must remain untouched when the final loser is stale"
    );
}
