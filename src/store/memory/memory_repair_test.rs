use super::*;

use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};
use tempfile::tempdir;

#[tokio::test]
async fn compatibility_repair_rolls_back_feedback_batch_and_replays_receipt() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("compatibility-repair.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "compatibility repair authority test").unwrap();
    let (db, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .unwrap();
    let owner = FactOwnerV1::Profile;
    let source_store_id = compatibility_source_store_id().unwrap();
    let owner_key = OwnerKey::new(&owner).unwrap();
    {
        let writer = db
            .writer_connection("seed compatibility repair rollback test")
            .await
            .unwrap();
        writer
            .execute_engine(
                "INSERT INTO memory_facts(fact_id, content)
                 VALUES(1, 'repair feedback fixture')",
                (),
            )
            .await
            .unwrap();
        writer
            .execute_engine(
                "INSERT INTO memory_feedback_events(
                    event_id, fact_id, action, trust_delta, old_trust, new_trust,
                    created_at, source, note
                 ) VALUES(1, 1, 'helpful', -0.1, 0.5, 0.4, 1, 'mcp', NULL)",
                (),
            )
            .await
            .unwrap();
        writer
            .execute_engine(
                "INSERT INTO memory_v2_facts(
                    fact_id, owner_kind, project_id, owner_json, identity_json, created_at
                 ) VALUES('repair-feedback-fixture', ?1, ?2, ?3, '{}', 1)",
                params![
                    owner_key.kind,
                    owner_key.project_id.as_str(),
                    owner_key.json.as_str(),
                ],
            )
            .await
            .unwrap();
        writer
            .execute_engine(
                "INSERT INTO memory_v2_legacy_map(
                    owner_kind, project_id, owner_json, source_store_id,
                    legacy_fact_id, fact_id, mapping_json
                 ) VALUES(?1, ?2, ?3, ?4, 1, 'repair-feedback-fixture', '{}')",
                params![
                    owner_key.kind,
                    owner_key.project_id.as_str(),
                    owner_key.json.as_str(),
                    source_store_id.as_str(),
                ],
            )
            .await
            .unwrap();
        writer
            .execute_engine(
                "INSERT INTO memory_v2_feedback_history_repair_progress(
                    owner_kind, project_id, source_store_id, owner_json,
                    feedback_frontier, feedback_cursor, phase,
                    started_at, updated_at, completed_at
                 ) VALUES(?1, ?2, ?3, ?4, 2, 0, 'pending', 1, 1, NULL)",
                params![
                    owner_key.kind,
                    owner_key.project_id.as_str(),
                    source_store_id.as_str(),
                    owner_key.json.as_str(),
                ],
            )
            .await
            .unwrap();
    }

    let store = DatabaseFactStore::new(&db);
    let db_for_failure = db.clone();
    let owner_for_failure = owner.clone();
    let source_for_failure = source_store_id.clone();
    let failed: FactCompatibilityResult<()> = store
        .compatibility_write(move |transaction| {
            Box::pin(async move {
                let outcome = db_for_failure
                    .repair_memory_v2_feedback_history_batch_in_transaction(
                        transaction,
                        &owner_for_failure,
                        &source_for_failure,
                        512,
                    )
                    .await
                    .map_err(|error| {
                        FactCompatibilityStoreError::Store(storage_error(
                            COMPATIBILITY_WRITE_OPERATION,
                            error,
                        ))
                    })?;
                assert_eq!(
                    outcome,
                    MemoryV2FeedbackHistoryRepairBatchOutcome::Advanced { processed: 1 }
                );
                Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "force compatibility repair transaction rollback",
                )
                .into())
            })
        })
        .await;
    assert!(failed.is_err());

    let progress = db
        .feedback_history_repair_progress(&owner, &source_store_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.feedback_frontier, 2);
    assert_eq!(progress.feedback_cursor, 0);
    assert!(!progress.complete);

    let operation_id = ProvenanceId::new("compatibility-repair-atomic-replay".to_owned()).unwrap();
    let request_digest = compatibility_repair_request_digest(
        &CompatibilityMemoryRepairCommandV1::new(owner.clone(), operation_id.clone(), None)
            .unwrap(),
    )
    .unwrap();
    let first = store
        .repair_compatibility_memory(
            CompatibilityMemoryRepairCommandV1::new(owner.clone(), operation_id.clone(), None)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        first.feedback_history_repair(),
        CompatibilityFeedbackRepairProgressV1::Incomplete {
            processed: 1,
            remaining: Some(1),
        }
    );
    let replay = store
        .repair_compatibility_memory(
            CompatibilityMemoryRepairCommandV1::new(owner.clone(), operation_id.clone(), None)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay, first);
    let progress = db
        .feedback_history_repair_progress(&owner, &source_store_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(progress.feedback_cursor, 1);
    assert!(!progress.complete);

    let receipt_owner = owner.clone();
    let receipt_operation_id = operation_id.clone();
    let receipt = store
        .compatibility_read(move |transaction| {
            Box::pin(async move {
                compatibility_lookup_operation_receipt_tx(
                    transaction,
                    &receipt_owner,
                    &receipt_operation_id,
                    "repair",
                    &request_digest,
                )
                .await
                .map_err(Into::into)
            })
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        compatibility_receipt_feedback_history_repair(&receipt.receipt).unwrap(),
        CompatibilityFeedbackRepairProgressV1::Incomplete {
            processed: 1,
            remaining: Some(1),
        }
    );
}
