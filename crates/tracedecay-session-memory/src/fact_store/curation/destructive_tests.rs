use std::sync::Arc;

use serde_json::json;
use tempfile::{TempDir, tempdir};
use tracedecay_domain::{
    Confidence, FactCategoryV1, FactEventId, FactOwnerV1, ProvenanceId, RunId,
};
use tracedecay_store::{
    FactStoreError, ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddMaterialV1,
    ProjectMemoryFactCurationAddV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationEvidenceV1, ProjectMemoryFactCurationMergeV1,
    ProjectMemoryFactCurationMutationKindV1, ProjectMemoryFactCurationOperationEffectV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactCurationRemoveDispositionV1,
    ProjectMemoryFactCurationRemoveV1, ProjectMemoryFactCurationUpdateV1, ProjectMemoryFactIdV1,
    ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeTargetV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactStore, ProjectMemoryFactUpdateCommandV1,
    ProjectMemoryFactUpdatePatchV1, derive_project_memory_fact_curation_child_operation_id,
};

use crate::db::engine::params;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use crate::store::memory::primitives::{OwnerKey, row_string};
use crate::store::memory::{DatabaseFactStore, FactWriteControl};

struct Fixture {
    database: Database,
    owner: FactOwnerV1,
    control: FactWriteControl,
    _directory: TempDir,
}

impl Fixture {
    async fn new(label: &str) -> Self {
        let directory = tempdir().expect("create destructive curation fixture");
        let path = directory.path().join(format!("{label}.db"));
        let authority = DatabaseAuthority::acquire_test(&path, "destructive curation authority")
            .expect("acquire destructive curation authority");
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .expect("publish destructive curation database");
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

    async fn seed(&self, label: &str) -> (ProjectMemoryFactIdV1, FactEventId) {
        let outcome = self
            .store()
            .add_project_memory_fact(
                add_command(self.owner.clone(), label, None, &format!("seed.{label}")),
                &self.control,
            )
            .await
            .expect("seed destructive curation fact");
        let commit = outcome
            .commit_receipt()
            .expect("seed must emit a canonical commit");
        (
            ProjectMemoryFactIdV1::new(self.owner.clone(), outcome.fact().fact_id().clone())
                .expect("owner-bound seed fact"),
            commit.last_event_id().clone(),
        )
    }
}

fn provenance_id(value: &str) -> ProvenanceId {
    ProvenanceId::new(value.to_owned()).expect("canonical operation identity")
}

fn run_id(value: &str) -> RunId {
    RunId::new(value.to_owned()).expect("canonical automation run identity")
}

fn add_command(
    owner: FactOwnerV1,
    label: &str,
    automation_run_id: Option<&RunId>,
    operation_id: &str,
) -> ProjectMemoryFactAddCommandV1 {
    let material = json!({
        "content": format!("Destructive curation fixture {label}."),
        "category": "project",
        "tags": ["automatic-curation"],
        "entities": ["TraceDecay"],
        "metadata": {"fixture": label},
    });
    let receipt = match sanitize_memory_fact_payload(material.clone())
        .expect("sanitize destructive curation material")
    {
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            assert_eq!(payload, material);
            receipt
        }
        MemoryFactSanitizationV1::Quarantined => {
            panic!("destructive curation fixture must remain durable")
        }
    };
    ProjectMemoryFactAddMaterialV1::new(
        owner,
        material["content"]
            .as_str()
            .expect("destructive fixture content")
            .to_owned(),
        FactCategoryV1::Project,
        None,
        vec!["automatic-curation".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({"fixture": label}),
        receipt,
        automation_run_id.map(|run_id| run_id.as_str().to_owned()),
        Confidence::new(0.8).expect("destructive fixture trust"),
        None,
    )
    .and_then(|material| material.into_command(provenance_id(operation_id)))
    .expect("destructive curation add command")
}

fn evidence(
    owner: &FactOwnerV1,
    fact: ProjectMemoryFactIdV1,
    expected_last_event_id: FactEventId,
    reason: &str,
) -> ProjectMemoryFactCurationEvidenceV1 {
    ProjectMemoryFactCurationEvidenceV1::new(
        owner,
        vec![tracedecay_store::ProjectMemoryFactCurationReviewRefV1::new(
            fact,
            expected_last_event_id,
        )],
        Confidence::new(0.9).expect("review confidence"),
        reason.to_owned(),
    )
    .expect("destructive curation evidence")
}

fn update_command(
    target: ProjectMemoryFactIdV1,
    expected: FactEventId,
    operation_id: &str,
    content: &str,
) -> ProjectMemoryFactUpdateCommandV1 {
    ProjectMemoryFactUpdateCommandV1::new(
        target,
        provenance_id(operation_id),
        Some(expected),
        ProjectMemoryFactUpdatePatchV1::new(
            Some(content.to_owned()),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("curation update patch"),
        None,
    )
    .expect("curation update command")
}

async fn lineage_event_ids(fixture: &Fixture) -> Vec<String> {
    let key = OwnerKey::new(&fixture.owner).expect("destructive curation owner key");
    let writer = fixture
        .database
        .writer_connection("read destructive curation lineage")
        .await
        .expect("destructive curation writer");
    let mut rows = writer
        .query_engine(
            "SELECT event_id FROM memory_v2_lineage_events
             WHERE owner_kind = ?1 AND project_id = ?2 ORDER BY event_sequence",
            params![key.kind, key.project_id.as_str()],
        )
        .await
        .expect("query destructive curation lineage");
    let mut event_ids = Vec::new();
    while let Some(row) = rows.next().await.expect("next destructive lineage event") {
        event_ids.push(
            row_string(&row, 0, "read destructive curation lineage").expect("lineage event id"),
        );
    }
    event_ids
}

async fn operation_receipt_count(fixture: &Fixture, operation_ids: &[&ProvenanceId]) -> usize {
    let key = OwnerKey::new(&fixture.owner).expect("destructive curation owner key");
    let writer = fixture
        .database
        .writer_connection("read destructive curation receipts")
        .await
        .expect("destructive curation writer");
    let mut count = 0;
    for operation_id in operation_ids {
        let mut rows = writer
            .query_engine(
                "SELECT COUNT(*) FROM memory_v2_operation_receipts
                 WHERE owner_kind = ?1 AND project_id = ?2 AND operation_id = ?3",
                params![key.kind, key.project_id.as_str(), operation_id.as_str()],
            )
            .await
            .expect("query destructive curation receipt count");
        let row = rows
            .next()
            .await
            .expect("next receipt count")
            .expect("receipt count");
        count += usize::try_from(
            crate::store::memory::primitives::row_i64(
                &row,
                0,
                "read destructive curation receipts",
            )
            .expect("receipt count value"),
        )
        .expect("nonnegative receipt count");
    }
    count
}

#[tokio::test]
async fn no_op_batch_has_no_anchor_and_replays_without_fabricated_changes() {
    let fixture = Fixture::new("truthful-no-op").await;
    let (duplicate, duplicate_event) = fixture.seed("duplicate").await;
    let (removed, removed_seed_event) = fixture.seed("removed").await;
    let initial_remove = ProjectMemoryFactRemoveCommandV1::new(
        removed.clone(),
        provenance_id("curation.prepare.removed"),
        Some(removed_seed_event),
        None,
    )
    .expect("prepare removed fact command");
    let removed_outcome = fixture
        .store()
        .remove_project_memory_fact(initial_remove, &fixture.control)
        .await
        .expect("prepare already-removed fact");
    let removed_event = removed_outcome
        .commit_receipt()
        .expect("preparation removal commit")
        .last_event_id()
        .clone();
    let run = run_id("automation.run.truthful-no-op");
    let outer_id = provenance_id("curation.outer.truthful-no-op");
    let add_id = derive_project_memory_fact_curation_child_operation_id(
        &outer_id,
        0,
        ProjectMemoryFactCurationMutationKindV1::Add,
    )
    .expect("stable add child identity");
    let remove_id = derive_project_memory_fact_curation_child_operation_id(
        &outer_id,
        1,
        ProjectMemoryFactCurationMutationKindV1::Remove,
    )
    .expect("stable remove child identity");
    let add = ProjectMemoryFactCurationAddV1::new(
        add_command(
            fixture.owner.clone(),
            "duplicate",
            Some(&run),
            add_id.as_str(),
        ),
        evidence(
            &fixture.owner,
            duplicate.clone(),
            duplicate_event.clone(),
            "Exact duplicate evidence",
        ),
    )
    .expect("curation duplicate add");
    let remove_command = ProjectMemoryFactRemoveCommandV1::new(
        removed.clone(),
        remove_id,
        Some(removed_event),
        None,
    )
    .expect("curation already-removed command");
    let remove = ProjectMemoryFactCurationRemoveV1::new(
        remove_command,
        evidence(
            &fixture.owner,
            duplicate,
            duplicate_event,
            "Deleted target already settled",
        ),
    )
    .expect("curation already-removed operation");
    let request = ProjectMemoryFactCurationBatchV1::new(
        fixture.owner.clone(),
        outer_id,
        None,
        Confidence::new(0.8).expect("minimum curation confidence"),
        vec![
            ProjectMemoryFactCurationOperationV1::Add(add),
            ProjectMemoryFactCurationOperationV1::Remove(remove),
        ],
    )
    .and_then(|request| request.with_automation_run_id(run))
    .expect("truthful no-op curation batch");

    let first = fixture
        .store()
        .apply_project_memory_fact_curation(request.clone(), &fixture.control)
        .await
        .expect("apply truthful no-op batch");
    assert_eq!(first.replay_fact_id(), None);
    assert_eq!(first.replay_event_id(), None);
    assert!(first.changed_facts().is_empty());
    assert_eq!(first.accepted_operations(), 2);
    assert_eq!(first.facts_added(), 0);
    assert_eq!(first.facts_removed(), 0);
    assert!(matches!(
        &first.operation_effects()[0],
        ProjectMemoryFactCurationOperationEffectV1::Add { commit: None, .. }
    ));
    assert!(matches!(
        &first.operation_effects()[1],
        ProjectMemoryFactCurationOperationEffectV1::Remove {
            disposition: ProjectMemoryFactCurationRemoveDispositionV1::AlreadyRemoved,
            commit: None,
            ..
        }
    ));

    let replayed = fixture
        .store()
        .apply_project_memory_fact_curation(request, &fixture.control)
        .await
        .expect("replay truthful no-op batch");
    assert!(replayed.replayed());
    assert_eq!(
        serde_json::to_value(replayed).expect("serialize replayed no-op receipt"),
        serde_json::to_value(first).expect("serialize original no-op receipt")
    );
}

#[tokio::test]
async fn mixed_destructive_batch_commits_once_and_exactly_replays_every_effect() {
    let fixture = Fixture::new("mixed-batch").await;
    let (update_target, update_event) = fixture.seed("mixed-update").await;
    let (merge_winner, merge_winner_event) = fixture.seed("mixed-winner").await;
    let (merge_loser, merge_loser_event) = fixture.seed("mixed-loser").await;
    let (remove_target, remove_event) = fixture.seed("mixed-remove").await;
    let (evidence_fact, evidence_event) = fixture.seed("mixed-evidence").await;
    let run = run_id("automation.run.mixed-destructive");
    let outer_id = provenance_id("curation.outer.mixed-destructive");
    let child_id = |index, kind| {
        derive_project_memory_fact_curation_child_operation_id(&outer_id, index, kind)
            .expect("stable mixed child identity")
    };
    let add = ProjectMemoryFactCurationAddV1::new(
        add_command(
            fixture.owner.clone(),
            "mixed-new",
            Some(&run),
            child_id(0, ProjectMemoryFactCurationMutationKindV1::Add).as_str(),
        ),
        evidence(
            &fixture.owner,
            evidence_fact.clone(),
            evidence_event.clone(),
            "Mixed add evidence",
        ),
    )
    .expect("mixed curation add");
    let update = ProjectMemoryFactCurationUpdateV1::new(
        update_command(
            update_target.clone(),
            update_event,
            child_id(1, ProjectMemoryFactCurationMutationKindV1::Update).as_str(),
            "The mixed batch updates this canonical fact.",
        ),
        evidence(
            &fixture.owner,
            evidence_fact.clone(),
            evidence_event.clone(),
            "Mixed update evidence",
        ),
    )
    .expect("mixed curation update");
    let merge = ProjectMemoryFactCurationMergeV1::new(
        ProjectMemoryFactMergeCommandV1::new(
            fixture.owner.clone(),
            child_id(2, ProjectMemoryFactCurationMutationKindV1::Merge),
            ProjectMemoryFactMergeTargetV1::new(merge_winner.clone(), merge_winner_event)
                .expect("mixed winner snapshot"),
            vec![
                ProjectMemoryFactMergeTargetV1::new(merge_loser.clone(), merge_loser_event)
                    .expect("mixed loser snapshot"),
            ],
            None,
            None,
        )
        .expect("mixed merge command"),
        evidence(
            &fixture.owner,
            evidence_fact.clone(),
            evidence_event.clone(),
            "Mixed merge evidence",
        ),
    )
    .expect("mixed curation merge");
    let remove = ProjectMemoryFactCurationRemoveV1::new(
        ProjectMemoryFactRemoveCommandV1::new(
            remove_target.clone(),
            child_id(3, ProjectMemoryFactCurationMutationKindV1::Remove),
            Some(remove_event),
            None,
        )
        .expect("mixed remove command"),
        evidence(
            &fixture.owner,
            evidence_fact,
            evidence_event,
            "Mixed remove evidence",
        ),
    )
    .expect("mixed curation remove");
    let request = ProjectMemoryFactCurationBatchV1::new(
        fixture.owner.clone(),
        outer_id,
        None,
        Confidence::new(0.8).expect("minimum mixed confidence"),
        vec![
            ProjectMemoryFactCurationOperationV1::Add(add),
            ProjectMemoryFactCurationOperationV1::Update(update),
            ProjectMemoryFactCurationOperationV1::Merge(merge),
            ProjectMemoryFactCurationOperationV1::Remove(remove),
        ],
    )
    .and_then(|request| request.with_automation_run_id(run))
    .expect("mixed destructive curation batch");

    let first = fixture
        .store()
        .apply_project_memory_fact_curation(request.clone(), &fixture.control)
        .await
        .expect("commit mixed destructive batch");
    assert_eq!(first.accepted_operations(), 4);
    assert_eq!(first.facts_added(), 1);
    assert_eq!(first.facts_updated(), 1);
    assert_eq!(first.facts_merged(), 1);
    assert_eq!(first.facts_removed(), 1);
    assert_eq!(first.changed_facts().len(), 4);
    assert!(first.replay_fact_id().is_some());
    assert!(first.replay_event_id().is_some());
    assert!(matches!(
        &first.operation_effects()[0],
        ProjectMemoryFactCurationOperationEffectV1::Add {
            commit: Some(_),
            ..
        }
    ));
    assert!(matches!(
        &first.operation_effects()[1],
        ProjectMemoryFactCurationOperationEffectV1::Update { .. }
    ));
    assert!(matches!(
        &first.operation_effects()[2],
        ProjectMemoryFactCurationOperationEffectV1::Merge { outcome }
            if outcome.deleted_losers() == [merge_loser]
                && outcome.winner() == &merge_winner
    ));
    assert!(matches!(
        &first.operation_effects()[3],
        ProjectMemoryFactCurationOperationEffectV1::Remove {
            disposition: ProjectMemoryFactCurationRemoveDispositionV1::Removed,
            commit: Some(_),
            ..
        }
    ));
    let before_replay = lineage_event_ids(&fixture).await;
    let replayed = fixture
        .store()
        .apply_project_memory_fact_curation(request, &fixture.control)
        .await
        .expect("replay mixed destructive batch");
    assert!(replayed.replayed());
    assert_eq!(lineage_event_ids(&fixture).await, before_replay);
    assert_eq!(
        serde_json::to_value(replayed).expect("serialize mixed replay"),
        serde_json::to_value(first).expect("serialize mixed commit")
    );
}

#[tokio::test]
async fn stale_late_child_rolls_back_prior_child_and_all_operation_receipts() {
    let fixture = Fixture::new("atomic-rollback").await;
    let (update_target, update_event) = fixture.seed("update-target").await;
    let (remove_target, remove_event) = fixture.seed("remove-target").await;
    let (evidence_fact, evidence_event) = fixture.seed("evidence").await;
    let outer_id = provenance_id("curation.outer.atomic-rollback");
    let update_id = derive_project_memory_fact_curation_child_operation_id(
        &outer_id,
        0,
        ProjectMemoryFactCurationMutationKindV1::Update,
    )
    .expect("stable update child identity");
    let update = ProjectMemoryFactCurationUpdateV1::new(
        update_command(
            update_target,
            update_event,
            update_id.as_str(),
            "This update must roll back with the later conflict.",
        ),
        evidence(
            &fixture.owner,
            evidence_fact.clone(),
            evidence_event.clone(),
            "Update review evidence",
        ),
    )
    .expect("curation update operation");
    let stale_remove_id = derive_project_memory_fact_curation_child_operation_id(
        &outer_id,
        1,
        ProjectMemoryFactCurationMutationKindV1::Remove,
    )
    .expect("stable remove child identity");
    let stale_remove = ProjectMemoryFactCurationRemoveV1::new(
        ProjectMemoryFactRemoveCommandV1::new(
            remove_target.clone(),
            stale_remove_id.clone(),
            Some(remove_event.clone()),
            None,
        )
        .expect("stale remove command"),
        evidence(
            &fixture.owner,
            evidence_fact,
            evidence_event,
            "Remove review evidence",
        ),
    )
    .expect("stale remove operation");
    fixture
        .store()
        .update_project_memory_fact(
            update_command(
                remove_target,
                remove_event,
                "curation.prepare.advance-remove",
                "The remove target advanced after review.",
            ),
            &fixture.control,
        )
        .await
        .expect("advance remove target after review");
    let before = lineage_event_ids(&fixture).await;
    let request = ProjectMemoryFactCurationBatchV1::new(
        fixture.owner.clone(),
        outer_id.clone(),
        None,
        Confidence::new(0.8).expect("minimum curation confidence"),
        vec![
            ProjectMemoryFactCurationOperationV1::Update(update),
            ProjectMemoryFactCurationOperationV1::Remove(stale_remove),
        ],
    )
    .expect("atomic rollback curation batch");

    assert!(matches!(
        fixture
            .store()
            .apply_project_memory_fact_curation(request, &fixture.control)
            .await,
        Err(FactStoreError::CommitConflict { .. })
    ));
    assert_eq!(lineage_event_ids(&fixture).await, before);
    assert_eq!(
        operation_receipt_count(&fixture, &[&outer_id, &update_id, &stale_remove_id]).await,
        0,
        "outer and already-executed child receipts must roll back together"
    );
}

#[tokio::test]
async fn cancellation_before_admission_writes_no_child_or_outer_receipt() {
    let fixture = Fixture::new("cancelled-admission").await;
    let (target, target_event) = fixture.seed("cancelled-target").await;
    let outer_id = provenance_id("curation.outer.cancelled");
    let child_id = derive_project_memory_fact_curation_child_operation_id(
        &outer_id,
        0,
        ProjectMemoryFactCurationMutationKindV1::Update,
    )
    .expect("stable cancelled child identity");
    let update = ProjectMemoryFactCurationUpdateV1::new(
        update_command(
            target.clone(),
            target_event.clone(),
            child_id.as_str(),
            "Cancellation must keep this content out of canonical storage.",
        ),
        evidence(
            &fixture.owner,
            target,
            target_event,
            "Cancellation fixture evidence",
        ),
    )
    .expect("cancelled curation update");
    let request = ProjectMemoryFactCurationBatchV1::new(
        fixture.owner.clone(),
        outer_id.clone(),
        None,
        Confidence::new(0.8).expect("minimum curation confidence"),
        vec![ProjectMemoryFactCurationOperationV1::Update(update)],
    )
    .expect("cancelled curation batch");
    let cancelled = FactWriteControl::new(Arc::new(|| true), Arc::new(|| false));
    let before = lineage_event_ids(&fixture).await;

    assert!(matches!(
        fixture
            .store()
            .apply_project_memory_fact_curation(request, &cancelled)
            .await,
        Err(FactStoreError::Storage { .. })
    ));
    assert_eq!(lineage_event_ids(&fixture).await, before);
    assert_eq!(
        operation_receipt_count(&fixture, &[&outer_id, &child_id]).await,
        0
    );
}

#[tokio::test]
async fn cancellation_at_outer_commit_rolls_back_child_and_outer_receipts() {
    let fixture = Fixture::new("cancelled-commit").await;
    let (target, target_event) = fixture.seed("cancelled-commit-target").await;
    let outer_id = provenance_id("curation.outer.cancelled-commit");
    let child_id = derive_project_memory_fact_curation_child_operation_id(
        &outer_id,
        0,
        ProjectMemoryFactCurationMutationKindV1::Update,
    )
    .expect("stable commit-cancelled child identity");
    let update = ProjectMemoryFactCurationUpdateV1::new(
        update_command(
            target.clone(),
            target_event.clone(),
            child_id.as_str(),
            "Commit cancellation must roll this child mutation back.",
        ),
        evidence(
            &fixture.owner,
            target,
            target_event,
            "Commit cancellation evidence",
        ),
    )
    .expect("commit-cancelled curation update");
    let request = ProjectMemoryFactCurationBatchV1::new(
        fixture.owner.clone(),
        outer_id.clone(),
        None,
        Confidence::new(0.8).expect("minimum curation confidence"),
        vec![ProjectMemoryFactCurationOperationV1::Update(update)],
    )
    .expect("commit-cancelled curation batch");
    let denied_commit = FactWriteControl::new(Arc::new(|| false), Arc::new(|| false));
    let before = lineage_event_ids(&fixture).await;

    assert!(matches!(
        fixture
            .store()
            .apply_project_memory_fact_curation(request, &denied_commit)
            .await,
        Err(FactStoreError::Storage { .. })
    ));
    assert_eq!(lineage_event_ids(&fixture).await, before);
    assert_eq!(
        operation_receipt_count(&fixture, &[&outer_id, &child_id]).await,
        0
    );
}

#[test]
fn child_identity_is_stable_and_noncanonical_ids_are_rejected_structurally() {
    let owner = FactOwnerV1::Profile;
    let outer_id = provenance_id("curation.identity.outer");
    let target = ProjectMemoryFactIdV1::new(
        owner.clone(),
        tracedecay_domain::FactId::derive(
            &tracedecay_domain::FactIdentityMaterialV1::new(
                owner.clone(),
                tracedecay_domain::FactIdentitySourceV1::Application {
                    operation_id: provenance_id("curation.identity.fact"),
                },
            )
            .expect("identity material"),
        )
        .expect("derived fact identity"),
    )
    .expect("owner-bound identity target");
    let event = FactEventId::new("curation.identity.event".to_owned()).expect("event identity");
    let update = |operation_id: ProvenanceId| {
        ProjectMemoryFactCurationOperationV1::Update(
            ProjectMemoryFactCurationUpdateV1::new(
                update_command(
                    target.clone(),
                    event.clone(),
                    operation_id.as_str(),
                    "Stable child identity fixture.",
                ),
                evidence(
                    &owner,
                    target.clone(),
                    event.clone(),
                    "Stable identity evidence",
                ),
            )
            .expect("identity update operation"),
        )
    };
    let child_id = derive_project_memory_fact_curation_child_operation_id(
        &outer_id,
        0,
        ProjectMemoryFactCurationMutationKindV1::Update,
    )
    .expect("stable child identity");
    assert_eq!(
        child_id,
        derive_project_memory_fact_curation_child_operation_id(
            &outer_id,
            0,
            ProjectMemoryFactCurationMutationKindV1::Update,
        )
        .expect("repeat stable child identity")
    );
    assert_ne!(
        child_id,
        derive_project_memory_fact_curation_child_operation_id(
            &outer_id,
            1,
            ProjectMemoryFactCurationMutationKindV1::Update,
        )
        .expect("position-bound child identity")
    );
    assert_ne!(
        child_id,
        derive_project_memory_fact_curation_child_operation_id(
            &outer_id,
            0,
            ProjectMemoryFactCurationMutationKindV1::Remove,
        )
        .expect("kind-bound child identity")
    );

    assert!(matches!(
        ProjectMemoryFactCurationBatchV1::new(
            owner.clone(),
            outer_id.clone(),
            None,
            Confidence::new(0.8).expect("minimum confidence"),
            vec![update(outer_id.clone())],
        ),
        Err(FactStoreError::Contract(_))
    ));
    assert!(matches!(
        ProjectMemoryFactCurationBatchV1::new(
            owner.clone(),
            outer_id,
            None,
            Confidence::new(0.8).expect("minimum confidence"),
            vec![update(child_id.clone()), update(child_id)],
        ),
        Err(FactStoreError::Contract(_))
    ));
}
