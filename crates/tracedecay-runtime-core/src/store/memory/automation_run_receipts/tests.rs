use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tempfile::{TempDir, tempdir};
use tracedecay_domain::{Confidence, FactCategoryV1, FactOwnerV1, ProjectId, ProvenanceId, RunId};
use tracedecay_store::{
    FactReadControl, FactStoreError, FactWriteControl, MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS,
    ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryAutomaticFactReceiptV1,
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddMaterialV1,
    ProjectMemoryFactCurationBatchV1, ProjectMemoryFactCurationOperationV1, ProjectMemoryFactIdV1,
    ProjectMemoryFactNormalizeTagsV1, ProjectMemoryFactStore,
};

use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use crate::store::memory::DatabaseFactStore;
use crate::store::memory::automatic_facts::project_memory_automatic_fact_request_value;

use super::*;

async fn database(label: &str) -> (TempDir, Database) {
    let (directory, _path, database) = database_at(label).await;
    (directory, database)
}

async fn database_at(label: &str) -> (TempDir, PathBuf, Database) {
    let directory = tempdir().expect("create automation receipt recovery fixture directory");
    let path = directory.path().join(format!("{label}.db"));
    let authority = DatabaseAuthority::acquire_test(&path, "automation receipt recovery authority")
        .expect("acquire automation receipt recovery authority");
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .expect("publish automation receipt recovery runtime");
    (directory, path, database)
}

async fn reopen_database(path: &Path) -> Database {
    let authority = DatabaseAuthority::acquire_test(path, "automation receipt recovery reopen")
        .expect("reacquire automation receipt recovery authority");
    Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Existing)
        .await
        .expect("reopen existing automation receipt recovery runtime")
        .0
}

fn expect_storage_message<T>(result: Result<T, FactStoreError>, expected: &str) {
    match result {
        Err(FactStoreError::Storage { source, .. }) => assert_eq!(source.to_string(), expected),
        Err(error) => panic!("expected storage failure '{expected}', got {error}"),
        Ok(_) => panic!("expected storage failure '{expected}', got success"),
    }
}

fn read_control() -> FactReadControl {
    FactReadControl::new(Arc::new(|| false))
}

fn interrupt_on_check(cancel_at: usize) -> (Arc<AtomicUsize>, FactReadControl) {
    let checks = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&checks);
    let control = FactReadControl::new(Arc::new(move || {
        observed.fetch_add(1, Ordering::AcqRel) + 1 >= cancel_at
    }));
    (checks, control)
}

fn write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

fn fact_command(
    owner: FactOwnerV1,
    operation_id: &str,
    automation_run_id: Option<&RunId>,
) -> ProjectMemoryFactAddCommandV1 {
    let material = json!({
        "content": format!("Canonical automation receipt fixture {operation_id}."),
        "category": "project",
        "tags": ["automation-receipt-recovery"],
        "entities": ["TraceDecay"],
        "metadata": {"fixture": operation_id},
    });
    let sanitization_receipt = match sanitize_memory_fact_payload(material.clone())
        .expect("sanitize automation receipt fixture")
    {
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            assert_eq!(payload, material);
            receipt
        }
        MemoryFactSanitizationV1::Quarantined => {
            panic!("automation receipt fixture must remain durable")
        }
    };
    ProjectMemoryFactAddMaterialV1::new(
        owner,
        material["content"]
            .as_str()
            .expect("fixture content")
            .to_owned(),
        FactCategoryV1::Project,
        None,
        vec!["automation-receipt-recovery".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({"fixture": operation_id}),
        sanitization_receipt,
        automation_run_id.map(|run_id| run_id.as_str().to_owned()),
        Confidence::new(0.8).expect("fixture confidence"),
        None,
    )
    .and_then(|material| {
        material.into_command(
            ProvenanceId::new(operation_id.to_owned()).expect("fixture operation identity"),
        )
    })
    .expect("canonical automation receipt command")
}

async fn seed_automatic_receipt(
    store: &DatabaseFactStore<'_>,
    owner: FactOwnerV1,
    run_id: &RunId,
    apply_id: &str,
) -> ProjectMemoryAutomaticFactReceiptV1 {
    let apply_id = ProvenanceId::new(apply_id.to_owned()).expect("automatic apply identity");
    let operation_id = format!("{}.operation", apply_id.as_str());
    store
        .apply_project_memory_automatic_fact(
            apply_id,
            fact_command(owner, &operation_id, Some(run_id)),
            ProjectMemoryAutomaticFactEvidenceV1::new(
                Some(format!("{}.evidence", run_id.as_str())),
                Some(json!({"run_id": run_id.as_str()})),
                Some(json!({"validated": true})),
            )
            .expect("automatic receipt evidence"),
            &write_control(),
        )
        .await
        .expect("commit automatic fact receipt")
        .receipt()
        .clone()
}

async fn seed_quarantined_receipt(
    database: &Database,
    owner: FactOwnerV1,
    run_id: &RunId,
    apply_id: &str,
) -> ProjectMemoryAutomaticFactReceiptV1 {
    let command = fact_command(
        owner.clone(),
        &format!("{apply_id}.operation"),
        Some(run_id),
    );
    let evidence = ProjectMemoryAutomaticFactEvidenceV1::new(
        Some(format!("{apply_id}.evidence")),
        Some(json!({"run_id": run_id.as_str()})),
        Some(json!({"validated": false})),
    )
    .expect("quarantined automatic receipt evidence");
    let key = OwnerKey::new(&owner).expect("quarantined receipt owner key");
    let transaction = database
        .begin_memory_write_transaction(PROJECT_MEMORY_READ_OPERATION)
        .await
        .expect("begin quarantined receipt fixture transaction");
    transaction
        .execute(
            "INSERT INTO memory_v2_automatic_fact_receipts(
                apply_id, owner_kind, project_id, owner_json, idempotency_key,
                request_digest, request_json, evidence_json, state, quarantine_reason,
                applied_fact_id, applied_assertion_id, applied_event_id, recorded_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'quarantined',
                      'canonical fixture quarantine', NULL, NULL, NULL, 0)",
            params![
                apply_id,
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                command.operation_id().as_str(),
                command.input_digest(),
                project_memory_automatic_fact_request_value(&command).to_string(),
                serde_json::to_string(&evidence).expect("serialize quarantined evidence"),
            ],
        )
        .await
        .expect("insert quarantined receipt fixture");
    transaction
        .commit()
        .await
        .expect("commit quarantined receipt fixture");
    DatabaseFactStore::new(database)
        .get_project_memory_automatic_fact_receipt(
            owner,
            ProvenanceId::new(apply_id.to_owned()).expect("quarantined apply identity"),
            &read_control(),
        )
        .await
        .expect("read quarantined receipt fixture")
        .expect("quarantined receipt exists")
}

async fn seed_curation_receipt(
    store: &DatabaseFactStore<'_>,
    owner: FactOwnerV1,
    run_id: &RunId,
    suffix: &str,
) -> tracedecay_store::ProjectMemoryFactCurationReceiptV1 {
    let added = store
        .add_project_memory_fact(
            fact_command(owner.clone(), &format!("curation.seed.{suffix}"), None),
            &write_control(),
        )
        .await
        .expect("seed curation fact");
    let target = ProjectMemoryFactIdV1::new(owner.clone(), added.fact().fact_id().clone())
        .expect("owner-bound curation target");
    let reviewed = tracedecay_store::ProjectMemoryFactCurationReviewRefV1::new(
        target,
        added.commit_receipt().unwrap().last_event_id().clone(),
    );
    let request = ProjectMemoryFactCurationBatchV1::new(
        owner,
        ProvenanceId::new(format!("curation.operation.{suffix}"))
            .expect("curation operation identity"),
        None,
        Confidence::new(0.5).expect("curation minimum confidence"),
        vec![ProjectMemoryFactCurationOperationV1::NormalizeTags(
            ProjectMemoryFactNormalizeTagsV1::new(
                reviewed.clone(),
                vec![format!("normalized-{suffix}")],
                vec![reviewed],
                Confidence::new(0.9).expect("curation confidence"),
            )
            .expect("normalization operation"),
        )],
    )
    .and_then(|request| request.with_automation_run_id(run_id.clone()))
    .expect("automation-bound curation request");
    store
        .apply_project_memory_fact_curation(request, &write_control())
        .await
        .expect("commit curation receipt")
}

#[tokio::test]
async fn postcommit_recovery_returns_exact_nonempty_automatic_receipts_deterministically() {
    let (_directory, database) = database("postcommit-automatic").await;
    let store = DatabaseFactStore::new(&database);
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.automatic-recovery").expect("automation run identity");
    let first = seed_automatic_receipt(&store, owner.clone(), &run_id, "automatic.apply.z").await;
    let second = seed_automatic_receipt(&store, owner.clone(), &run_id, "automatic.apply.a").await;
    let quarantined = seed_quarantined_receipt(
        &database,
        owner.clone(),
        &run_id,
        "automatic.apply.quarantined",
    )
    .await;

    let recovered = store
        .project_memory_automation_run_receipts(owner.clone(), run_id.clone(), &read_control())
        .await
        .expect("recover committed automatic receipts");
    let replayed = store
        .project_memory_automation_run_receipts(owner, run_id, &read_control())
        .await
        .expect("replay committed automatic receipts");

    let mut expected = vec![first, second, quarantined];
    expected.sort_by(|left, right| {
        (left.recorded_at(), left.apply_id()).cmp(&(right.recorded_at(), right.apply_id()))
    });
    assert_eq!(recovered.automatic_fact_receipts(), expected);
    assert_eq!(replayed, recovered);
    let durable_results = recovered
        .automatic_fact_results()
        .expect("reconstitute deterministic durable results");
    assert_eq!(
        durable_results
            .iter()
            .filter(|result| matches!(
                result.disposition(),
                tracedecay_store::ProjectMemoryAutomaticFactApplyDispositionV1::Applied
            ))
            .count(),
        2
    );
    assert_eq!(
        durable_results
            .iter()
            .filter(|result| matches!(
                result.disposition(),
                tracedecay_store::ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined
            ))
            .count(),
        1
    );
    assert!(recovered.curation_receipt().is_none());
    assert!(!recovered.is_empty());
}

#[tokio::test]
async fn committed_automatic_result_recovers_with_digest_parity_after_physical_reopen() {
    let (_directory, path, database) = database_at("physical-reopen").await;
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.physical-reopen").expect("automation run identity");
    let receipt = seed_automatic_receipt(
        &DatabaseFactStore::new(&database),
        owner.clone(),
        &run_id,
        "automatic.apply.physical-reopen",
    )
    .await;
    let expected = tracedecay_store::ProjectMemoryAutomaticFactApplyResultV1::new(
        receipt,
        tracedecay_store::ProjectMemoryAutomaticFactApplyDispositionV1::Applied,
    )
    .expect("canonical committed automatic result");
    let expected_digest = expected
        .canonical_digest()
        .expect("digest committed automatic result");
    drop(database);

    let reopened = reopen_database(&path).await;
    let recovered = DatabaseFactStore::new(&reopened)
        .project_memory_automation_run_receipts(owner, run_id, &read_control())
        .await
        .expect("recover automatic result after physical reopen")
        .automatic_fact_results()
        .expect("project recovered durable result");

    assert_eq!(recovered, vec![expected]);
    assert_eq!(
        recovered[0]
            .canonical_digest()
            .expect("digest recovered automatic result"),
        expected_digest
    );
}

#[tokio::test]
async fn same_owner_and_run_are_isolated_across_two_physical_databases() {
    let (_left_directory, left_database) = database("physical-isolation-left").await;
    let (_right_directory, right_database) = database("physical-isolation-right").await;
    let left_store = DatabaseFactStore::new(&left_database);
    let right_store = DatabaseFactStore::new(&right_database);
    let run_id = RunId::new("run.same-physical-identity").expect("automation run identity");
    let project_owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.same-physical-identity").expect("project identity"),
    };

    let left_profile = seed_automatic_receipt(
        &left_store,
        FactOwnerV1::Profile,
        &run_id,
        "automatic.apply.left.profile",
    )
    .await;
    let right_profile = seed_automatic_receipt(
        &right_store,
        FactOwnerV1::Profile,
        &run_id,
        "automatic.apply.right.profile",
    )
    .await;
    let left_project = seed_automatic_receipt(
        &left_store,
        project_owner.clone(),
        &run_id,
        "automatic.apply.left.project",
    )
    .await;
    let right_project = seed_automatic_receipt(
        &right_store,
        project_owner.clone(),
        &run_id,
        "automatic.apply.right.project",
    )
    .await;

    for (store, owner, expected) in [
        (&left_store, FactOwnerV1::Profile, left_profile),
        (&right_store, FactOwnerV1::Profile, right_profile),
        (&left_store, project_owner.clone(), left_project),
        (&right_store, project_owner, right_project),
    ] {
        let recovered = store
            .project_memory_automation_run_receipts(owner, run_id.clone(), &read_control())
            .await
            .expect("recover exact physical authority receipt");
        assert_eq!(recovered.automatic_fact_receipts(), [expected]);
    }
}

#[tokio::test]
async fn exact_run_lookup_plans_use_the_canonical_expression_indexes() {
    let (_directory, database) = database("automation-run-query-plan").await;
    let run_id = RunId::new("run.query-plan").expect("automation run identity");
    let snapshot = database
        .begin_memory_read_transaction(PROJECT_MEMORY_READ_OPERATION)
        .await
        .expect("begin query-plan snapshot");

    let mut curation_plan = snapshot
        .query(
            "EXPLAIN QUERY PLAN
             SELECT operation_id
             FROM memory_v2_operation_receipts
             WHERE owner_kind = ?1 AND project_id = ?2 AND operation_kind = 'curation'
               AND json_extract(receipt_json, '$.automation_run_id') = ?3
             ORDER BY recorded_at ASC, operation_id ASC LIMIT 2",
            params!["profile", "", run_id.as_str()],
        )
        .await
        .expect("explain curation recovery lookup");
    let mut curation_details = Vec::new();
    while let Some(row) = curation_plan
        .next()
        .await
        .expect("read curation query plan")
    {
        curation_details.push(row_string(&row, 3, PROJECT_MEMORY_READ_OPERATION).unwrap());
    }
    drop(curation_plan);
    assert!(
        curation_details
            .iter()
            .any(|detail| detail.contains("idx_memory_v2_operation_receipts_automation_run")),
        "curation recovery plan must use the run index: {curation_details:?}"
    );

    let mut automatic_plan = snapshot
        .query(
            "EXPLAIN QUERY PLAN
             SELECT apply_id
             FROM memory_v2_automatic_fact_receipts
             WHERE owner_kind = ?1 AND project_id = ?2 AND owner_json = ?3
               AND json_extract(request_json, '$.automation_run_id') = ?4
             ORDER BY recorded_at ASC, apply_id ASC LIMIT ?5",
            params![
                "profile",
                "",
                serde_json::to_string(&FactOwnerV1::Profile).expect("serialize profile owner"),
                run_id.as_str(),
                i64::try_from(MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS + 1)
                    .expect("bounded automatic fetch limit"),
            ],
        )
        .await
        .expect("explain automatic recovery lookup");
    let mut automatic_details = Vec::new();
    while let Some(row) = automatic_plan
        .next()
        .await
        .expect("read automatic query plan")
    {
        automatic_details.push(row_string(&row, 3, PROJECT_MEMORY_READ_OPERATION).unwrap());
    }
    drop(automatic_plan);
    assert!(
        automatic_details
            .iter()
            .any(|detail| detail.contains("idx_memory_v2_automatic_fact_receipts_automation_run")),
        "automatic recovery plan must use the run index: {automatic_details:?}"
    );
    snapshot.commit().await.expect("finish query-plan snapshot");
}

#[tokio::test]
async fn exact_curation_receipt_is_recovered_from_the_immutable_operation_receipt() {
    let (_directory, database) = database("postcommit-curation").await;
    let store = DatabaseFactStore::new(&database);
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.curation-recovery").expect("automation run identity");
    let expected = seed_curation_receipt(&store, owner.clone(), &run_id, "exact").await;

    let recovered = store
        .project_memory_automation_run_receipts(owner, run_id, &read_control())
        .await
        .expect("recover committed curation receipt");

    assert_eq!(recovered.curation_receipt(), Some(&expected));
    assert!(recovered.automatic_fact_receipts().is_empty());
    assert!(!recovered.is_empty());
}

#[tokio::test]
async fn zero_receipt_is_proven_for_an_exact_foreign_run_owner_and_project() {
    let (_directory, database) = database("proven-empty-isolation").await;
    let store = DatabaseFactStore::new(&database);
    let committed_run = RunId::new("run.committed").expect("committed run identity");
    seed_automatic_receipt(
        &store,
        FactOwnerV1::Profile,
        &committed_run,
        "automatic.apply.committed",
    )
    .await;

    let foreign_run = store
        .project_memory_automation_run_receipts(
            FactOwnerV1::Profile,
            RunId::new("run.foreign").expect("foreign run identity"),
            &read_control(),
        )
        .await
        .expect("read exact foreign run");
    assert!(foreign_run.is_empty());

    let foreign_owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.foreign").expect("foreign project identity"),
    };
    let snapshot = database
        .begin_memory_read_transaction(PROJECT_MEMORY_READ_OPERATION)
        .await
        .expect("begin exact foreign-owner read");
    let foreign_project = project_memory_automation_run_receipts_tx(
        &snapshot,
        &foreign_owner,
        &committed_run,
        &read_control(),
    )
    .await
    .expect("read exact foreign project");
    snapshot.commit().await.expect("finish foreign-owner read");
    assert!(foreign_project.is_empty());
}

#[tokio::test]
async fn multiple_curation_receipts_for_one_run_fail_closed() {
    let (_directory, database) = database("ambiguous-curation").await;
    let store = DatabaseFactStore::new(&database);
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.ambiguous-curation").expect("automation run identity");
    seed_curation_receipt(&store, owner.clone(), &run_id, "first").await;
    seed_curation_receipt(&store, owner.clone(), &run_id, "second").await;

    let result = store
        .project_memory_automation_run_receipts(owner, run_id, &read_control())
        .await;

    assert!(matches!(
        result,
        Err(FactStoreError::BatchLimitExceeded {
            field: "memory automation curation receipts",
            count: 2,
            max: 1,
        })
    ));
}

#[tokio::test]
async fn rebound_curation_envelope_is_rejected() {
    let (_directory, database) = database("rebound-curation-envelope").await;
    let store = DatabaseFactStore::new(&database);
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.rebound-curation").expect("automation run identity");
    let receipt = seed_curation_receipt(&store, owner.clone(), &run_id, "rebound").await;
    let transaction = database
        .begin_memory_write_transaction(PROJECT_MEMORY_READ_OPERATION)
        .await
        .expect("begin curation envelope corruption");
    transaction
        .execute_batch("DROP TRIGGER memory_v2_operation_receipts_no_update;")
        .await
        .expect("disable operation receipt immutability in isolated fixture");
    assert_eq!(
        transaction
            .execute(
                "UPDATE memory_v2_operation_receipts
                 SET request_digest = ?1
                 WHERE owner_kind = 'profile' AND project_id = '' AND operation_id = ?2",
                params!["0".repeat(64), receipt.operation_id().as_str()],
            )
            .await
            .expect("corrupt curation receipt envelope"),
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
        .expect("restore operation receipt immutability");
    transaction
        .commit()
        .await
        .expect("commit curation envelope corruption");

    let result = store
        .project_memory_automation_run_receipts(owner, run_id, &read_control())
        .await;

    expect_storage_message(
        result,
        "memory automation curation receipt does not match its immutable envelope",
    );
}

#[tokio::test]
async fn rebound_automatic_request_digest_is_rejected() {
    let (_directory, database) = database("rebound-automatic-digest").await;
    let store = DatabaseFactStore::new(&database);
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.rebound-automatic").expect("automation run identity");
    let receipt =
        seed_automatic_receipt(&store, owner.clone(), &run_id, "automatic.apply.rebound").await;
    let transaction = database
        .begin_memory_write_transaction(PROJECT_MEMORY_READ_OPERATION)
        .await
        .expect("begin automatic receipt corruption");
    transaction
        .execute_batch("DROP TRIGGER memory_v2_automatic_fact_receipts_no_update;")
        .await
        .expect("disable automatic receipt immutability in isolated fixture");
    assert_eq!(
        transaction
            .execute(
                "UPDATE memory_v2_automatic_fact_receipts
                 SET request_digest = ?1
                 WHERE owner_kind = 'profile' AND project_id = '' AND apply_id = ?2",
                params!["0".repeat(64), receipt.apply_id().as_str()],
            )
            .await
            .expect("corrupt automatic receipt digest"),
        1
    );
    transaction
        .execute_batch(
            "CREATE TRIGGER memory_v2_automatic_fact_receipts_no_update
             BEFORE UPDATE ON memory_v2_automatic_fact_receipts BEGIN
                 SELECT RAISE(ABORT, 'memory_v2 automatic fact receipts are immutable');
             END;",
        )
        .await
        .expect("restore automatic receipt immutability");
    transaction
        .commit()
        .await
        .expect("commit automatic receipt corruption");

    let result = store
        .project_memory_automation_run_receipts(owner, run_id, &read_control())
        .await;

    expect_storage_message(
        result,
        "memory automation automatic fact receipt does not match its immutable request digest",
    );
}

#[tokio::test]
async fn missing_automatic_operation_envelope_is_rejected() {
    let (_directory, database) = database("missing-automatic-envelope").await;
    let store = DatabaseFactStore::new(&database);
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.missing-automatic-envelope").expect("automation run identity");
    let receipt = seed_automatic_receipt(
        &store,
        owner.clone(),
        &run_id,
        "automatic.apply.missing-envelope",
    )
    .await;
    let transaction = database
        .begin_memory_write_transaction(PROJECT_MEMORY_READ_OPERATION)
        .await
        .expect("begin missing automatic envelope fixture");
    transaction
        .execute_batch("DROP TRIGGER memory_v2_operation_receipts_no_delete;")
        .await
        .expect("disable envelope delete protection in isolated fixture");
    assert_eq!(
        transaction
            .execute(
                "DELETE FROM memory_v2_operation_receipts
                 WHERE owner_kind = 'profile' AND project_id = '' AND operation_id = ?1",
                params![receipt.request().operation_id().as_str()],
            )
            .await
            .expect("remove automatic operation envelope"),
        1
    );
    transaction
        .execute_batch(
            "CREATE TRIGGER memory_v2_operation_receipts_no_delete
             BEFORE DELETE ON memory_v2_operation_receipts BEGIN
                 SELECT RAISE(ABORT, 'memory_v2 operation receipts are immutable');
             END;",
        )
        .await
        .expect("restore envelope delete protection");
    transaction
        .commit()
        .await
        .expect("commit missing automatic envelope fixture");

    expect_storage_message(
        store
            .project_memory_automation_run_receipts(owner, run_id, &read_control())
            .await,
        "memory automation automatic fact receipt does not match its immutable operation envelope",
    );
}

#[tokio::test]
async fn rebound_automatic_operation_envelope_is_rejected() {
    let (_directory, database) = database("rebound-automatic-envelope").await;
    let store = DatabaseFactStore::new(&database);
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.rebound-automatic-envelope").expect("automation run identity");
    let receipt = seed_automatic_receipt(
        &store,
        owner.clone(),
        &run_id,
        "automatic.apply.rebound-envelope",
    )
    .await;
    let transaction = database
        .begin_memory_write_transaction(PROJECT_MEMORY_READ_OPERATION)
        .await
        .expect("begin rebound automatic envelope fixture");
    transaction
        .execute_batch("DROP TRIGGER memory_v2_operation_receipts_no_update;")
        .await
        .expect("disable envelope update protection in isolated fixture");
    assert_eq!(
        transaction
            .execute(
                "UPDATE memory_v2_operation_receipts
                 SET receipt_json = ?1
                 WHERE owner_kind = 'profile' AND project_id = '' AND operation_id = ?2",
                params![
                    json!({"apply_id": "automatic.apply.foreign", "state": "applied"}).to_string(),
                    receipt.request().operation_id().as_str(),
                ],
            )
            .await
            .expect("rebind automatic operation envelope"),
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
        .expect("restore envelope update protection");
    transaction
        .commit()
        .await
        .expect("commit rebound automatic envelope fixture");

    expect_storage_message(
        store
            .project_memory_automation_run_receipts(owner, run_id, &read_control())
            .await,
        "memory automation automatic fact receipt does not match its immutable operation envelope",
    );
}

#[tokio::test]
async fn recovery_observes_preinterrupted_and_live_midscan_cancellation() {
    let (_directory, database) = database("recovery-cancellation").await;
    let store = DatabaseFactStore::new(&database);
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.recovery-cancellation").expect("automation run identity");
    seed_automatic_receipt(
        &store,
        owner.clone(),
        &run_id,
        "automatic.apply.cancel.first",
    )
    .await;
    seed_automatic_receipt(
        &store,
        owner.clone(),
        &run_id,
        "automatic.apply.cancel.second",
    )
    .await;

    let preinterrupted = FactReadControl::new(Arc::new(|| true));
    assert!(matches!(
        store
            .project_memory_automation_run_receipts(owner.clone(), run_id.clone(), &preinterrupted,)
            .await,
        Err(FactStoreError::ReadCancelled)
    ));

    let (checks, midscan) = interrupt_on_check(5);
    assert!(matches!(
        store
            .project_memory_automation_run_receipts(owner, run_id, &midscan)
            .await,
        Err(FactStoreError::ReadCancelled)
    ));
    assert_eq!(checks.load(Ordering::Acquire), 5);
}

#[tokio::test]
async fn automatic_receipt_overflow_fails_instead_of_truncating() {
    let (_directory, database) = database("automatic-overflow").await;
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.automatic-overflow").expect("automation run identity");
    let key = OwnerKey::new(&owner).expect("profile owner key");
    let transaction = database
        .begin_memory_write_transaction(PROJECT_MEMORY_READ_OPERATION)
        .await
        .expect("begin overflow fixture transaction");
    for index in 0..=MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS {
        transaction
            .execute(
                "INSERT INTO memory_v2_automatic_fact_receipts(
                    apply_id, owner_kind, project_id, owner_json, idempotency_key,
                    request_digest, request_json, evidence_json, state, quarantine_reason,
                    applied_fact_id, applied_assertion_id, applied_event_id, recorded_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, '{}', 'quarantined',
                          'bounded fixture', NULL, NULL, NULL, ?8)",
                params![
                    format!("overflow.apply.{index:03}"),
                    key.kind,
                    key.project_id.as_str(),
                    key.json.as_str(),
                    format!("overflow.operation.{index:03}"),
                    format!("overflow-digest-{index:03}"),
                    json!({"automation_run_id": run_id.as_str()}).to_string(),
                    i64::try_from(index).expect("bounded fixture timestamp"),
                ],
            )
            .await
            .expect("insert bounded overflow fixture row");
    }
    transaction
        .commit()
        .await
        .expect("commit overflow fixture rows");

    let result = DatabaseFactStore::new(&database)
        .project_memory_automation_run_receipts(owner, run_id, &read_control())
        .await;

    assert!(matches!(
        result,
        Err(FactStoreError::BatchLimitExceeded {
            field: "memory automation automatic fact receipts",
            count,
            max: MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS,
        }) if count == MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS + 1
    ));
}
