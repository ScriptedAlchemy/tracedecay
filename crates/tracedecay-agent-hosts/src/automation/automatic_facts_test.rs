use std::path::Path;

use super::*;
use crate::application::memory::MemoryApplication;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::store::memory::DatabaseFactStore;
use tracedecay_domain::FactOwnerV1;

async fn database(path: &Path, mode: TestDatabaseRuntimeMode) -> Database {
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(path, "automatic fact receipt test").unwrap();
    Database::publish_test_runtime(path, &authority, mode)
        .await
        .unwrap()
        .0
}

fn request(content: &str) -> AddFactRequest {
    AddFactRequest {
        content: content.to_string(),
        category: MemoryCategory::Project,
        source: Some("automatic-fact-test".to_string()),
        tags: vec!["automation".to_string()],
        entities: vec!["TraceDecay".to_string()],
        trust: Some(0.9),
        metadata: serde_json::json!({"fixture": "automatic-fact-receipt"}),
    }
}

fn admitted_fact(content: &str, validation: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "add_fact_request": request(content),
        "validation": validation,
    })
}

#[tokio::test]
async fn automatic_apply_commits_a_terminal_receipt_with_canonical_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let admitted = admitted_fact(
        "Keep automatic fact effects in the canonical memory authority",
        serde_json::json!({"dedupe": {"source_index": 3}}),
    );

    let batch = record_session_automatic_facts(
        &memory,
        "run-terminal-effect",
        Some("evidence-hash-123"),
        &[admitted],
    )
    .await
    .unwrap();

    assert!(batch.retry_error.is_none());
    assert_eq!(batch.receipts.len(), 1);
    let receipt = &batch.receipts[0];
    assert_eq!(receipt.state, AutomaticFactState::Applied);
    assert_eq!(receipt.run_id, "run-terminal-effect");
    assert_eq!(receipt.evidence_hash.as_deref(), Some("evidence-hash-123"));
    assert_eq!(
        receipt.validation,
        Some(serde_json::json!({"dedupe": {"source_index": 3}}))
    );
    assert!(receipt.applied_canonical_fact_id.is_some());
    assert!(receipt.applied_fact_id.is_some());

    let loaded = load_automatic_fact_receipt(&memory, &receipt.apply_id)
        .await
        .unwrap();
    assert_eq!(loaded.as_ref(), Some(receipt));
    assert_eq!(
        list_automatic_fact_receipts(&memory, Some(AutomaticFactState::Applied), 10)
            .await
            .unwrap(),
        batch.receipts
    );
    assert!(
        list_automatic_fact_receipts(&memory, Some(AutomaticFactState::Quarantined), 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn automatic_apply_replays_the_exact_terminal_effect_without_another_fact() {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("memory.db");
    let db = database(&database_path, TestDatabaseRuntimeMode::Initialize).await;
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
    let admitted = admitted_fact(
        "Replay this exact terminal automatic fact effect once",
        serde_json::json!({"dedupe": {"source_index": 0}}),
    );

    let first = record_session_automatic_facts(
        &memory,
        "run-exact-replay",
        Some("evidence-hash-replay"),
        std::slice::from_ref(&admitted),
    )
    .await
    .unwrap();
    drop(memory);
    drop(db);

    let db = database(&database_path, TestDatabaseRuntimeMode::Existing).await;
    let memory = MemoryApplication::new(owner, DatabaseFactStore::new(&db)).unwrap();
    let replay = record_session_automatic_facts(
        &memory,
        "run-exact-replay",
        Some("evidence-hash-replay"),
        &[admitted],
    )
    .await
    .unwrap();

    assert!(first.retry_error.is_none());
    assert!(replay.retry_error.is_none());
    assert_eq!(replay.receipts, first.receipts);
    assert_eq!(
        memory
            .list_facts_untracked(None, None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn automatic_apply_collapses_semantic_duplicates_without_losing_first_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let first = admitted_fact(
        "Keep terminal receipt evidence with the first automatic fact effect",
        serde_json::json!({"source_index": 0}),
    );
    let duplicate = admitted_fact(
        "  keep terminal receipt evidence with the FIRST automatic fact effect  ",
        serde_json::json!({"source_index": 1}),
    );

    let batch = record_session_automatic_facts(
        &memory,
        "run-semantic-duplicate",
        Some("evidence-hash-duplicate"),
        &[first, duplicate],
    )
    .await
    .unwrap();

    assert!(batch.retry_error.is_none());
    assert_eq!(batch.receipts.len(), 1);
    assert_eq!(
        batch.receipts[0].validation,
        Some(serde_json::json!({"source_index": 0}))
    );
    assert_eq!(
        list_automatic_fact_receipts(&memory, None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        memory
            .list_facts_untracked(None, None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn invalid_automatic_command_uses_the_memory_application_error_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let mut invalid_request = request("Reject invalid automatic fact trust at the typed boundary");
    invalid_request.trust = Some(1.1);
    let admitted = serde_json::json!({
        "add_fact_request": invalid_request,
        "validation": {"status": "accepted"},
    });

    let error = match record_session_automatic_facts(
        &memory,
        "run-invalid-command",
        Some("evidence-hash-invalid-command"),
        &[admitted],
    )
    .await
    {
        Ok(_) => panic!("invalid automatic command must fail before authority apply"),
        Err(error) => error,
    };

    let TraceDecayError::Database { operation, message } = error else {
        panic!("memory application error must retain its canonical classification");
    };
    assert_eq!(operation, "memory application");
    assert!(message.contains("trust must be between 0.0 and 1.0"));
}

#[test]
fn automatic_fact_state_serializes_only_terminal_values() {
    for (state, wire) in [
        (AutomaticFactState::Applied, "applied"),
        (AutomaticFactState::Quarantined, "quarantined"),
    ] {
        assert_eq!(
            serde_json::to_value(state).unwrap(),
            serde_json::json!(wire)
        );
        assert_eq!(
            serde_json::from_value::<AutomaticFactState>(serde_json::json!(wire)).unwrap(),
            state
        );
    }
    assert!(AutomaticFactState::parse("retry").is_err());
}
